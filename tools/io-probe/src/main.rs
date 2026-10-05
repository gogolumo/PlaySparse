use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use memmap2::MmapOptions;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::time::Instant;

#[derive(Parser)]
#[command(about = "Compare ordinary filesystem reads with a mounted PlaySparse view")]
struct Args {
    original: Option<PathBuf>,
    mounted: Option<PathBuf>,
    #[arg(long, default_value_t = 64)]
    iterations: usize,
    #[arg(long)]
    output: Option<PathBuf>,
    /// Optional pre-existing NTFS/WOF or other baseline directory.
    #[arg(long)]
    baseline: Option<PathBuf>,
    /// Maximum sequential bytes per file; 0 reads every byte.
    #[arg(long, default_value_t = 67_108_864)]
    sequential_limit: u64,
    /// Read fixture.json and assets next to the running executable.
    #[arg(long)]
    self_test: bool,
}

#[derive(Default)]
struct Samples {
    nanos: Vec<u64>,
    bytes: u64,
    hash: blake3::Hasher,
}

impl Samples {
    fn add(&mut self, elapsed: u128, data: &[u8]) {
        self.nanos.push(elapsed.min(u64::MAX as u128) as u64);
        self.bytes += data.len() as u64;
        self.hash.update(data);
    }

    fn report(mut self) -> Timing {
        self.nanos.sort_unstable();
        let percentile = |p: usize| -> f64 {
            if self.nanos.is_empty() {
                return 0.0;
            }
            let index = (self.nanos.len() * p).div_ceil(100).saturating_sub(1);
            self.nanos[index.min(self.nanos.len() - 1)] as f64 / 1_000_000.0
        };
        let seconds = self.nanos.iter().map(|n| *n as f64).sum::<f64>() / 1e9;
        Timing {
            operations: self.nanos.len(),
            bytes: self.bytes,
            p50_ms: percentile(50),
            p95_ms: percentile(95),
            p99_ms: percentile(99),
            measured_operation_seconds: seconds,
            throughput_mib_s: if seconds > 0.0 {
                self.bytes as f64 / 1_048_576.0 / seconds
            } else {
                0.0
            },
            blake3: self.hash.finalize().to_hex().to_string(),
            errors: 0,
        }
    }
}

#[derive(Serialize)]
struct Timing {
    operations: usize,
    bytes: u64,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    measured_operation_seconds: f64,
    throughput_mib_s: f64,
    blake3: String,
    errors: usize,
}

#[derive(Serialize)]
struct Workload {
    name: String,
    read_bytes: usize,
    threads: usize,
    original: Timing,
    candidate: Timing,
    bytes_equal: bool,
    wall_seconds: f64,
}

#[derive(Serialize)]
struct Comparison {
    candidate: String,
    logical_bytes: u64,
    files: usize,
    directory_enumeration_equal: bool,
    workloads: Vec<Workload>,
}

#[derive(Serialize)]
struct Report {
    schema: u32,
    os: &'static str,
    arch: &'static str,
    original: String,
    iterations_per_thread: usize,
    sequential_limit_per_file: u64,
    page_cache_policy: &'static str,
    equality: &'static str,
    comparisons: Vec<Comparison>,
    process_cpu_seconds: Option<f64>,
    peak_rss_bytes: Option<u64>,
}

#[derive(Deserialize)]
struct Fixture {
    schema: u32,
    files: Vec<FixtureFile>,
}
#[derive(Deserialize)]
struct FixtureFile {
    path: String,
    size: u64,
    samples: Vec<FixtureSample>,
}
#[derive(Deserialize)]
struct FixtureSample {
    offset: u64,
    data_hex: String,
}

fn files(root: &Path) -> Result<BTreeMap<PathBuf, u64>> {
    fn visit(root: &Path, current: &Path, result: &mut BTreeMap<PathBuf, u64>) -> Result<()> {
        for entry in fs::read_dir(current)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                visit(root, &entry.path(), result)?;
            } else if kind.is_file() {
                result.insert(
                    entry.path().strip_prefix(root)?.to_owned(),
                    entry.metadata()?.len(),
                );
            } else {
                bail!("unsupported fixture entry {}", entry.path().display());
            }
        }
        Ok(())
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result)?;
    ensure!(
        !result.is_empty(),
        "directory contains no files: {}",
        root.display()
    );
    Ok(result)
}

