use std::io::Read;

use crate::codecs::bcj;
use crate::codecs::bcj::bcj2::Bcj2Decoder;
use crate::codecs::lzma::lzma2::{Lzma2Decoder, dictionary_size};
use crate::codecs::lzma::{LzmaDecoder, Properties};
use crate::codecs::ppmd;
use crate::codecs::{bzip2, deflate};
use crate::crypto::Password;
use crate::crypto::sevenz_aes::{self, CbcDecryptReader};
use crate::sevenz::folder::{Coder, Folder};
use crate::sevenz::spec::Codec;
use crate::utils::error::{Error, Result, Unsupported};
use crate::utils::io::Limited;

pub type Stream<'a> = Box<dyn Read + Send + 'a>;

pub fn decoder<'a>(folder: &Folder, packed: Vec<Stream<'a>>, password: Option<&Password>) -> Result<Stream<'a>> {
    if packed.len() != folder.packed_indices.len() {
        return Err(Error::malformed(format!("7z folder takes {} packed streams but {} were opened", folder.packed_indices.len(), packed.len())));
    }

    let mut slots: Vec<Option<Stream<'a>>> = packed.into_iter().map(Some).collect();
    build(folder, folder.main_out_stream()?, &mut slots, 0, password)
}

fn build<'a>(folder: &Folder, out_index: usize, slots: &mut Vec<Option<Stream<'a>>>, depth: usize, password: Option<&Password>) -> Result<Stream<'a>> {
    if depth > folder.coders.len() {
        return Err(Error::malformed("7z folder binds its coders into a cycle"));
    }

    let coder_index = folder.coder_of_out_stream(out_index)?;
    let coder = &folder.coders[coder_index];
    if coder.out_streams != 1 {
        return Err(Error::Unsupported(Unsupported::Other("a 7z coder with more than one output stream, which no released 7-Zip writes")));
    }

    let first_in = folder.first_in_stream_of(coder_index);
    let mut inputs = Vec::with_capacity(coder.in_streams);
    for stream in first_in..first_in + coder.in_streams {
        inputs.push(match folder.bind_pair_for_in_stream(stream) {
            Some(pair) => build(folder, pair.out_index, slots, depth + 1, password)?,
            None => {
                let slot = folder.packed_stream_at(stream).ok_or_else(|| Error::malformed("7z folder leaves a coder input unconnected"))?;
                slots.get_mut(slot).and_then(Option::take).ok_or_else(|| Error::malformed("7z folder takes the same packed stream twice"))?
            }
        });
    }

    let size = *folder.unpack_sizes.get(out_index).ok_or_else(|| Error::malformed("7z folder is missing a coder's unpacked size"))?;
    coder_reader(coder, inputs, size, password)
}

