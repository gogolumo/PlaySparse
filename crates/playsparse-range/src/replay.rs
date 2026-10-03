//! Repeat identical read requests against fresh userspace caches. OS/device
//! caches are uncontrolled; RSS is process peak, so separate CLI processes are
//! required when comparing memory peaks between modes.
use super::*;
use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader, Read},
};

struct Query {
    path: Arc<str>,
    offset: u64,
    len: usize,
}
const MAX_READS: usize = 100_000;
const MAX_LINE: u64 = 256 * 1024;
fn workload(store: &Store, trace: &Path) -> Result<Vec<Query>> {
    let mut input = BufReader::new(File::open(trace)?);
    let mut line = Vec::new();
    let mut queries = Vec::new();
    let mut paths = HashMap::<String, Arc<str>>::new();
    let mut events = 0usize;
    loop {
        line.clear();
        let count = input
            .by_ref()
            .take(MAX_LINE + 1)
            .read_until(b'\n', &mut line)?;
        if count == 0 {
            break;
        }
        events += 1;
        if count as u64 > MAX_LINE || events > 2_000_000 {
            return Err(Error::Invalid("replay trace exceeds bounded limits".into()));
        }
        let event: playsparse_trace::Event = serde_json::from_slice(&line)
            .map_err(|error| Error::Invalid(format!("invalid replay event: {error}")))?;
        if event.version != playsparse_trace::VERSION {
            return Err(Error::Invalid("unsupported trace version".into()));
        }
        if event.op != "read" || !event.success {
            continue;
        }
        if event.source == "overlay" {
            return Err(Error::Invalid("replay requires readonly base traffic; overlay byte versions are not retained in traces".into()));
        }
        if event.requested > MAX_READ_BYTES as u64
            || event.path.len() > 4096
            || queries.len() >= MAX_READS
        {
            return Err(Error::Invalid("replay read/path limits exceeded".into()));
        }
        store.manifest().file(&event.path)?;
        let path = paths
            .entry(event.path.clone())
            .or_insert_with(|| Arc::from(event.path));
        queries.push(Query {
            path: path.clone(),
            offset: event.offset,
            len: event.requested as usize,
        });
        if paths.len() > 4096 {
            return Err(Error::Invalid("replay exceeds 4096 distinct paths".into()));
        }
    }
    if queries.is_empty() {
        return Err(Error::Invalid(
            "replay trace has no successful base reads".into(),
        ));
    }
    Ok(queries)
}

fn run(
    store: &Path,
    queries: &[Query],
    cache_bytes: usize,
    policy: Option<Policy>,
) -> Result<serde_json::Value> {
    let reader = RangeResolver::open_configured(
        store,
        RuntimeOptions {
            cache_bytes,
            policy,
            ..Default::default()
        },
    )?;
    let before = resources();
    let started = Instant::now();
    let mut latencies = Vec::with_capacity(queries.len());
    let mut digest = blake3::Hasher::new();
    let mut returned = 0u64;
    for query in queries {
        let start = Instant::now();
        let bytes = reader.read_range(&query.path, query.offset, query.len)?;
        latencies.push(start.elapsed().as_nanos().min(u64::MAX as u128) as u64);
        returned += bytes.len() as u64;
        digest.update(&(bytes.len() as u64).to_le_bytes());
        digest.update(&bytes);
    }
    reader.stop_prefetch();
    let wall = started.elapsed().as_secs_f64();
    let after = resources();
    let metrics = reader.metrics();
    latencies.sort_unstable();
    let percentile =
        |percent: usize| latencies[(latencies.len() * percent).div_ceil(100).saturating_sub(1)];
    let cpu = before
        .0
        .zip(after.0)
        .map(|(before, after)| (after - before).max(0.0));
    Ok(
        serde_json::json!({"queries":queries.len(),"returned_bytes":returned,"content_blake3":digest.finalize().to_hex().to_string(),"latency_ns":{"p50":percentile(50),"p95":percentile(95),"p99":percentile(99)},"wall_seconds":wall,"cpu_seconds":cpu,"process_peak_rss_bytes":after.1,"rss_basis":"process lifetime peak; use separate replay-one child processes for mode comparison","cache_state":"fresh empty userspace cache; OS and device cache uncontrolled","cache":metrics,"read_amplification":if returned==0{0.0}else{metrics.raw_bytes_loaded as f64/returned as f64}}),
    )
}

pub fn replay_one(
    store: &Path,
    trace: &Path,
    cache_bytes: usize,
    policy: Option<Policy>,
) -> Result<serde_json::Value> {
    let base = Store::open(store)?;
    let queries = workload(&base, trace)?;
    run(store, &queries, cache_bytes, policy)
}