fn read_at(file: &mut File, offset: u64, len: usize) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; len];
    let mut used = 0;
    while used < len {
        let count = file.read(&mut bytes[used..])?;
        if count == 0 {
            break;
        }
        used += count;
    }
    bytes.truncate(used);
    Ok(bytes)
}

fn checked_pair(
    a: &mut File,
    b: &mut File,
    offset: u64,
    len: usize,
    sa: &mut Samples,
    sb: &mut Samples,
) -> Result<()> {
    let start = Instant::now();
    let original = read_at(a, offset, len)?;
    let original_ns = start.elapsed().as_nanos();
    let start = Instant::now();
    let candidate = read_at(b, offset, len)?;
    let candidate_ns = start.elapsed().as_nanos();
    ensure!(
        original == candidate,
        "byte mismatch at offset {offset}, requested {len}"
    );
    sa.add(original_ns, &original);
    sb.add(candidate_ns, &candidate);
    Ok(())
}

fn offset(i: usize, size: u64, seed: &mut u64) -> u64 {
    let boundaries = [
        0,
        262_143,
        262_144,
        4_294_967_295,
        4_294_967_296 + 123,
        8_589_934_591,
        8_589_934_592 + 123,
        size.saturating_sub(37),
        size,
        size.saturating_add(1),
    ];
    if i < boundaries.len() {
        let value = boundaries[i];
        if value <= size.saturating_add(1) {
            return value;
        }
    }
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed % size.max(1)
}

fn random(
    original: &Path,
    candidate: &Path,
    size: u64,
    len: usize,
    threads: usize,
    iterations: usize,
) -> Result<Workload> {
    let start = Instant::now();
    let barrier = Arc::new(Barrier::new(threads));
    // Open every handle before spawning so an open failure cannot strand a barrier.
    let handles = (0..threads)
        .map(|_| Ok((File::open(original)?, File::open(candidate)?)))
        .collect::<Result<Vec<_>>>()?;
    let results = std::thread::scope(|scope| -> Result<Vec<(Samples, Samples)>> {
        let jobs: Vec<_> = handles
            .into_iter()
            .enumerate()
            .map(|(thread, (mut a, mut b))| {
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || -> Result<(Samples, Samples)> {
                    let mut sa = Samples::default();
                    let mut sb = Samples::default();
                    let mut seed = 0xcafef00d12345678 ^ thread as u64;
                    barrier.wait();
                    for i in 0..iterations.max(10) {
                        checked_pair(
                            &mut a,
                            &mut b,
                            offset(i, size, &mut seed),
                            len,
                            &mut sa,
                            &mut sb,
                        )?;
                    }
                    Ok((sa, sb))
                })
            })
            .collect();
        jobs.into_iter()
            .map(|job| {
                job.join()
                    .map_err(|_| anyhow::anyhow!("reader thread panicked"))?
            })
            .collect()
    })?;
    let mut sa = Samples::default();
    let mut sb = Samples::default();
    for (a, b) in results {
        sa.nanos.extend(a.nanos);
        sb.nanos.extend(b.nanos);
        sa.bytes += a.bytes;
        sb.bytes += b.bytes;
        // Preserve deterministic per-thread digest ordering without buffering data.
        sa.hash.update(a.hash.finalize().as_bytes());
        sb.hash.update(b.hash.finalize().as_bytes());
    }
    Ok(Workload {
        name: "random".into(),
        read_bytes: len,
        threads,
        original: sa.report(),
        candidate: sb.report(),
        bytes_equal: true,
        wall_seconds: start.elapsed().as_secs_f64(),
    })
}

