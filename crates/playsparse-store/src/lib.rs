//! Immutable CAS with fixed-width pack index and verify-before-publish transaction.
use playsparse_core::{
    ChunkRef, Chunker, Codec, Error, FORMAT_VERSION, FileEntry, Layout, MAX_CHUNK_BYTES,
    MAX_METADATA_BYTES, Manifest, Result, parse_hash, valid_path,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Instant,
};

#[cfg(test)]
mod game_tests;
pub mod tiers;
pub use tiers::{HttpTier, SourceKind, TierConfig, TierMetrics};

pub use playsparse_core::{Chunker as Chunking, Layout as ObjectLayout};
const INDEX_MAGIC: &[u8; 8] = b"PSPIDX01";
const PACK_MAGIC: &[u8; 8] = b"PSPPACK1";
const RECORD_BYTES: u64 = 56;
const PACK_LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct PackOptions {
    pub layout: Layout,
    pub chunker: Chunker,
    pub chunk_size: u32,
    pub level: i32,
}
impl Default for PackOptions {
    fn default() -> Self {
        Self {
            layout: Layout::Packs,
            chunker: Chunker::Cdc,
            chunk_size: 256 * 1024,
            level: 3,
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct PackStats {
    pub logical_bytes: u64,
    pub physical_bytes: u64,
    pub object_bytes: u64,
    pub metadata_bytes: u64,
    pub allocated_bytes: Option<u64>,
    pub filesystem_entries: u64,
    pub files: usize,
    pub unique_objects: usize,
    pub reused_chunks: u64,
    pub pack_seconds: f64,
    pub pack_cpu_seconds: Option<f64>,
    pub zstd_attempted_objects: u64,
    pub measured_raw_objects: u64,
    pub measured_raw_bytes: u64,
    pub verified_before_publish: bool,
}
#[derive(Clone, Debug)]
pub struct ObjectRecord {
    pub hash: [u8; 32],
    pub pack_id: u32,
    pub offset: u64,
    pub compressed_size: u32,
    pub raw_size: u32,
    pub codec: Codec,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Commit {
    version: u32,
    manifest_blake3: String,
    index_blake3: String,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}
fn metadata_bytes(path: &Path) -> Result<Vec<u8>> {
    let mut file = File::open(path)?;
    if file.metadata()?.len() > MAX_METADATA_BYTES {
        return Err(invalid("metadata exceeds configured format limit"));
    }
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        return Err(invalid("metadata grew beyond limit"));
    }
    Ok(bytes)
}
fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = OpenOptions::new().write(true).create_new(true).open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}
fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}
fn publish_directory(stage: &Path, destination: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let source = std::ffi::CString::new(stage.as_os_str().as_bytes())
            .map_err(|_| invalid("NUL in stage path"))?;
        let target = std::ffi::CString::new(destination.as_os_str().as_bytes())
            .map_err(|_| invalid("NUL in destination path"))?;
        // SAFETY: both C strings remain alive for this call. Exclusive rename
        // refuses even an externally created empty destination directory.
        #[cfg(target_os = "linux")]
        let status = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(target_os = "macos")]
        let status =
            unsafe { libc::renamex_np(source.as_ptr(), target.as_ptr(), libc::RENAME_EXCL) };
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err(invalid(
            "exclusive directory publication is unsupported on this Unix OS",
        ));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if status != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(source: *const u16, destination: *const u16, flags: u32) -> i32;
        }
        let source: Vec<u16> = stage.as_os_str().encode_wide().collect();
        let target: Vec<u16> = destination.as_os_str().encode_wide().collect();
        if source.contains(&0) || target.contains(&0) {
            return Err(invalid("NUL in publication path"));
        }
        let source: Vec<u16> = source.into_iter().chain([0]).collect();
        let target: Vec<u16> = target.into_iter().chain([0]).collect();
        // SAFETY: both terminated UTF-16 buffers remain alive for this call.
        // No REPLACE_EXISTING or COPY_ALLOWED flags: same-volume publication
        // must refuse an existing destination, including an empty directory.
        if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), 0) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}
fn object_path(root: &Path, hash: &[u8; 32]) -> PathBuf {
    let hex = blake3::Hash::from_bytes(*hash).to_string();
    root.join("objects")
        .join(&hex[..2])
        .join(format!("{}.pso", &hex[2..]))
}
fn pack_path(root: &Path, id: u32) -> PathBuf {
    root.join("packs").join(format!("pack-{id:04}.psp"))
}

fn source_file_mode(metadata: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        // Non-Unix filesystems do not expose POSIX mode bits. Preserve the
        // portable read-only signal instead of marking every packed file as
        // read-only, otherwise writable overlays reject normal Windows files.
        if metadata.permissions().readonly() {
            0o444
        } else {
            0o644
        }
    }
}

