//! Bounded, best-effort runtime telemetry. Producer callbacks never do file I/O.
use playsparse_core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Read, Write},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

pub const VERSION: u32 = 1;
pub const QUEUE_CAPACITY: usize = 4096;
const MAX_LINE: usize = 256 * 1024;
const MAX_EVENTS: usize = 2_000_000;
const MAX_IDENTITIES: usize = 100_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub version: u32,
    pub session: String,
    pub ts_ns: u64,
    pub op: String,
    pub path: String,
    pub offset: u64,
    pub requested: u64,
    pub returned: u64,
    pub latency_ns: u64,
    pub cache: String,
    pub source: String,
    pub worker: String,
    pub success: bool,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct TraceMetrics {
    pub events_seen: u64,
    pub events_written: u64,
    pub events_dropped: u64,
    pub queue_high_watermark: u64,
    pub writer_errors: u64,
}
#[derive(Default)]
struct Counters {
    seen: AtomicU64,
    written: AtomicU64,
    dropped: AtomicU64,
    queued: AtomicU64,
    high: AtomicU64,
    errors: AtomicU64,
}
enum Message {
    Event(Event),
    Stop,
}
pub struct TraceWriter {
    sender: SyncSender<Message>,
    worker: Mutex<Option<JoinHandle<()>>>,
    counters: Arc<Counters>,
    closed: AtomicBool,
    active: AtomicU64,
    start: Instant,
    session: String,
    capacity: u64,
}
impl TraceWriter {
    /// Exclusive destination creation prevents accidental destruction of old evidence.
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        Self::with_sink(BufWriter::new(file), QUEUE_CAPACITY)
    }
    fn with_sink(mut sink: impl Write + Send + 'static, capacity: usize) -> Result<Self> {
        if !(1..=65536).contains(&capacity) {
            return Err(Error::Invalid(
                "trace queue capacity must be 1..65536".into(),
            ));
        }
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let counters = Arc::new(Counters::default());
        let writer_counts = counters.clone();
        let worker = thread::Builder::new()
            .name("playsparse-trace".into())
            .spawn(move || {
                let mut failed = false;
                while let Ok(message) = receiver.recv() {
                    let Message::Event(event) = message else {
                        break;
                    };
                    writer_counts.queued.fetch_sub(1, Ordering::Relaxed);
                    if !failed {
                        if serde_json::to_writer(&mut sink, &event).is_err()
                            || sink.write_all(b"\n").is_err()
                        {
                            failed = true;
                            writer_counts.errors.fetch_add(1, Ordering::Relaxed);
                        } else {
                            writer_counts.written.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    if failed {
                        writer_counts.dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
                if sink.flush().is_err() {
                    writer_counts.errors.fetch_add(1, Ordering::Relaxed);
                }
            })?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Ok(Self {
            sender,
            worker: Mutex::new(Some(worker)),
            counters,
            closed: AtomicBool::new(false),
            active: AtomicU64::new(0),
            start: Instant::now(),
            session: format!("{}-{nonce}", std::process::id()),
            capacity: capacity as u64,
        })
    }
    /// Only fixed categories and virtual paths belong here; never contents or URLs.
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &self,
        op: &str,
        path: &str,
        offset: u64,
        requested: u64,
        returned: u64,
        latency_ns: u64,
        cache: &str,
        source: &str,
        success: bool,
    ) {
        self.counters.seen.fetch_add(1, Ordering::Relaxed);
        self.active.fetch_add(1, Ordering::SeqCst);
        let _producer = Producer(&self.active);
        if self.closed.load(Ordering::SeqCst)
            || path.len() > 4096
            || op.len() > 32
            || cache.len() > 32
            || source.len() > 32
        {
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let event = Event {
            version: VERSION,
            session: self.session.clone(),
            ts_ns: self.start.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            op: op.into(),
            path: path.into(),
            offset,
            requested,
            returned,
            latency_ns,
            cache: cache.into(),
            source: source.into(),
            worker: format!("{:?}", thread::current().id()),
            success,
        };
        let queued = self.counters.queued.fetch_add(1, Ordering::Relaxed) + 1;
        match self.sender.try_send(Message::Event(event)) {
            Ok(()) => {
                self.counters
                    .high
                    .fetch_max(queued.min(self.capacity), Ordering::Relaxed);
            }
            Err(_) => {
                self.counters.queued.fetch_sub(1, Ordering::Relaxed);
                self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    pub fn metrics(&self) -> TraceMetrics {
        TraceMetrics {
            events_seen: self.counters.seen.load(Ordering::Relaxed),
            events_written: self.counters.written.load(Ordering::Relaxed),
            events_dropped: self.counters.dropped.load(Ordering::Relaxed),
            queue_high_watermark: self.counters.high.load(Ordering::Relaxed),
            writer_errors: self.counters.errors.load(Ordering::Relaxed),
        }
    }
    /// Called after producers finish (unmount). Drain the queue and flush the file.
    pub fn shutdown(&self) {
        let mut worker = self.worker.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = worker.take() {
            self.closed.store(true, Ordering::SeqCst);
            while self.active.load(Ordering::SeqCst) != 0 {
                thread::yield_now();
            }
            let _ = self.sender.send(Message::Stop);
            if handle.join().is_err() {
                self.counters.errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}
struct Producer<'a>(&'a AtomicU64);
impl Drop for Producer<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Drop for TraceWriter {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Exact quantiles within an explicit analysis-size bound, not unbounded ingestion.
pub fn summarize(path: &Path) -> Result<serde_json::Value> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut line = Vec::new();
    let mut operations = BTreeMap::<String, u64>::new();
    let mut sources = BTreeMap::<String, u64>::new();
    let mut files = BTreeMap::<String, u64>::new();
    let mut ranges = BTreeMap::<(String, u64, u64), u64>::new();
    let mut previous_end = BTreeMap::new();
    let mut latencies = Vec::new();
    let mut sessions = BTreeSet::new();
    let (
        mut reads,
        mut writes,
        mut read_bytes,
        mut write_bytes,
        mut sequential,
        mut repeated,
        mut hits,
        mut misses,
    ) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    loop {
        line.clear();
        let count = reader
            .by_ref()
            .take((MAX_LINE + 1) as u64)
            .read_until(b'\n', &mut line)?;
        if count == 0 {
            break;
        }
        if count > MAX_LINE || latencies.len() >= MAX_EVENTS {
            return Err(Error::Invalid(
                "trace analysis exceeds bounded line/event limits".into(),
            ));
        }
        let event: Event = serde_json::from_slice(&line)
            .map_err(|e| Error::Invalid(format!("trace event {}: {e}", latencies.len() + 1)))?;
        if event.version != VERSION
            || (!event.path.is_empty() && !playsparse_core::valid_path(&event.path))
        {
            return Err(Error::Invalid(
                "unsupported trace version or unsafe virtual path".into(),
            ));
        }
        sessions.insert(event.session.clone());
        *operations.entry(event.op.clone()).or_default() += 1;
        *sources.entry(event.source).or_default() += 1;
        *files.entry(event.path.clone()).or_default() += 1;
        latencies.push(event.latency_ns);
        if event.op == "read" {
            reads += 1;
            read_bytes += event.returned;
            if previous_end.insert(
                (event.session, event.worker, event.path.clone()),
                event.offset.saturating_add(event.returned),
            ) == Some(event.offset)
            {
                sequential += 1;
            }
            let visits = ranges
                .entry((event.path, event.offset, event.requested))
                .or_default();
            if *visits > 0 {
                repeated += 1;
            }
            *visits += 1;
            match event.cache.as_str() {
                "hit" => hits += 1,
                "miss" | "mixed" => misses += 1,
                _ => {}
            }
        } else if event.op == "write" {
            writes += 1;
            write_bytes += event.returned;
        }
        if files.len() > MAX_IDENTITIES
            || ranges.len() > MAX_IDENTITIES
            || sessions.len() > MAX_IDENTITIES
            || operations.len() > 64
            || sources.len() > 64
        {
            return Err(Error::Invalid(
                "trace analysis exceeds identity limits".into(),
            ));
        }
    }
    latencies.sort_unstable();
    let percentile = |p: usize| {
        latencies
            .get((latencies.len() * p).div_ceil(100).saturating_sub(1))
            .copied()
            .unwrap_or(0)
    };
    let ratio = |n: u64, d: u64| if d == 0 { 0.0 } else { n as f64 / d as f64 };
    let mut hot_files: Vec<_> = files
        .into_iter()
        .map(|(path, events)| serde_json::json!({"path":path,"events":events}))
        .collect();
    hot_files.sort_by_key(|v| std::cmp::Reverse(v["events"].as_u64().unwrap_or(0)));
    hot_files.truncate(20);
    let mut hot_ranges: Vec<_> = ranges.into_iter().map(|((path,offset,length),events)| serde_json::json!({"path":path,"offset":offset,"length":length,"events":events})).collect();
    hot_ranges.sort_by_key(|v| std::cmp::Reverse(v["events"].as_u64().unwrap_or(0)));
    hot_ranges.truncate(20);
    Ok(
        serde_json::json!({"version":VERSION,"events":latencies.len(),"sessions":sessions,"operations":operations,"read_operations":reads,"write_operations":writes,"read_bytes":read_bytes,"write_bytes":write_bytes,"hot_files":hot_files,"hot_ranges":hot_ranges,"sequential_read_percentage":100.0*ratio(sequential,reads),"reread_ratio":ratio(repeated,reads),"cache_hit_ratio":ratio(hits,hits+misses),"cache_miss_ratio":ratio(misses,hits+misses),"cache_ratio_basis":"read events; mixed counts as miss; bypass excluded","latency_ns":{"p50":percentile(50),"p95":percentile(95),"p99":percentile(99)},"sources":sources}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(trace: &TraceWriter) {
        trace.record(
            "read",
            "data/file",
            0,
            4,
            4,
            23,
            "miss",
            "primary-local",
            true,
        );
    }
    #[test]
    fn concurrent_schema_shutdown_and_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace");
        let trace = Arc::new(TraceWriter::open(&path).unwrap());
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let trace = trace.clone();
                thread::spawn(move || {
                    for _ in 0..40 {
                        event(&trace)
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        trace.shutdown();
        let m = trace.metrics();
        assert_eq!(m.events_seen, m.events_written + m.events_dropped);
        assert_eq!(m.writer_errors, 0);
        assert!(m.queue_high_watermark <= QUEUE_CAPACITY as u64);
        let summary = summarize(&path).unwrap();
        assert_eq!(summary["events"], m.events_written);
        assert_eq!(summary["latency_ns"]["p95"], 23);
        event(&trace);
        assert_eq!(trace.metrics().events_dropped, m.events_dropped + 1);
        assert!(TraceWriter::open(&path).is_err());
    }
    struct Blocked {
        gate: mpsc::Receiver<()>,
        blocked: bool,
    }
    impl Write for Blocked {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            if !self.blocked {
                self.blocked = true;
                let _ = self.gate.recv();
            }
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn full_queue_drops_without_blocking_producer() {
        let (tx, rx) = mpsc::channel();
        let trace = TraceWriter::with_sink(
            Blocked {
                gate: rx,
                blocked: false,
            },
            1,
        )
        .unwrap();
        for _ in 0..100 {
            event(&trace)
        }
        assert!(trace.metrics().events_dropped >= 98);
        tx.send(()).unwrap();
        trace.shutdown();
        assert_eq!(
            trace.metrics().events_seen,
            trace.metrics().events_written + trace.metrics().events_dropped
        );
    }
    #[test]
    fn shutdown_racing_producers_accounts_every_event() {
        let tmp = tempfile::tempdir().unwrap();
        let trace = Arc::new(TraceWriter::open(&tmp.path().join("trace")).unwrap());
        let producers: Vec<_> = (0..8)
            .map(|_| {
                let trace = trace.clone();
                thread::spawn(move || {
                    for _ in 0..2000 {
                        event(&trace)
                    }
                })
            })
            .collect();
        trace.shutdown();
        for p in producers {
            p.join().unwrap();
        }
        let m = trace.metrics();
        assert_eq!(m.events_seen, 16000);
        assert_eq!(m.events_seen, m.events_written + m.events_dropped);
    }
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("injected"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn failed_writer_and_invalid_destination_are_safe() {
        let trace = TraceWriter::with_sink(Broken, 4).unwrap();
        for _ in 0..20 {
            event(&trace)
        }
        trace.shutdown();
        assert_eq!(trace.metrics().writer_errors, 1);
        assert_eq!(trace.metrics().events_dropped, 20);
        let tmp = tempfile::tempdir().unwrap();
        assert!(TraceWriter::open(&tmp.path().join("absent/file")).is_err());
    }
    #[test]
    fn rejects_unknown_schema_and_oversized_line() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bad");
        std::fs::write(&path, b"{\"version\":2}\n").unwrap();
        assert!(summarize(&path).is_err());
        std::fs::write(&path, vec![b' '; MAX_LINE + 1]).unwrap();
        assert!(summarize(&path).is_err());
    }
}
