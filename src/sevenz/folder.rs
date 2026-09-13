use crate::sevenz::number::{count, number};
use crate::sevenz::spec::{Codec, CoderId, coder_flags};
use crate::utils::bytes::Cursor;
use crate::utils::error::{Error, Result, Unsupported};

const MAX_CODERS: usize = 64;

#[derive(Debug, Clone)]
pub struct Coder {
    pub id: CoderId,
    pub codec: Codec,
    pub in_streams: usize,
    pub out_streams: usize,
    pub properties: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindPair {
    pub in_index: usize,
    pub out_index: usize,
}

#[derive(Debug, Clone)]
pub struct Folder {
    pub coders: Vec<Coder>,
    pub bind_pairs: Vec<BindPair>,
    pub packed_indices: Vec<usize>,
    pub unpack_sizes: Vec<u64>,
    pub crc: Option<u32>,
}

impl Folder {
    pub fn read(cursor: &mut Cursor<'_>) -> Result<Self> {
        let num_coders = count(cursor, "coders in a folder")?;
        if num_coders == 0 || num_coders > MAX_CODERS {
            return Err(Error::malformed(format!("7z folder declares {num_coders} coders; 1 to {MAX_CODERS} are legal")));
        }

        let mut coders = Vec::with_capacity(num_coders);
        let mut total_in = 0usize;
        let mut total_out = 0usize;

        for _ in 0..num_coders {
            let flags = cursor.u8("a coder's flags")?;
            if flags & 0x80 != 0 {
                return Err(Error::Unsupported(Unsupported::Other("a 7z folder offering alternative coders, which no released 7-Zip writes")));
            }
            if flags & 0x40 != 0 {
                return Err(Error::malformed("7z coder flags set a reserved bit"));
            }

            let id = CoderId::new(cursor.slice((flags & coder_flags::ID_SIZE) as usize, "a coder id")?)?;
            let (in_streams, out_streams) = if flags & coder_flags::COMPLEX != 0 {
                (count(cursor, "a coder's input streams")?, count(cursor, "a coder's output streams")?)
            } else {
                (1, 1)
            };
            if in_streams == 0 || out_streams == 0 {
                return Err(Error::malformed("7z coder declares no input or no output stream"));
            }

            let properties = if flags & coder_flags::HAS_ATTRIBUTES != 0 {
                let len = count(cursor, "a coder's property bytes")?;
                cursor.slice(len, "a coder's properties")?.to_vec()
            } else {
                Vec::new()
            };

            total_in += in_streams;
            total_out += out_streams;
            coders.push(Coder { id, codec: Codec::from_id(id)?, in_streams, out_streams, properties });
        }

        let mut bind_pairs = Vec::with_capacity(total_out - 1);
        for _ in 0..total_out - 1 {
            let in_index = count(cursor, "a bind pair's input index")?;
            let out_index = count(cursor, "a bind pair's output index")?;
            if in_index >= total_in || out_index >= total_out {
                return Err(Error::malformed("7z bind pair references a stream the folder does not have"));
            }
            bind_pairs.push(BindPair { in_index, out_index });
        }

        let num_packed = total_in.checked_sub(bind_pairs.len()).ok_or_else(|| Error::malformed("7z folder binds more inputs than its coders have"))?;
        if num_packed == 0 {
            return Err(Error::malformed("7z folder takes no packed stream"));
        }

        let packed_indices = if num_packed == 1 {
            let only = (0..total_in).find(|&i| !bind_pairs.iter().any(|pair| pair.in_index == i));
            vec![only.ok_or_else(|| Error::malformed("7z folder leaves no input for its packed stream"))?]
        } else {
            let mut indices = Vec::with_capacity(num_packed);
            for _ in 0..num_packed {
                let index = count(cursor, "a packed stream index")?;
                if index >= total_in {
                    return Err(Error::malformed("7z packed stream references an input the folder does not have"));
                }
                indices.push(index);
            }
            indices
        };

        Ok(Folder { coders, bind_pairs, packed_indices, unpack_sizes: Vec::new(), crc: None })
    }

    pub fn total_in_streams(&self) -> usize {
        self.coders.iter().map(|coder| coder.in_streams).sum()
    }

    pub fn total_out_streams(&self) -> usize {
        self.coders.iter().map(|coder| coder.out_streams).sum()
    }

    pub fn first_in_stream_of(&self, coder: usize) -> usize {
        self.coders[..coder].iter().map(|c| c.in_streams).sum()
    }

    pub fn first_out_stream_of(&self, coder: usize) -> usize {
        self.coders[..coder].iter().map(|c| c.out_streams).sum()
    }

    pub fn coder_of_out_stream(&self, out_index: usize) -> Result<usize> {
        let mut seen = 0usize;
        for (index, coder) in self.coders.iter().enumerate() {
            seen += coder.out_streams;
            if out_index < seen {
                return Ok(index);
            }
        }
        Err(Error::malformed("7z folder references an output stream past its last coder"))
    }

    pub fn bind_pair_for_in_stream(&self, in_index: usize) -> Option<&BindPair> {
        self.bind_pairs.iter().find(|pair| pair.in_index == in_index)
    }

    pub fn bind_pair_for_out_stream(&self, out_index: usize) -> Option<&BindPair> {
        self.bind_pairs.iter().find(|pair| pair.out_index == out_index)
    }

    pub fn main_out_stream(&self) -> Result<usize> {
        (0..self.total_out_streams())
            .find(|&index| self.bind_pair_for_out_stream(index).is_none())
            .ok_or_else(|| Error::malformed("7z folder binds every output stream, so it produces nothing"))
    }

    pub fn unpack_size(&self) -> Result<u64> {
        let main = self.main_out_stream()?;
        self.unpack_sizes.get(main).copied().ok_or_else(|| Error::malformed("7z folder is missing the unpacked size of its output stream"))
    }

    pub fn packed_stream_at(&self, in_index: usize) -> Option<usize> {
        self.packed_indices.iter().position(|&i| i == in_index)
    }

    pub fn is_linear(&self) -> bool {
        self.coders.iter().all(|coder| coder.in_streams == 1 && coder.out_streams == 1)
    }
}

pub fn read_unpack_sizes(cursor: &mut Cursor<'_>, folders: &mut [Folder]) -> Result<()> {
    for folder in folders {
        let outputs = folder.total_out_streams();
        folder.unpack_sizes = Vec::with_capacity(outputs);
        for _ in 0..outputs {
            folder.unpack_sizes.push(number(cursor, "a coder's unpacked size")?);
        }
    }
    Ok(())
}