// Check an as-yet nonexistent parent before mkdir can change the source tree.
fn projected_parent(parent: &Path) -> Result<PathBuf> {
    if parent.exists() {
        return Ok(parent.canonicalize()?);
    }
    if parent
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(invalid(
            "a nonexistent destination parent must not contain '..'; use an absolute normalized path",
        ));
    }
    let mut existing = parent.to_path_buf();
    let mut suffix = Vec::new();
    loop {
        if existing.as_os_str().is_empty() {
            existing = PathBuf::from(".");
        }
        match fs::symlink_metadata(&existing) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                suffix.push(
                    existing
                        .file_name()
                        .ok_or_else(|| invalid("destination ancestor"))?
                        .to_os_string(),
                );
                existing = existing
                    .parent()
                    .ok_or_else(|| invalid("destination ancestor"))?
                    .to_path_buf();
            }
            Err(error) => return Err(error.into()),
        }
    }
    let mut projected = existing.canonicalize()?;
    for name in suffix.into_iter().rev() {
        projected.push(name);
    }
    Ok(projected)
}

pub fn encode_index(records: &BTreeMap<[u8; 32], ObjectRecord>) -> Result<Vec<u8>> {
    let len = 16u64
        .checked_add(records.len() as u64 * RECORD_BYTES)
        .ok_or_else(|| invalid("index overflow"))?;
    if len > MAX_METADATA_BYTES {
        return Err(invalid("index exceeds format memory limit"));
    }
    let mut out = Vec::with_capacity(len as usize);
    out.extend_from_slice(INDEX_MAGIC);
    out.extend_from_slice(&(records.len() as u64).to_le_bytes());
    for record in records.values() {
        out.extend_from_slice(&record.hash);
        out.extend_from_slice(&record.pack_id.to_le_bytes());
        out.extend_from_slice(&record.offset.to_le_bytes());
        out.extend_from_slice(&record.compressed_size.to_le_bytes());
        out.extend_from_slice(&record.raw_size.to_le_bytes());
        out.push(record.codec.byte());
        out.extend_from_slice(&[0; 3]);
    }
    Ok(out)
}
fn u32_at(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}
fn u64_at(b: &[u8]) -> u64 {
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}
pub fn decode_index(bytes: &[u8]) -> Result<BTreeMap<[u8; 32], ObjectRecord>> {
    if bytes.len() < 16 || &bytes[..8] != INDEX_MAGIC {
        return Err(invalid("index header"));
    }
    let count = u64_at(&bytes[8..16]);
    if count
        .checked_mul(RECORD_BYTES)
        .and_then(|n| n.checked_add(16))
        != Some(bytes.len() as u64)
        || bytes.len() as u64 > MAX_METADATA_BYTES
    {
        return Err(invalid("index length/count"));
    }
    let mut result = BTreeMap::new();
    let mut last = None;
    for b in bytes[16..].as_chunks::<56>().0 {
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&b[..32]);
        if last.is_some_and(|h| h >= hash) || b[53..56] != [0; 3] {
            return Err(invalid("unsorted index/reserved bits"));
        }
        let record = ObjectRecord {
            hash,
            pack_id: u32_at(&b[32..36]),
            offset: u64_at(&b[36..44]),
            compressed_size: u32_at(&b[44..48]),
            raw_size: u32_at(&b[48..52]),
            codec: Codec::from_byte(b[52])?,
        };
        if record.raw_size == 0
            || record.raw_size > MAX_CHUNK_BYTES
            || record.compressed_size == 0
            || record.compressed_size > MAX_CHUNK_BYTES
            || (record.codec == Codec::Raw && record.raw_size != record.compressed_size)
        {
            return Err(invalid("object lengths"));
        }
        last = Some(hash);
        result.insert(hash, record);
    }
    Ok(result)
}

