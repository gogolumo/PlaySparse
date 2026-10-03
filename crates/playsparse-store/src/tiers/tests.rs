use super::*;
use crate::{PackOptions, pack_directory};
use playsparse_core::{Chunker, parse_hash};
use std::{
    collections::BTreeMap,
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex, atomic::AtomicBool},
    thread::{self, JoinHandle},
};

struct Fixture {
    _temp: tempfile::TempDir,
    path: PathBuf,
    source: PathBuf,
    base: PathBuf,
    metadata: PathBuf,
    hash: [u8; 32],
    raw: Vec<u8>,
}

impl Fixture {
    fn new(layout: Layout) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let path = fs::canonicalize(temp.path()).unwrap();
        let source = path.join("source");
        fs::create_dir(&source).unwrap();
        let raw: Vec<_> = (0..4096).map(|index| (index / 16) as u8).collect();
        fs::write(source.join("file"), &raw).unwrap();
        // A second object makes each requested range strictly smaller than its
        // pack and catches accidental whole-pack HTTP downloads.
        fs::write(source.join("other"), vec![b'x'; 4096]).unwrap();
        let base = path.join("base");
        pack_directory(
            &source,
            &base,
            &PackOptions {
                layout,
                chunker: Chunker::Fixed,
                chunk_size: 4096,
                ..Default::default()
            },
        )
        .unwrap();
        let store = Store::open(&base).unwrap();
        let hash = parse_hash(&store.manifest().file("file").unwrap().chunks[0].hash).unwrap();
        let metadata = path.join("metadata-only");
        fs::create_dir_all(metadata.join("index")).unwrap();
        for relative in ["manifest.json", "index/objects.idx", "COMMITTED.json"] {
            fs::copy(base.join(relative), metadata.join(relative)).unwrap();
        }
        Self {
            _temp: temp,
            path,
            source,
            base,
            metadata,
            hash,
            raw,
        }
    }
}

#[derive(Clone, Copy)]
enum ResponseMode {
    Normal,
    NotFound,
    Truncated,
    WrongRange,
    WrongBytes,
    IgnoreRange,
    Redirect,
    DelayHeaders,
    DelayBody,
}

struct Server {
    url: String,
    address: String,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<(String, u64, u64)>>>,
    worker: Option<JoinHandle<()>>,
}

impl Server {
    fn new(root: &Path, mode: ResponseMode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let url = format!("http://{address}");
        let root = root.to_path_buf();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = requests.clone();
        let worker = thread::spawn(move || {
            let mut clients = Vec::new();
            while !stopped.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let root = root.clone();
                        let observed = observed.clone();
                        clients.push(thread::spawn(move || serve(stream, &root, mode, &observed)));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(error) => panic!("test server accept failed: {error}"),
                }
            }
            for client in clients {
                client.join().unwrap();
            }
        });
        Self {
            url,
            address,
            stop,
            requests,
            worker: Some(worker),
        }
    }

    fn config(&self) -> TierConfig {
        TierConfig {
            remote: Some(HttpTier {
                base_url: self.url.clone(),
                timeout_ms: 1000,
                retries: 0,
            }),
            ..Default::default()
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(&self.address);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn serve(
    mut stream: TcpStream,
    root: &Path,
    mode: ResponseMode,
    observed: &Mutex<Vec<(String, u64, u64)>>,
) {
    // macOS accepted sockets inherit O_NONBLOCK from the listener.
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut headers = Vec::new();
    let mut byte = [0];
    while headers.len() < 16 * 1024 && !headers.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => headers.push(byte[0]),
            _ => return,
        }
    }
    let headers = String::from_utf8(headers).unwrap();
    let path = headers
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .trim_start_matches('/');
    let range = headers
        .lines()
        .find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("range").then_some(value.trim())
            })
        })
        .unwrap();
    let (start, end) = range
        .strip_prefix("bytes=")
        .unwrap()
        .split_once('-')
        .unwrap();
    let start: u64 = start.parse().unwrap();
    let end: u64 = end.parse().unwrap();
    observed.lock().unwrap().push((path.into(), start, end));
    if matches!(mode, ResponseMode::DelayHeaders) {
        thread::sleep(Duration::from_millis(250));
    }
    if matches!(mode, ResponseMode::NotFound) {
        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    }
    if matches!(mode, ResponseMode::Redirect) {
        let _ = stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    }
    let all = fs::read(root.join(path)).unwrap();
    let mut body = all[start as usize..=end as usize].to_vec();
    if matches!(mode, ResponseMode::WrongBytes) {
        body[0] ^= 0xff;
    }
    let header_start = if matches!(mode, ResponseMode::WrongRange) {
        start + 1
    } else {
        start
    };
    let status = if matches!(mode, ResponseMode::IgnoreRange) {
        200
    } else {
        206
    };
    let response = format!(
        "HTTP/1.1 {status} Response\r\nContent-Range: bytes {header_start}-{end}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        all.len(),
        body.len()
    );
    if stream.write_all(response.as_bytes()).is_err() {
        return;
    }
    if matches!(mode, ResponseMode::DelayBody) {
        thread::sleep(Duration::from_millis(250));
    }
    if matches!(mode, ResponseMode::Truncated) {
        body.truncate(body.len() / 2);
    }
    let _ = stream.write_all(&body);
}

