//! Byte-bounded LRU plus single-flight loads; I/O occurs outside the cache mutex.
use parking_lot::{Condvar, Mutex};
use playsparse_core::{Error, Result};
use serde::Serialize;
use std::{collections::HashMap, sync::Arc};

pub type Chunk = Arc<Vec<u8>>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheOutcome {
    Hit,
    Miss,
    SharedLoad,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct CacheMetrics {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub resident_bytes: usize,
    pub decompressions: u64,
    pub decompressions_avoided: u64,
    pub in_flight_waits: u64,
    pub raw_bytes_loaded: u64,
    pub hit_ratio: f64,
}
type FlightResult = std::result::Result<Chunk, String>;
struct Flight {
    result: Mutex<Option<FlightResult>>,
    ready: Condvar,
}
struct State {
    cache: lru::LruCache<[u8; 32], Chunk>,
    flights: HashMap<[u8; 32], Arc<Flight>>,
    metrics: CacheMetrics,
    active: usize,
}
pub struct ChunkCache {
    capacity: usize,
    state: Mutex<State>,
    permit: Condvar,
}
impl ChunkCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            state: Mutex::new(State {
                cache: lru::LruCache::unbounded(),
                flights: HashMap::new(),
                metrics: CacheMetrics::default(),
                active: 0,
            }),
            permit: Condvar::new(),
        }
    }
    pub fn metrics(&self) -> CacheMetrics {
        let mut m = self.state.lock().metrics.clone();
        let accesses = m.hits + m.misses;
        m.hit_ratio = if accesses == 0 {
            0.0
        } else {
            m.hits as f64 / accesses as f64
        };
        m
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
        let mut state = self.state.lock();
        // At most eight independent loaders at a time. Recheck after waiting:
        // another caller may have populated or started the exact same object.
        loop {
            if let Some(chunk) = state.cache.get(&hash).cloned() {
                state.metrics.hits += 1;
                state.metrics.decompressions_avoided += 1;
                return Ok((chunk, CacheOutcome::Hit));
            }
            if let Some(flight) = state.flights.get(&hash).cloned() {
                state.metrics.hits += 1;
                state.metrics.in_flight_waits += 1;
                state.metrics.decompressions_avoided += 1;
                drop(state);
                let mut result = flight.result.lock();
                while result.is_none() {
                    flight.ready.wait(&mut result);
                }
                return match result.as_ref() {
                    Some(Ok(chunk)) => Ok((chunk.clone(), CacheOutcome::SharedLoad)),
                    Some(Err(e)) => Err(Error::Corrupt(e.clone())),
                    None => Err(Error::Corrupt("flight completed without result".into())),
                };
            }
            if state.active < 8 {
                break;
            }
            self.permit.wait(&mut state);
        }
        let flight = Arc::new(Flight {
            result: Mutex::new(None),
            ready: Condvar::new(),
        });
        state.flights.insert(hash, flight.clone());
        state.active += 1;
        state.metrics.misses += 1;
        drop(state);
        // A library caller or codec panic must not strand followers or leak a
        // load permit. Convert it to a hard read error and publish it to waiters.
        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(loader)) {
            Ok(result) => result.map(Arc::new).map_err(|e| e.to_string()),
            Err(_) => Err("object loader panicked".into()),
        };
        let mut state = self.state.lock();
        if let Ok(chunk) = &result {
            state.metrics.decompressions += 1;
            state.metrics.raw_bytes_loaded += chunk.len() as u64;
            if chunk.len() <= self.capacity {
                while state.metrics.resident_bytes > self.capacity - chunk.len() {
                    if let Some((_, evicted)) = state.cache.pop_lru() {
                        state.metrics.resident_bytes -= evicted.len();
                        state.metrics.evictions += 1;
                    } else {
                        break;
                    }
                }
                state.metrics.resident_bytes += chunk.len();
                state.cache.put(hash, chunk.clone());
            }
        }
        *flight.result.lock() = Some(result.clone());
        state.flights.remove(&hash);
        state.active -= 1;
        flight.ready.notify_all();
        self.permit.notify_all();
        drop(state);
        result
            .map(|chunk| (chunk, CacheOutcome::Miss))
            .map_err(Error::Corrupt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