pub struct Store {
    root: PathBuf,
    manifest: Manifest,
    index: BTreeMap<[u8; 32], ObjectRecord>,
    packs: BTreeMap<u32, File>,
    pack_lengths: BTreeMap<u32, u64>,
    tiered: Option<tiers::Tiered>,
    tier_counters: tiers::Counters,
}
impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        Self::open_inner(root, false)
    }
    fn open_inner(root: &Path, staging: bool) -> Result<Self> {
        Self::open_metadata(root, staging, false)
    }

    pub fn open_with_tiers(root: &Path, config: TierConfig) -> Result<Self> {
        config.validate()?;
        let mut store = Self::open_metadata(root, false, true)?;
        store.tiered = Some(tiers::Tiered::open(&store, config)?);
        Ok(store)
    }

    fn open_metadata(root: &Path, staging: bool, allow_missing: bool) -> Result<Self> {
        let manifest_bytes = metadata_bytes(&root.join("manifest.json"))?;
        let index_bytes = metadata_bytes(&root.join("index/objects.idx"))?;
        if !staging {
            let commit_bytes = metadata_bytes(&root.join("COMMITTED.json"))?;
            if commit_bytes.len() > 4096 {
                return Err(invalid("commit marker size"));
            }
            let commit: Commit =
                serde_json::from_slice(&commit_bytes).map_err(|e| invalid(e.to_string()))?;
            if commit.version != FORMAT_VERSION
                || blake3::hash(&manifest_bytes).to_string() != commit.manifest_blake3
                || blake3::hash(&index_bytes).to_string() != commit.index_blake3
            {
                return Err(invalid("commit marker metadata digest mismatch"));
            }
        }
        let manifest: Manifest =
            serde_json::from_slice(&manifest_bytes).map_err(|e| invalid(e.to_string()))?;
        manifest.validate()?;
        let index = decode_index(&index_bytes)?;
        let mut packs = BTreeMap::new();
        let mut pack_lengths = BTreeMap::new();
        let mut unavailable_packs = std::collections::BTreeSet::new();
        for record in index.values() {
            if manifest.layout == Layout::Packs {
                if !unavailable_packs.contains(&record.pack_id)
                    && let std::collections::btree_map::Entry::Vacant(entry) =
                        packs.entry(record.pack_id)
                {
                    let file = match File::open(pack_path(root, record.pack_id)) {
                        Ok(file) => Some(file),
                        Err(error)
                            if allow_missing && error.kind() == std::io::ErrorKind::NotFound =>
                        {
                            None
                        }
                        Err(error) => return Err(error.into()),
                    };
                    if let Some(file) = file {
                        let mut header = [0u8; 8];
                        read_at_exact(&file, 0, &mut header)?;
                        if &header != PACK_MAGIC {
                            return Err(invalid("pack header"));
                        }
                        pack_lengths.insert(record.pack_id, file.metadata()?.len());
                        entry.insert(file);
                    } else {
                        unavailable_packs.insert(record.pack_id);
                    }
                }
                if record.offset < 8
                    || record
                        .offset
                        .checked_add(record.compressed_size as u64)
                        .is_none_or(|n| {
                            pack_lengths
                                .get(&record.pack_id)
                                .is_some_and(|length| n > *length)
                        })
                {
                    return Err(invalid("object outside pack"));
                }
            } else {
                if record.offset != 0 || record.pack_id != 0 {
                    return Err(invalid("loose object locator"));
                }
                match fs::metadata(object_path(root, &record.hash)) {
                    Ok(metadata)
                        if metadata.len() == record.compressed_size as u64
                            && metadata.is_file() => {}
                    Err(error) if allow_missing && error.kind() == std::io::ErrorKind::NotFound => {
                    }
                    Err(error) => return Err(error.into()),
                    _ => return Err(invalid("loose object length/type")),
                }
            }
        }
        // Reject aliases/overlap, holes and trailing garbage in physical pack coverage.
        if manifest.layout == Layout::Packs {
            let mut spans: Vec<_> = index.values().collect();
            spans.sort_by_key(|r| (r.pack_id, r.offset));
            let mut ends = BTreeMap::new();
            for record in spans {
                let end = ends.entry(record.pack_id).or_insert(8u64);
                if *end != record.offset {
                    return Err(invalid("pack index coverage/overlap"));
                }
                *end = end
                    .checked_add(record.compressed_size as u64)
                    .ok_or_else(|| invalid("pack coverage overflow"))?;
            }
            for (id, end) in ends {
                if pack_lengths.get(&id).is_some_and(|length| *length != end) {
                    return Err(invalid("pack trailing data"));
                }
                pack_lengths.insert(id, end);
            }
        }
        for file in &manifest.files {
            for chunk in &file.chunks {
                let hash = parse_hash(&chunk.hash)?;
                if index
                    .get(&hash)
                    .is_none_or(|r| r.raw_size != chunk.raw_size)
                {
                    return Err(invalid("manifest references missing/wrong-size object"));
                }
            }
        }
        Ok(Self {
            root: root.into(),
            manifest,
            index,
            packs,
            pack_lengths,
            tiered: None,
            tier_counters: tiers::Counters::default(),
        })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn index(&self) -> &BTreeMap<[u8; 32], ObjectRecord> {
        &self.index
    }
    pub fn lookup(&self, hash: &[u8; 32]) -> Result<&ObjectRecord> {
        self.index
            .get(hash)
            .ok_or_else(|| Error::Corrupt("missing object".into()))
    }
    pub fn read_object(&self, hash: &[u8; 32]) -> Result<Vec<u8>> {
        self.read_object_with_source(hash).map(|(bytes, _)| bytes)
    }

    pub fn tier_metrics(&self) -> TierMetrics {
        self.tier_counters.snapshot()
    }

    pub fn read_object_with_source(&self, hash: &[u8; 32]) -> Result<(Vec<u8>, SourceKind)> {
        let result = if let Some(tiered) = &self.tiered {
            tiered.read(self, hash)
        } else {
            self.read_primary(hash).map(|(raw, encoded)| {
                self.tier_counters.hit(SourceKind::PrimaryLocal, encoded);
                (raw, SourceKind::PrimaryLocal)
            })
        };
        if result.is_err() {
            self.tier_counters.error();
            if matches!(&result, Err(Error::Corrupt(_)))
                || matches!(&result, Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof)
            {
                self.tier_counters.integrity();
            }
        }
        result
    }

    fn read_primary(&self, hash: &[u8; 32]) -> Result<(Vec<u8>, u64)> {
        let record = self.lookup(hash)?;
        let mut encoded = vec![0u8; record.compressed_size as usize];
        match self.manifest.layout {
            Layout::Packs => read_at_exact(
                self.packs.get(&record.pack_id).ok_or_else(|| {
                    Error::Io(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "primary pack unavailable",
                    ))
                })?,
                record.offset,
                &mut encoded,
            )?,
            Layout::Loose => {
                read_at_exact(&File::open(object_path(&self.root, hash))?, 0, &mut encoded)?
            }
        }
        let raw = decode_object(record, encoded, hash)?;
        Ok((raw, record.compressed_size as u64))
    }

    pub fn verify(&self) -> Result<VerifyStats> {
        self.verify_observed(&mut |_| Ok(()))
    }
    /// Callback failures abort verification; called between bounded chunk reads.
    pub fn verify_observed(
        &self,
        observer: &mut dyn FnMut(Progress) -> Result<()>,
    ) -> Result<VerifyStats> {
        let mut bytes = 0u64;
        // Whole-file integrity checked incrementally; no full-file buffer.
        for file in &self.manifest.files {
            let mut hasher = blake3::Hasher::new();
            for chunk in &file.chunks {
                observer(Progress {
                    stage: "verifying",
                    bytes,
                    files: 0,
                })?;
                let raw = self.read_object(&parse_hash(&chunk.hash)?)?;
                hasher.update(&raw);
                bytes += raw.len() as u64;
            }
            if hasher.finalize().to_string() != file.hash {
                return Err(Error::Corrupt(format!("whole-file digest: {}", file.path)));
            }
        }
        // Also validate any unreferenced objects rather than overlooking orphan corruption.
        let referenced: std::collections::BTreeSet<_> = self
            .manifest
            .files
            .iter()
            .flat_map(|f| f.chunks.iter())
            .map(|c| parse_hash(&c.hash))
            .collect::<Result<_>>()?;
        for hash in self.index.keys() {
            observer(Progress {
                stage: "verifying",
                bytes,
                files: self.manifest.files.len(),
            })?;
            if !referenced.contains(hash) {
                self.read_object(hash)?;
            }
        }
        Ok(VerifyStats {
            ok: true,
            checked_files: self.manifest.files.len(),
            checked_bytes: bytes,
            checked_objects: self.index.len(),
        })
    }
}

