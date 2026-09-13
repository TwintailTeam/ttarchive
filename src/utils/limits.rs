use crate::utils::error::{Error, Result, Unsupported};

pub const PREALLOC_MAX: usize = 1 << 20;

pub const MAX_CODEC_MEMORY: u32 = 1 << 30;

pub fn prealloc(declared: u64) -> usize {
    declared.min(PREALLOC_MAX as u64) as usize
}

pub fn codec_memory(declared: u32, codec: &'static str) -> Result<usize> {
    if declared > MAX_CODEC_MEMORY {
        return Err(Error::Unsupported(Unsupported::Other(codec)));
    }
    Ok(declared as usize)
}

pub fn zeroed(len: usize) -> Result<Vec<u8>> {
    let mut probe: Vec<u8> = Vec::new();
    probe.try_reserve_exact(len).map_err(|_| Error::malformed(format!("this build cannot set aside the {len} bytes the archive asks a codec for")))?;
    drop(probe);
    Ok(vec![0u8; len])
}
