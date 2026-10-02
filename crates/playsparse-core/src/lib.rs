//! Versioned, validated immutable storage metadata. No filesystem side effects.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const FORMAT_VERSION: u32 = 1;
pub const MAX_CHUNK_BYTES: u32 = 16 * 1024 * 1024;
pub const MAX_READ_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_METADATA_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid store: {0}")]
    Invalid(String),
    #[error("corrupt object: {0}")]
    Corrupt(String),
    #[error("file not found: {0}")]
    NotFound(String),
    #[error("read exceeds bounded request limit ({MAX_READ_BYTES} bytes)")]
    ReadTooLarge,
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    Packs,
    Loose,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Chunker {
    Cdc,
    Fixed,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    Raw,
    Zstd,
}
impl Codec {
    pub fn byte(self) -> u8 {
        match self {
            Self::Raw => 0,
            Self::Zstd => 1,
        }
    }
    pub fn from_byte(b: u8) -> Result<Self> {
        match b {
            0 => Ok(Self::Raw),
            1 => Ok(Self::Zstd),
            _ => Err(Error::Invalid("unknown codec".into())),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkRef {
    pub hash: String,
    pub offset: u64,
    pub raw_size: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub mode: u32,
    pub hash: String,
    pub chunks: Vec<ChunkRef>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    pub layout: Layout,
    pub chunker: Chunker,
    pub chunk_size: u32,
    pub zstd_level: i32,
    pub directories: Vec<String>,
    pub files: Vec<FileEntry>,
}

pub fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':', '\0'])
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && !p.chars().any(char::is_control))
}
pub fn parse_hash(hash: &str) -> Result<[u8; 32]> {
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Invalid("noncanonical BLAKE3 digest".into()));
    }
    blake3::Hash::from_hex(hash)
        .map(|h| *h.as_bytes())
        .map_err(|e| Error::Invalid(e.to_string()))
}
impl Manifest {
    pub fn validate(&self) -> Result<()> {
        if self.format != "playsparse-store" || self.version != FORMAT_VERSION {
            return Err(Error::Invalid(
                "unsupported format; Python v0 is a separate reference format".into(),
            ));
        }
        if !(4096..=MAX_CHUNK_BYTES / 4).contains(&self.chunk_size)
            || !self.chunk_size.is_power_of_two()
        {
            return Err(Error::Invalid("invalid chunk size".into()));
        }
        let mut names = BTreeSet::new();
        let mut previous: Option<&str> = None;
        for path in &self.directories {
            if !valid_path(path)
                || previous.is_some_and(|p| p >= path.as_str())
                || !names.insert(path.as_str())
            {
                return Err(Error::Invalid("invalid or unsorted directory path".into()));
            }
            previous = Some(path);
        }
        previous = None;
        for file in &self.files {
            if !valid_path(&file.path)
                || previous.is_some_and(|p| p >= file.path.as_str())
                || !names.insert(&file.path)
                || file.mode & !0o777 != 0
            {
                return Err(Error::Invalid(
                    "invalid, duplicate or unsorted file path/mode".into(),
                ));
            }
            previous = Some(&file.path);
            parse_hash(&file.hash)?;
            let mut end = 0u64;
            for chunk in &file.chunks {
                parse_hash(&chunk.hash)?;
                if chunk.offset != end || chunk.raw_size == 0 || chunk.raw_size > MAX_CHUNK_BYTES {
                    return Err(Error::Invalid(
                        "non-contiguous or oversized chunk table".into(),
                    ));
                }
                end = end
                    .checked_add(chunk.raw_size as u64)
                    .ok_or_else(|| Error::Invalid("chunk offset overflow".into()))?;
            }
            if end != file.size {
                return Err(Error::Invalid(
                    "file size differs from chunk coverage".into(),
                ));
            }
        }
        for name in names.iter() {
            let mut parent = *name;
            while let Some((p, _)) = parent.rsplit_once('/') {
                if self
                    .directories
                    .binary_search_by(|s| s.as_str().cmp(p))
                    .is_err()
                {
                    return Err(Error::Invalid("missing directory parent".into()));
                }
                parent = p;
            }
        }
        Ok(())
    }
    pub fn file(&self, path: &str) -> Result<&FileEntry> {
        self.files
            .binary_search_by(|f| f.path.as_str().cmp(path))
            .map(|i| &self.files[i])
            .map_err(|_| Error::NotFound(path.into()))
    }
}

/// O(log n), including exact boundaries and EOF, with all arithmetic in u64.
pub fn intersecting_chunks(file: &FileEntry, offset: u64, len: usize) -> std::ops::Range<usize> {
    if len == 0 || offset >= file.size {
        return 0..0;
    }
    let end = offset.saturating_add(len as u64).min(file.size);
    let start = file
        .chunks
        .partition_point(|c| c.offset.saturating_add(c.raw_size as u64) <= offset);
    let stop = file.chunks.partition_point(|c| c.offset < end);
    start..stop
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn official_blake3_vectors() {
        assert_eq!(
            blake3::hash(b"").to_hex().as_str(),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
        assert_eq!(
            blake3::hash(b"abc").to_hex().as_str(),
            "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
        );
    }
    #[test]
    fn reject_unsafe_paths() {
        for p in [
            "../escape",
            "/root",
            "a//b",
            "C:/foo",
            "a\\b",
            "a/../b",
            "a\0",
        ] {
            assert!(!valid_path(p));
        }
    }
    #[test]
    fn large_offsets() {
        let file = FileEntry {
            path: "large".into(),
            size: 12 << 30,
            mode: 0o444,
            hash: blake3::hash(b"").to_string(),
            chunks: (0..12288)
                .map(|i| ChunkRef {
                    hash: blake3::hash(b"x").to_string(),
                    offset: i * 1024 * 1024,
                    raw_size: 1024 * 1024,
                })
                .collect(),
        };
        assert_eq!(intersecting_chunks(&file, (8 << 30) - 1, 2), 8191..8193);
        assert_eq!(intersecting_chunks(&file, 12 << 30, 1), 0..0);
        assert_eq!(intersecting_chunks(&file, u64::MAX, 2), 0..0);
    }
}