fn digest_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, path: &Path, output: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, output);
            } else {
                output.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    blake3::hash(&fs::read(path).unwrap()).as_bytes().to_vec(),
                );
            }
        }
    }
    let mut output = BTreeMap::new();
    walk(root, root, &mut output);
    output
}

#[test]
fn primary_and_metadata_only_remote_ranges_preserve_identity_and_bounds() {
    for layout in [Layout::Packs, Layout::Loose] {
        let fixture = Fixture::new(layout);
        let original = digest_tree(&fixture.base);
        let source = digest_tree(&fixture.source);
        assert!(Store::open(&fixture.metadata).is_err());
        let server = Server::new(&fixture.base, ResponseMode::Normal);
        let local = Store::open_with_tiers(&fixture.base, server.config()).unwrap();
        let (raw, tier) = local.read_object_with_source(&fixture.hash).unwrap();
        assert_eq!(raw, fixture.raw);
        assert_eq!(tier, SourceKind::PrimaryLocal);
        assert!(server.requests.lock().unwrap().is_empty());
        let remote = Store::open_with_tiers(&fixture.metadata, server.config()).unwrap();
        let (raw, tier) = remote.read_object_with_source(&fixture.hash).unwrap();
        assert_eq!(raw, fixture.raw);
        assert_eq!(tier, SourceKind::RemoteHttp);
        let record = remote.lookup(&fixture.hash).unwrap();
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].2 - requests[0].1 + 1,
            record.compressed_size as u64
        );
        if layout == Layout::Packs {
            assert!(requests[0].1 >= 8);
            assert!((record.compressed_size as u64) < remote.pack_lengths[&record.pack_id]);
        }
        drop(requests);
        let metrics = remote.tier_metrics();
        assert_eq!(metrics.remote_hits, 1);
        assert_eq!(metrics.remote_requests, 1);
        assert_eq!(metrics.remote_ranges, 1);
        assert_eq!(metrics.remote_bytes, record.compressed_size as u64);
        assert_eq!(metrics.errors, 0);
        assert_eq!(digest_tree(&fixture.base), original);
        assert_eq!(digest_tree(&fixture.source), source);
    }
}

