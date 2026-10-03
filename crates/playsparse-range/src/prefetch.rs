use parking_lot::Mutex;
use playsparse_cache::{CacheMetrics, ChunkCache};
use playsparse_policy::Prefetch;
use playsparse_store::Store;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

struct Task {
    hash: [u8; 32],
    bytes: usize,
    deadline: Instant,
}
#[derive(Default)]
struct State {
    pending: HashMap<[u8; 32], usize>,
    reserved: usize,
    requests: u64,
    expired: u64,
    dropped: u64,
    high: usize,
    peak: usize,
}
pub(super) struct Prefetcher {
    sender: Mutex<Option<SyncSender<Task>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    budget: usize,
    ttl: Duration,
}
impl Prefetcher {
    pub(super) fn new(
        config: &Prefetch,
        store: Arc<Store>,
        cache: Arc<ChunkCache>,
    ) -> std::io::Result<Self> {
        Self::start(config, cache, move |hash| {
            store
                .read_object_with_source(&hash)
                .map(|(bytes, source)| (bytes, source.as_str()))
        })
    }
    fn start(
        config: &Prefetch,
        cache: Arc<ChunkCache>,
        loader: impl Fn([u8; 32]) -> playsparse_core::Result<(Vec<u8>, &'static str)> + Send + 'static,
    ) -> std::io::Result<Self> {
        let budget = config.budget_bytes.min(cache.capacity());
        let (sender, receiver) = mpsc::sync_channel::<Task>(config.queue_depth);
        let state = Arc::new(Mutex::new(State::default()));
        let worker_state = state.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = thread::Builder::new()
            .name("playsparse-prefetch".into())
            .spawn(move || {
                while let Ok(task) = receiver.recv() {
                    if worker_stop.load(Ordering::Relaxed) || Instant::now() > task.deadline {
                        worker_state.lock().expired += 1;
                    } else {
                        let _ = cache.get_or_load_tagged(task.hash, 0, true, || loader(task.hash));
                    }
                    let mut state = worker_state.lock();
                    state.pending.remove(&task.hash);
                    state.reserved -= task.bytes;
                }
            })?;
        Ok(Self {
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(Some(worker)),
            state,
            stop,
            budget,
            ttl: Duration::from_millis(config.ttl_ms),
        })
    }
    pub(super) fn request(&self, hash: [u8; 32], bytes: usize, cache: &ChunkCache) {
        self.request_deadline(hash, bytes, cache, Instant::now() + self.ttl);
    }
    fn request_deadline(
        &self,
        hash: [u8; 32],
        bytes: usize,
        cache: &ChunkCache,
        deadline: Instant,
    ) {
        if self.stop.load(Ordering::Relaxed)
            || bytes == 0
            || bytes > self.budget
            || cache.contains(&hash)
        {
            return;
        }
        let mut state = self.state.lock();
        if state.pending.contains_key(&hash) {
            return;
        }
        if state.reserved > self.budget - bytes {
            state.dropped += 1;
            return;
        }
        let task = Task {
            hash,
            bytes,
            deadline,
        };
        if self
            .sender
            .lock()
            .as_ref()
            .is_none_or(|sender| sender.try_send(task).is_err())
        {
            state.dropped += 1;
            return;
        }
        state.pending.insert(hash, bytes);
        state.reserved += bytes;
        state.requests += 1;
        state.high = state.high.max(state.pending.len());
        state.peak = state.peak.max(state.reserved);
    }
    pub(super) fn add_metrics(&self, metrics: &mut CacheMetrics) {
        let state = self.state.lock();
        metrics.prefetch_requests = state.requests;
        metrics.prefetch_expired = state.expired;
        metrics.prefetch_dropped = state.dropped;
        metrics.prefetch_queue_high_watermark = state.high;
        metrics.prefetch_reserved_bytes = state.reserved;
        metrics.prefetch_peak_reserved_bytes = state.peak;
    }
    pub(super) fn stop(&self) {
        let mut worker = self.worker.lock();
        self.stop.store(true, Ordering::Relaxed);
        self.sender.lock().take();
        if let Some(worker) = worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Prefetcher {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reservations_dedup_queue_and_cancellation_stay_bounded() {
        let cache = Arc::new(ChunkCache::new(8));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let config = Prefetch {
            budget_bytes: 8,
            queue_depth: 1,
            ..Default::default()
        };
        let worker = Prefetcher::start(&config, cache.clone(), move |hash| {
            if hash == [1; 32] {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            }
            Ok((vec![hash[0]; 4], "primary-local"))
        })
        .unwrap();
        worker.request([1; 32], 4, &cache);
        started_rx.recv().unwrap();
        worker.request([1; 32], 4, &cache);
        worker.request([2; 32], 4, &cache);
        worker.request([3; 32], 4, &cache);
        let mut metrics = cache.metrics();
        worker.add_metrics(&mut metrics);
        assert_eq!(metrics.prefetch_requests, 2);
        assert_eq!(metrics.prefetch_reserved_bytes, 8);
        assert_eq!(metrics.prefetch_peak_reserved_bytes, 8);
        assert_eq!(metrics.prefetch_dropped, 1);
        release_tx.send(()).unwrap();
        worker.stop();
        worker.add_metrics(&mut metrics);
        assert_eq!(metrics.prefetch_reserved_bytes, 0);
        assert!(metrics.prefetch_queue_high_watermark <= 2);
        assert!(cache.metrics().peak_resident_bytes <= 8);
    }
    #[test]
    fn expired_requests_never_execute_loader() {
        let cache = Arc::new(ChunkCache::new(8));
        let worker = Prefetcher::start(
            &Prefetch {
                budget_bytes: 8,
                ..Default::default()
            },
            cache.clone(),
            |_| panic!("expired prefetch must not perform I/O"),
        )
        .unwrap();
        worker.request_deadline([1; 32], 4, &cache, Instant::now() - Duration::from_secs(1));
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut metrics = cache.metrics();
        loop {
            worker.add_metrics(&mut metrics);
            if metrics.prefetch_expired == 1 || Instant::now() > deadline {
                break;
            }
            std::thread::yield_now();
        }
        worker.stop();
        assert_eq!(metrics.prefetch_expired, 1);
        assert_eq!(cache.metrics().raw_bytes_loaded, 0);
        assert_eq!(cache.metrics().prefetch_errors, 0);
    }
}
