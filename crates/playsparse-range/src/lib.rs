//! Binary-search range resolution over verified tier sources, with optional
//! bounded prefetch and measured decaying retention policy.
mod prefetch;
mod replay;
use parking_lot::Mutex;
pub use playsparse_cache::CacheMetrics;
use playsparse_cache::{CacheOutcome, ChunkCache};
use playsparse_core::{Error, MAX_READ_BYTES, Manifest, Result, intersecting_chunks, parse_hash};
use playsparse_policy::{Eviction, Policy, Tracker};
use playsparse_store::{Store, TierConfig};
use playsparse_trace::TraceWriter;
pub use replay::{replay_one, replay_synthetic, replay_trace};
use std::{collections::BTreeSet, path::Path, sync::Arc, time::Instant};

pub struct RuntimeOptions {
    pub cache_bytes: usize,
    pub trace: Option<Arc<TraceWriter>>,
    pub policy: Option<Policy>,
    pub tiers: Option<TierConfig>,
}
impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            cache_bytes: 64 * 1024 * 1024,
            trace: None,
            policy: None,
            tiers: None,
        }
    }
}
#[derive(Default)]
struct ReadStats {
    hits: bool,
    misses: bool,
    sources: BTreeSet<&'static str>,
}
pub struct RangeResolver {
    store: Arc<Store>,
    cache: Arc<ChunkCache>,
    trace: Option<Arc<TraceWriter>>,
    policy: Option<Policy>,
    tracker: Mutex<Tracker>,
    prefetch: Option<prefetch::Prefetcher>,
    start: Instant,
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
        Self::open_configured(
            path,
            RuntimeOptions {
                cache_bytes,
                trace,
                ..Default::default()
            },
        )
    }
    pub fn open_configured(path: &Path, options: RuntimeOptions) -> Result<Self> {
        if let Some(policy) = &options.policy {
            policy.validate()?;
        }
        let capacity = options
            .policy
            .as_ref()
            .and_then(|policy| policy.cache_bytes)
            .map_or(options.cache_bytes, |limit| limit.min(options.cache_bytes));
        let store = Arc::new(if let Some(tiers) = options.tiers {
            Store::open_with_tiers(path, tiers)?
        } else {
            Store::open(path)?
        });
        let adaptive = options
            .policy
            .as_ref()
            .is_some_and(|policy| policy.eviction == Eviction::DecayingHotness);
        let cache = Arc::new(ChunkCache::with_adaptive(
            capacity,
            adaptive,
            options
                .policy
                .as_ref()
                .map_or(64, |policy| policy.decay_accesses),
        ));
        let prefetch = options
            .policy
            .as_ref()
            .filter(|policy| {
                policy.prefetch.enabled && capacity > 0 && policy.prefetch.budget_bytes > 0
            })
            .map(|policy| prefetch::Prefetcher::new(&policy.prefetch, store.clone(), cache.clone()))
            .transpose()?;
        Ok(Self {
            store,
            cache,
            trace: options.trace,
            policy: options.policy,
            tracker: Mutex::new(Tracker::default()),
            prefetch,
            start: Instant::now(),
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
        let mut metrics = self.cache.metrics();
        if let Some(prefetch) = &self.prefetch {
            prefetch.add_metrics(&mut metrics);
        }
        metrics
    }
    /// Finish background work before capturing benchmark CPU and final counters.
    /// Outstanding queued requests are cancelled; an active verified load finishes.
    pub fn stop_prefetch(&self) {
        if let Some(prefetch) = &self.prefetch {
            prefetch.stop();
        }
        self.cache.retire_prefetch();
    }
    pub fn read_range(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.read_range_as(path, path, offset, len)
    }
    pub fn read_range_as(
        &self,
        path: &str,
        visible_path: &str,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>> {
        let started = Instant::now();
        let mut stats = ReadStats::default();
        let result = self.read_impl(path, visible_path, offset, len, true, &mut stats);
        if let Some(trace) = &self.trace {
            let cache = match (stats.hits, stats.misses) {
                (true, false) => "hit",
                (true, true) => "mixed",
                (false, true) => "miss",
                _ => "bypass",
            };
            let source = if stats.sources.len() > 1 {
                "mixed"
            } else {
                stats
                    .sources
                    .iter()
                    .next()
                    .copied()
                    .unwrap_or(if result.is_err() {
                        "unresolved"
                    } else {
                        "primary-local"
                    })
            };
            trace.record(
                "read",
                visible_path,
                offset,
                len as u64,
                result.as_ref().map_or(0, |bytes| bytes.len() as u64),
                started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                cache,
                source,
                result.is_ok(),
            );
        }
        result
    }
    pub fn read_range_untraced(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.read_impl(path, path, offset, len, false, &mut ReadStats::default())
    }
    fn read_impl(
        &self,
        path: &str,
        visible_path: &str,
        offset: u64,
        len: usize,
        observe: bool,
        stats: &mut ReadStats,
    ) -> Result<Vec<u8>> {
        if len > MAX_READ_BYTES {
            return Err(Error::ReadTooLarge);
        }
        let file = self.manifest().file(path)?;
        let end = offset.saturating_add(len as u64).min(file.size);
        let returned = end.saturating_sub(offset) as usize;
        let observation = if observe {
            self.policy.as_ref().map(|policy| {
                self.tracker.lock().observe(
                    policy,
                    visible_path,
                    &format!("{:?}", std::thread::current().id()),
                    offset,
                    returned,
                    self.start.elapsed().as_millis().min(u64::MAX as u128) as u64,
                )
            })
        } else {
            None
        };
        let indices = intersecting_chunks(file, offset, len);
        let mut output = Vec::with_capacity(returned);
        for index in indices.clone() {
            let chunk = &file.chunks[index];
            let hash = parse_hash(&chunk.hash)?;
            let loaded = self
                .cache
                .get_or_load_tagged(
                    hash,
                    observation.map_or(1, |observation| observation.priority),
                    false,
                    || {
                        self.store
                            .read_object_with_source(&hash)
                            .map(|(bytes, source)| (bytes, source.as_str()))
                    },
                )
                .inspect_err(|_| {
                    stats.misses = true;
                    stats.sources.insert("unresolved");
                })?;
            match loaded.outcome {
                CacheOutcome::Hit => stats.hits = true,
                CacheOutcome::Miss | CacheOutcome::SharedLoad => stats.misses = true,
            };
            stats.sources.insert(loaded.source);
            let start = offset.saturating_sub(chunk.offset) as usize;
            let stop = (end - chunk.offset).min(chunk.raw_size as u64) as usize;
            output.extend_from_slice(&loaded.chunk[start..stop]);
        }
        if observation.is_some_and(|observation| {
            observation.sequential
                && indices.end > indices.start
                && observation.forward_bytes >= file.chunks[indices.end - 1].raw_size as u64
        }) && let (Some(prefetch), Some(policy)) = (&self.prefetch, &self.policy)
        {
            for chunk in file
                .chunks
                .iter()
                .skip(indices.end)
                .take(policy.prefetch.max_chunks)
            {
                prefetch.request(
                    parse_hash(&chunk.hash)?,
                    chunk.raw_size as usize,
                    &self.cache,
                );
            }
        }
        Ok(output)
    }
}

impl Drop for RangeResolver {
    fn drop(&mut self) {
        self.stop_prefetch();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use playsparse_core::{Chunker, Layout};
    use playsparse_store::{PackOptions, pack_directory};
    use proptest::prelude::*;
    #[test]
    fn tiny_sequential_burst_does_not_predict_whole_chunks_until_chunk_coverage() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let bytes: Vec<u8> = (0..32768).map(|index| (index / 4096) as u8).collect();
        std::fs::write(source.join("file"), &bytes).unwrap();
        let store = tmp.path().join("store");
        pack_directory(
            &source,
            &store,
            &PackOptions {
                chunk_size: 4096,
                chunker: Chunker::Fixed,
                ..Default::default()
            },
        )
        .unwrap();
        let reader = RangeResolver::open_configured(
            &store,
            RuntimeOptions {
                cache_bytes: 65536,
                policy: Some(Policy::default()),
                ..Default::default()
            },
        )
        .unwrap();
        for offset in 0..3 {
            assert_eq!(
                reader.read_range("file", offset, 1).unwrap(),
                &bytes[offset as usize..offset as usize + 1]
            );
        }
        assert_eq!(reader.metrics().prefetch_requests, 0);
        assert_eq!(reader.metrics().raw_bytes_loaded, 4096);
        assert_eq!(reader.read_range("file", 3, 4093).unwrap(), bytes[3..4096]);
        assert_eq!(reader.metrics().prefetch_requests, 2);
        assert!(reader.read_range("file", u64::MAX, 1).unwrap().is_empty());
        assert_eq!(reader.metrics().prefetch_requests, 2);
        reader.stop_prefetch();
        assert_eq!(reader.metrics().prefetch_reserved_bytes, 0);
        assert_eq!(
            reader.metrics().prefetch_bytes_loaded,
            reader.metrics().prefetch_useful_bytes + reader.metrics().prefetch_wasted_bytes
        );
    }
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
        assert_eq!(events[2].source, "unresolved");
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