#[test]
fn secondary_own_locator_promotion_and_reopen_without_upstream() {
    let fixture = Fixture::new(Layout::Packs);
    let secondary = fixture.path.join("secondary-loose");
    pack_directory(
        &fixture.source,
        &secondary,
        &PackOptions {
            layout: Layout::Loose,
            chunker: Chunker::Fixed,
            chunk_size: 4096,
            level: 1,
        },
    )
    .unwrap();
    let secondary_digest = digest_tree(&secondary);
    let base_digest = digest_tree(&fixture.base);
    let cache = fixture.path.join("cache");
    let config = TierConfig {
        primary_cache: Some(cache.clone()),
        secondary: Some(secondary.clone()),
        promote: true,
        ..Default::default()
    };
    let store = Store::open_with_tiers(&fixture.metadata, config.clone()).unwrap();
    let (raw, source) = store.read_object_with_source(&fixture.hash).unwrap();
    assert_eq!(raw, fixture.raw);
    assert_eq!(source, SourceKind::SecondaryLocal);
    let metrics = store.tier_metrics();
    assert_eq!(metrics.secondary_hits, 1);
    assert_eq!(metrics.promotions, 1);
    assert_eq!(metrics.promotion_bytes, 4096);
    let (raw, source) = store.read_object_with_source(&fixture.hash).unwrap();
    assert_eq!(raw, fixture.raw);
    assert_eq!(source, SourceKind::PrimaryLocal);
    assert_eq!(store.tier_metrics().primary_cache_hits, 1);
    drop(store);
    let store = Store::open_with_tiers(
        &fixture.metadata,
        TierConfig {
            secondary: None,
            promote: false,
            ..config
        },
    )
    .unwrap();
    assert_eq!(
        store.read_object_with_source(&fixture.hash).unwrap(),
        (fixture.raw.clone(), SourceKind::PrimaryLocal)
    );
    let cache_files: Vec<_> = fs::read_dir(cache).unwrap().collect();
    assert_eq!(cache_files.len(), 1);
    assert_eq!(digest_tree(&fixture.base), base_digest);
    assert_eq!(digest_tree(&secondary), secondary_digest);
}

#[test]
fn remote_promotion_is_persistent_verified_and_offline_readable() {
    let fixture = Fixture::new(Layout::Packs);
    let server = Server::new(&fixture.base, ResponseMode::Normal);
    let cache = fixture.path.join("cache");
    let config = TierConfig {
        primary_cache: Some(cache.clone()),
        promote: true,
        ..server.config()
    };
    let store = Store::open_with_tiers(&fixture.metadata, config.clone()).unwrap();
    assert_eq!(
        store.read_object_with_source(&fixture.hash).unwrap().1,
        SourceKind::RemoteHttp
    );
    assert_eq!(store.tier_metrics().promotions, 1);
    drop(store);
    drop(server);
    let offline = Store::open_with_tiers(&fixture.metadata, config).unwrap();
    assert_eq!(offline.read_object(&fixture.hash).unwrap(), fixture.raw);
    assert_eq!(offline.tier_metrics().remote_requests, 0);
    assert_eq!(offline.tier_metrics().primary_cache_hits, 1);
    let promoted = cache.join(cache_name(&fixture.hash));
    let mut bytes = fs::read(&promoted).unwrap();
    bytes[CACHE_HEADER + 5] ^= 1;
    fs::write(promoted, bytes).unwrap();
    assert!(matches!(
        offline.read_object(&fixture.hash),
        Err(Error::Corrupt(_))
    ));
    assert_eq!(offline.tier_metrics().integrity_errors, 1);
    assert_eq!(offline.tier_metrics().remote_requests, 0);
}

#[test]
fn remote_protocol_and_integrity_failures_never_return_unverified_bytes() {
    for mode in [
        ResponseMode::NotFound,
        ResponseMode::Truncated,
        ResponseMode::WrongRange,
        ResponseMode::WrongBytes,
        ResponseMode::IgnoreRange,
        ResponseMode::Redirect,
    ] {
        let fixture = Fixture::new(Layout::Packs);
        let server = Server::new(&fixture.base, mode);
        let cache = fixture.path.join("cache");
        let store = Store::open_with_tiers(
            &fixture.metadata,
            TierConfig {
                primary_cache: Some(cache.clone()),
                promote: true,
                ..server.config()
            },
        )
        .unwrap();
        assert!(store.read_object(&fixture.hash).is_err());
        assert_eq!(store.tier_metrics().remote_hits, 0);
        assert_eq!(store.tier_metrics().promotions, 0);
        assert_eq!(store.tier_metrics().errors, 1);
        assert_eq!(server.requests.lock().unwrap().len(), 1);
        assert_eq!(fs::read_dir(cache).unwrap().count(), 0);
    }
}

