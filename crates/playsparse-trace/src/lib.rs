//! Bounded, best-effort runtime telemetry. Producer callbacks never do file I/O.
use playsparse_core::{Error, Result};
use serde::{Deserialize, Serialize};
mod ranges;
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
const MAX_PATH_BYTES: usize = 4096;
const MAX_CATEGORY_BYTES: usize = 32;
const MAX_IDENTITY_BYTES: usize = 64;
const MAX_RETAINED_KEY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub version: u32,
    #[serde(deserialize_with = "deserialize_bounded_string::<_, MAX_IDENTITY_BYTES>")]
    pub session: String,
    pub ts_ns: u64,
    #[serde(deserialize_with = "deserialize_bounded_string::<_, MAX_CATEGORY_BYTES>")]
    pub op: String,
    #[serde(deserialize_with = "deserialize_bounded_string::<_, MAX_PATH_BYTES>")]
    pub path: String,
    pub offset: u64,
    pub requested: u64,
    pub returned: u64,
    pub latency_ns: u64,
    #[serde(deserialize_with = "deserialize_bounded_string::<_, MAX_CATEGORY_BYTES>")]
    pub cache: String,
    #[serde(deserialize_with = "deserialize_bounded_string::<_, MAX_CATEGORY_BYTES>")]
    pub source: String,
    #[serde(deserialize_with = "deserialize_bounded_string::<_, MAX_IDENTITY_BYTES>")]
    pub worker: String,
    pub success: bool,
}

fn deserialize_bounded_string<'de, D, const LIMIT: usize>(
    deserializer: D,
) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value.len() > LIMIT {
        return Err(serde::de::Error::custom(format!(
            "trace string field exceeds {LIMIT} byte limit"
        )));
    }
    Ok(value)
}

