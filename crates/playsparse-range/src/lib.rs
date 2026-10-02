//! Binary-search range resolution. Never reconstructs a file on a read.
pub use playsparse_cache::CacheMetrics;
use playsparse_cache::ChunkCache;
use playsparse_core::{Error, MAX_READ_BYTES, Manifest, Result, intersecting_chunks, parse_hash};
use playsparse_store::Store;
use std::path::Path;

pub struct RangeResolver {
    store: Store,
    cache: ChunkCache,
}
impl RangeResolver {
    pub fn open(path: &Path, cache_bytes: usize) -> Result<Self> {
        Ok(Self {
            store: Store::open(path)?,
            cache: ChunkCache::new(cache_bytes),
        })
    }
    pub fn manifest(&self) -> &Manifest {
        self.store.manifest()
    }
    pub fn store(&self) -> &Store {
        &self.store
    }
    pub fn metrics(&self) -> CacheMetrics {
        self.cache.metrics()
    }
    pub fn read_range(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        if len > MAX_READ_BYTES {
            return Err(Error::ReadTooLarge);
        }
        let file = self.manifest().file(path)?;
        let end = offset.saturating_add(len as u64).min(file.size);
        let mut output = Vec::with_capacity(end.saturating_sub(offset) as usize);
        for index in intersecting_chunks(file, offset, len) {
            let chunk = &file.chunks[index];
            let hash = parse_hash(&chunk.hash)?;
            let raw = self
                .cache
                .get_or_load(hash, || self.store.read_object(&hash))?;
            let start = offset.saturating_sub(chunk.offset) as usize;
            let stop = (end - chunk.offset).min(chunk.raw_size as u64) as usize;
            output.extend_from_slice(&raw[start..stop]);
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use playsparse_core::{Chunker, Layout};
    use playsparse_store::{PackOptions, pack_directory};
    use proptest::prelude::*;
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]
        #[test] fn ranges_equal_source(data in proptest::collection::vec(any::<u8>(),0..80000), offset in 0u64..100000, len in 0usize..50000, cdc in any::<bool>(), loose in any::<bool>()) {
            let tmp=tempfile::tempdir().unwrap();let src=tmp.path().join("src");std::fs::create_dir(&src).unwrap();std::fs::write(src.join("file"),&data).unwrap();
            let options=PackOptions {chunk_size:4096,chunker:if cdc{Chunker::Cdc}else{Chunker::Fixed},layout:if loose{Layout::Loose}else{Layout::Packs},..Default::default()};
            let store=tmp.path().join("store");pack_directory(&src,&store,&options).unwrap();let reader=RangeResolver::open(&store,8192).unwrap();
            let start=(offset as usize).min(data.len());let end=start.saturating_add(len).min(data.len());
            prop_assert_eq!(reader.read_range("file",offset,len).unwrap(),&data[start..end]);prop_assert!(reader.metrics().resident_bytes<=8192);
        }
    }
    #[test]
    fn loads_only_intersecting_chunks_and_handles_overflow() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir(&src).unwrap();
        let data: Vec<_> = (0..65536).map(|i| (i / 4096) as u8).collect();
        std::fs::write(src.join("file"), &data).unwrap();
        let store = tmp.path().join("store");
        pack_directory(
            &src,
            &store,
            &PackOptions {
                chunker: Chunker::Fixed,
                chunk_size: 4096,
                ..Default::default()
            },
        )
        .unwrap();
        let reader = RangeResolver::open(&store, 65536).unwrap();
        assert_eq!(reader.read_range("file", 4095, 2).unwrap(), [0, 1]);
        assert_eq!(reader.metrics().decompressions, 2);
        assert_eq!(reader.read_range("file", 4096, 10).unwrap(), [1; 10]);
        assert_eq!(reader.metrics().decompressions, 2);
        assert!(reader.read_range("file", u64::MAX, 10).unwrap().is_empty());
        assert!(reader.read_range("file", 65536, 10).unwrap().is_empty());
        assert!(reader.read_range("file", 0, MAX_READ_BYTES + 1).is_err());
        assert!(reader.read_range("missing", 0, 1).is_err());
    }
}
