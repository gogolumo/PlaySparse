//! Binary-search range resolution. Never reconstructs a file on a read.
pub use playsparse_cache::CacheMetrics;
use playsparse_cache::{CacheOutcome, ChunkCache};
use playsparse_core::{Error, MAX_READ_BYTES, Manifest, Result, intersecting_chunks, parse_hash};
use playsparse_store::Store;
use playsparse_trace::TraceWriter;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

pub struct RangeResolver {
    store: Store,
    cache: ChunkCache,
    trace: Option<Arc<TraceWriter>>,
}
impl RangeResolver {
    pub fn open(path: &Path, cache_bytes: usize) -> Result<Self> {
        Self::open_with_trace(path, cache_bytes, None)
    }
    pub fn open_with_trace(
        path: &Path,
        cache_bytes: usize,
        trace: Option<Arc<TraceWriter>>,
    ) -> Result<Self> {
        Ok(Self {
            store: Store::open(path)?,
            cache: ChunkCache::new(cache_bytes),
            trace,
        })
    }
    pub fn trace(&self) -> Option<&Arc<TraceWriter>> {
        self.trace.as_ref()
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
        self.read_range_as(path, path, offset, len)
    }
    /// A renamed overlay base reference still reports its application-visible path.
    pub fn read_range_as(
        &self,
        path: &str,
        visible_path: &str,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>> {
        let started = Instant::now();
        let mut misses = false;
        let mut hits = false;
        let result = self.read_impl(path, offset, len, &mut hits, &mut misses);
        if let Some(trace) = &self.trace {
            let cache = match (hits, misses) {
                (true, false) => "hit",
                (true, true) => "mixed",
                (false, true) => "miss",
                _ => "bypass",
            };
            trace.record(
                "read",
                visible_path,
                offset,
                len as u64,
                result.as_ref().map_or(0, |v| v.len() as u64),
                started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                cache,
                if misses {
                    "primary-local"
                } else if hits {
                    "memory-cache"
                } else {
                    "primary-local"
                },
                result.is_ok(),
            );
        }
        result
    }
    /// Internal copy-up must not be reported as application filesystem traffic.
    pub fn read_range_untraced(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.read_impl(path, offset, len, &mut false, &mut false)
    }
    fn read_impl(
        &self,
        path: &str,
        offset: u64,
        len: usize,
        hits: &mut bool,
        misses: &mut bool,
    ) -> Result<Vec<u8>> {
        if len > MAX_READ_BYTES {
            return Err(Error::ReadTooLarge);
        }
        let file = self.manifest().file(path)?;
        let end = offset.saturating_add(len as u64).min(file.size);
        let mut output = Vec::with_capacity(end.saturating_sub(offset) as usize);
        for index in intersecting_chunks(file, offset, len) {
            let chunk = &file.chunks[index];
            let hash = parse_hash(&chunk.hash)?;
            let (raw, outcome) = self
                .cache
                .get_or_load_status(hash, || self.store.read_object(&hash))?;
            match outcome {
                CacheOutcome::Hit => *hits = true,
                CacheOutcome::Miss | CacheOutcome::SharedLoad => *misses = true,
            }
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
    fn traced_reads_preserve_bytes_and_record_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("file"), b"verified bytes").unwrap();
        let store = tmp.path().join("store");
        pack_directory(&src, &store, &PackOptions::default()).unwrap();
        let path = tmp.path().join("trace");
        let trace = Arc::new(TraceWriter::open(&path).unwrap());
        let reader = RangeResolver::open_with_trace(&store, 65536, Some(trace.clone())).unwrap();
        assert_eq!(reader.read_range("file", 0, 4).unwrap(), b"veri");
        assert_eq!(reader.read_range("file", 0, 4).unwrap(), b"veri");
        assert!(reader.read_range("absent", 0, 4).is_err());
        trace.shutdown();
        let summary = playsparse_trace::summarize(&path).unwrap();
        assert_eq!(summary["read_operations"], 3);
        assert_eq!(summary["read_bytes"], 8);
        let lines = std::fs::read_to_string(path).unwrap();
        let events: Vec<playsparse_trace::Event> = lines
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(events[0].cache, "miss");
        assert_eq!(events[1].cache, "hit");
        assert!(!events[2].success);
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