fn decode_object(record: &ObjectRecord, encoded: Vec<u8>, hash: &[u8; 32]) -> Result<Vec<u8>> {
    let raw = match record.codec {
        Codec::Raw => encoded,
        Codec::Zstd => zstd::bulk::decompress(&encoded, record.raw_size as usize)
            .map_err(|e| Error::Corrupt(e.to_string()))?,
    };
    if raw.len() != record.raw_size as usize || blake3::hash(&raw).as_bytes() != hash {
        return Err(Error::Corrupt(blake3::Hash::from_bytes(*hash).to_string()));
    }
    Ok(raw)
}
#[derive(Debug, Serialize)]
pub struct VerifyStats {
    pub ok: bool,
    pub checked_files: usize,
    pub checked_bytes: u64,
    pub checked_objects: usize,
}

pub fn read_at_exact(file: &File, mut offset: u64, mut buf: &mut [u8]) -> std::io::Result<()> {
    while !buf.is_empty() {
        #[cfg(unix)]
        let result = {
            use std::os::unix::fs::FileExt;
            file.read_at(buf, offset)
        };
        #[cfg(windows)]
        let result = {
            use std::os::windows::fs::FileExt;
            file.seek_read(buf, offset)
        };
        let n = match result {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            other => other?,
        };
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "short object read",
            ));
        }
        offset += n as u64;
        buf = &mut buf[n..];
    }
    Ok(())
}

struct Writer {
    root: PathBuf,
    options: PackOptions,
    index: BTreeMap<[u8; 32], ObjectRecord>,
    pack: Option<File>,
    pack_id: u32,
    pack_end: u64,
    reused: u64,
    zstd_attempted: u64,
    measured_raw: u64,
    measured_raw_bytes: u64,
}
impl Writer {
    fn put(&mut self, raw: &[u8], measured_raw: bool) -> Result<String> {
        let hash = *blake3::hash(raw).as_bytes();
        let hex = blake3::Hash::from_bytes(hash).to_string();
        if self.index.contains_key(&hash) {
            self.reused += 1;
            return Ok(hex);
        }
        let compressed = if measured_raw {
            self.measured_raw += 1;
            self.measured_raw_bytes += raw.len() as u64;
            Vec::new()
        } else {
            self.zstd_attempted += 1;
            zstd::bulk::compress(raw, self.options.level)?
        };
        let (codec, data) = if !measured_raw && compressed.len() < raw.len() {
            (Codec::Zstd, compressed.as_slice())
        } else {
            (Codec::Raw, raw)
        };
        let (pack_id, offset) = match self.options.layout {
            Layout::Loose => {
                let path = object_path(&self.root, &hash);
                let parent = path.parent().ok_or_else(|| invalid("object parent"))?;
                fs::create_dir_all(parent)?;
                write_synced(&path, data)?;
                (0, 0)
            }
            Layout::Packs => {
                if self.pack.is_some() && self.pack_end + data.len() as u64 > PACK_LIMIT {
                    if let Some(file) = self.pack.take() {
                        file.sync_all()?;
                    }
                    self.pack_id = self
                        .pack_id
                        .checked_add(1)
                        .ok_or_else(|| invalid("pack id overflow"))?;
                }
                if self.pack.is_none() {
                    let mut file = OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .open(pack_path(&self.root, self.pack_id))?;
                    file.write_all(PACK_MAGIC)?;
                    self.pack = Some(file);
                    self.pack_end = 8;
                }
                let offset = self.pack_end;
                self.pack
                    .as_mut()
                    .ok_or_else(|| invalid("pack writer unavailable"))?
                    .write_all(data)?;
                self.pack_end += data.len() as u64;
                (self.pack_id, offset)
            }
        };
        self.index.insert(
            hash,
            ObjectRecord {
                hash,
                pack_id,
                offset,
                compressed_size: data.len() as u32,
                raw_size: raw.len() as u32,
                codec,
            },
        );
        Ok(hex)
    }
}
// Keep the lock inode stable: deleting it while another writer holds an open
// handle would permit a third writer to acquire a different lock inode.
struct Lock {
    _file: File,
}

