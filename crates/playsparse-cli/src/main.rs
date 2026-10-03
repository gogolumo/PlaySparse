use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use playsparse_core::{Chunker, Codec, Layout};
use playsparse_range::RangeResolver;
use playsparse_store::{
    PackOptions, Store, directory_allocation, directory_bytes, pack_directory, read_at_exact,
};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Parser)]
#[command(
    name = "playsparse",
    version,
    about = "Immutable compressed CAS with experimental writable overlays"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Clone, Copy, ValueEnum)]
enum LayoutArg {
    Packs,
    Loose,
}
#[derive(Clone, Copy, ValueEnum)]
enum ChunkerArg {
    Cdc,
    Fixed,
}
#[derive(Subcommand)]
enum Command {
    Pack {
        source: PathBuf,
        store: PathBuf,
        #[arg(long, value_enum, default_value = "packs")]
        layout: LayoutArg,
        #[arg(long, value_enum, default_value = "cdc")]
        chunker: ChunkerArg,
        #[arg(long,default_value="256K",value_parser=parse_size)]
        chunk_size: usize,
        #[arg(long, default_value_t = 3)]
        level: i32,
    },
    Verify {
        store: PathBuf,
    },
    Analyze {
        source: PathBuf,
        #[arg(long,default_value="256K",value_parser=parse_size)]
        chunk_size: usize,
    },
    Mount {
        store: PathBuf,
        mountpoint: PathBuf,
        #[arg(long,default_value="256M",value_parser=parse_size)]
        cache: usize,
        #[arg(long)]
        overlay: Option<PathBuf>,
        #[arg(long)]
        trace: Option<PathBuf>,
        #[arg(long)]
        policy: Option<PathBuf>,
        #[arg(long)]
        tiers: Option<PathBuf>,
    },
    /// Persist bounded cache/prefetch suggestions from an access trace.
    Optimize {
        trace: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Compare static and adaptive reads on exactly the same recorded workload.
    Replay {
        store: PathBuf,
        trace: PathBuf,
        #[arg(long)]
        policy: Option<PathBuf>,
        #[arg(long,default_value="256M",value_parser=parse_size)]
        cache: usize,
        #[arg(long, default_value_t = 3)]
        repetitions: usize,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Single replay mode for isolated-process CPU/RSS benchmark runs.
    ReplayOne {
        store: PathBuf,
        trace: PathBuf,
        #[arg(long)]
        policy: Option<PathBuf>,
        #[arg(long,default_value="256M",value_parser=parse_size)]
        cache: usize,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Inspect/reset a persistent overlay or publish its merged tree as a new store.
    Overlay {
        #[command(subcommand)]
        command: OverlayCommand,
    },
    /// Analyze recorded runtime callback traffic.
    Trace {
        #[command(subcommand)]
        command: TraceCommand,
    },
    Unmount {
        mountpoint: PathBuf,
    },
    Benchmark {
        source: PathBuf,
        store: PathBuf,
        #[arg(long, default_value_t = 200)]
        iterations: usize,
        #[arg(long,default_value="256M",value_parser=parse_size)]
        cache: usize,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    Doctor {
        #[arg(long)]
        store: Option<PathBuf>,
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
    /// Diagnostic direct range read (bounded to 16 MiB). Binary bytes on stdout.
    Read {
        store: PathBuf,
        path: String,
        offset: u64,
        length: usize,
        #[arg(long,default_value="64M",value_parser=parse_size)]
        cache: usize,
    },
}
#[derive(Subcommand)]
enum OverlayCommand {
    Status {
        overlay: PathBuf,
    },
    Discard {
        overlay: PathBuf,
    },
    Commit {
        base: PathBuf,
        overlay: PathBuf,
        new_store: PathBuf,
    },
}
#[derive(Subcommand)]
enum TraceCommand {
    Summarize { trace: PathBuf },
}
fn parse_size(s: &str) -> std::result::Result<usize, String> {
    let upper = s.trim().to_ascii_uppercase();
    let unit = upper.trim_end_matches('B').trim_end_matches('I');
    let (number, multiplier) = match unit.as_bytes().last() {
        Some(b'K') => (&unit[..unit.len() - 1], 1024u64),
        Some(b'M') => (&unit[..unit.len() - 1], 1024 * 1024),
        Some(b'G') => (&unit[..unit.len() - 1], 1024 * 1024 * 1024),
        _ => (unit, 1),
    };
    let n = number
        .parse::<u64>()
        .map_err(|_| "invalid integer size")?
        .checked_mul(multiplier)
        .ok_or("size overflow")?;
    usize::try_from(n).map_err(|_| "size too large for platform".into())
}
fn print(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    match Cli::parse().command {
        Command::Pack {
            source,
            store,
            layout,
            chunker,
            chunk_size,
            level,
        } => print(&pack_directory(
            &source,
            &store,
            &PackOptions {
                layout: match layout {
                    LayoutArg::Packs => Layout::Packs,
                    LayoutArg::Loose => Layout::Loose,
                },
                chunker: match chunker {
                    ChunkerArg::Cdc => Chunker::Cdc,
                    ChunkerArg::Fixed => Chunker::Fixed,
                },
                chunk_size: u32::try_from(chunk_size).context("chunk size exceeds u32")?,
                level,
            },
        )?),
        Command::Verify { store } => print(&Store::open(&store)?.verify()?),
        Command::Analyze { source, chunk_size } => {
            print(&analyze(&source, u32::try_from(chunk_size)?)?)
        }
        Command::Mount {
            store,
            mountpoint,
            cache,
            overlay,
            trace,
            policy,
            tiers,
        } => mount(
            &store,
            &mountpoint,
            cache,
            overlay.as_deref(),
            trace.as_deref(),
            policy.as_deref(),
            tiers.as_deref(),
        ),
        Command::Optimize { trace, output } => {
            let policy = playsparse_policy::optimize_trace(&trace)?;
            write_json_exclusive(&output, &serde_json::to_value(&policy)?)?;
            print(&policy)
        }
        Command::Replay {
            store,
            trace,
            policy,
            cache,
            repetitions,
            output,
        } => {
            let policy = policy
                .map(|path| playsparse_policy::Policy::load(&path))
                .transpose()?
                .unwrap_or_default();
            let value =
                playsparse_range::replay_trace(&store, &trace, cache, &policy, repetitions)?;
            if let Some(path) = output {
                write_json_exclusive(&path, &value)?;
            }
            print(&value)
        }
        Command::ReplayOne {
            store,
            trace,
            policy,
            cache,
            output,
        } => {
            let policy = policy
                .map(|path| playsparse_policy::Policy::load(&path))
                .transpose()?;
            let value = playsparse_range::replay_one(&store, &trace, cache, policy)?;
            if let Some(path) = output {
                write_json_exclusive(&path, &value)?;
            }
            print(&value)
        }
        Command::Overlay { command } => match command {
            OverlayCommand::Status { overlay } => {
                print(&playsparse_overlay::Overlay::status(&overlay)?)
            }
            OverlayCommand::Discard { overlay } => {
                playsparse_overlay::Overlay::discard(&overlay)?;
                print(&json!({"overlay_reset":true}))
            }
            OverlayCommand::Commit {
                base,
                overlay,
                new_store,
            } => overlay_commit(&base, &overlay, &new_store),
        },
        Command::Trace {
            command: TraceCommand::Summarize { trace },
        } => print(&playsparse_trace::summarize(&trace)?),
        Command::Unmount { mountpoint } => unmount(&mountpoint),
        Command::Benchmark {
            source,
            store,
            iterations,
            cache,
            output,
        } => {
            if !(1..=1_000_000).contains(&iterations) {
                bail!("iterations must be 1..1000000");
            }
            let value = benchmark(&source, &store, iterations, cache)?;
            if let Some(path) = output {
                if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    fs::create_dir_all(parent)?;
                }
                fs::write(path, serde_json::to_vec_pretty(&value)?)?;
            }
            print(&value)
        }
        Command::Doctor { store, path } => print(&doctor(store.as_deref(), &path)),
        Command::Read {
            store,
            path,
            offset,
            length,
            cache,
        } => {
            use std::io::Write;
            let bytes = RangeResolver::open(&store, cache)?.read_range(&path, offset, length)?;
            std::io::stdout().write_all(&bytes)?;
            Ok(())
        }
    }
}
fn write_json_exclusive(path: &Path, value: &Value) -> Result<()> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
fn resolved_location(path: &Path) -> Result<PathBuf> {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if text.len() == 2 && text.as_bytes()[0].is_ascii_alphabetic() && text.ends_with(':') {
            // An unused WinFsp drive has no parent to canonicalize. Compare its
            // future absolute root with the other resolved storage locations.
            return Ok(PathBuf::from(format!(
                r"\\?\{}\",
                text.to_ascii_uppercase()
            )));
        }
    }
    if path.exists() {
        return Ok(path.canonicalize()?);
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        bail!("new storage paths must not contain parent traversal");
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut ancestor = absolute.as_path();
    let mut missing = Vec::new();
    while !ancestor.exists() {
        missing.push(
            ancestor
                .file_name()
                .context("path needs an existing ancestor")?
                .to_os_string(),
        );
        ancestor = ancestor
            .parent()
            .context("path needs an existing ancestor")?;
    }
    let mut resolved = ancestor.canonicalize()?;
    for component in missing.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn mount(
    store: &Path,
    mountpoint: &Path,
    cache: usize,
    overlay: Option<&Path>,
    trace_path: Option<&Path>,
    policy_path: Option<&Path>,
    tiers_path: Option<&Path>,
) -> Result<()> {
    let base = store.canonicalize()?;
    let mount = resolved_location(mountpoint)?;
    let writable = overlay.map(resolved_location).transpose()?;
    if let Some(root) = &writable
        && (root.starts_with(&mount)
            || mount.starts_with(root)
            || root.starts_with(&base)
            || base.starts_with(root))
    {
        bail!("overlay, store and mountpoint must be separate directory trees");
    }
    let policy = policy_path
        .map(playsparse_policy::Policy::load)
        .transpose()?;
    let tiers = tiers_path
        .map(playsparse_store::TierConfig::load)
        .transpose()?;
    if let Some(config) = &tiers {
        if let Some(path) = &config.primary_cache {
            let resolved = resolved_location(path)?;
            if resolved.starts_with(&base)
                || base.starts_with(&resolved)
                || resolved.starts_with(&mount)
                || mount.starts_with(&resolved)
                || writable
                    .as_ref()
                    .is_some_and(|root| resolved.starts_with(root) || root.starts_with(&resolved))
            {
                bail!("primary tier cache must be outside store, overlay and mountpoint");
            }
        }
        if let Some(path) = &config.secondary {
            let resolved = path.canonicalize()?;
            if resolved.starts_with(&mount)
                || mount.starts_with(&resolved)
                || writable
                    .as_ref()
                    .is_some_and(|root| resolved.starts_with(root) || root.starts_with(&resolved))
            {
                bail!("secondary store must be outside overlay and mountpoint");
            }
        }
    }
    let trace = if let Some(path) = trace_path {
        let resolved = resolved_location(path)?;
        if resolved.starts_with(&base)
            || resolved.starts_with(&mount)
            || writable
                .as_ref()
                .is_some_and(|root| resolved.starts_with(root))
            || tiers
                .as_ref()
                .and_then(|config| config.secondary.as_ref())
                .map(|path| path.canonicalize())
                .transpose()?
                .is_some_and(|root| resolved.starts_with(root))
        {
            bail!(
                "trace destination must be outside base/secondary stores, mountpoint and overlay"
            );
        }
        Some(std::sync::Arc::new(playsparse_trace::TraceWriter::open(
            &resolved,
        )?))
    } else {
        None
    };
    #[cfg(windows)]
    let result = playsparse_vfs_win::mount_configured(
        store,
        mountpoint,
        overlay,
        playsparse_range::RuntimeOptions {
            cache_bytes: cache,
            trace: trace.clone(),
            policy,
            tiers,
        },
    );
    #[cfg(unix)]
    let result = playsparse_vfs_fuse::mount_configured(
        store,
        mountpoint,
        overlay,
        playsparse_range::RuntimeOptions {
            cache_bytes: cache,
            trace: trace.clone(),
            policy,
            tiers,
        },
    );
    #[cfg(not(any(windows, unix)))]
    let result: Result<()> = Err(anyhow::anyhow!("unsupported OS"));
    if let Some(trace) = trace {
        trace.shutdown();
        eprintln!(
            "{}",
            json!({"event":"trace_closed","trace":trace.metrics()})
        );
    }
    result
}
fn overlay_commit(base: &Path, overlay_path: &Path, new_store: &Path) -> Result<()> {
    use std::io::Write;
    let new_location = resolved_location(new_store)?;
    let base_location = base.canonicalize()?;
    let overlay_location = overlay_path.canonicalize()?;
    if new_location.starts_with(&base_location)
        || new_location.starts_with(&overlay_location)
        || base_location.starts_with(&new_location)
        || overlay_location.starts_with(&new_location)
    {
        bail!("new store must be outside immutable base and overlay");
    }
    if new_store.exists() {
        bail!("new store already exists");
    }
    let resolver = std::sync::Arc::new(RangeResolver::open(base, 64 << 20)?);
    let overlay = playsparse_overlay::Overlay::open(resolver, overlay_path)?;
    let stage = tempfile::tempdir()?;
    let entries = overlay.entries()?;
    for entry in entries.iter().filter(|e| e.is_dir && !e.path.is_empty()) {
        fs::create_dir_all(stage.path().join(&entry.path))?;
    }
    for entry in entries.iter().filter(|e| !e.is_dir) {
        let handle = overlay.open_file(&entry.path, false, false)?;
        let target = stage.path().join(&entry.path);
        let mut file = File::create(&target)?;
        let mut offset = 0;
        while offset < entry.size {
            let bytes = overlay.read(
                &handle,
                offset,
                ((entry.size - offset).min(playsparse_core::MAX_READ_BYTES as u64)) as usize,
            )?;
            if bytes.is_empty() {
                bail!("unexpected EOF committing overlay");
            }
            file.write_all(&bytes)?;
            offset += bytes.len() as u64;
        }
        file.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&target, fs::Permissions::from_mode(entry.mode))?;
        }
        #[cfg(windows)]
        {
            let mut permissions = fs::metadata(&target)?.permissions();
            permissions.set_readonly(entry.mode & 0o222 == 0);
            fs::set_permissions(&target, permissions)?;
        }
    }
    let packed = pack_directory(stage.path(), new_store, &PackOptions::default())?;
    let verified = Store::open(new_store)?.verify()?;
    print(
        &json!({"pack":packed,"verify":verified,"base_opened_read_only":true,"method":"bounded streaming merged tree into disposable staging directory; full temporary logical space required"}),
    )
}
fn unmount(mountpoint: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        playsparse_vfs_win::unmount(mountpoint)
    }
    #[cfg(unix)]
    {
        playsparse_vfs_fuse::unmount(mountpoint)
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = mountpoint;
        bail!("unsupported OS")
    }
}

fn analyze(source: &Path, chunk_size: u32) -> Result<Value> {
    let work = tempfile::tempdir()?;
    let fixed = pack_directory(
        source,
        &work.path().join("fixed"),
        &PackOptions {
            chunker: Chunker::Fixed,
            chunk_size,
            ..Default::default()
        },
    )?;
    let cdc = pack_directory(
        source,
        &work.path().join("cdc"),
        &PackOptions {
            chunk_size,
            ..Default::default()
        },
    )?;
    let store = Store::open(&work.path().join("cdc"))?;
    let mut hashes = std::collections::BTreeSet::new();
    let mut duplicates = 0u64;
    let mut cdc_reuse = 0u64;
    let mut refs = std::collections::BTreeSet::new();
    for file in &store.manifest().files {
        if !hashes.insert(&file.hash) {
            duplicates += file.size;
        }
        for chunk in &file.chunks {
            if !refs.insert(&chunk.hash) {
                cdc_reuse += chunk.raw_size as u64;
            }
        }
    }
    let compressible: u64 = store
        .index()
        .values()
        .filter(|r| r.codec == Codec::Zstd)
        .map(|r| r.raw_size as u64)
        .sum();
    let incompressible: u64 = store
        .index()
        .values()
        .filter(|r| r.codec == Codec::Raw)
        .map(|r| r.raw_size as u64)
        .sum();
    Ok(
        json!({"title":"PlaySparse Analysis","measurement":"measured full scan, two temporary verified stores; no extrapolation","source_modified":false,"files":cdc.files,"logical_bytes":cdc.logical_bytes,"exact_duplicate_file_bytes":duplicates,"cdc_duplicate_reuse_bytes":cdc_reuse,"unique_compressible_raw_bytes":compressible,"unique_incompressible_raw_bytes":incompressible,"already_compressed_bytes":null,"high_entropy_bytes":null,"classification_note":"Codec choice is measured. Already-compressed and high-entropy attribution is not inferred from filename or compression ratio.","fixed_chunks":fixed,"cdc_chunks":cdc,"projected_safe_mode":{"measurement":"measured on this input","physical_bytes":cdc.physical_bytes,"metadata_bytes":cdc.metadata_bytes},"temporary_stores_removed_on_exit":true}),
    )
}
fn percentiles(mut values: Vec<f64>) -> Value {
    values.sort_by(f64::total_cmp);
    if values.is_empty() {
        return Value::Null;
    }
    let at = |p: f64| {
        values[((values.len() as f64 * p).ceil() as usize)
            .saturating_sub(1)
            .min(values.len() - 1)]
    };
    json!({"samples":values.len(),"p50_ms":at(0.50),"p95_ms":at(0.95),"p99_ms":at(0.99)})
}
fn benchmark(source: &Path, root: &Path, iterations: usize, cache_bytes: usize) -> Result<Value> {
    let total_start = Instant::now();
    let cpu_start = cpu_seconds();
    let open_start = Instant::now();
    let cold = RangeResolver::open(root, 0)?;
    let cold_open_ms = open_start.elapsed().as_secs_f64() * 1000.0;
    let open_start = Instant::now();
    let warm = RangeResolver::open(root, cache_bytes)?;
    let warm_open_ms = open_start.elapsed().as_secs_f64() * 1000.0;
    let files: Vec<_> = cold
        .manifest()
        .files
        .iter()
        .filter(|f| f.size > 0)
        .collect();
    if files.is_empty() {
        bail!("benchmark requires nonempty files");
    }
    let mut workloads = Vec::new();
    let mut seed = 0x504c415953504152u64;
    let mut requests = 0u64;
    for length in [4096usize, 65536, 1048576] {
        let raw_before = cold.metrics().raw_bytes_loaded;
        let mut reads = Vec::new();
        for i in 0..iterations {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let file = files[i % files.len()];
            let offset = seed % file.size;
            let len = (file.size - offset).min(length as u64) as usize;
            reads.push((file.path.as_str(), offset, len));
        }
        let mut native_times = Vec::new();
        let mut cold_times = Vec::new();
        let mut warm_times = Vec::new();
        let mut hash = blake3::Hasher::new();
        for (path, offset, len) in &reads {
            let input = File::open(source.join(path))?;
            let mut expected = vec![0; *len];
            let start = Instant::now();
            read_at_exact(&input, *offset, &mut expected)?;
            native_times.push(start.elapsed().as_secs_f64() * 1000.0);
            let start = Instant::now();
            let actual = cold.read_range(path, *offset, *len)?;
            cold_times.push(start.elapsed().as_secs_f64() * 1000.0);
            if actual != expected {
                bail!("cold bytes differ at {path}:{offset}");
            }
            hash.update(&actual);
            warm.read_range(path, *offset, *len)?;
            requests += *len as u64;
        }
        let requested: u64 = reads.iter().map(|r| r.2 as u64).sum();
        for (path, offset, len) in reads {
            let start = Instant::now();
            let actual = warm.read_range(path, offset, len)?;
            warm_times.push(start.elapsed().as_secs_f64() * 1000.0);
            let mut expected = vec![0; len];
            read_at_exact(&File::open(source.join(path))?, offset, &mut expected)?;
            if actual != expected {
                bail!("warm bytes differ at {path}:{offset}");
            }
        }
        let loaded = cold.metrics().raw_bytes_loaded - raw_before;
        workloads.push(json!({"read_bytes":length,"original":percentiles(native_times),"playsparse_chunk_cache_cold":percentiles(cold_times),"playsparse_warm_replay":percentiles(warm_times),"verified_cold_reads":iterations,"verified_warm_reads":iterations,"requested_bytes":requested,"raw_bytes_loaded":loaded,"read_amplification":loaded as f64/requested.max(1) as f64,"aggregate_read_blake3":hash.finalize().to_string()}));
    }
    let mut original_hasher = blake3::Hasher::new();
    let mut cas_hasher = blake3::Hasher::new();
    let mut total = 0u64;
    let start = Instant::now();
    for file in &cold.manifest().files {
        let mut input = File::open(source.join(&file.path))?;
        let mut buf = vec![0; 1024 * 1024];
        loop {
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            original_hasher.update(&buf[..n]);
            total += n as u64;
        }
    }
    let native_seconds = start.elapsed().as_secs_f64();
    let start = Instant::now();
    for file in &cold.manifest().files {
        let mut offset = 0u64;
        while offset < file.size {
            let bytes = cold.read_range(&file.path, offset, 1024 * 1024)?;
            if bytes.is_empty() {
                bail!("sequential short read");
            }
            offset += bytes.len() as u64;
            cas_hasher.update(&bytes);
        }
    }
    let cas_seconds = start.elapsed().as_secs_f64();
    let native_hash = original_hasher.finalize();
    let cas_hash = cas_hasher.finalize();
    if native_hash != cas_hash {
        bail!("sequential digest mismatch");
    }
    let cold_metrics = cold.metrics();
    let keys: Vec<_> = cold.store().index().keys().collect();
    let mut lookups = Vec::new();
    let mut chunk_reads = Vec::new();
    for i in 0..iterations {
        let key = keys[i % keys.len()];
        let start = Instant::now();
        std::hint::black_box(cold.store().lookup(key)?);
        lookups.push(start.elapsed().as_secs_f64() * 1000.0);
        let start = Instant::now();
        std::hint::black_box(cold.store().read_object(key)?);
        chunk_reads.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let allocation = directory_allocation(root)?;
    let elapsed = total_start.elapsed().as_secs_f64();
    Ok(
        json!({"kind":"direct range resolver benchmark (not mounted I/O)","os":std::env::consts::OS,"arch":std::env::consts::ARCH,"cold_definition":"decompressed chunk cache disabled; OS page cache is uncontrolled, not disk-cold","store_open_first_ms":cold_open_ms,"store_open_repeat_ms":warm_open_ms,"logical_bytes":total,"physical_bytes":directory_bytes(root)?,"physical_bytes_definition":"sum of encoded store file lengths; allocated_bytes measures POSIX blocks separately","allocated_bytes":allocation.0,"filesystem_entries":allocation.1,"object_lookup":percentiles(lookups),"random_chunk_read":percentiles(chunk_reads),"iterations":iterations,"workloads":workloads,"sequential":{"original_mib_s":total as f64/1048576.0/native_seconds,"playsparse_mib_s":total as f64/1048576.0/cas_seconds,"blake3":native_hash.to_string(),"bytes_equal":true},"cold_cache":cold_metrics,"warm_cache":warm.metrics(),"requested_random_bytes":requests,"process_cpu_seconds":cpu_seconds().zip(cpu_start).map(|(end,start)|end-start),"wall_seconds":elapsed,"peak_rss_bytes":peak_rss(),"native_filesystem_compression_baseline":"Windows WOF measurement required; not available on this OS"}),
    )
}
#[cfg(unix)]
fn usage() -> Option<libc::rusage> {
    let mut r = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: rusage points to valid initialized writable storage; libc fills it.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, r.as_mut_ptr()) } == 0 {
        Some(unsafe { r.assume_init() })
    } else {
        None
    }
}
fn cpu_seconds() -> Option<f64> {
    #[cfg(unix)]
    {
        usage().map(|r| {
            r.ru_utime.tv_sec as f64
                + r.ru_utime.tv_usec as f64 / 1e6
                + r.ru_stime.tv_sec as f64
                + r.ru_stime.tv_usec as f64 / 1e6
        })
    }
    #[cfg(not(unix))]
    {
        None
    }
}
fn peak_rss() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        usage().map(|r| r.ru_maxrss as u64)
    }
    #[cfg(target_os = "linux")]
    {
        usage().map(|r| r.ru_maxrss as u64 * 1024)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}