pub fn replay_trace(
    store: &Path,
    trace: &Path,
    cache_bytes: usize,
    policy: &Policy,
    repetitions: usize,
) -> Result<serde_json::Value> {
    let base = Store::open(store)?;
    let queries = workload(&base, trace)?;
    compare(store, &queries, cache_bytes, policy, repetitions)
}
pub fn replay_synthetic(
    store: &Path,
    cache_bytes: usize,
    policy: &Policy,
    repetitions: usize,
) -> Result<serde_json::Value> {
    let base = Store::open(store)?;
    let file = base
        .manifest()
        .files
        .iter()
        .max_by_key(|file| file.size)
        .ok_or_else(|| Error::Invalid("empty replay store".into()))?;
    let path: Arc<str> = Arc::from(file.path.as_str());
    let mut queries = Vec::new();
    for chunk in file.chunks.iter().take(128) {
        queries.push(Query {
            path: path.clone(),
            offset: chunk.offset,
            len: (chunk.raw_size as usize).min(65536),
        });
    }
    for cycle in 0..128usize {
        for chunk in file.chunks.iter().take(4) {
            queries.push(Query {
                path: path.clone(),
                offset: chunk.offset,
                len: 4096.min(chunk.raw_size as usize),
            });
        }
        if let Some(chunk) = file.chunks.get(cycle % file.chunks.len().max(1)) {
            queries.push(Query {
                path: path.clone(),
                offset: chunk.offset,
                len: 4096.min(chunk.raw_size as usize),
            });
        }
    }
    if queries.is_empty() {
        return Err(Error::Invalid(
            "synthetic replay needs nonempty data".into(),
        ));
    }
    compare(store, &queries, cache_bytes, policy, repetitions)
}
fn compare(
    store: &Path,
    queries: &[Query],
    cache_bytes: usize,
    policy: &Policy,
    repetitions: usize,
) -> Result<serde_json::Value> {
    if !(1..=10).contains(&repetitions) {
        return Err(Error::Invalid("replay repetitions must be 1..10".into()));
    }
    policy.validate()?;
    let mut runs = Vec::new();
    let mut expected = None;
    for repetition in 0..repetitions {
        for adaptive in if repetition % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        } {
            let result = run(
                store,
                queries,
                cache_bytes,
                adaptive.then(|| policy.clone()),
            )?;
            let digest = result["content_blake3"].as_str().unwrap().to_string();
            if expected
                .as_ref()
                .is_some_and(|expected| expected != &digest)
            {
                return Err(Error::Corrupt(
                    "static/adaptive replay byte hashes differ".into(),
                ));
            }
            expected = Some(digest);
            runs.push(serde_json::json!({"repetition":repetition+1,"mode":if adaptive {"adaptive"}else {"static"},"result":result}));
        }
    }
    Ok(
        serde_json::json!({"version":1,"cache_bytes":cache_bytes,"workload_reads":queries.len(),"bytes_verified_equal":true,"order":"alternating modes; fresh userspace cache each run; OS cache uncontrolled","policy":policy,"runs":runs}),
    )
}

fn resources() -> (Option<f64>, Option<u64>) {
    #[cfg(unix)]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        // SAFETY: getrusage fills the correctly sized output on success.
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
            return (None, None);
        }
        let usage = unsafe { usage.assume_init() };
        let cpu = usage.ru_utime.tv_sec as f64
            + usage.ru_utime.tv_usec as f64 / 1e6
            + usage.ru_stime.tv_sec as f64
            + usage.ru_stime.tv_usec as f64 / 1e6;
        (
            Some(cpu),
            Some(usage.ru_maxrss.max(0) as u64 * if cfg!(target_os = "macos") { 1 } else { 1024 }),
        )
    }
    #[cfg(windows)]
    {
        use windows::Win32::{
            Foundation::FILETIME,
            System::{
                ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
                Threading::{GetCurrentProcess, GetProcessTimes},
            },
        };
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        let ticks =
            |time: FILETIME| ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64;
        let cpu = unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        }
        .ok()
        .map(|_| (ticks(kernel) + ticks(user)) as f64 / 1e7);
        let mut memory = PROCESS_MEMORY_COUNTERS {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ..Default::default()
        };
        let rss = unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut memory,
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            )
        }
        .ok()
        .map(|_| memory.PeakWorkingSetSize as u64);
        (cpu, rss)
    }
    #[cfg(not(any(unix, windows)))]
    {
        (None, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use playsparse_store::{PackOptions, pack_directory};
    #[test]
    fn deterministic_replay_and_prefetch_accounting() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let bytes: Vec<u8> = (0..65536).map(|index| (index / 4096) as u8).collect();
        std::fs::write(source.join("file"), bytes).unwrap();
        let store = tmp.path().join("store");
        pack_directory(
            &source,
            &store,
            &PackOptions {
                chunk_size: 4096,
                chunker: playsparse_core::Chunker::Fixed,
                ..Default::default()
            },
        )
        .unwrap();
        let report = replay_synthetic(&store, 16384, &Policy::default(), 2).unwrap();
        assert_eq!(report["bytes_verified_equal"], true);
        assert_eq!(report["runs"].as_array().unwrap().len(), 4);
        for run in report["runs"].as_array().unwrap() {
            let cache = &run["result"]["cache"];
            assert!(cache["peak_resident_bytes"].as_u64().unwrap() <= 16384);
            assert_eq!(
                cache["prefetch_bytes_loaded"].as_u64().unwrap(),
                cache["prefetch_useful_bytes"].as_u64().unwrap()
                    + cache["prefetch_wasted_bytes"].as_u64().unwrap()
            );
        }
    }
}