#[test]
fn header_and_body_timeouts_have_bounded_retry_and_metrics() {
    for mode in [ResponseMode::DelayHeaders, ResponseMode::DelayBody] {
        let fixture = Fixture::new(Layout::Packs);
        let server = Server::new(&fixture.base, mode);
        let mut config = server.config();
        config.remote.as_mut().unwrap().timeout_ms = 35;
        config.remote.as_mut().unwrap().retries = 2;
        let store = Store::open_with_tiers(&fixture.metadata, config).unwrap();
        let start = std::time::Instant::now();
        let result = store.read_object(&fixture.hash);
        assert!(
            matches!(result, Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::TimedOut)
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        let metrics = store.tier_metrics();
        assert_eq!(metrics.remote_requests, 3);
        assert_eq!(metrics.timeouts, 3);
        assert_eq!(metrics.remote_hits, 0);
        assert_eq!(metrics.errors, 1);
    }
}

#[test]
fn unreachable_remote_is_sanitized_and_local_fallback_avoids_http() {
    let fixture = Fixture::new(Layout::Packs);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let config = TierConfig {
        remote: Some(HttpTier {
            base_url: url.clone(),
            timeout_ms: 50,
            retries: 2,
        }),
        ..Default::default()
    };
    let store = Store::open_with_tiers(&fixture.metadata, config.clone()).unwrap();
    let error = store.read_object(&fixture.hash).unwrap_err();
    assert!(!error.to_string().contains(&url));
    assert_eq!(store.tier_metrics().remote_requests, 3);
    let store = Store::open_with_tiers(&fixture.base, config.clone()).unwrap();
    assert_eq!(store.read_object(&fixture.hash).unwrap(), fixture.raw);
    assert_eq!(store.tier_metrics().remote_requests, 0);
    let store = Store::open_with_tiers(
        &fixture.metadata,
        TierConfig {
            secondary: Some(fixture.base.clone()),
            ..config
        },
    )
    .unwrap();
    assert_eq!(
        store.read_object_with_source(&fixture.hash).unwrap().1,
        SourceKind::SecondaryLocal
    );
    assert_eq!(store.tier_metrics().remote_requests, 0);
}

#[test]
fn config_versions_fields_bounds_relative_paths_and_credentials_are_checked() {
    let fixture = Fixture::new(Layout::Packs);
    let path = fixture.path.join("tiers.json");
    fs::write(&path, br#"{"version":1,"secondary":"base","primary_cache":"cache","promote":true,"remote":{"base_url":"http://127.0.0.1:9","timeout_ms":100,"retries":2}}"#).unwrap();
    let config = TierConfig::load(&path).unwrap();
    assert_eq!(config.secondary.unwrap(), fixture.base);
    assert_eq!(config.primary_cache.unwrap(), fixture.path.join("cache"));
    for bytes in [
        r#"{"version":2}"#,
        r#"{"version":1,"unknown":true}"#,
        r#"{"version":1,"promote":true}"#,
        r#"{"version":1,"secondary":"../escape"}"#,
        r#"{"version":1,"remote":{"base_url":"ftp://host"}}"#,
        r#"{"version":1,"remote":{"base_url":"http://user:password@host"}}"#,
        r#"{"version":1,"remote":{"base_url":"https://host?token=secret"}}"#,
        r#"{"version":1,"remote":{"base_url":"https://host#secret"}}"#,
        r#"{"version":1,"remote":{"base_url":"http://host","timeout_ms":0}}"#,
        r#"{"version":1,"remote":{"base_url":"http://host","timeout_ms":60001}}"#,
        r#"{"version":1,"remote":{"base_url":"http://host","retries":3}}"#,
        r#"{"version":1,"remote":{"base_url":"http://host","headers":{"Authorization":"secret"}}}"#,
    ] {
        fs::write(&path, bytes).unwrap();
        let error = TierConfig::load(&path).unwrap_err();
        assert!(!error.to_string().contains("password"));
        assert!(!error.to_string().contains("secret"));
    }
}

#[test]
fn local_corruption_is_hard_error_and_missing_secondary_identity_falls_remote() {
    let fixture = Fixture::new(Layout::Loose);
    let server = Server::new(&fixture.base, ResponseMode::Normal);
    let other_source = fixture.path.join("unrelated-source");
    fs::create_dir(&other_source).unwrap();
    fs::write(other_source.join("unrelated"), b"unrelated").unwrap();
    let unrelated = fixture.path.join("unrelated");
    pack_directory(&other_source, &unrelated, &PackOptions::default()).unwrap();
    let store = Store::open_with_tiers(
        &fixture.metadata,
        TierConfig {
            secondary: Some(unrelated),
            ..server.config()
        },
    )
    .unwrap();
    assert_eq!(
        store.read_object_with_source(&fixture.hash).unwrap().1,
        SourceKind::RemoteHttp
    );
    let store = Store::open_with_tiers(&fixture.base, server.config()).unwrap();
    let object = super::super::object_path(&fixture.base, &fixture.hash);
    let mut bytes = fs::read(&object).unwrap();
    bytes[0] ^= 1;
    fs::write(object, bytes).unwrap();
    assert!(matches!(
        store.read_object(&fixture.hash),
        Err(Error::Corrupt(_))
    ));
    assert_eq!(store.tier_metrics().remote_requests, 0);
    assert_eq!(store.tier_metrics().integrity_errors, 1);
}

#[test]
fn metadata_only_open_retains_seal_index_and_available_pack_checks() {
    let fixture = Fixture::new(Layout::Packs);
    let config = TierConfig::default();
    assert!(Store::open_with_tiers(&fixture.metadata, config.clone()).is_ok());
    fs::write(fixture.metadata.join("manifest.json"), b"{}").unwrap();
    assert!(Store::open_with_tiers(&fixture.metadata, config.clone()).is_err());
    fs::copy(
        fixture.base.join("manifest.json"),
        fixture.metadata.join("manifest.json"),
    )
    .unwrap();
    fs::create_dir(fixture.metadata.join("packs")).unwrap();
    fs::write(fixture.metadata.join("packs/pack-0000.psp"), b"BADPACK!").unwrap();
    assert!(Store::open_with_tiers(&fixture.metadata, config).is_err());
}

#[test]
fn concurrent_atomic_promotion_is_single_object_and_never_replaces_peer_data() {
    let fixture = Fixture::new(Layout::Packs);
    let cache = fixture.path.join("cache");
    let config = TierConfig {
        primary_cache: Some(cache.clone()),
        secondary: Some(fixture.base.clone()),
        promote: true,
        ..Default::default()
    };
    let barrier = Arc::new(std::sync::Barrier::new(9));
    let mut workers = Vec::new();
    for _ in 0..8 {
        let store = Store::open_with_tiers(&fixture.metadata, config.clone()).unwrap();
        let hash = fixture.hash;
        let expected = fixture.raw.clone();
        let barrier = barrier.clone();
        workers.push(thread::spawn(move || {
            barrier.wait();
            assert_eq!(store.read_object(&hash).unwrap(), expected);
            store.tier_metrics().promotions
        }));
    }
    barrier.wait();
    let promotions: u64 = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .sum();
    assert_eq!(promotions, 1);
    assert_eq!(fs::read_dir(&cache).unwrap().count(), 1);
    let cache_dir = CacheDirectory::open(&cache, false).unwrap();
    assert_eq!(cache_dir.read(&fixture.hash, 4096).unwrap(), fixture.raw);
    assert!(!cache_dir.promote(&fixture.hash, &fixture.raw).unwrap());
}

#[cfg(unix)]
#[test]
fn promotion_rejects_symlink_escapes_overlap_and_destination_links() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new(Layout::Packs);
    let outside = fixture.path.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("valuable"), b"never modify").unwrap();
    let cache = fixture.path.join("cache");
    symlink(&outside, &cache).unwrap();
    let config = TierConfig {
        primary_cache: Some(cache.clone()),
        promote: true,
        ..Default::default()
    };
    assert!(Store::open_with_tiers(&fixture.base, config.clone()).is_err());
    assert!(
        Store::open_with_tiers(
            &fixture.base,
            TierConfig {
                primary_cache: Some(fixture.base.join("cache")),
                ..config.clone()
            }
        )
        .is_err()
    );
    assert!(!fixture.base.join("cache").exists());
    fs::remove_file(&cache).unwrap();
    fs::create_dir(&cache).unwrap();
    let destination = cache.join(cache_name(&fixture.hash));
    symlink(outside.join("valuable"), destination).unwrap();
    let store = Store::open_with_tiers(
        &fixture.metadata,
        TierConfig {
            secondary: Some(fixture.base.clone()),
            ..config
        },
    )
    .unwrap();
    assert!(store.read_object(&fixture.hash).is_err());
    assert_eq!(fs::read(outside.join("valuable")).unwrap(), b"never modify");
    assert_eq!(store.tier_metrics().promotions, 0);
}