pub fn pack_directory(
    source: &Path,
    destination: &Path,
    options: &PackOptions,
) -> Result<PackStats> {
    pack_directory_inner(source, destination, options, None, &mut |_| Ok(()))
}
/// Experimental offline plan; the runtime still reads the unchanged v1 format.
pub fn pack_directory_with_plan(
    source: &Path,
    destination: &Path,
    options: &PackOptions,
    plan: &playsparse_game::PackingPlan,
) -> Result<PackStats> {
    pack_directory_inner(source, destination, options, Some(plan), &mut |_| Ok(()))
}
/// Real engine work counters; no estimated percentage or synthetic stages.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Progress {
    pub stage: &'static str,
    pub bytes: u64,
    pub files: usize,
}
/// Abort cooperatively by returning an error. Staging is removed by RAII;
/// the final callback precedes atomic publication and cannot roll it back.
pub fn pack_directory_observed(
    source: &Path,
    destination: &Path,
    options: &PackOptions,
    observer: &mut dyn FnMut(Progress) -> Result<()>,
) -> Result<PackStats> {
    pack_directory_inner(source, destination, options, None, observer)
}
fn pack_directory_inner(
    source: &Path,
    destination: &Path,
    options: &PackOptions,
    plan: Option<&playsparse_game::PackingPlan>,
    observer: &mut dyn FnMut(Progress) -> Result<()>,
) -> Result<PackStats> {
    let start = Instant::now();
    let cpu_start = playsparse_core::process_resources().0;
    if let Some(plan) = plan {
        if options.chunker != Chunker::Cdc
            || options.chunk_size != playsparse_game::TARGET_BYTES
            || options.level != 3
        {
            return Err(invalid("game-aware v1 requires CDC 256K and Zstd level 3"));
        }
        plan.verify_source(source)?;
    }
    if !(4096..=MAX_CHUNK_BYTES / 4).contains(&options.chunk_size)
        || !options.chunk_size.is_power_of_two()
        || !zstd::compression_level_range().contains(&options.level)
    {
        return Err(invalid(
            "require power-of-two chunk size 4K..4M and valid Zstd level",
        ));
    }
    let source = source.canonicalize()?;
    if !source.is_dir() {
        return Err(invalid("source must be a directory"));
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if projected_parent(parent)?.starts_with(&source) {
        return Err(invalid(
            "store must be new and outside source tree; no source directories were created",
        ));
    }
    fs::create_dir_all(parent)?;
    let parent = parent.canonicalize()?;
    let name = destination
        .file_name()
        .ok_or_else(|| invalid("destination name"))?;
    let destination = parent.join(name);
    if parent.starts_with(&source) || destination.exists() {
        return Err(invalid("store must be new and outside source tree"));
    }
    let lock_path = parent.join(format!(".{}.pack.lock", name.to_string_lossy()));
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    lock_file
        .try_lock()
        .map_err(|e| invalid(format!("destination is locked: {e}")))?;
    let lock = Lock { _file: lock_file };
    let stage = tempfile::Builder::new()
        .prefix(".playsparse-transaction-")
        .tempdir_in(&parent)?;
    fs::create_dir(stage.path().join("packs"))?;
    fs::create_dir(stage.path().join("objects"))?;
    fs::create_dir(stage.path().join("index"))?;
    let mut writer = Writer {
        root: stage.path().into(),
        options: options.clone(),
        index: BTreeMap::new(),
        pack: None,
        pack_id: 0,
        pack_end: 0,
        reused: 0,
        zstd_attempted: 0,
        measured_raw: 0,
        measured_raw_bytes: 0,
    };
    let mut manifest = Manifest {
        format: "playsparse-store".into(),
        version: FORMAT_VERSION,
        layout: options.layout,
        chunker: options.chunker,
        chunk_size: options.chunk_size,
        zstd_level: options.level,
        directories: Vec::new(),
        files: Vec::new(),
    };
    let mut processed = 0u64;
    let mut completed_files = 0usize;
    let mut entries = Vec::new();
    let mut metadata_budget = 0u64;
    for entry in walkdir::WalkDir::new(&source)
        .min_depth(1)
        .follow_links(false)
    {
        observer(Progress {
            stage: "scanning",
            bytes: 0,
            files: entries.len(),
        })?;
        let entry = entry.map_err(|e| invalid(e.to_string()))?;
        if entry.file_type().is_symlink() {
            return Err(invalid(format!(
                "symlinks are unsupported: {}",
                entry.path().display()
            )));
        }
        let rel = entry
            .path()
            .strip_prefix(&source)
            .map_err(|e| invalid(e.to_string()))?;
        let path = rel
            .to_str()
            .ok_or_else(|| invalid("non-UTF8 path"))?
            .replace(std::path::MAIN_SEPARATOR, "/");
        if !valid_path(&path) {
            return Err(invalid(format!("unsupported path: {path}")));
        }
        metadata_budget = metadata_budget
            .checked_add(path.len() as u64 + 256)
            .ok_or_else(|| invalid("metadata overflow"))?;
        if metadata_budget > MAX_METADATA_BYTES {
            return Err(invalid(
                "source directory metadata exceeds bounded format limit",
            ));
        }
        if entry.file_type().is_dir() {
            manifest.directories.push(path);
        } else if entry.file_type().is_file() {
            entries.push((path, entry.into_path()));
        } else {
            return Err(invalid("special files unsupported"));
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    manifest.directories.sort();
    if let Some(plan) = plan
        && (entries.len() != plan.files.len()
            || manifest.directories != plan.source_identity.directories)
    {
        return Err(invalid("source namespace changed after plan validation"));
    }
    for (path, fullpath) in entries {
        let planned = if let Some(plan) = plan {
            let index = plan
                .files
                .binary_search_by(|f| f.path.cmp(&path))
                .map_err(|_| invalid("source path absent from plan"))?;
            Some(&plan.files[index])
        } else {
            None
        };
        let measured_raw = planned.is_some_and(|f| {
            f.compression_strategy == playsparse_game::CompressionStrategy::MeasuredRaw
        });
        let input = File::open(&fullpath)?;
        let before = input.metadata()?;
        let mode = source_file_mode(&before);
        let mut file = FileEntry {
            path,
            size: before.len(),
            mode,
            hash: String::new(),
            chunks: Vec::new(),
        };
        let mut hasher = blake3::Hasher::new();
        let mut offset = 0u64;
        let mut add = |raw: &[u8]| -> Result<()> {
            observer(Progress {
                stage: "packing",
                bytes: processed,
                files: completed_files,
            })?;
            processed += raw.len() as u64;
            metadata_budget += 192;
            if metadata_budget > MAX_METADATA_BYTES {
                return Err(invalid("chunk metadata exceeds bounded format limit"));
            }
            hasher.update(raw);
            let hash = writer.put(raw, measured_raw)?;
            file.chunks.push(ChunkRef {
                hash,
                offset,
                raw_size: raw.len() as u32,
            });
            offset += raw.len() as u64;
            Ok(())
        };
        match options.chunker {
            Chunker::Cdc => {
                let size = options.chunk_size as usize;
                if let Some(planned) = planned
                    .filter(|p| p.chunk_strategy == playsparse_game::ChunkStrategy::ZipRecordsCdc)
                {
                    let mut reader = input.try_clone()?;
                    let mut previous = 0;
                    for end in &planned.boundaries {
                        let segment = (&mut reader).take(end - previous);
                        let cdc = fastcdc::v2020::StreamCDC::new(segment, size / 4, size, size * 4);
                        for chunk in cdc {
                            add(&chunk.map_err(|e| invalid(e.to_string()))?.data)?;
                        }
                        previous = *end;
                    }
                } else {
                    let cdc = fastcdc::v2020::StreamCDC::new(
                        input.try_clone()?,
                        size / 4,
                        size,
                        size * 4,
                    );
                    for chunk in cdc {
                        let chunk = chunk.map_err(|e| invalid(e.to_string()))?;
                        add(&chunk.data)?;
                    }
                }
            }
            Chunker::Fixed => {
                let mut input = input.try_clone()?;
                let mut buffer = vec![0u8; options.chunk_size as usize];
                loop {
                    let mut n = 0;
                    while n < buffer.len() {
                        let read = input.read(&mut buffer[n..])?;
                        if read == 0 {
                            break;
                        }
                        n += read;
                    }
                    if n == 0 {
                        break;
                    }
                    add(&buffer[..n])?;
                }
            }
        }
        let after = input.metadata()?;
        if offset != before.len()
            || before.len() != after.len()
            || before.modified()? != after.modified()?
        {
            return Err(invalid("source changed during pack"));
        }
        file.hash = hasher.finalize().to_string();
        if planned
            .is_some_and(|p| p.size != file.size || p.blake3 != file.hash || p.mode != file.mode)
        {
            return Err(invalid("source content changed after plan validation"));
        }
        manifest.files.push(file);
        completed_files += 1;
    }
    if let Some(pack) = writer.pack.take() {
        pack.sync_all()?;
    }
    observer(Progress {
        stage: "writing manifests",
        bytes: processed,
        files: completed_files,
    })?;
    manifest.validate()?;
    let manifest_bytes = serde_json::to_vec(&manifest).map_err(|e| invalid(e.to_string()))?;
    if manifest_bytes.len() as u64 > MAX_METADATA_BYTES {
        return Err(invalid("manifest exceeds metadata limit"));
    }
    let index_bytes = encode_index(&writer.index)?;
    write_synced(&stage.path().join("manifest.json"), &manifest_bytes)?;
    write_synced(&stage.path().join("index/objects.idx"), &index_bytes)?;
    Store::open_inner(stage.path(), true)?.verify_observed(observer)?;
    let commit = Commit {
        version: FORMAT_VERSION,
        manifest_blake3: blake3::hash(&manifest_bytes).to_string(),
        index_blake3: blake3::hash(&index_bytes).to_string(),
    };
    write_synced(
        &stage.path().join("COMMITTED.json"),
        &serde_json::to_vec(&commit).map_err(|e| invalid(e.to_string()))?,
    )?;
    for entry in walkdir::WalkDir::new(stage.path()).contents_first(true) {
        let entry = entry.map_err(|e| invalid(e.to_string()))?;
        if entry.file_type().is_dir() {
            sync_dir(entry.path())?;
        }
    }
    let object_bytes = writer
        .index
        .values()
        .map(|r| r.compressed_size as u64)
        .sum();
    let physical_bytes = directory_bytes(stage.path())?;
    let logical_bytes = manifest.files.iter().map(|f| f.size).sum();
    let allocation = directory_allocation(stage.path())?;
    let stats = PackStats {
        logical_bytes,
        physical_bytes,
        object_bytes,
        metadata_bytes: physical_bytes - object_bytes,
        allocated_bytes: allocation.0,
        filesystem_entries: allocation.1,
        files: manifest.files.len(),
        unique_objects: writer.index.len(),
        reused_chunks: writer.reused,
        pack_seconds: start.elapsed().as_secs_f64(),
        pack_cpu_seconds: playsparse_core::process_resources()
            .0
            .zip(cpu_start)
            .map(|(end, start)| end - start),
        zstd_attempted_objects: writer.zstd_attempted,
        measured_raw_objects: writer.measured_raw,
        measured_raw_bytes: writer.measured_raw_bytes,
        verified_before_publish: true,
    };
    // TempDir is still owned until publication succeeds, so failures remove only staging.
    if destination.exists() {
        return Err(invalid("destination appeared during pack"));
    }
    observer(Progress {
        stage: "publishing",
        bytes: processed,
        files: completed_files,
    })?;
    publish_directory(stage.path(), &destination)?;
    sync_dir(&parent)?;
    drop(stage);
    drop(lock);
    tracing::info!(store=%destination.display(),logical_bytes,physical_bytes,"published verified store");
    Ok(stats)
}
pub fn directory_bytes(path: &Path) -> Result<u64> {
    let mut total = 0u64;
    for entry in walkdir::WalkDir::new(path) {
        let entry = entry.map_err(|e| invalid(e.to_string()))?;
        if entry.file_type().is_file() {
            total = total
                .checked_add(entry.metadata().map_err(|e| invalid(e.to_string()))?.len())
                .ok_or_else(|| invalid("directory size overflow"))?;
        }
    }
    Ok(total)
}
/// POSIX allocated blocks, including directory blocks; inode-table costs are not
/// available from stat and are not invented. None where the OS API is absent.
pub fn directory_allocation(path: &Path) -> Result<(Option<u64>, u64)> {
    let mut entries = 0u64;
    #[cfg(unix)]
    let mut allocated = 0u64;
    for entry in walkdir::WalkDir::new(path) {
        let entry = entry.map_err(|e| invalid(e.to_string()))?;
        entries += 1;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            allocated = allocated
                .checked_add(
                    entry
                        .metadata()
                        .map_err(|e| invalid(e.to_string()))?
                        .blocks()
                        * 512,
                )
                .ok_or_else(|| invalid("allocation overflow"))?;
        }
        #[cfg(not(unix))]
        let _ = entry;
    }
    #[cfg(unix)]
    return Ok((Some(allocated), entries));
    #[cfg(not(unix))]
    return Ok((None, entries));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pack_verify_dedup_both_layouts() {
        for layout in [Layout::Packs, Layout::Loose] {
            let tmp = tempfile::tempdir().unwrap();
            let src = tmp.path().join("src");
            fs::create_dir_all(src.join("empty/sub")).unwrap();
            let data = vec![42u8; 2 * 1024 * 1024];
            fs::write(src.join("a"), &data).unwrap();
            fs::write(src.join("b"), &data).unwrap();
            fs::write(src.join("emptyfile"), []).unwrap();
            let options = PackOptions {
                layout,
                ..Default::default()
            };
            let dest = tmp.path().join("store");
            let stats = pack_directory(&src, &dest, &options).unwrap();
            assert!(stats.reused_chunks > 0);
            assert!(stats.verified_before_publish);
            assert_eq!(fs::read(src.join("a")).unwrap(), data);
            let store = Store::open(&dest).unwrap();
            assert!(store.verify().unwrap().ok);
            assert_eq!(store.manifest.directories, ["empty", "empty/sub"]);
            assert!(pack_directory(&src, &dest, &options).is_err());
        }
    }
    #[test]
    fn corrupt_missing_and_partial_are_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("a"), vec![1; 10000]).unwrap();
        let store = tmp.path().join("store");
        pack_directory(&src, &store, &PackOptions::default()).unwrap();
        fs::write(store.join("manifest.json"), b"{}").unwrap();
        assert!(Store::open(&store).is_err());
        fs::remove_file(store.join("COMMITTED.json")).unwrap();
        assert!(Store::open(&store).is_err());
    }
    #[test]
    fn object_corruption_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("a"), vec![1; 10000]).unwrap();
        let store = tmp.path().join("store");
        pack_directory(&src, &store, &PackOptions::default()).unwrap();
        let path = pack_path(&store, 0);
        let mut b = fs::read(&path).unwrap();
        b[8] ^= 255;
        fs::write(path, b).unwrap();
        assert!(Store::open(&store).unwrap().verify().is_err());
        fs::remove_file(pack_path(&store, 0)).unwrap();
        assert!(Store::open(&store).is_err());
    }
    #[cfg(windows)]
    #[test]
    fn windows_pack_preserves_writable_and_readonly_intent() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir(&src).unwrap();
        let file = src.join("a");
        fs::write(&file, b"data").unwrap();

        let writable_store = tmp.path().join("writable-store");
        pack_directory(&src, &writable_store, &PackOptions::default()).unwrap();
        let writable = Store::open(&writable_store).unwrap();
        assert_ne!(writable.manifest.files[0].mode & 0o222, 0);

        let mut permissions = fs::metadata(&file).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&file, permissions).unwrap();

        let readonly_store = tmp.path().join("readonly-store");
        pack_directory(&src, &readonly_store, &PackOptions::default()).unwrap();
        let readonly = Store::open(&readonly_store).unwrap();
        assert_eq!(readonly.manifest.files[0].mode & 0o222, 0);
    }

    #[test]
    fn deterministic_chunker_index_manifest_and_pack() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir(&src).unwrap();
        let data: Vec<_> = (0u64..300000)
            .map(|i| ((i.wrapping_mul(6364136223846793005) >> 32) % 251) as u8)
            .collect();
        fs::write(src.join("a"), data).unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        pack_directory(&src, &a, &PackOptions::default()).unwrap();
        pack_directory(&src, &b, &PackOptions::default()).unwrap();
        for path in [
            "manifest.json",
            "index/objects.idx",
            "COMMITTED.json",
            "packs/pack-0000.psp",
        ] {
            assert_eq!(
                fs::read(a.join(path)).unwrap(),
                fs::read(b.join(path)).unwrap()
            );
        }
    }
    #[test]
    fn index_rejects_length_codec_reserved_and_duplicate() {
        let record = ObjectRecord {
            hash: [1; 32],
            pack_id: 0,
            offset: 8,
            compressed_size: 4,
            raw_size: 4,
            codec: Codec::Raw,
        };
        let mut records = BTreeMap::new();
        records.insert(record.hash, record);
        let bytes = encode_index(&records).unwrap();
        assert_eq!(decode_index(&bytes).unwrap().len(), 1);
        for i in [8usize, 68, 69] {
            let mut corrupt = bytes.clone();
            corrupt[i] = 255;
            assert!(decode_index(&corrupt).is_err());
        }
        assert!(decode_index(&bytes[..bytes.len() - 1]).is_err());
    }
    #[test]
    fn exclusive_publish_never_replaces_existing_empty_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let stage = tmp.path().join("stage");
        let dest = tmp.path().join("dest");
        fs::create_dir(&stage).unwrap();
        fs::write(stage.join("data"), b"staged").unwrap();
        fs::create_dir(&dest).unwrap();
        assert!(publish_directory(&stage, &dest).is_err());
        assert!(stage.join("data").exists());
        assert!(!dest.join("data").exists());
    }
    proptest::proptest! { #[test] fn arbitrary_index_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(),0..4096)) {let _=decode_index(&bytes);} }
}
#[cfg(test)]
mod publication_tests {
    use super::*;

    #[test]
    fn rejects_nested_destination_without_mutating_the_source() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("original"), b"original").unwrap();
        assert!(
            pack_directory(
                &source,
                &source.join("new/nested/store"),
                &PackOptions::default()
            )
            .is_err()
        );
        assert!(!source.join("new").exists());
        assert_eq!(fs::read(source.join("original")).unwrap(), b"original");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_alias_into_source_before_creating_destination_parents() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        fs::create_dir(&source).unwrap();
        std::os::unix::fs::symlink(&source, tmp.path().join("alias")).unwrap();
        assert!(
            pack_directory(
                &source,
                &tmp.path().join("alias/new/store"),
                &PackOptions::default()
            )
            .is_err()
        );
        assert_eq!(fs::read_dir(&source).unwrap().count(), 0);
    }

    #[test]
    fn creates_nested_destination_only_outside_the_source() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("data"), b"data").unwrap();
        let destination = tmp.path().join("output/nested/store");
        pack_directory(&source, &destination, &PackOptions::default()).unwrap();
        assert!(Store::open(&destination).unwrap().verify().unwrap().ok);
        assert_eq!(fs::read_dir(source).unwrap().count(), 1);
    }
}
