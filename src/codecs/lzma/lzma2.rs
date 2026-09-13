use std::io::{Read, Write};

use crate::codecs::lzma::decode::{LzmaCore, Properties, Stop};
use crate::codecs::lzma::encode::{Encoder, Finder, MATCH_MAX_LEN, Sliding, properties_byte};
use crate::codecs::lzma::range::RangeDecoder;
use crate::codecs::lzma::window::Window;
use crate::utils::error::{Error, Result};

const CONTROL_END: u8 = 0x00;
const CONTROL_STORED_RESET: u8 = 0x01;
const CONTROL_STORED: u8 = 0x02;

pub fn dictionary_size(byte: u8) -> Result<u32> {
    if byte > 40 {
        return Err(Error::malformed(format!("invalid lzma2 dictionary size byte {byte}")));
    }
    if byte == 40 {
        return Ok(u32::MAX);
    }
    let base = 2 | (byte as u32 & 1);
    Ok(base << (byte as u32 / 2 + 11))
}

pub struct Lzma2Decoder<R> {
    inner: R,
    core: Option<LzmaCore>,
    window: Window,
    props: Option<Properties>,
    dict_size: u32,
    started: bool,
    after_stored: bool,
    finished: bool,
}

impl<R: Read> Lzma2Decoder<R> {
    pub fn new(inner: R, dict_size: u32) -> Self {
        Lzma2Decoder {
            inner,
            core: None,
            window: Window::new(dict_size as usize),
            props: None,
            dict_size,
            started: false,
            after_stored: false,
            finished: false,
        }
    }

    pub fn into_inner(self) -> R {
        self.inner
    }

    fn byte(&mut self) -> Result<Option<u8>> {
        let mut b = [0u8; 1];
        match self.inner.read(&mut b) {
            Ok(0) => Ok(None),
            Ok(_) => Ok(Some(b[0])),
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => self.byte(),
            Err(e) => Err(Error::from(e)),
        }
    }

    fn u16be(&mut self) -> Result<u32> {
        let high = self.byte()?.ok_or_else(|| Error::malformed("truncated lzma2 chunk header"))?;
        let low = self.byte()?.ok_or_else(|| Error::malformed("truncated lzma2 chunk header"))?;
        Ok(((high as u32) << 8) | low as u32)
    }

    fn chunk(&mut self) -> Result<()> {
        let Some(control) = self.byte()? else {
            self.finished = true;
            return Ok(());
        };

        if control == CONTROL_END {
            self.finished = true;
            return Ok(());
        }

        if control == CONTROL_STORED || control == CONTROL_STORED_RESET {
            if control == CONTROL_STORED_RESET {
                self.window.reset_dictionary();
                self.started = true;
                self.core = None;
                self.props = None;
            } else if !self.started {
                return Err(Error::malformed("lzma2 stream begins with a chunk that does not reset the dictionary"));
            }

            let len = self.u16be()? as usize + 1;
            self.window.extend_from_reader(&mut self.inner, len)?;
            self.after_stored = true;
            return Ok(());
        }

        if control < 0x80 {
            return Err(Error::malformed(format!("invalid lzma2 control byte {control:#04x}")));
        }

        let reset = (control >> 5) & 0x3;
        let unpacked = (((control as u64) & 0x1f) << 16) + self.u16be()? as u64 + 1;
        let packed = self.u16be()? as u64 + 1;

        if reset >= 2 {
            let byte = self.byte()?.ok_or_else(|| Error::malformed("lzma2 chunk claims properties but has none"))?;
            self.props = Some(Properties::from_byte(byte, self.dict_size)?);
        }
        let props = self.props.ok_or_else(|| Error::malformed("lzma2 chunk uses properties that were never sent"))?;

        if reset == 3 {
            self.window.reset_dictionary();
        }
        if self.after_stored && reset == 0 && self.core.is_none() {
            return Err(Error::malformed("lzma2 chunk continues a state that no earlier chunk established"));
        }
        self.after_stored = false;

        if reset >= 1 || self.core.is_none() {
            match &mut self.core {
                Some(core) => core.reset(props),
                none => *none = Some(LzmaCore::new(props)),
            }
        }
        if !self.started {
            if reset != 3 {
                return Err(Error::malformed("lzma2 stream begins with a chunk that does not reset the dictionary"));
            }
            self.started = true;
        }

        let limit = self.window.total() + unpacked;
        let mut section = (&mut self.inner).take(packed);
        let mut rc = RangeDecoder::new(&mut section)?;
        let core = self.core.as_mut().expect("core was just created");

        if core.decode(&mut rc, &mut self.window, limit)? == Stop::Marker {
            self.finished = true;
            return Ok(());
        }

        if self.window.total() != limit {
            return Err(Error::malformed(format!("lzma2 chunk produced {} bytes, not the {unpacked} it declared", self.window.total() + unpacked - limit)));
        }

        std::io::copy(&mut section, &mut std::io::sink())?;
        Ok(())
    }
}