// libc statvfs field widths differ between Unix targets.
#[allow(clippy::unnecessary_cast)]
fn doctor(store: Option<&Path>, path: &Path) -> Value {
    let health = store.map(|s| match Store::open(s).and_then(|s| s.verify()) {
        Ok(v) => json!(v),
        Err(e) => json!({"ok":false,"error":e.to_string()}),
    });
    #[cfg(unix)]
    let mount = playsparse_vfs_fuse::availability();
    #[cfg(windows)]
    let mount = playsparse_vfs_win::availability();
    #[cfg(not(any(unix, windows)))]
    let mount = "unsupported OS";
    let permissions=fs::metadata(path).map(|m|json!({"path":path,"exists":true,"readonly_flag":m.permissions().readonly(),"note":"actual mount privilege is checked by backend at mount time"})).unwrap_or_else(|e|json!({"path":path,"exists":false,"error":e.to_string()}));
    #[cfg(unix)]
    let disk = {
        use std::os::unix::ffi::OsStrExt;
        let bytes = std::ffi::CString::new(path.as_os_str().as_bytes());
        bytes.ok().and_then(|p| {
            let mut s = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
            // SAFETY: nul-terminated path and writable statvfs buffer remain alive.
            if unsafe { libc::statvfs(p.as_ptr(), s.as_mut_ptr()) } == 0 {
                let s = unsafe { s.assume_init() };
                Some((s.f_bavail as u64).saturating_mul(s.f_frsize as u64))
            } else {
                None
            }
        })
    };
    #[cfg(not(any(unix, windows)))]
    let disk: Option<u64> = None;
    #[cfg(windows)]
    let windows_resources = playsparse_vfs_win::system_resources(path);
    #[cfg(windows)]
    let disk = windows_resources
        .get("available_disk_bytes")
        .and_then(Value::as_u64);
    #[cfg(target_os = "linux")]
    let memory = fs::read_to_string("/proc/meminfo").ok().and_then(|s| {
        s.lines()
            .find(|l| l.starts_with("MemAvailable:"))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
            .map(|v| v * 1024)
    });
    #[cfg(target_os = "macos")]
    let memory = macos_available_memory();
    #[cfg(windows)]
    let memory = windows_resources
        .get("available_ram_bytes")
        .and_then(Value::as_u64);
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    let memory: Option<u64> = None;
    let mut report = json!({"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"mount_backend":mount,"permissions":permissions,"available_disk_bytes":disk,"available_ram_bytes":memory,"memory_note":"Linux MemAvailable; Windows ullAvailPhys; macOS free+speculative pages (excludes reclaimable inactive/compressed pages); null on measurement failure","codecs":["raw","zstd (in-process)"],"hash":"BLAKE3 raw bytes","store_health":health,"windows_physical_test":"REQUIRED"});
    #[cfg(windows)]
    {
        report["windows_resources"] = windows_resources;
    }
    // Keep the variable mutable on all platforms without cfg-dependent warnings.
    report["filesystem_support"] =
        json!("read-only native callbacks; actual driver mount test required on each platform");
    report
}

#[cfg(target_os = "macos")]
fn macos_available_memory() -> Option<u64> {
    let result = std::process::Command::new("/usr/bin/vm_stat")
        .output()
        .ok()?;
    if !result.status.success() {
        return None;
    }
    let text = String::from_utf8(result.stdout).ok()?;
    let header = text.lines().next()?;
    let page_size = header
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    let mut pages = 0u64;
    for label in ["Pages free:", "Pages speculative:"] {
        let line = text.lines().find(|line| line.starts_with(label))?;
        pages = pages.checked_add(
            line.split(':')
                .nth(1)?
                .trim()
                .trim_end_matches('.')
                .parse::<u64>()
                .ok()?,
        )?;
    }
    pages.checked_mul(page_size)
}

#[cfg(windows)]
#[cfg(test)]
mod mount_path_tests {
    use super::*;
    #[test]
    fn unused_drive_mount_root_does_not_require_a_parent() {
        assert_eq!(
            resolved_location(Path::new("Q:")).unwrap(),
            PathBuf::from(r"\\?\Q:\")
        );
    }
}