#[cfg(unix)]
#[test]
fn promotion_startup_never_follows_a_concurrently_substituted_ancestor() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new(Layout::Packs);
    let switch = fixture.path.join("switch");
    let held = fixture.path.join("held");
    fs::create_dir(&switch).unwrap();
    fs::create_dir(switch.join("leaf")).unwrap();
    fs::create_dir(fixture.base.join("leaf")).unwrap();
    let before = digest_tree(&fixture.base);
    let config = TierConfig {
        primary_cache: Some(switch.join("leaf")),
        secondary: Some(fixture.base.clone()),
        promote: true,
        ..Default::default()
    };
    // The ordinary path must work before applying concurrent namespace pressure.
    assert_eq!(
        Store::open_with_tiers(&fixture.metadata, config.clone())
            .unwrap()
            .read_object(&fixture.hash)
            .unwrap(),
        fixture.raw
    );
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let target = fixture.base.clone();
    let attacker = thread::spawn(move || {
        while !stopped.load(Ordering::Relaxed) {
            if fs::rename(&switch, &held).is_ok() {
                if symlink(&target, &switch).is_ok() {
                    thread::sleep(Duration::from_micros(25));
                    let _ = fs::remove_file(&switch);
                } else {
                    // A racing create may recreate this scratch directory in
                    // the rename gap. Removing it never follows a final link.
                    let _ = fs::remove_dir_all(&switch);
                }
                let _ = fs::rename(&held, &switch);
                thread::sleep(Duration::from_micros(25));
            }
        }
    });
    for _ in 0..1000 {
        if let Ok(store) = Store::open_with_tiers(&fixture.metadata, config.clone()) {
            // Namespace pressure may legitimately fail an open/promotion.
            let _ = store.read_object(&fixture.hash);
        }
        if fs::read_dir(fixture.base.join("leaf"))
            .unwrap()
            .next()
            .is_some()
        {
            break;
        }
    }
    stop.store(true, Ordering::Relaxed);
    attacker.join().unwrap();
    assert_eq!(digest_tree(&fixture.base), before);
    assert!(
        fs::read_dir(fixture.base.join("leaf"))
            .unwrap()
            .next()
            .is_none()
    );
}
