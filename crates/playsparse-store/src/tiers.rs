//! Optional verified local/HTTP object sources. Logical BLAKE3 identity is
//! independent of each source's codec, pack offset, and physical location.
use super::{Store, decode_object, read_at_exact};
use playsparse_core::{Error, Layout, MAX_CHUNK_BYTES, Result, directory::DirectoryAnchor};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(not(unix))]
use std::fs::OpenOptions;

const CONFIG_LIMIT: u64 = 1024 * 1024;
const CACHE_MAGIC: &[u8; 8] = b"PSPTIER1";
const CACHE_HEADER: usize = 44;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    PrimaryLocal,
    SecondaryLocal,
    RemoteHttp,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PrimaryLocal => "primary-local",
            Self::SecondaryLocal => "secondary-local",
            Self::RemoteHttp => "remote-http",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TierConfig {
    pub version: u32,
    #[serde(default)]
    pub primary_cache: Option<PathBuf>,
    #[serde(default)]
    pub secondary: Option<PathBuf>,
    #[serde(default)]
    pub remote: Option<HttpTier>,
    #[serde(default)]
    pub promote: bool,
}

impl Default for TierConfig {
    fn default() -> Self {
        Self {
            version: 1,
            primary_cache: None,
            secondary: None,
            remote: None,
            promote: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpTier {
    pub base_url: String,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    /// Additional attempts after a transport timeout/connection failure, 0..2.
    #[serde(default)]
    pub retries: u32,
}

fn default_timeout() -> u64 {
    3000
}

impl TierConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let mut file = File::open(path)?;
        if file.metadata()?.len() > CONFIG_LIMIT {
            return Err(Error::Invalid("tier config exceeds 1 MiB limit".into()));
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(CONFIG_LIMIT + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > CONFIG_LIMIT {
            return Err(Error::Invalid("tier config grew beyond 1 MiB limit".into()));
        }
        let mut config: Self = serde_json::from_slice(&bytes)
            .map_err(|_| Error::Invalid("invalid tier config JSON/schema".into()))?;
        let parent = fs::canonicalize(
            path.parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?;
        for location in [&mut config.primary_cache, &mut config.secondary]
            .into_iter()
            .flatten()
        {
            if location.is_relative() {
                *location = parent.join(&*location);
            }
        }
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err(Error::Invalid("unsupported tier config version".into()));
        }
        if self.promote && self.primary_cache.is_none() {
            return Err(Error::Invalid(
                "promotion requires a separate primary_cache directory".into(),
            ));
        }
        for path in [&self.primary_cache, &self.secondary].into_iter().flatten() {
            if path.as_os_str().is_empty()
                || path
                    .components()
                    .any(|component| matches!(component, Component::ParentDir))
            {
                return Err(Error::Invalid(
                    "tier paths must be nonempty and contain no parent traversal".into(),
                ));
            }
        }
        if let Some(remote) = &self.remote {
            if !(10..=60_000).contains(&remote.timeout_ms) || remote.retries > 2 {
                return Err(Error::Invalid(
                    "HTTP timeout must be 10..60000 ms; retries must be 0..2".into(),
                ));
            }
            let uri: ureq::http::Uri = remote
                .base_url
                .parse()
                .map_err(|_| Error::Invalid("invalid HTTP tier URL".into()))?;
            if !matches!(uri.scheme_str(), Some("http" | "https"))
                || uri.host().is_none()
                || uri
                    .authority()
                    .is_none_or(|authority| authority.as_str().contains('@'))
                || uri.query().is_some()
                || remote.base_url.contains(['#', '\\'])
            {
                return Err(Error::Invalid(
                    "HTTP tier URL must be http(s), without userinfo, query, or fragment".into(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct TierMetrics {
    pub primary_hits: u64,
    pub primary_bytes: u64,
    pub primary_cache_hits: u64,
    pub secondary_hits: u64,
    pub secondary_bytes: u64,
    pub remote_hits: u64,
    pub remote_bytes: u64,
    pub remote_requests: u64,
    pub remote_ranges: u64,
    pub promotions: u64,
    pub promotion_bytes: u64,
    pub errors: u64,
    pub remote_errors: u64,
    pub timeouts: u64,
    pub integrity_errors: u64,
}

#[derive(Default)]
pub(crate) struct Counters {
    primary_hits: AtomicU64,
    primary_bytes: AtomicU64,
    primary_cache_hits: AtomicU64,
    secondary_hits: AtomicU64,
    secondary_bytes: AtomicU64,
    remote_hits: AtomicU64,
    remote_bytes: AtomicU64,
    remote_requests: AtomicU64,
    remote_ranges: AtomicU64,
    promotions: AtomicU64,
    promotion_bytes: AtomicU64,
    errors: AtomicU64,
    remote_errors: AtomicU64,
    timeouts: AtomicU64,
    integrity_errors: AtomicU64,
}

impl Counters {
    pub(crate) fn snapshot(&self) -> TierMetrics {
        macro_rules! get {
            ($field:ident) => {
                self.$field.load(Ordering::Relaxed)
            };
        }
        TierMetrics {
            primary_hits: get!(primary_hits),
            primary_bytes: get!(primary_bytes),
            primary_cache_hits: get!(primary_cache_hits),
            secondary_hits: get!(secondary_hits),
            secondary_bytes: get!(secondary_bytes),
            remote_hits: get!(remote_hits),
            remote_bytes: get!(remote_bytes),
            remote_requests: get!(remote_requests),
            remote_ranges: get!(remote_ranges),
            promotions: get!(promotions),
            promotion_bytes: get!(promotion_bytes),
            errors: get!(errors),
            remote_errors: get!(remote_errors),
            timeouts: get!(timeouts),
            integrity_errors: get!(integrity_errors),
        }
    }
    pub(crate) fn hit(&self, source: SourceKind, bytes: u64) {
        match source {
            SourceKind::PrimaryLocal => {
                self.primary_hits.fetch_add(1, Ordering::Relaxed);
                self.primary_bytes.fetch_add(bytes, Ordering::Relaxed);
            }
            SourceKind::SecondaryLocal => {
                self.secondary_hits.fetch_add(1, Ordering::Relaxed);
                self.secondary_bytes.fetch_add(bytes, Ordering::Relaxed);
            }
            SourceKind::RemoteHttp => {
                self.remote_hits.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    pub(crate) fn error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }
    pub(crate) fn integrity(&self) {
        self.integrity_errors.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) struct Tiered {
    cache: Option<CacheDirectory>,
    secondary: Option<Box<Store>>,
    remote: Option<(HttpTier, ureq::Agent)>,
    promote: bool,
}

impl Tiered {
    pub(crate) fn open(base: &Store, config: TierConfig) -> Result<Self> {
        config.validate()?;
        let base_root = fs::canonicalize(base.root())?;
        let secondary = config
            .secondary
            .as_ref()
            .map(|path| Store::open(path).map(Box::new))
            .transpose()?;
        let cache = if let Some(path) = &config.primary_cache {
            reject_links(path)?;
            let projected = projected_path(path)?;
            if projected.starts_with(&base_root) || base_root.starts_with(&projected) {
                return Err(Error::Invalid(
                    "primary cache must be separate from the immutable base".into(),
                ));
            }
            if let Some(secondary) = &secondary {
                let secondary_root = fs::canonicalize(secondary.root())?;
                if projected.starts_with(&secondary_root) || secondary_root.starts_with(&projected)
                {
                    return Err(Error::Invalid(
                        "primary cache must be separate from the secondary store".into(),
                    ));
                }
            }
            Some(CacheDirectory::open(path, config.promote)?)
        } else {
            None
        };
        let remote = config.remote.map(|remote| {
            let agent = ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_millis(remote.timeout_ms)))
                .http_status_as_error(false)
                .max_redirects(0)
                .proxy(None)
                .build()
                .into();
            (remote, agent)
        });
        Ok(Self {
            cache,
            secondary,
            remote,
            promote: config.promote,
        })
    }

    pub(crate) fn read(&self, base: &Store, hash: &[u8; 32]) -> Result<(Vec<u8>, SourceKind)> {
        let record = base.lookup(hash)?;
        if let Some(cache) = &self.cache {
            match cache.read(hash, record.raw_size) {
                Ok(raw) => {
                    base.tier_counters
                        .primary_cache_hits
                        .fetch_add(1, Ordering::Relaxed);
                    base.tier_counters
                        .hit(SourceKind::PrimaryLocal, raw.len() as u64);
                    return Ok((raw, SourceKind::PrimaryLocal));
                }
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        match base.read_primary(hash) {
            Ok((raw, encoded)) => {
                base.tier_counters.hit(SourceKind::PrimaryLocal, encoded);
                return Ok((raw, SourceKind::PrimaryLocal));
            }
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if let Some(secondary) = &self.secondary
            && let Some(secondary_record) = secondary.index().get(hash)
        {
            if secondary_record.raw_size != record.raw_size {
                return Err(Error::Corrupt(
                    "secondary object raw-size identity mismatch".into(),
                ));
            }
            let (raw, encoded) = secondary.read_primary(hash)?;
            self.promote(base, hash, &raw)?;
            base.tier_counters.hit(SourceKind::SecondaryLocal, encoded);
            return Ok((raw, SourceKind::SecondaryLocal));
        }
        if self.remote.is_some() {
            let encoded = self.remote_read(base, record)?;
            let raw = decode_object(record, encoded, hash).inspect_err(|_| {
                base.tier_counters
                    .remote_errors
                    .fetch_add(1, Ordering::Relaxed);
            })?;
            self.promote(base, hash, &raw)?;
            base.tier_counters.hit(SourceKind::RemoteHttp, 0);
            return Ok((raw, SourceKind::RemoteHttp));
        }
        Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "object absent from configured local tiers",
        )))
    }

    fn promote(&self, base: &Store, hash: &[u8; 32], raw: &[u8]) -> Result<()> {
        if self.promote {
            let cache = self
                .cache
                .as_ref()
                .ok_or_else(|| Error::Invalid("missing primary cache".into()))?;
            if cache.promote(hash, raw)? {
                base.tier_counters
                    .promotions
                    .fetch_add(1, Ordering::Relaxed);
                base.tier_counters
                    .promotion_bytes
                    .fetch_add(raw.len() as u64, Ordering::Relaxed);
            }
        }
        Ok(())
    }

    fn remote_read(&self, base: &Store, record: &super::ObjectRecord) -> Result<Vec<u8>> {
        let (remote, agent) = self
            .remote
            .as_ref()
            .ok_or_else(|| Error::Invalid("remote tier unavailable".into()))?;
        let (location, start, total) = match base.manifest().layout {
            Layout::Packs => (
                format!("packs/pack-{:04}.psp", record.pack_id),
                record.offset,
                *base
                    .pack_lengths
                    .get(&record.pack_id)
                    .ok_or_else(|| Error::Corrupt("missing logical pack length".into()))?,
            ),
            Layout::Loose => {
                let hex = blake3::Hash::from_bytes(record.hash).to_string();
                (
                    format!("objects/{}/{}.pso", &hex[..2], &hex[2..]),
                    0,
                    record.compressed_size as u64,
                )
            }
        };
        let end = start
            .checked_add(record.compressed_size as u64)
            .and_then(|end| end.checked_sub(1))
            .ok_or_else(|| Error::Corrupt("remote range overflow".into()))?;
        let url = format!("{}/{location}", remote.base_url.trim_end_matches('/'));
        let range = format!("bytes={start}-{end}");
        let expected_range = format!("bytes {start}-{end}/{total}");
        for attempt in 0..=remote.retries {
            base.tier_counters
                .remote_requests
                .fetch_add(1, Ordering::Relaxed);
            base.tier_counters
                .remote_ranges
                .fetch_add(1, Ordering::Relaxed);
            let response = agent
                .get(&url)
                .header("Range", &range)
                .header("Accept-Encoding", "identity")
                .call();
            let mut response = match response {
                Ok(response) => response,
                Err(error) => {
                    base.tier_counters
                        .remote_errors
                        .fetch_add(1, Ordering::Relaxed);
                    let timeout = matches!(error, ureq::Error::Timeout(_));
                    if timeout {
                        base.tier_counters.timeouts.fetch_add(1, Ordering::Relaxed);
                    }
                    if attempt < remote.retries {
                        continue;
                    }
                    return Err(transport_error(timeout));
                }
            };
            let status = response.status().as_u16();
            if status != 206 {
                base.tier_counters
                    .remote_errors
                    .fetch_add(1, Ordering::Relaxed);
                if (500..600).contains(&status) && attempt < remote.retries {
                    continue;
                }
                return Err(if status == 404 {
                    Error::Io(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "remote object unavailable (HTTP 404)",
                    ))
                } else {
                    Error::Corrupt(format!("remote range requires HTTP 206; received {status}"))
                });
            }
            let headers = response.headers();
            let content_range = headers
                .get("Content-Range")
                .and_then(|value| value.to_str().ok());
            let content_length = headers
                .get("Content-Length")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok());
            let encoding = headers
                .get("Content-Encoding")
                .and_then(|value| value.to_str().ok());
            if content_range != Some(expected_range.as_str())
                || content_length != Some(record.compressed_size as u64)
                || encoding.is_some_and(|encoding| encoding != "identity")
            {
                base.tier_counters
                    .remote_errors
                    .fetch_add(1, Ordering::Relaxed);
                return Err(Error::Corrupt(
                    "remote Content-Range/length/encoding mismatch".into(),
                ));
            }
            let mut encoded = Vec::with_capacity(record.compressed_size as usize);
            let result = response
                .body_mut()
                .as_reader()
                .take(record.compressed_size as u64 + 1)
                .read_to_end(&mut encoded);
            base.tier_counters
                .remote_bytes
                .fetch_add(encoded.len() as u64, Ordering::Relaxed);
            if let Err(error) = result {
                base.tier_counters
                    .remote_errors
                    .fetch_add(1, Ordering::Relaxed);
                let timeout = body_timeout(&error);
                if timeout {
                    base.tier_counters.timeouts.fetch_add(1, Ordering::Relaxed);
                    if attempt < remote.retries {
                        continue;
                    }
                    return Err(transport_error(true));
                }
                return Err(Error::Corrupt("truncated remote object body".into()));
            }
            if encoded.len() != record.compressed_size as usize {
                base.tier_counters
                    .remote_errors
                    .fetch_add(1, Ordering::Relaxed);
                return Err(Error::Corrupt("remote object body length mismatch".into()));
            }
            return Ok(encoded);
        }
        Err(transport_error(false))
    }
}

fn transport_error(timeout: bool) -> Error {
    std::io::Error::new(
        if timeout {
            std::io::ErrorKind::TimedOut
        } else {
            std::io::ErrorKind::ConnectionAborted
        },
        if timeout {
            "HTTP tier request timed out"
        } else {
            "HTTP tier transport failed"
        },
    )
    .into()
}

fn body_timeout(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) || error
        .get_ref()
        .and_then(|error| error.downcast_ref::<ureq::Error>())
        .is_some_and(|error| matches!(error, ureq::Error::Timeout(_)))
}

// Promotion files contain verified RAW chunks. This intentionally trades disk
// space for independence from the secondary store's codec and physical layout.
struct CacheDirectory {
    _path: PathBuf,
    _anchor: DirectoryAnchor,
}

impl CacheDirectory {
    fn open(path: &Path, create: bool) -> Result<Self> {
        let anchor = DirectoryAnchor::open(path, create)?;
        Ok(Self {
            _path: anchor.path().to_path_buf(),
            _anchor: anchor,
        })
    }

    fn open_file(&self, name: &str, new: bool) -> Result<File> {
        #[cfg(unix)]
        let file = {
            use std::os::fd::{AsRawFd, FromRawFd};
            let name = std::ffi::CString::new(name)
                .map_err(|_| Error::Invalid("invalid cache object name".into()))?;
            let flags = libc::O_CLOEXEC
                | libc::O_NONBLOCK
                | libc::O_NOFOLLOW
                | if new {
                    libc::O_RDWR | libc::O_CREAT | libc::O_EXCL
                } else {
                    libc::O_RDONLY
                };
            // SAFETY: retained directory and generated opaque name are valid.
            let descriptor = unsafe {
                libc::openat(self._anchor.file().as_raw_fd(), name.as_ptr(), flags, 0o600)
            };
            if descriptor < 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ELOOP) {
                    return Err(Error::Corrupt("symlink in primary cache object".into()));
                }
                return Err(error.into());
            }
            unsafe { File::from_raw_fd(descriptor) }
        };
        #[cfg(not(unix))]
        let file = {
            reject_links(&self._path)?;
            let mut options = OpenOptions::new();
            options.read(true).write(new).create_new(new);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.custom_flags(0x00200000);
            }
            options.open(self._path.join(name))?
        };
        if !file.metadata()?.is_file() || is_reparse(&file.metadata()?) {
            return Err(Error::Corrupt(
                "cache object is a link or nonregular file".into(),
            ));
        }
        Ok(file)
    }

    fn read(&self, hash: &[u8; 32], expected_size: u32) -> Result<Vec<u8>> {
        let file = self.open_file(&cache_name(hash), false)?;
        if file.metadata()?.len() != expected_size as u64 + CACHE_HEADER as u64 {
            return Err(Error::Corrupt("promoted object length mismatch".into()));
        }
        let mut header = [0u8; CACHE_HEADER];
        read_at_exact(&file, 0, &mut header)?;
        if &header[..8] != CACHE_MAGIC
            || super::u32_at(&header[8..12]) != expected_size
            || &header[12..44] != hash
        {
            return Err(Error::Corrupt("promoted object header mismatch".into()));
        }
        let mut raw = vec![0; expected_size as usize];
        read_at_exact(&file, CACHE_HEADER as u64, &mut raw)?;
        if blake3::hash(&raw).as_bytes() != hash {
            return Err(Error::Corrupt("promoted object BLAKE3 mismatch".into()));
        }
        Ok(raw)
    }

    fn promote(&self, hash: &[u8; 32], raw: &[u8]) -> Result<bool> {
        if raw.is_empty()
            || raw.len() > MAX_CHUNK_BYTES as usize
            || blake3::hash(raw).as_bytes() != hash
        {
            return Err(Error::Corrupt(
                "promotion requires a bounded verified raw object".into(),
            ));
        }
        let temp = format!(
            ".promote-{:08x}-{:032x}-{:016x}.tmp",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let destination = cache_name(hash);
        let mut file = self.open_file(&temp, true)?;
        let result = (|| {
            file.write_all(CACHE_MAGIC)?;
            file.write_all(&(raw.len() as u32).to_le_bytes())?;
            file.write_all(hash)?;
            file.write_all(raw)?;
            file.sync_all()?;
            self.publish(&temp, &destination)
        })();
        drop(file);
        let _ = self.remove(&temp);
        match result {
            Ok(()) => {
                self.sync()?;
                Ok(true)
            }
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                self.read(hash, raw.len() as u32)?;
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    fn publish(&self, source: &str, destination: &str) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let source = std::ffi::CString::new(source)
                .map_err(|_| Error::Invalid("invalid promotion name".into()))?;
            let destination = std::ffi::CString::new(destination)
                .map_err(|_| Error::Invalid("invalid promotion name".into()))?;
            // SAFETY: hard-link publication is anchored, atomic, and refuses an
            // existing name. Readers can never observe the partial temp object.
            if unsafe {
                libc::linkat(
                    self._anchor.file().as_raw_fd(),
                    source.as_ptr(),
                    self._anchor.file().as_raw_fd(),
                    destination.as_ptr(),
                    0,
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
            }
            reject_links(&self._path)?;
            let from: Vec<u16> = self
                ._path
                .join(source)
                .as_os_str()
                .encode_wide()
                .chain([0])
                .collect();
            let to: Vec<u16> = self
                ._path
                .join(destination)
                .as_os_str()
                .encode_wide()
                .chain([0])
                .collect();
            // SAFETY: terminated UTF-16 names remain live; flags=0 forbids replace.
            if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(())
        }
    }

    fn remove(&self, name: &str) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let name = std::ffi::CString::new(name)
                .map_err(|_| Error::Invalid("invalid promotion name".into()))?;
            // SAFETY: unlink is anchored to our retained directory.
            if unsafe { libc::unlinkat(self._anchor.file().as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            reject_links(&self._path)?;
            fs::remove_file(self._path.join(name))?;
            Ok(())
        }
    }
    fn sync(&self) -> Result<()> {
        #[cfg(unix)]
        self._anchor.file().sync_all()?;
        Ok(())
    }
}

fn cache_name(hash: &[u8; 32]) -> String {
    format!("{}.psc", blake3::Hash::from_bytes(*hash))
}

fn is_reparse(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

fn reject_links(path: &Path) -> Result<()> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut prefix = PathBuf::new();
    for component in absolute.components() {
        if matches!(component, Component::ParentDir) {
            return Err(Error::Invalid("parent traversal in primary cache".into()));
        }
        prefix.push(component);
        if matches!(component, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        match fs::symlink_metadata(&prefix) {
            Ok(metadata) if metadata.file_type().is_symlink() || is_reparse(&metadata) => {
                return Err(Error::Invalid(
                    "symlink/reparse point in primary cache path".into(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn projected_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut ancestor = absolute.as_path();
    let mut tail = Vec::new();
    while !ancestor.exists() {
        tail.push(
            ancestor
                .file_name()
                .ok_or_else(|| Error::Invalid("invalid cache directory".into()))?
                .to_os_string(),
        );
        ancestor = ancestor
            .parent()
            .ok_or_else(|| Error::Invalid("cache directory has no parent".into()))?;
    }
    let mut projected = fs::canonicalize(ancestor)?;
    for component in tail.into_iter().rev() {
        projected.push(component);
    }
    Ok(projected)
}

#[cfg(test)]
mod tests;
