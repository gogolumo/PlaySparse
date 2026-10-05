use parking_lot::Mutex;
use playsparse_cache::{CacheMetrics, ChunkCache};
use playsparse_policy::Prefetch;
use playsparse_store::Store;
use std::{
    collections::{HashMap, VecDeque},
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
    recent: HashMap<[u8; 32], Instant>,
    recent_order: VecDeque<([u8; 32], Instant)>,
    reserved: usize,
    requests: u64,
    expired: u64,
    dropped: u64,
    skipped: u64,
    duplicates: u64,
    high: usize,
    peak: usize,
}
const MAX_RECENT: usize = 4096;
impl State {
    fn remember(&mut self, hash: [u8; 32], until: Instant) {
        while self.recent_order.len() >= MAX_RECENT {
            if let Some((old, deadline)) = self.recent_order.pop_front()
                && self.recent.get(&old) == Some(&deadline)
            {
                self.recent.remove(&old);
            }
        }
        self.recent.insert(hash, until);
        self.recent_order.push_back((hash, until));
    }
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
        let ttl = Duration::from_millis(config.ttl_ms);
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
                        let _ = cache.prefetch(task.hash, task.bytes, || loader(task.hash));
                    }
                    let mut state = worker_state.lock();
                    state.pending.remove(&task.hash);
                    state.reserved -= task.bytes;
                    state.remember(task.hash, Instant::now() + ttl);
                }
            })?;
        Ok(Self {
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(Some(worker)),
            state,
            stop,
            budget,
            ttl,
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
        let now = Instant::now();
        if state.pending.contains_key(&hash)
            || state.recent.get(&hash).is_some_and(|until| now < *until)
        {
            state.duplicates += 1;
            return;
        }
        if !cache.can_prefetch(&hash, bytes) {
            state.skipped += 1;
            state.remember(hash, now + self.ttl);
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
        metrics.prefetch_skipped += state.skipped;
        metrics.prefetch_duplicates = state.duplicates;
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
    fn wait_idle(worker: &Prefetcher) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !worker.state.lock().pending.is_empty() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(worker.state.lock().pending.is_empty());
    }
    #[test]
    fn worker_rechecks_queued_admission_after_demand_changes_the_cache() {
        let cache = Arc::new(ChunkCache::with_adaptive(12, true, 1_000_000));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = Prefetcher::start(
            &Prefetch {
                budget_bytes: 12,
                queue_depth: 2,
                ttl_ms: 30_000,
                ..Default::default()
            },
            cache.clone(),
            move |hash| {
                assert_eq!(hash, [1; 32], "queued object must be refused before I/O");
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok((vec![1; 4], "primary-local"))
            },
        )
        .unwrap();
        worker.request([1; 32], 4, &cache);
        started_rx.recv().unwrap();
        worker.request([2; 32], 8, &cache);
        cache
            .get_or_load_tagged([3; 32], 100, false, || Ok((vec![3; 12], "primary-local")))
            .unwrap();
        release_tx.send(()).unwrap();
        wait_idle(&worker);
        worker.stop();
        let mut metrics = cache.metrics();
        worker.add_metrics(&mut metrics);
        assert_eq!(metrics.prefetch_requests, 2);
        assert_eq!(metrics.prefetch_chunks_loaded, 1);
        assert_eq!(metrics.prefetch_wasted_bytes, 4);
        assert_eq!(metrics.prefetch_skipped, 1);
        assert_eq!(metrics.prefetch_reserved_bytes, 0);
        assert!(cache.contains(&[3; 32]));
    }
    #[test]
    fn failed_objects_are_not_requeued_by_repeated_subchunk_predictions() {
        let cache = Arc::new(ChunkCache::new(8));
        let worker = Prefetcher::start(
            &Prefetch {
                budget_bytes: 8,
                ttl_ms: 30_000,
                ..Default::default()
            },
            cache.clone(),
            |_| Err(playsparse_core::Error::Corrupt("injected failure".into())),
        )
        .unwrap();
        worker.request([1; 32], 4, &cache);
        wait_idle(&worker);
        for _ in 0..1000 {
            worker.request([1; 32], 4, &cache);
        }
        worker.stop();
        let mut metrics = cache.metrics();
        worker.add_metrics(&mut metrics);
        assert_eq!(metrics.prefetch_requests, 1);
        assert_eq!(metrics.prefetch_errors, 1);
        assert_eq!(metrics.prefetch_duplicates, 1000);
        assert_eq!(metrics.raw_bytes_loaded, 0);
    }
    #[test]
    fn rejected_admission_history_is_bounded_without_source_io() {
        let cache = Arc::new(ChunkCache::with_adaptive(8, true, 1_000_000));
        cache
            .get_or_load_tagged([255; 32], 100, false, || Ok((vec![1; 8], "primary-local")))
            .unwrap();
        let worker = Prefetcher::start(
            &Prefetch {
                budget_bytes: 8,
                ttl_ms: 30_000,
                ..Default::default()
            },
            cache.clone(),
            |_| panic!("protected cache must not perform speculative I/O"),
        )
        .unwrap();
        for index in 0..2 * MAX_RECENT {
            let mut hash = [0; 32];
            hash[..8].copy_from_slice(&(index as u64).to_le_bytes());
            worker.request(hash, 4, &cache);
        }
        worker.stop();
        let state = worker.state.lock();
        assert!(state.recent.len() <= MAX_RECENT);
        assert!(state.recent_order.len() <= MAX_RECENT);
        assert_eq!(state.skipped, (2 * MAX_RECENT) as u64);
        assert_eq!(state.requests, 0);
        assert_eq!(cache.metrics().raw_bytes_loaded, 8);
    }
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
