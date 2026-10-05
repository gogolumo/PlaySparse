//! Byte-bounded cache, decaying retention priority and single-flight loads.
//! Load I/O occurs outside the cache lock; retained payloads have a hard budget.
use parking_lot::{Condvar, Mutex};
use playsparse_core::{Error, Result};
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
};

pub type Chunk = Arc<Vec<u8>>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheOutcome {
    Hit,
    Miss,
    SharedLoad,
}
pub struct CacheLookup {
    pub chunk: Chunk,
    pub outcome: CacheOutcome,
    pub source: &'static str,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct CacheMetrics {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub resident_bytes: usize,
    pub peak_resident_bytes: usize,
    pub resident_entries: usize,
    pub decompressions: u64,
    pub decompressions_avoided: u64,
    pub in_flight_waits: u64,
    pub raw_bytes_loaded: u64,
    pub hit_ratio: f64,
    pub prefetch_requests: u64,
    pub prefetch_bytes_loaded: u64,
    pub prefetch_chunks_loaded: u64,
    pub prefetch_hits: u64,
    pub prefetch_useful_bytes: u64,
    pub prefetch_wasted_bytes: u64,
    pub prefetch_resident_unconsumed_bytes: u64,
    pub prefetch_errors: u64,
    pub prefetch_expired: u64,
    pub prefetch_dropped: u64,
    pub prefetch_skipped: u64,
    pub prefetch_duplicates: u64,
    pub prefetch_queue_high_watermark: usize,
    pub prefetch_reserved_bytes: usize,
    pub prefetch_peak_reserved_bytes: usize,
}
#[derive(Clone)]
struct Loaded {
    chunk: Chunk,
    source: &'static str,
}
type FlightResult = std::result::Result<Loaded, String>;
struct Flight {
    result: Mutex<Option<FlightResult>>,
    ready: Condvar,
    demanded: AtomicBool,
    priority: AtomicU32,
}
struct Cached {
    loaded: Loaded,
    priority: u32,
    epoch: u64,
    prefetched: bool,
}
struct State {
    cache: lru::LruCache<[u8; 32], Cached>,
    flights: HashMap<[u8; 32], Arc<Flight>>,
    metrics: CacheMetrics,
    active: usize,
    epoch: u64,
}
pub struct ChunkCache {
    capacity: usize,
    adaptive: bool,
    decay_accesses: u64,
    state: Mutex<State>,
    permit: Condvar,
}
const MAX_ENTRIES: usize = 65536;
const MAX_SPECULATIVE_VICTIMS: usize = 64;
const MAX_LOADS: usize = 8;
impl ChunkCache {
    pub fn new(capacity: usize) -> Self {
        Self::with_adaptive(capacity, false, 64)
    }
    pub fn with_adaptive(capacity: usize, adaptive: bool, decay_accesses: u64) -> Self {
        Self {
            capacity,
            adaptive,
            decay_accesses: decay_accesses.max(1),
            state: Mutex::new(State {
                cache: lru::LruCache::unbounded(),
                flights: HashMap::new(),
                metrics: CacheMetrics::default(),
                active: 0,
                epoch: 0,
            }),
            permit: Condvar::new(),
        }
    }
    pub fn contains(&self, hash: &[u8; 32]) -> bool {
        self.state.lock().cache.contains(hash)
    }
    pub fn capacity(&self) -> usize {
        self.capacity
    }
    /// A cheap queue preflight. Admission is checked again under the cache lock
    /// immediately before loading and after loading, since demand can race it.
    pub fn can_prefetch(&self, hash: &[u8; 32], bytes: usize) -> bool {
        let state = self.state.lock();
        !state.cache.contains(hash)
            && !state.flights.contains_key(hash)
            && state.active < MAX_LOADS
            && self.prefetch_victims(&state, bytes, 0).is_some()
    }
    /// Load only known-size speculation which can currently be retained safely.
    /// None means no new source I/O was started, including resident/in-flight
    /// races. A later demand follower turns the flight into demand admission.
    pub fn prefetch(
        &self,
        hash: [u8; 32],
        bytes: usize,
        loader: impl FnOnce() -> Result<(Vec<u8>, &'static str)>,
    ) -> Result<Option<CacheLookup>> {
        self.get_or_load_inner(hash, 0, true, Some(bytes), loader)
    }
    pub fn metrics(&self) -> CacheMetrics {
        let mut metrics = self.state.lock().metrics.clone();
        let accesses = metrics.hits + metrics.misses;
        metrics.hit_ratio = if accesses == 0 {
            0.0
        } else {
            metrics.hits as f64 / accesses as f64
        };
        metrics
    }
    /// Terminal workload accounting: unused speculative objects are wasted,
    /// even if their valid bytes remain resident until ordinary cache eviction.
    pub fn retire_prefetch(&self) {
        let mut state = self.state.lock();
        for (_, cached) in state.cache.iter_mut() {
            cached.prefetched = false;
        }
        state.metrics.prefetch_wasted_bytes += state.metrics.prefetch_resident_unconsumed_bytes;
        state.metrics.prefetch_resident_unconsumed_bytes = 0;
    }
    pub fn get_or_load(
        &self,
        hash: [u8; 32],
        loader: impl FnOnce() -> Result<Vec<u8>>,
    ) -> Result<Chunk> {
        self.get_or_load_status(hash, loader)
            .map(|(chunk, _)| chunk)
    }
    pub fn get_or_load_status(
        &self,
        hash: [u8; 32],
        loader: impl FnOnce() -> Result<Vec<u8>>,
    ) -> Result<(Chunk, CacheOutcome)> {
        self.get_or_load_tagged(hash, 1, false, || {
            loader().map(|bytes| (bytes, "primary-local"))
        })
        .map(|result| (result.chunk, result.outcome))
    }
    pub fn get_or_load_tagged(
        &self,
        hash: [u8; 32],
        priority: u32,
        prefetch: bool,
        loader: impl FnOnce() -> Result<(Vec<u8>, &'static str)>,
    ) -> Result<CacheLookup> {
        self.get_or_load_inner(hash, priority, prefetch, None, loader)?
            .ok_or_else(|| Error::Corrupt("demand load was unexpectedly skipped".into()))
    }
    fn get_or_load_inner(
        &self,
        hash: [u8; 32],
        priority: u32,
        prefetch: bool,
        expected_bytes: Option<usize>,
        loader: impl FnOnce() -> Result<(Vec<u8>, &'static str)>,
    ) -> Result<Option<CacheLookup>> {
        let mut state = self.state.lock();
        if !prefetch {
            state.epoch = state.epoch.saturating_add(1);
        }
        let epoch = state.epoch;
        loop {
            if expected_bytes.is_some()
                && (state.cache.contains(&hash) || state.flights.contains_key(&hash))
            {
                state.metrics.prefetch_skipped += 1;
                return Ok(None);
            }
            let cached = if prefetch {
                state.cache.peek_mut(&hash)
            } else {
                state.cache.get_mut(&hash)
            };
            if let Some(cached) = cached {
                let loaded = cached.loaded.clone();
                let consumed = !prefetch && cached.prefetched;
                if !prefetch {
                    cached.prefetched = false;
                    cached.priority = (cached.priority
                        >> (epoch.saturating_sub(cached.epoch) / self.decay_accesses).min(31))
                    .saturating_add(1)
                    .max(priority);
                    cached.epoch = epoch;
                }
                if !prefetch {
                    state.metrics.hits += 1;
                    state.metrics.decompressions_avoided += 1;
                }
                if consumed {
                    state.metrics.prefetch_hits += 1;
                    state.metrics.prefetch_useful_bytes += loaded.chunk.len() as u64;
                    state.metrics.prefetch_resident_unconsumed_bytes -= loaded.chunk.len() as u64;
                }
                return Ok(Some(CacheLookup {
                    chunk: loaded.chunk,
                    outcome: CacheOutcome::Hit,
                    source: "memory-cache",
                }));
            }
            if let Some(flight) = state.flights.get(&hash).cloned() {
                if !prefetch {
                    flight.demanded.store(true, Ordering::Relaxed);
                    flight.priority.fetch_max(priority, Ordering::Relaxed);
                    state.metrics.hits += 1;
                    state.metrics.in_flight_waits += 1;
                    state.metrics.decompressions_avoided += 1;
                }
                drop(state);
                let mut result = flight.result.lock();
                while result.is_none() {
                    flight.ready.wait(&mut result);
                }
                return match result.as_ref() {
                    Some(Ok(loaded)) => Ok(Some(CacheLookup {
                        chunk: loaded.chunk.clone(),
                        outcome: CacheOutcome::SharedLoad,
                        source: loaded.source,
                    })),
                    Some(Err(error)) => Err(Error::Corrupt(error.clone())),
                    None => Err(Error::Corrupt("flight completed without result".into())),
                };
            }
            if let Some(bytes) = expected_bytes
                && self.prefetch_victims(&state, bytes, priority).is_none()
            {
                state.metrics.prefetch_skipped += 1;
                return Ok(None);
            }
            if state.active < MAX_LOADS {
                break;
            }
            if expected_bytes.is_some() {
                state.metrics.prefetch_skipped += 1;
                return Ok(None);
            }
            self.permit.wait(&mut state);
        }
        let flight = Arc::new(Flight {
            result: Mutex::new(None),
            ready: Condvar::new(),
            demanded: AtomicBool::new(!prefetch),
            priority: AtomicU32::new(priority),
        });
        state.flights.insert(hash, flight.clone());
        state.active += 1;
        if !prefetch {
            state.metrics.misses += 1;
        }
        drop(state);
        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(loader)) {
            Ok(result) => result
                .and_then(|(bytes, source)| {
                    if expected_bytes.is_some_and(|expected| expected != bytes.len()) {
                        return Err(Error::Corrupt(
                            "prefetch object size differs from manifest".into(),
                        ));
                    }
                    Ok(Loaded {
                        chunk: Arc::new(bytes),
                        source,
                    })
                })
                .map_err(|error| error.to_string()),
            Err(_) => Err("object loader panicked".into()),
        };
        let mut state = self.state.lock();
        if let Ok(loaded) = &result {
            let priority = flight.priority.load(Ordering::Relaxed);
            let size = loaded.chunk.len();
            state.metrics.decompressions += 1;
            state.metrics.raw_bytes_loaded += size as u64;
            let useful = prefetch && flight.demanded.load(Ordering::Relaxed);
            if prefetch {
                state.metrics.prefetch_bytes_loaded += size as u64;
                state.metrics.prefetch_chunks_loaded += 1;
                if useful {
                    state.metrics.prefetch_hits += 1;
                    state.metrics.prefetch_useful_bytes += size as u64;
                }
            }
            let valid_size = size <= self.capacity && size > 0;
            let speculative = prefetch && !useful;
            let victims = if valid_size && speculative {
                self.prefetch_victims(&state, size, priority)
            } else {
                None
            };
            let admit = valid_size && (!speculative || victims.is_some());
            if admit {
                if let Some(victims) = victims {
                    for key in victims {
                        Self::evict(&mut state, &key);
                    }
                } else {
                    while state.metrics.resident_bytes > self.capacity - size
                        || state.cache.len() >= MAX_ENTRIES
                    {
                        let Some(key) = self.victim(&state) else {
                            break;
                        };
                        Self::evict(&mut state, &key);
                    }
                }
                state.metrics.resident_bytes += size;
                state.metrics.peak_resident_bytes = state
                    .metrics
                    .peak_resident_bytes
                    .max(state.metrics.resident_bytes);
                if prefetch && !useful {
                    state.metrics.prefetch_resident_unconsumed_bytes += size as u64;
                }
                let epoch = state.epoch;
                state.cache.put(
                    hash,
                    Cached {
                        loaded: loaded.clone(),
                        priority,
                        epoch,
                        prefetched: prefetch && !useful,
                    },
                );
                state.metrics.resident_entries = state.cache.len();
            } else if prefetch && !useful {
                state.metrics.prefetch_wasted_bytes += size as u64;
            }
        } else if prefetch {
            state.metrics.prefetch_errors += 1;
        }
        *flight.result.lock() = Some(result.clone());
        state.flights.remove(&hash);
        state.active -= 1;
        flight.ready.notify_all();
        self.permit.notify_all();
        drop(state);
        result
            .map(|loaded| {
                Some(CacheLookup {
                    chunk: loaded.chunk,
                    outcome: CacheOutcome::Miss,
                    source: loaded.source,
                })
            })
            .map_err(Error::Corrupt)
    }
    fn evict(state: &mut State, key: &[u8; 32]) {
        let evicted = state.cache.pop(key).unwrap();
        let bytes = evicted.loaded.chunk.len();
        state.metrics.resident_bytes -= bytes;
        state.metrics.evictions += 1;
        if evicted.prefetched {
            state.metrics.prefetch_wasted_bytes += bytes as u64;
            state.metrics.prefetch_resident_unconsumed_bytes -= bytes as u64;
        }
    }
    /// Plan every required eviction without changing cache state. At most 64
    /// victims and 128 oldest entries are considered. When the bounded search
    /// cannot prove safe admission, speculation conservatively does no work.
    fn prefetch_victims(
        &self,
        state: &State,
        bytes: usize,
        priority: u32,
    ) -> Option<Vec<[u8; 32]>> {
        if bytes == 0 || bytes > self.capacity {
            return None;
        }
        let mut remaining = state.metrics.resident_bytes;
        let mut entries = state.cache.len();
        if remaining <= self.capacity - bytes && entries < MAX_ENTRIES {
            return Some(Vec::new());
        }
        let mut candidates: Vec<_> = state
            .cache
            .iter()
            .rev()
            .take(2 * MAX_SPECULATIVE_VICTIMS)
            .map(|(hash, cached)| {
                (
                    *hash,
                    cached.loaded.chunk.len(),
                    cached.priority
                        >> (state.epoch.saturating_sub(cached.epoch) / self.decay_accesses).min(31),
                )
            })
            .collect();
        let mut victims = Vec::new();
        while remaining > self.capacity - bytes || entries >= MAX_ENTRIES {
            if victims.len() >= MAX_SPECULATIVE_VICTIMS {
                return None;
            }
            let index = if self.adaptive {
                candidates
                    .iter()
                    .take(64)
                    .enumerate()
                    .min_by_key(|(_, (_, _, score))| *score)
                    .map(|(index, _)| index)?
            } else {
                if candidates.is_empty() {
                    return None;
                }
                0
            };
            let (hash, size, score) = candidates.remove(index);
            if score > priority {
                return None;
            }
            victims.push(hash);
            remaining -= size;
            entries -= 1;
        }
        Some(victims)
    }
    fn victim(&self, state: &State) -> Option<[u8; 32]> {
        if !self.adaptive {
            return state.cache.peek_lru().map(|(hash, _)| *hash);
        }
        // Bound eviction CPU per object, while preserving LRU order on ties.
        state
            .cache
            .iter()
            .rev()
            .take(64)
            .min_by_key(|(_, cached)| {
                cached.priority
                    >> (state.epoch.saturating_sub(cached.epoch) / self.decay_accesses).min(31)
            })
            .map(|(hash, _)| *hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mixed_size_cache() -> ChunkCache {
        let cache = ChunkCache::with_adaptive(12, true, 1_000_000);
        cache
            .get_or_load_tagged([1; 32], 0, false, || Ok((vec![1; 4], "primary-local")))
            .unwrap();
        cache
            .get_or_load_tagged([2; 32], 100, false, || Ok((vec![2; 8], "primary-local")))
            .unwrap();
        cache
    }
    #[test]
    fn speculation_checks_every_variable_size_victim_without_partial_eviction() {
        let cache = mixed_size_cache();
        // Evicting the cheap four-byte object alone cannot fit eight bytes.
        // The second required victim is protected even though the first is not.
        cache
            .get_or_load_tagged([3; 32], 0, true, || Ok((vec![3; 8], "secondary-local")))
            .unwrap();
        assert!(cache.contains(&[1; 32]));
        assert!(cache.contains(&[2; 32]));
        assert!(!cache.contains(&[3; 32]));
        assert_eq!(cache.metrics().evictions, 0);
        assert_eq!(cache.metrics().prefetch_wasted_bytes, 8);
        let cache = mixed_size_cache();
        assert!(!cache.can_prefetch(&[3; 32], 8));
        assert!(
            cache
                .prefetch([3; 32], 8, || panic!(
                    "rejected speculation must not do I/O"
                ))
                .unwrap()
                .is_none()
        );
        assert_eq!(cache.metrics().raw_bytes_loaded, 12);
        assert_eq!(cache.metrics().prefetch_skipped, 1);
    }
    #[test]
    fn admission_is_rechecked_after_racing_demand_without_evicting_protected_bytes() {
        let cache = Arc::new(ChunkCache::with_adaptive(12, true, 1_000_000));
        cache
            .get_or_load_tagged([1; 32], 0, false, || Ok((vec![1; 4], "primary-local")))
            .unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let producer = {
            let cache = cache.clone();
            std::thread::spawn(move || {
                cache
                    .prefetch([3; 32], 8, || {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok((vec![3; 8], "secondary-local"))
                    })
                    .unwrap()
            })
        };
        started_rx.recv().unwrap();
        cache
            .get_or_load_tagged([2; 32], 100, false, || Ok((vec![2; 8], "primary-local")))
            .unwrap();
        release_tx.send(()).unwrap();
        producer.join().unwrap();
        assert!(cache.contains(&[1; 32]));
        assert!(cache.contains(&[2; 32]));
        assert!(!cache.contains(&[3; 32]));
        assert_eq!(cache.metrics().evictions, 0);
        assert_eq!(cache.metrics().prefetch_wasted_bytes, 8);
    }
    #[test]
    fn racing_demand_makes_a_known_size_prefetch_flight_demand_admission() {
        let cache = Arc::new(ChunkCache::with_adaptive(12, true, 1_000_000));
        cache
            .get_or_load_tagged([1; 32], 0, false, || Ok((vec![1; 4], "primary-local")))
            .unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let producer = {
            let cache = cache.clone();
            std::thread::spawn(move || {
                cache
                    .prefetch([3; 32], 8, || {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok((vec![3; 8], "remote-http"))
                    })
                    .unwrap()
            })
        };
        started_rx.recv().unwrap();
        cache
            .get_or_load_tagged([2; 32], 100, false, || Ok((vec![2; 8], "primary-local")))
            .unwrap();
        let consumer = {
            let cache = cache.clone();
            std::thread::spawn(move || {
                cache
                    .get_or_load_tagged([3; 32], 1, false, || {
                        panic!("demand must join existing flight")
                    })
                    .unwrap()
            })
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while cache.metrics().in_flight_waits == 0 && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        let joined = cache.metrics().in_flight_waits;
        release_tx.send(()).unwrap();
        producer.join().unwrap();
        let result = consumer.join().unwrap();
        assert_eq!(joined, 1);
        assert_eq!(result.source, "remote-http");
        assert!(cache.contains(&[3; 32]));
        assert_eq!(cache.metrics().prefetch_useful_bytes, 8);
        assert_eq!(cache.metrics().prefetch_wasted_bytes, 0);
    }
    #[test]
    fn speculative_hits_do_not_age_demand_priority_or_refresh_lru_recency() {
        let cache = ChunkCache::with_adaptive(8, true, 1);
        for key in 1..=2 {
            cache
                .get_or_load_tagged([key; 32], 100, false, || {
                    Ok((vec![key; 4], "primary-local"))
                })
                .unwrap();
        }
        for _ in 0..128 {
            cache
                .get_or_load_tagged([1; 32], 0, true, || {
                    panic!("resident speculative lookup must not load")
                })
                .unwrap();
        }
        cache
            .get_or_load_tagged([3; 32], 0, true, || Ok((vec![3; 4], "primary-local")))
            .unwrap();
        assert!(cache.contains(&[1; 32]));
        assert!(cache.contains(&[2; 32]));
        assert!(!cache.contains(&[3; 32]));

        let lru = ChunkCache::new(8);
        for key in 1..=2 {
            lru.get_or_load([key; 32], || Ok(vec![key; 4])).unwrap();
        }
        lru.get_or_load_tagged([1; 32], 0, true, || panic!("speculative hit must not load"))
            .unwrap();
        lru.get_or_load([3; 32], || Ok(vec![3; 4])).unwrap();
        assert!(!lru.contains(&[1; 32]));
        assert!(lru.contains(&[2; 32]));
    }
    #[test]
    fn speculative_entry_pressure_and_search_limits_do_not_start_loads() {
        let cache = ChunkCache::with_adaptive(MAX_ENTRIES + 1, true, 1_000_000);
        for index in 0..MAX_ENTRIES {
            let mut hash = [0; 32];
            hash[..8].copy_from_slice(&(index as u64).to_le_bytes());
            cache
                .get_or_load_tagged(hash, 100, false, || Ok((vec![1], "primary-local")))
                .unwrap();
        }
        assert!(
            cache
                .prefetch([255; 32], 1, || panic!(
                    "entry pressure must protect demanded objects"
                ))
                .unwrap()
                .is_none()
        );
        assert_eq!(cache.metrics().resident_entries, MAX_ENTRIES);
        assert_eq!(cache.metrics().evictions, 0);
        let cache = ChunkCache::new(128);
        for key in 0..128 {
            cache
                .get_or_load_tagged([key; 32], 0, true, || Ok((vec![key], "primary-local")))
                .unwrap();
        }
        assert!(
            cache
                .prefetch([254; 32], 65, || panic!(
                    "unbounded victim planning must be rejected"
                ))
                .unwrap()
                .is_none()
        );
        assert_eq!(cache.metrics().evictions, 0);
        assert_eq!(cache.metrics().resident_bytes, 128);
    }
    #[test]
    fn invalid_known_sizes_do_not_load_and_mismatched_size_fails_closed() {
        let cache = ChunkCache::new(8);
        for bytes in [0, 9] {
            assert!(
                cache
                    .prefetch([1; 32], bytes, || panic!(
                        "invalid reservation must not load"
                    ))
                    .unwrap()
                    .is_none()
            );
        }
        assert!(
            cache
                .prefetch([1; 32], 4, || Ok((vec![1; 5], "primary-local")))
                .is_err()
        );
        assert!(!cache.contains(&[1; 32]));
        assert_eq!(cache.metrics().prefetch_errors, 1);
    }
    #[test]
    fn speculation_does_not_wait_behind_saturated_demand_loads() {
        let cache = Arc::new(ChunkCache::new(32));
        let started = Arc::new(std::sync::Barrier::new(MAX_LOADS + 1));
        let release = Arc::new(std::sync::Barrier::new(MAX_LOADS + 1));
        let threads: Vec<_> = (0..MAX_LOADS)
            .map(|key| {
                let cache = cache.clone();
                let started = started.clone();
                let release = release.clone();
                std::thread::spawn(move || {
                    cache
                        .get_or_load([key as u8; 32], || {
                            started.wait();
                            release.wait();
                            Ok(vec![1])
                        })
                        .unwrap()
                })
            })
            .collect();
        started.wait();
        assert!(!cache.can_prefetch(&[255; 32], 1));
        let (tx, rx) = std::sync::mpsc::channel();
        let speculative = {
            let cache = cache.clone();
            std::thread::spawn(move || {
                let skipped = cache
                    .prefetch([255; 32], 1, || {
                        panic!("saturated speculation must not start I/O")
                    })
                    .is_ok_and(|result| result.is_none());
                tx.send(skipped).unwrap();
            })
        };
        let skipped = rx.recv_timeout(std::time::Duration::from_secs(1));
        release.wait();
        for thread in threads {
            thread.join().unwrap();
        }
        speculative.join().unwrap();
        assert!(skipped.unwrap());
        assert_eq!(cache.metrics().prefetch_chunks_loaded, 0);
    }
    #[test]
    fn adaptive_priority_changes_eviction_and_decays_under_new_traffic() {
        let cache = ChunkCache::with_adaptive(8, true, 1);
        cache
            .get_or_load_tagged([1; 32], 100, false, || Ok((vec![1; 4], "primary-local")))
            .unwrap();
        cache
            .get_or_load_tagged([2; 32], 1, false, || Ok((vec![2; 4], "primary-local")))
            .unwrap();
        cache
            .get_or_load_tagged([3; 32], 1, false, || Ok((vec![3; 4], "primary-local")))
            .unwrap();
        assert!(cache.contains(&[1; 32]));
        assert!(!cache.contains(&[2; 32]));
        for key in 4..32 {
            cache.get_or_load([key; 32], || Ok(vec![key; 4])).unwrap();
        }
        assert!(!cache.contains(&[1; 32]));
        assert!(cache.metrics().peak_resident_bytes <= 8);
    }
    #[test]
    fn prefetch_useful_wasted_and_final_accounting_are_conserved() {
        let cache = ChunkCache::new(8);
        for key in 1..=2 {
            cache
                .get_or_load_tagged([key; 32], 0, true, || Ok((vec![key; 4], "secondary-local")))
                .unwrap();
        }
        let hit = cache
            .get_or_load_tagged([1; 32], 1, false, || panic!("prefetch hit must not load"))
            .unwrap();
        assert_eq!(hit.source, "memory-cache");
        cache.get_or_load([3; 32], || Ok(vec![3; 4])).unwrap();
        cache.retire_prefetch();
        let metrics = cache.metrics();
        assert_eq!(metrics.prefetch_bytes_loaded, 8);
        assert_eq!(metrics.prefetch_useful_bytes, 4);
        assert_eq!(metrics.prefetch_wasted_bytes, 4);
        assert_eq!(metrics.prefetch_hits, 1);
        assert_eq!(metrics.prefetch_resident_unconsumed_bytes, 0);
        let tiny = ChunkCache::new(2);
        tiny.get_or_load_tagged([1; 32], 0, true, || Ok((vec![1; 4], "primary-local")))
            .unwrap();
        assert_eq!(tiny.metrics().prefetch_wasted_bytes, 4);
        assert_eq!(tiny.metrics().resident_bytes, 0);
    }
    #[test]
    fn racing_demand_claims_prefetch_once_and_preserves_loaded_tier() {
        let cache = Arc::new(ChunkCache::new(8));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let producer = {
            let cache = cache.clone();
            std::thread::spawn(move || {
                cache
                    .get_or_load_tagged([1; 32], 0, true, || {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok((vec![42; 4], "remote-http"))
                    })
                    .unwrap()
            })
        };
        started_rx.recv().unwrap();
        let consumer = {
            let cache = cache.clone();
            std::thread::spawn(move || {
                cache
                    .get_or_load_tagged([1; 32], 99, false, || {
                        panic!("single-flight follower must not load")
                    })
                    .unwrap()
            })
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while cache.metrics().in_flight_waits == 0 && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        let shared = cache.metrics().in_flight_waits;
        release_tx.send(()).unwrap();
        producer.join().unwrap();
        let loaded = consumer.join().unwrap();
        assert_eq!(shared, 1);
        assert_eq!(loaded.source, "remote-http");
        assert_eq!(loaded.outcome, CacheOutcome::SharedLoad);
        let metrics = cache.metrics();
        assert_eq!(metrics.prefetch_bytes_loaded, 4);
        assert_eq!(metrics.prefetch_useful_bytes, 4);
        assert_eq!(metrics.prefetch_hits, 1);
        assert_eq!(metrics.prefetch_resident_unconsumed_bytes, 0);
    }
    #[test]
    fn speculative_errors_do_not_strand_demand_or_count_loaded_bytes() {
        let cache = ChunkCache::new(8);
        assert!(
            cache
                .get_or_load_tagged([1; 32], 0, true, || Err(Error::Corrupt("failure".into())))
                .is_err()
        );
        assert_eq!(cache.metrics().prefetch_errors, 1);
        assert_eq!(cache.metrics().prefetch_bytes_loaded, 0);
        assert_eq!(cache.get_or_load([1; 32], || Ok(vec![1])).unwrap()[0], 1);
    }
    #[test]
    fn weighted_lru_and_oversized_bypass() {
        let cache = ChunkCache::new(5);
        cache.get_or_load([1; 32], || Ok(vec![1; 3])).unwrap();
        cache.get_or_load([2; 32], || Ok(vec![2; 3])).unwrap();
        assert_eq!(cache.metrics().resident_bytes, 3);
        assert_eq!(cache.metrics().evictions, 1);
        cache
            .get_or_load([2; 32], || panic!("hit must not load"))
            .unwrap();
        cache.get_or_load([3; 32], || Ok(vec![3; 9])).unwrap();
        assert!(cache.metrics().resident_bytes <= 5);
    }
    #[test]
    fn sixteen_threads_share_one_load_even_with_zero_cache() {
        let cache = Arc::new(ChunkCache::new(0));
        let start = Arc::new(std::sync::Barrier::new(16));
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let cache = cache.clone();
                let start = start.clone();
                let count = count.clone();
                std::thread::spawn(move || {
                    start.wait();
                    let chunk = cache
                        .get_or_load([1; 32], || {
                            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            std::thread::sleep(std::time::Duration::from_millis(100));
                            Ok(vec![42; 1024])
                        })
                        .unwrap();
                    assert_eq!(chunk[0], 42);
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(cache.metrics().decompressions_avoided, 15);
    }
    #[test]
    fn errors_wake_waiters_and_allow_retry() {
        let cache = ChunkCache::new(10);
        assert!(
            cache
                .get_or_load([1; 32], || Err(Error::Corrupt("broken".into())))
                .is_err()
        );
        assert_eq!(cache.get_or_load([1; 32], || Ok(vec![7])).unwrap()[0], 7);
    }
    #[test]
    fn panicking_loader_releases_flight_and_permit() {
        let cache = ChunkCache::new(10);
        assert!(
            cache
                .get_or_load([1; 32], || panic!("injected loader failure"))
                .is_err()
        );
        assert_eq!(cache.get_or_load([1; 32], || Ok(vec![9])).unwrap()[0], 9);
    }
}