impl<R: Read> Read for Lzma2Decoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        while self.window.pending() == 0 && !self.finished {
            self.window.drain();
            self.chunk()?;
        }
        Ok(self.window.take(buf))
    }
}

const CHUNK: usize = 1 << 15;
const MAX_PACKED: usize = 1 << 16;

const RESERVE: usize = MATCH_MAX_LEN as usize + 1;

pub struct Writer<W: Write> {
    out: W,
    encoder: Encoder,
    finder: Finder,
    window: Sliding,
    props: Properties,
    props_byte: u8,
    at: usize,
    first: bool,
    fresh_state: bool,
}

impl<W: Write> Writer<W> {
    pub fn new(out: W, props: Properties, depth: usize, expected: usize) -> Self {
        let dict = props.dict_size as usize;

        Writer {
            out,
            encoder: Encoder::new(props),
            finder: Finder::new(expected, dict, depth),
            window: Sliding::new(dict + MATCH_MAX_LEN as usize),
            props,
            props_byte: properties_byte(props),
            at: 0,
            first: true,
            fresh_state: true,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<()> {
        self.window.push(bytes);
        self.drain(false)
    }

    pub fn finish(mut self) -> Result<W> {
        self.drain(true)?;
        self.out.write_all(&[CONTROL_END])?;
        Ok(self.out)
    }

    fn drain(&mut self, last: bool) -> Result<()> {
        loop {
            let available = self.window.end() - self.at;
            let take = if last {
                available.min(CHUNK)
            } else if available >= CHUNK + RESERVE {
                CHUNK
            } else {
                0
            };

            if take == 0 {
                break;
            }
            self.chunk(take)?;
        }

        self.window.retain(self.at, self.props.dict_size as usize + MATCH_MAX_LEN as usize);
        Ok(())
    }

    fn chunk(&mut self, unpacked: usize) -> Result<()> {
        let props = self.props;
        let props_byte = self.props_byte;
        let from = self.at;
        let end = from + unpacked;

        let Writer { out, encoder, finder, window, first, fresh_state, .. } = self;
        let feed = window.feed();
        let packed = encoder.encode_range(&feed, from, end, finder, Vec::new())?;

        if packed.len() <= MAX_PACKED && packed.len() < unpacked {
            let reset: u8 = if *first {
                3
            } else if *fresh_state {
                2
            } else {
                0
            };

            let size = (unpacked - 1) as u32;
            let stored = (packed.len() - 1) as u32;

            let mut header = [0u8; 6];
            header[0] = 0x80 | (reset << 5) | ((size >> 16) as u8);
            header[1] = (size >> 8) as u8;
            header[2] = size as u8;
            header[3] = (stored >> 8) as u8;
            header[4] = stored as u8;
            header[5] = props_byte;

            out.write_all(&header[..if reset >= 2 { 6 } else { 5 }])?;
            out.write_all(&packed)?;
            *fresh_state = false;
        } else {
            let control = if *first { CONTROL_STORED_RESET } else { CONTROL_STORED };
            out.write_all(&[control, ((unpacked - 1) >> 8) as u8, (unpacked - 1) as u8])?;
            out.write_all(feed.slice(from, end))?;

            *encoder = Encoder::new(props);
            *fresh_state = true;
        }

        *first = false;
        self.at = end;
        Ok(())
    }
}

pub fn compress(data: &[u8], props: Properties, depth: usize) -> Result<Vec<u8>> {
    let mut writer = Writer::new(Vec::with_capacity(data.len() / 3 + 64), props, depth, data.len());
    writer.push(data)?;
    writer.finish()
}

pub fn dictionary_code(wanted: u32) -> u8 {
    for byte in 0..=40u8 {
        if dictionary_size(byte).is_ok_and(|size| size >= wanted) {
            return byte;
        }
    }
    40
}