fn compare(original: &Path, candidate: &Path, args: &Args) -> Result<Comparison> {
    let listing = files(original)?;
    ensure!(
        listing == files(candidate)?,
        "directory enumeration or file sizes differ"
    );
    let mut workloads = Vec::new();
    for name in ["open", "stat"] {
        let start = Instant::now();
        let mut sa = Samples::default();
        let mut sb = Samples::default();
        for path in listing.keys() {
            for _ in 0..args.iterations {
                for (root, sample) in [(original, &mut sa), (candidate, &mut sb)] {
                    let instant = Instant::now();
                    if name == "open" {
                        let file = File::open(root.join(path))?;
                        sample.add(
                            instant.elapsed().as_nanos(),
                            &file.metadata()?.len().to_le_bytes(),
                        );
                    } else {
                        let metadata = fs::metadata(root.join(path))?;
                        sample.add(instant.elapsed().as_nanos(), &metadata.len().to_le_bytes());
                    }
                }
            }
        }
        workloads.push(Workload {
            name: name.into(),
            read_bytes: 0,
            threads: 1,
            original: sa.report(),
            candidate: sb.report(),
            bytes_equal: true,
            wall_seconds: start.elapsed().as_secs_f64(),
        });
    }
    let start = Instant::now();
    let mut sa = Samples::default();
    let mut sb = Samples::default();
    for (path, size) in &listing {
        let limit = if args.sequential_limit == 0 {
            *size
        } else {
            (*size).min(args.sequential_limit)
        };
        let mut a = File::open(original.join(path))?;
        let mut b = File::open(candidate.join(path))?;
        let mut position = 0;
        while position < limit {
            let len = (limit - position).min(1_048_576) as usize;
            checked_pair(&mut a, &mut b, position, len, &mut sa, &mut sb)?;
            position += len as u64;
        }
    }
    workloads.push(Workload {
        name: "sequential".into(),
        read_bytes: 1_048_576,
        threads: 1,
        original: sa.report(),
        candidate: sb.report(),
        bytes_equal: true,
        wall_seconds: start.elapsed().as_secs_f64(),
    });
    let (largest, size) = listing
        .iter()
        .max_by_key(|(_, size)| **size)
        .context("no files")?;
    for len in [4096, 65_536, 1_048_576] {
        for threads in [1, 2, 4, 8, 16] {
            workloads.push(random(
                &original.join(largest),
                &candidate.join(largest),
                *size,
                len,
                threads,
                args.iterations,
            )?);
        }
    }
    if *size > 0 {
        let start = Instant::now();
        let a = File::open(original.join(largest))?;
        let b = File::open(candidate.join(largest))?;
        // These read-only files must stay unchanged for the lifetime of the maps.
        // The generated fixture and mounted-smoke runner enforce this condition.
        let ma = unsafe { MmapOptions::new().map(&a) }?;
        let mb = unsafe { MmapOptions::new().map(&b) }?;
        let mut sa = Samples::default();
        let mut sb = Samples::default();
        let mut seed = 0xdeadbeef87654321;
        for i in 0..args.iterations.max(10) {
            let position = offset(i, *size, &mut seed).min(size.saturating_sub(1));
            let lo = usize::try_from(position).context("mmap needs a 64-bit address space")?;
            let hi = (lo + 4096).min(ma.len());
            let instant = Instant::now();
            let da = ma[lo..hi].to_vec();
            let elapsed = instant.elapsed().as_nanos();
            sa.add(elapsed, &da);
            let instant = Instant::now();
            let db = mb[lo..hi].to_vec();
            let elapsed = instant.elapsed().as_nanos();
            sb.add(elapsed, &db);
            ensure!(da == db, "mmap mismatch at {position}");
        }
        workloads.push(Workload {
            name: "mmap_random_pages".into(),
            read_bytes: 4096,
            threads: 1,
            original: sa.report(),
            candidate: sb.report(),
            bytes_equal: true,
            wall_seconds: start.elapsed().as_secs_f64(),
        });
    }
    Ok(Comparison {
        candidate: candidate.display().to_string(),
        logical_bytes: listing.values().sum(),
        files: listing.len(),
        directory_enumeration_equal: true,
        workloads,
    })
}

fn decode_hex(text: &str) -> Result<Vec<u8>> {
    ensure!(text.len().is_multiple_of(2), "invalid sample hex length");
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let text = std::str::from_utf8(pair)?;
            Ok(u8::from_str_radix(text, 16)?)
        })
        .collect()
}