fn coder_reader<'a>(coder: &Coder, inputs: Vec<Stream<'a>>, size: u64, password: Option<&Password>) -> Result<Stream<'a>> {
    let single = |mut inputs: Vec<Stream<'a>>| -> Result<Stream<'a>> {
        match inputs.len() {
            1 => Ok(inputs.pop().expect("one input")),
            other => Err(Error::malformed(format!("7z {} coder takes {other} inputs, not one", coder.codec.name()))),
        }
    };

    match coder.codec {
        Codec::Copy => Ok(Box::new(Limited::new(single(inputs)?, size))),
        Codec::Lzma => {
            let props: [u8; 5] = coder.properties[..]
                .try_into()
                .map_err(|_| Error::malformed(format!("7z LZMA coder carries {} property bytes; the format has 5", coder.properties.len())))?;
            Ok(Box::new(LzmaDecoder::new(single(inputs)?, Properties::from_bytes(props)?, Some(size))?))
        }
        Codec::Lzma2 => {
            let byte = *coder.properties.first().ok_or_else(|| Error::malformed("7z LZMA2 coder carries no dictionary size"))?;
            Ok(Box::new(Limited::new(Lzma2Decoder::new(single(inputs)?, dictionary_size(byte)?), size)))
        }
        Codec::Ppmd => Ok(Box::new(ppmd::h::from_properties(single(inputs)?, &coder.properties, size)?)),
        Codec::Bzip2 => Ok(Box::new(Limited::new(bzip2::Bzip2Reader::new(single(inputs)?), size))),
        Codec::Deflate => Ok(Box::new(Limited::new(deflate::InflateReader::new(single(inputs)?), size))),
        Codec::Deflate64 => Ok(Box::new(Limited::new(deflate::InflateReader::deflate64(single(inputs)?), size))),
        Codec::Delta => Ok(Box::new(Filtered::new(single(inputs)?, bcj::DELTA, coder.properties.clone(), size)?)),
        Codec::Branch(branch) => Ok(Box::new(Filtered::new(single(inputs)?, branch.xz_filter_id(), coder.properties.clone(), size)?)),
        Codec::Bcj2 => {
            if inputs.len() != 4 {
                return Err(Error::malformed(format!("7z BCJ2 coder takes {} inputs, not four", inputs.len())));
            }
            let mut inputs = inputs.into_iter();
            let (main, call, jump, range) = (inputs.next(), inputs.next(), inputs.next(), inputs.next());
            let four = main.zip(call).zip(jump).zip(range).expect("four inputs");
            let (((main, call), jump), range) = four;
            Ok(Box::new(Bcj2Decoder::new(main, call, jump, range, size)?))
        }
        Codec::Aes256Sha256 => {
            let password = password.ok_or_else(|| Error::PasswordRequired { entry: "a 7z folder".into() })?;
            let properties = sevenz_aes::Properties::parse(&coder.properties)?;
            let key = properties.key(password);
            Ok(Box::new(Limited::new(CbcDecryptReader::new(single(inputs)?, &key, properties.iv)?, size)))
        }
    }
}

const FILTER_CHUNK: usize = 64 * 1024;

struct Filtered<R> {
    inner: R,
    converter: bcj::Converter,
    hint: u64,
    pending: Vec<u8>,
    ready: Vec<u8>,
    at: usize,
    produced: u64,
    drained: bool,
}

impl<R: Read> Filtered<R> {
    fn new(inner: R, filter: u64, properties: Vec<u8>, hint: u64) -> Result<Self> {
        let converter = bcj::Converter::decode(filter, &properties)?;
        Ok(Filtered { inner, converter, hint, pending: Vec::with_capacity(FILTER_CHUNK), ready: Vec::new(), at: 0, produced: 0, drained: false })
    }

    fn fill(&mut self) -> std::io::Result<()> {
        let mut buffer = [0u8; 8192];
        let wanted = FILTER_CHUNK.saturating_sub(self.pending.len()).max(1);
        let mut taken = 0usize;

        while taken < wanted {
            let room = (wanted - taken).min(buffer.len());
            match self.inner.read(&mut buffer[..room])? {
                0 => {
                    self.drained = true;
                    break;
                }
                n => {
                    self.pending.extend_from_slice(&buffer[..n]);
                    taken += n;
                }
            }
        }

        let done = self.converter.convert(&mut self.pending, self.drained)?;
        self.ready.clear();
        self.ready.extend_from_slice(&self.pending[..done]);
        self.pending.drain(..done);
        self.at = 0;
        self.produced += done as u64;

        if self.drained && self.produced != self.hint {
            return Err(Error::malformed(format!("7z filtered folder decoded to {} bytes, not the {} it declares", self.produced, self.hint)).into());
        }
        Ok(())
    }
}

impl<R: Read> Read for Filtered<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        while self.at == self.ready.len() {
            if self.drained {
                return Ok(0);
            }
            self.fill()?;
        }

        let take = (self.ready.len() - self.at).min(buf.len());
        buf[..take].copy_from_slice(&self.ready[self.at..self.at + take]);
        self.at += take;
        Ok(take)
    }
}