fn reserve_key_bytes(retained: &mut usize, additional: usize) -> Result<()> {
    *retained = retained
        .checked_add(additional)
        .filter(|bytes| *bytes <= MAX_RETAINED_KEY_BYTES)
        .ok_or_else(|| Error::Invalid("trace analysis exceeds retained key byte limit".into()))?;
    Ok(())
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
            || path.len() > MAX_PATH_BYTES
            || op.len() > MAX_CATEGORY_BYTES
            || cache.len() > MAX_CATEGORY_BYTES
            || source.len() > MAX_CATEGORY_BYTES
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
    let mut ranges = ranges::Ranges::new()?;
    let mut file_ids = BTreeMap::<String, u64>::new();
    let mut previous_end = BTreeMap::new();
    let mut latencies = Vec::new();
    let mut sessions = BTreeSet::new();
    let mut retained_key_bytes = 0;
    let (
        mut reads,
        mut writes,
        mut read_bytes,
        mut write_bytes,
        mut sequential,
        mut hits,
        mut misses,
    ) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
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
        if !sessions.contains(&event.session) {
            reserve_key_bytes(&mut retained_key_bytes, event.session.len())?;
            sessions.insert(event.session.clone());
        }
        if !operations.contains_key(&event.op) {
            reserve_key_bytes(&mut retained_key_bytes, event.op.len())?;
        }
        *operations.entry(event.op.clone()).or_default() += 1;
        if !sources.contains_key(&event.source) {
            reserve_key_bytes(&mut retained_key_bytes, event.source.len())?;
        }
        *sources.entry(event.source).or_default() += 1;
        if !files.contains_key(&event.path) {
            reserve_key_bytes(&mut retained_key_bytes, event.path.len())?;
            reserve_key_bytes(&mut retained_key_bytes, event.path.len())?;
            file_ids.insert(event.path.clone(), file_ids.len() as u64);
        }
        *files.entry(event.path.clone()).or_default() += 1;
        latencies.push(event.latency_ns);
        if event.op == "read" {
            reads += 1;
            read_bytes = read_bytes
                .checked_add(event.returned)
                .ok_or_else(|| Error::Invalid("trace read byte total overflows u64".into()))?;
            let stream = (event.session, event.worker, event.path.clone());
            if !previous_end.contains_key(&stream) {
                reserve_key_bytes(
                    &mut retained_key_bytes,
                    stream.0.len() + stream.1.len() + stream.2.len(),
                )?;
            }
            if previous_end.insert(stream, event.offset.saturating_add(event.returned))
                == Some(event.offset)
            {
                sequential += 1;
            }
            ranges.add((file_ids[&event.path], event.offset, event.requested))?;
            match event.cache.as_str() {
                "hit" => hits += 1,
                "miss" | "mixed" => misses += 1,
                _ => {}
            }
        } else if event.op == "write" {
            writes += 1;
            write_bytes = write_bytes
                .checked_add(event.returned)
                .ok_or_else(|| Error::Invalid("trace write byte total overflows u64".into()))?;
        }
        if files.len() > MAX_IDENTITIES
            || sessions.len() > MAX_IDENTITIES
            || previous_end.len() > MAX_IDENTITIES
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
    let mut hot_files: Vec<_> = files.into_iter().collect();
    hot_files.sort_by_key(|(_, events)| std::cmp::Reverse(*events));
    hot_files.truncate(20);
    let hot_files: Vec<_> = hot_files
        .into_iter()
        .map(|(path, events)| serde_json::json!({"path":path,"events":events}))
        .collect();
    let (unique_ranges, hot_ranges) = ranges.finish()?;
    let repeated = reads - unique_ranges;
    let mut paths = vec![String::new(); file_ids.len()];
    for (path, id) in file_ids {
        paths[id as usize] = path;
    }
    let hot_ranges: Vec<_> = hot_ranges.into_iter().map(|((id,offset,length),events)| serde_json::json!({"path":paths[id as usize],"offset":offset,"length":length,"events":events})).collect();
    Ok(
        serde_json::json!({"version":VERSION,"range_aggregation":"exact external sorted runs; scratch removed on exit","unique_read_ranges":unique_ranges,"analysis_limits":{"events":MAX_EVENTS,"file_session_stream_identities":MAX_IDENTITIES,"retained_key_bytes":MAX_RETAINED_KEY_BYTES,"range_sort_run_keys":16384,"range_sort_max_runs":128},"events":latencies.len(),"sessions":sessions,"operations":operations,"read_operations":reads,"write_operations":writes,"read_bytes":read_bytes,"write_bytes":write_bytes,"hot_files":hot_files,"hot_ranges":hot_ranges,"sequential_read_percentage":100.0*ratio(sequential,reads),"reread_ratio":ratio(repeated,reads),"cache_hit_ratio":ratio(hits,hits+misses),"cache_miss_ratio":ratio(misses,hits+misses),"cache_ratio_basis":"read events; mixed counts as miss; bypass excluded","latency_ns":{"p50":percentile(50),"p95":percentile(95),"p99":percentile(99)},"sources":sources}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn imported_event(op: &str) -> Event {
        Event {
            version: VERSION,
            session: "session".into(),
            ts_ns: 0,
            op: op.into(),
            path: "data/file".into(),
            offset: 0,
            requested: 4,
            returned: 4,
            latency_ns: 23,
            cache: "miss".into(),
            source: "primary-local".into(),
            worker: "worker".into(),
            success: true,
        }
    }
    fn write_imported(path: &Path, events: impl IntoIterator<Item = Event>) {
        let mut writer = BufWriter::new(File::create(path).unwrap());
        for event in events {
            serde_json::to_writer(&mut writer, &event).unwrap();
            writer.write_all(b"\n").unwrap();
        }
        writer.flush().unwrap();
    }
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
    #[test]
    fn rejects_oversized_imported_fields_in_all_event_consumers() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace");
        for field in ["session", "worker", "path", "op", "cache", "source"] {
            let mut event = imported_event("read");
            let (value, limit) = match field {
                "session" => (&mut event.session, MAX_IDENTITY_BYTES),
                "worker" => (&mut event.worker, MAX_IDENTITY_BYTES),
                "path" => (&mut event.path, MAX_PATH_BYTES),
                "op" => (&mut event.op, MAX_CATEGORY_BYTES),
                "cache" => (&mut event.cache, MAX_CATEGORY_BYTES),
                "source" => (&mut event.source, MAX_CATEGORY_BYTES),
                _ => unreachable!(),
            };
            *value = "x".repeat(limit + 1);
            let serialized = serde_json::to_vec(&event).unwrap();
            assert!(
                serde_json::from_slice::<Event>(&serialized).is_err(),
                "{field} was accepted by Event deserialization"
            );
            write_imported(&path, [event]);
            assert!(summarize(&path).is_err(), "{field} was accepted");
        }
    }
    #[test]
    fn summarizes_more_than_100k_unique_ranges_exactly() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace");
        write_imported(
            &path,
            (0..110001).map(|index| {
                let mut event = imported_event("read");
                event.offset = if index == 110000 { 90000 } else { index };
                event
            }),
        );
        let report = summarize(&path).unwrap();
        assert_eq!(report["unique_read_ranges"], 110000);
        assert_eq!(report["hot_ranges"][0]["offset"], 90000);
        assert_eq!(report["hot_ranges"][0]["events"], 2);
        assert_eq!(report["read_bytes"], 440004);
        assert_eq!(report["reread_ratio"], 1.0 / 110001.0);
    }
    #[test]
    fn rejects_unbounded_distinct_read_workers() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace");
        write_imported(
            &path,
            (0..=MAX_IDENTITIES).map(|index| {
                let mut event = imported_event("read");
                event.worker = format!("worker-{index}");
                event
            }),
        );
        let error = summarize(&path).unwrap_err().to_string();
        assert!(error.contains("identity limits"), "{error}");
    }
    #[test]
    fn rejects_retained_string_keys_before_identity_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace");
        let suffix = "x".repeat(MAX_PATH_BYTES - 6);
        let events = MAX_RETAINED_KEY_BYTES / MAX_PATH_BYTES + 1;
        assert!(events < MAX_IDENTITIES);
        write_imported(
            &path,
            (0..events).map(|index| {
                let mut event = imported_event("getattr");
                event.path = format!("{index:05}/{suffix}");
                event
            }),
        );
        let error = summarize(&path).unwrap_err().to_string();
        assert!(error.contains("retained key byte limit"), "{error}");
    }
    #[test]
    fn rejects_overflowing_read_and_write_totals() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace");
        for op in ["read", "write"] {
            write_imported(
                &path,
                [u64::MAX, 1].map(|returned| {
                    let mut event = imported_event(op);
                    event.returned = returned;
                    event
                }),
            );
            let error = summarize(&path).unwrap_err().to_string();
            assert!(error.contains("byte total overflows u64"), "{error}");
        }
    }
    #[test]
    fn preserves_operation_specific_request_and_return_values() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace");
        let mut event = imported_event("truncate");
        event.requested = 0;
        event.returned = u64::MAX;
        write_imported(&path, [event]);
        let summary = summarize(&path).unwrap();
        assert_eq!(summary["events"], 1);
        assert_eq!(summary["read_bytes"], 0);
        assert_eq!(summary["write_bytes"], 0);
    }
}