fn self_test() -> Result<serde_json::Value> {
    let executable = std::env::current_exe()?;
    let root = executable.parent().context("executable has no directory")?;
    // Buffer fixture parsing so serde_json does not turn a normal metadata read
    // into hundreds of thousands of one-byte VFS callbacks. Besides distorting
    // the probe, that pathological access pattern can exhaust the trace
    // summarizer's bounded identity budget on Windows mounts.
    let fixture_file = File::open(root.join("fixture.json"))?;
    let fixture: Fixture =
        serde_json::from_reader(BufReader::with_capacity(64 * 1024, fixture_file))?;
    ensure!(fixture.schema == 1, "unsupported fixture schema");
    let mut digest = blake3::Hasher::new();
    let mut samples = 0;
    let mut bytes = 0;
    for entry in fixture.files {
        let path = Path::new(&entry.path);
        ensure!(
            !path.is_absolute()
                && path
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
            "unsafe fixture path"
        );
        let mut file = File::open(root.join(path))?;
        ensure!(
            file.metadata()?.len() == entry.size,
            "size mismatch for {}",
            entry.path
        );
        let map = if entry.size > 0 {
            // Generated files are immutable while this process runs.
            Some(unsafe { MmapOptions::new().map(&file) }?)
        } else {
            None
        };
        for sample in entry.samples {
            let expected = decode_hex(&sample.data_hex)?;
            let actual = read_at(&mut file, sample.offset, expected.len())?;
            ensure!(
                actual == expected,
                "self-test read mismatch: {} @ {}",
                entry.path,
                sample.offset
            );
            if let Some(map) = &map
                && sample.offset < entry.size
            {
                let lo = usize::try_from(sample.offset)?;
                let hi = lo.checked_add(expected.len()).context("sample overflow")?;
                ensure!(
                    map.get(lo..hi) == Some(expected.as_slice()),
                    "self-test mmap mismatch"
                );
            }
            digest.update(entry.path.as_bytes());
            digest.update(&sample.offset.to_le_bytes());
            digest.update(&actual);
            samples += 1;
            bytes += actual.len();
        }
    }
    Ok(
        serde_json::json!({ "schema": 1, "self_test": "PASS", "executable": executable,
        "asset_root": root, "read_and_mmap_samples": samples, "verified_bytes": bytes,
        "sample_blake3": digest.finalize().to_hex().to_string() }),
    )
}

fn write_result(value: &impl Serialize, output: Option<&Path>) -> Result<()> {
    let json = serde_json::to_string_pretty(value)?;
    if let Some(output) = output {
        if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        fs::write(output, &json)?;
    }
    println!("{json}");
    Ok(())
}

fn resources() -> (Option<f64>, Option<u64>) {
    playsparse_core::process_resources()
}

fn run() -> Result<()> {
    let args = Args::parse();
    ensure!(
        args.iterations > 0 && args.iterations <= 1_000_000,
        "iterations must be 1..=1000000"
    );
    if args.self_test {
        return write_result(&self_test()?, args.output.as_deref());
    }
    let original = args
        .original
        .as_deref()
        .context("ORIGINAL directory required")?;
    let mounted = args
        .mounted
        .as_deref()
        .context("MOUNTED directory required")?;
    let mut comparisons = vec![compare(original, mounted, &args)?];
    if let Some(baseline) = &args.baseline {
        comparisons.push(compare(original, baseline, &args)?);
    }
    let (process_cpu_seconds, peak_rss_bytes) = resources();
    write_result(
        &Report {
            schema: 1,
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            original: original.display().to_string(),
            iterations_per_thread: args.iterations,
            sequential_limit_per_file: args.sequential_limit,
            page_cache_policy: "OS caches are not cleared; sequential and earlier workloads may warm later reads",
            equality: "ORIGINAL == CANDIDATE for every measured byte read",
            comparisons,
            process_cpu_seconds,
            peak_rss_bytes,
        },
        args.output.as_deref(),
    )
}

fn main() {
    if let Err(error) = run() {
        eprintln!("io-probe failed: {error:#}");
        std::process::exit(1);
    }
}
