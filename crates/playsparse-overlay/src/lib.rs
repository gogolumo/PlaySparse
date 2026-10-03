//! Persistent writable namespace over an immutable CAS, with stable open handles.
//! Mutable files are opaque objects; user paths never address physical storage.
mod storage;

use parking_lot::{Mutex, RwLock};
use playsparse_core::{Error, MAX_METADATA_BYTES, MAX_READ_BYTES, Result, valid_path};
use playsparse_range::RangeResolver;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::Instant,
};
use storage::Directory;

const FORMAT: &str = "playsparse-overlay";
const VERSION: u32 = 1;
const STATE: &str = "state.json";
const LOCK: &str = ".overlay.lock";
const LOCK_MARKER: &[u8] = b"playsparse-overlay-lock-v1\n";
const COPY_BUFFER: usize = 1024 * 1024;
const MAX_FILE_SIZE: u64 = i64::MAX as u64;

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub path: String,
    pub id: u64,
    pub size: u64,
    pub mode: u32,
    pub is_dir: bool,
}

/// Counters are per mounted session. File/byte/tombstone gauges describe the
/// current persisted overlay; physical allocation is unknown on Windows.
#[derive(Clone, Debug, Default, Serialize)]
pub struct OverlayMetrics {
    pub copy_up_files: u64,
    pub copy_up_bytes: u64,
    pub overlay_bytes_written: u64,
    pub overlay_files: u64,
    pub tombstones: u64,
    pub overlay_logical_bytes: u64,
    pub overlay_allocated_bytes: Option<u64>,
    pub metadata_bytes: u64,
}

#[derive(Clone)]
pub struct Handle {
    inner: Arc<Inner>,
    node: Arc<Node>,
    writable: bool,
}

#[derive(Clone)]
pub struct Overlay {
    inner: Arc<Inner>,
}

struct Inner {
    base: Arc<RangeResolver>,
    root: Directory,
    data: Directory,
    // Never unlink or replace this inode: another process must lock the same
    // object before opening, discarding, or collecting orphan data.
    _lock: File,
    namespace: RwLock<Namespace>,
    mutations: Mutex<()>,
    copy_up_files: AtomicU64,
    copy_up_bytes: AtomicU64,
    bytes_written: AtomicU64,
    temp_sequence: AtomicU64,
}

struct Node {
    id: u64,
    path: RwLock<String>,
    mode: AtomicU32,
    content: RwLock<Content>,
}

enum Content {
    Directory,
    Base { source: String, size: u64 },
    Data(Arc<File>),
}

#[derive(Clone)]
struct Namespace {
    nodes: BTreeMap<String, Arc<Node>>,
    ids: BTreeMap<u64, Arc<Node>>,
    snapshot: Snapshot,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    base_manifest_blake3: String,
    next_id: u64,
    overrides: BTreeMap<String, Record>,
    tombstones: BTreeSet<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    id: u64,
    mode: u32,
    content: RecordContent,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum RecordContent {
    Directory,
    Base { source: String },
    Data,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    format: String,
    version: u32,
    snapshot: Snapshot,
    snapshot_blake3: String,
}

fn io(kind: std::io::ErrorKind, message: impl Into<String>) -> Error {
    std::io::Error::new(kind, message.into()).into()
}

fn checked_path(path: &str, root: bool) -> Result<()> {
    if (root && path.is_empty()) || valid_path(path) {
        return Ok(());
    }
    Err(io(
        std::io::ErrorKind::InvalidInput,
        "expected normalized relative UTF-8 path",
    ))
}

fn checked_size(size: u64) -> Result<()> {
    if size > MAX_FILE_SIZE {
        return Err(io(
            std::io::ErrorKind::InvalidInput,
            "file exceeds portable signed 64-bit size",
        ));
    }
    Ok(())
}

fn checked_mode(mode: u32) -> Result<()> {
    if mode & !0o777 != 0 {
        return Err(io(
            std::io::ErrorKind::InvalidInput,
            "unsupported permission mode",
        ));
    }
    Ok(())
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn data_name(id: u64) -> String {
    format!("{id:016x}.data")
}

fn node(id: u64, path: &str, mode: u32, content: Content) -> Arc<Node> {
    Arc::new(Node {
        id,
        path: RwLock::new(path.into()),
        mode: AtomicU32::new(mode),
        content: RwLock::new(content),
    })
}

impl Overlay {
    pub fn root(&self) -> &Path {
        &self.inner.root.path
    }

    pub fn open(base: Arc<RangeResolver>, root: &Path) -> Result<Self> {
        storage::reject_links(root)?;
        // Resolve the nearest existing ancestor before creating any storage.
        // This rejects overlays inside the base and bases inside the overlay.
        let projected = projected_path(root)?;
        let base_root = std::fs::canonicalize(base.store().root())?;
        if projected.starts_with(&base_root) || base_root.starts_with(&projected) {
            return Err(io(
                std::io::ErrorKind::InvalidInput,
                "overlay and immutable store must not overlap",
            ));
        }
        let root = Directory::open(root, true)?;
        check_root_names(&root)?;
        let lock = lock_directory(&root, true)?;
        let data = root.child("data", true)?;
        let base_digest =
            blake3::hash(&serde_json::to_vec(base.manifest()).map_err(invalid_json)?).to_string();
        let mut nodes = BTreeMap::new();
        nodes.insert(String::new(), node(1, "", 0o755, Content::Directory));
        let mut next_id = 2u64;
        for path in &base.manifest().directories {
            nodes.insert(path.clone(), node(next_id, path, 0o755, Content::Directory));
            next_id = next_id
                .checked_add(1)
                .ok_or_else(|| Error::Invalid("inode overflow".into()))?;
        }
        for file in &base.manifest().files {
            checked_size(file.size)?;
            nodes.insert(
                file.path.clone(),
                node(
                    next_id,
                    &file.path,
                    file.mode,
                    Content::Base {
                        source: file.path.clone(),
                        size: file.size,
                    },
                ),
            );
            next_id = next_id
                .checked_add(1)
                .ok_or_else(|| Error::Invalid("inode overflow".into()))?;
        }
        let base_nodes = nodes.clone();
        let snapshot = match load_snapshot(&root) {
            Ok(snapshot) => snapshot,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Snapshot {
                base_manifest_blake3: base_digest.clone(),
                next_id,
                overrides: BTreeMap::new(),
                tombstones: BTreeSet::new(),
            },
            Err(error) => return Err(error),
        };
        if snapshot.base_manifest_blake3 != base_digest || snapshot.next_id < next_id {
            return Err(Error::Corrupt(
                "overlay belongs to another base manifest or has invalid inode allocator".into(),
            ));
        }
        for path in &snapshot.tombstones {
            nodes.remove(path);
        }
        for (path, record) in &snapshot.overrides {
            let content = match &record.content {
                RecordContent::Directory => Content::Directory,
                RecordContent::Base { source } => {
                    let source_node = base_nodes
                        .get(source)
                        .ok_or_else(|| Error::Corrupt("unknown overlay base reference".into()))?;
                    let Content::Base { size, .. } = &*source_node.content.read() else {
                        return Err(Error::Corrupt(
                            "overlay base reference points to directory".into(),
                        ));
                    };
                    if record.id != source_node.id {
                        return Err(Error::Corrupt(
                            "base reference inode identity mismatch".into(),
                        ));
                    }
                    Content::Base {
                        source: source.clone(),
                        size: *size,
                    }
                }
                RecordContent::Data => {
                    let file = data.open_file(&data_name(record.id), false, true)?;
                    checked_size(file.metadata()?.len())?;
                    Content::Data(Arc::new(file))
                }
            };
            if record.id < next_id {
                let original = base_nodes
                    .values()
                    .find(|node| node.id == record.id)
                    .ok_or_else(|| Error::Corrupt("unknown base inode".into()))?;
                if matches!(&*original.content.read(), Content::Directory)
                    != matches!(content, Content::Directory)
                {
                    return Err(Error::Corrupt("base inode changed file type".into()));
                }
            }
            nodes.insert(path.clone(), node(record.id, path, record.mode, content));
        }
        validate_tree(&nodes)?;
        let ids = nodes.values().map(|node| (node.id, node.clone())).collect();
        let namespace = Namespace {
            nodes,
            ids,
            snapshot,
        };
        let overlay = Self {
            inner: Arc::new(Inner {
                base,
                root,
                data,
                _lock: lock,
                namespace: RwLock::new(namespace),
                mutations: Mutex::new(()),
                copy_up_files: AtomicU64::new(0),
                copy_up_bytes: AtomicU64::new(0),
                bytes_written: AtomicU64::new(0),
                temp_sequence: AtomicU64::new(0),
            }),
        };
        // Initial state is always published before returning. Orphan collection
        // happens only after every referenced data object has been validated.
        {
            let _mutation = overlay.inner.mutations.lock();
            let candidate = overlay.inner.namespace.read().clone();
            overlay.collect_orphans()?;
            overlay.install(candidate, || {})?;
        }
        Ok(overlay)
    }

    pub fn metadata(&self, path: &str) -> Result<Entry> {
        checked_path(path, true)?;
        let node = self.lookup(path)?;
        entry(&node)
    }

    pub fn metadata_id(&self, id: u64) -> Result<Entry> {
        let node = self
            .inner
            .namespace
            .read()
            .ids
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("inode {id}")))?;
        entry(&node)
    }

    pub fn metadata_handle(&self, handle: &Handle) -> Result<Entry> {
        self.check_handle(handle)?;
        entry(&handle.node)
    }

    pub fn handle_entry(&self, handle: &Handle) -> Result<Entry> {
        self.metadata_handle(handle)
    }

    pub fn entries(&self) -> Result<Vec<Entry>> {
        let nodes: Vec<_> = self
            .inner
            .namespace
            .read()
            .nodes
            .values()
            .cloned()
            .collect();
        nodes.iter().map(|node| entry(node)).collect()
    }

    pub fn list(&self, path: &str) -> Result<Vec<Entry>> {
        checked_path(path, true)?;
        let node = self.lookup(path)?;
        if !matches!(&*node.content.read(), Content::Directory) {
            return Err(io(
                std::io::ErrorKind::NotADirectory,
                "list requires a directory",
            ));
        }
        let nodes: Vec<_> = self
            .inner
            .namespace
            .read()
            .nodes
            .iter()
            .filter(|(child, _)| !child.is_empty() && parent(child) == path)
            .map(|(_, node)| node.clone())
            .collect();
        nodes.iter().map(|node| entry(node)).collect()
    }

    pub fn list_handle(&self, handle: &Handle) -> Result<Vec<Entry>> {
        self.check_handle(handle)?;
        if !matches!(&*handle.node.content.read(), Content::Directory) {
            return Err(io(
                std::io::ErrorKind::NotADirectory,
                "handle is not a directory",
            ));
        }
        let namespace = self.inner.namespace.read();
        let Some(path) = namespace
            .nodes
            .iter()
            .find_map(|(path, node)| Arc::ptr_eq(node, &handle.node).then_some(path))
        else {
            return Ok(Vec::new());
        };
        let children: Vec<_> = namespace
            .nodes
            .iter()
            .filter(|(child, _)| !child.is_empty() && parent(child) == path)
            .map(|(_, node)| node.clone())
            .collect();
        drop(namespace);
        children.iter().map(|node| entry(node)).collect()
    }

    pub fn open_dir(&self, path: &str) -> Result<Handle> {
        checked_path(path, true)?;
        let node = self.lookup(path)?;
        if !matches!(&*node.content.read(), Content::Directory) {
            return Err(io(
                std::io::ErrorKind::NotADirectory,
                "open_dir requires a directory",
            ));
        }
        Ok(Handle {
            inner: self.inner.clone(),
            node,
            writable: false,
        })
    }

    pub fn open_file(&self, path: &str, write: bool, truncate: bool) -> Result<Handle> {
        checked_path(path, false)?;
        let node = self.lookup(path)?;
        self.open_node(node, write, truncate)
    }

    pub fn open_file_id(&self, id: u64, write: bool, truncate: bool) -> Result<Handle> {
        let node = self.lookup_id(id)?;
        self.open_node(node, write, truncate)
    }

    pub fn open_dir_id(&self, id: u64) -> Result<Handle> {
        let node = self.lookup_id(id)?;
        if !matches!(&*node.content.read(), Content::Directory) {
            return Err(io(
                std::io::ErrorKind::NotADirectory,
                "inode is not a directory",
            ));
        }
        Ok(Handle {
            inner: self.inner.clone(),
            node,
            writable: false,
        })
    }

    fn open_node(&self, node: Arc<Node>, write: bool, truncate: bool) -> Result<Handle> {
        if matches!(&*node.content.read(), Content::Directory) {
            return Err(io(
                std::io::ErrorKind::IsADirectory,
                "open_file requires a file",
            ));
        }
        let handle = Handle {
            inner: self.inner.clone(),
            node,
            writable: write,
        };
        if truncate {
            self.truncate(&handle, 0)?;
        }
        Ok(handle)
    }

    pub fn create(&self, path: &str, mode: u32, exclusive: bool) -> Result<Handle> {
        let start = Instant::now();
        let result = self.create_inner(path, mode, exclusive);
        self.record("create", path, 0, 0, 0, start, result.is_ok());
        result
    }

    fn create_inner(&self, path: &str, mode: u32, exclusive: bool) -> Result<Handle> {
        checked_path(path, false)?;
        checked_mode(mode)?;
        let _mutation = self.inner.mutations.lock();
        let mut candidate = self.inner.namespace.read().clone();
        if let Some(existing) = candidate.nodes.get(path) {
            if exclusive {
                return Err(io(std::io::ErrorKind::AlreadyExists, "file already exists"));
            }
            if matches!(&*existing.content.read(), Content::Directory) {
                return Err(io(
                    std::io::ErrorKind::IsADirectory,
                    "existing path is a directory",
                ));
            }
            return Ok(Handle {
                inner: self.inner.clone(),
                node: existing.clone(),
                writable: true,
            });
        }
        require_parent(&candidate, path)?;
        let id = allocate_id(&mut candidate.snapshot)?;
        let name = data_name(id);
        let file = self.fresh_data(id)?;
        let initialized = file
            .sync_all()
            .map_err(Error::from)
            .and_then(|()| self.inner.data.sync());
        if let Err(error) = initialized {
            drop(file);
            let _ = self.inner.data.remove(&name);
            return Err(error);
        }
        let node = node(id, path, mode, Content::Data(Arc::new(file)));
        candidate.nodes.insert(path.into(), node.clone());
        candidate.ids.insert(id, node.clone());
        candidate.snapshot.tombstones.remove(path);
        candidate.snapshot.overrides.insert(
            path.into(),
            Record {
                id,
                mode,
                content: RecordContent::Data,
            },
        );
        let result = self.install(candidate, || {});
        if result.is_err() && !self.inner.namespace.read().ids.contains_key(&id) {
            let _ = self.inner.data.remove(&name);
        }
        result?;
        Ok(Handle {
            inner: self.inner.clone(),
            node,
            writable: true,
        })
    }

    pub fn read(&self, handle: &Handle, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.check_handle(handle)?;
        if len > MAX_READ_BYTES {
            return Err(Error::ReadTooLarge);
        }
        let start = Instant::now();
        let content = handle.node.content.read();
        match &*content {
            Content::Base { source, .. } => {
                self.inner
                    .base
                    .read_range_as(source, &handle.node.path.read(), offset, len)
            }
            Content::Directory => Err(io(
                std::io::ErrorKind::IsADirectory,
                "cannot read a directory",
            )),
            Content::Data(file) => {
                let result = (|| {
                    let size = file.metadata()?.len();
                    let size = offset
                        .saturating_add(len as u64)
                        .min(size)
                        .saturating_sub(offset);
                    let mut bytes = vec![0; size as usize];
                    read_exact_at(file, offset, &mut bytes)?;
                    Ok(bytes)
                })();
                let path = handle.node.path.read().clone();
                self.record(
                    "read",
                    &path,
                    offset,
                    len as u64,
                    result
                        .as_ref()
                        .map_or(0, |bytes: &Vec<u8>| bytes.len() as u64),
                    start,
                    result.is_ok(),
                );
                result
            }
        }
    }

    pub fn write(&self, handle: &Handle, offset: u64, bytes: &[u8], append: bool) -> Result<usize> {
        let start = Instant::now();
        let result = self.write_inner(handle, offset, bytes, append);
        let path = handle.node.path.read().clone();
        self.record(
            "write",
            &path,
            offset,
            bytes.len() as u64,
            result.as_ref().copied().unwrap_or(0) as u64,
            start,
            result.is_ok(),
        );
        result
    }

    fn write_inner(
        &self,
        handle: &Handle,
        offset: u64,
        bytes: &[u8],
        append: bool,
    ) -> Result<usize> {
        self.check_writable(handle)?;
        if !append {
            checked_size(
                offset
                    .checked_add(bytes.len() as u64)
                    .ok_or_else(|| io(std::io::ErrorKind::InvalidInput, "write offset overflow"))?,
            )?;
        }
        if bytes.is_empty() {
            return Ok(0);
        }
        self.materialize(handle, false)?;
        let content = handle.node.content.write();
        let Content::Data(file) = &*content else {
            return Err(io(
                std::io::ErrorKind::IsADirectory,
                "cannot write a directory",
            ));
        };
        let offset = if append {
            file.metadata()?.len()
        } else {
            offset
        };
        checked_size(
            offset
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| io(std::io::ErrorKind::InvalidInput, "append offset overflow"))?,
        )?;
        let count = write_at(file, offset, bytes)?;
        self.inner
            .bytes_written
            .fetch_add(count as u64, Ordering::Relaxed);
        Ok(count)
    }

    pub fn truncate(&self, handle: &Handle, size: u64) -> Result<()> {
        let start = Instant::now();
        let result = (|| {
            self.check_writable(handle)?;
            checked_size(size)?;
            self.materialize(handle, size == 0)?;
            let content = handle.node.content.write();
            let Content::Data(file) = &*content else {
                return Err(io(
                    std::io::ErrorKind::IsADirectory,
                    "cannot truncate a directory",
                ));
            };
            file.set_len(size)?;
            Ok(())
        })();
        let path = handle.node.path.read().clone();
        self.record("truncate", &path, size, 0, 0, start, result.is_ok());
        result
    }

    pub fn flush(&self, handle: &Handle) -> Result<()> {
        self.check_handle(handle)?;
        let start = Instant::now();
        let result = (|| {
            let content = handle.node.content.read();
            if let Content::Data(file) = &*content {
                file.sync_all()?;
            }
            self.inner.data.sync()?;
            self.inner.root.sync()?;
            Ok(())
        })();
        let path = handle.node.path.read().clone();
        self.record("flush", &path, 0, 0, 0, start, result.is_ok());
        result
    }

    pub fn sync(&self) -> Result<()> {
        let nodes: Vec<_> = self
            .inner
            .namespace
            .read()
            .nodes
            .values()
            .cloned()
            .collect();
        for node in nodes {
            let content = node.content.read();
            if let Content::Data(file) = &*content {
                file.sync_all()?;
            }
        }
        self.inner.data.sync()?;
        self.inner.root.sync()?;
        Ok(())
    }

    pub fn flush_all(&self) -> Result<()> {
        self.sync()
    }

    pub fn mkdir(&self, path: &str, mode: u32) -> Result<()> {
        let start = Instant::now();
        let result = (|| {
            checked_path(path, false)?;
            checked_mode(mode)?;
            let _mutation = self.inner.mutations.lock();
            let mut candidate = self.inner.namespace.read().clone();
            if candidate.nodes.contains_key(path) {
                return Err(io(
                    std::io::ErrorKind::AlreadyExists,
                    "directory already exists",
                ));
            }
            require_parent(&candidate, path)?;
            let id = allocate_id(&mut candidate.snapshot)?;
            let node = node(id, path, mode, Content::Directory);
            candidate.nodes.insert(path.into(), node.clone());
            candidate.ids.insert(id, node);
            candidate.snapshot.tombstones.remove(path);
            candidate.snapshot.overrides.insert(
                path.into(),
                Record {
                    id,
                    mode,
                    content: RecordContent::Directory,
                },
            );
            self.install(candidate, || {})
        })();
        self.record("mkdir", path, 0, 0, 0, start, result.is_ok());
        result
    }

    pub fn unlink(&self, path: &str) -> Result<()> {
        self.remove(path, false)
    }

    pub fn rmdir(&self, path: &str) -> Result<()> {
        self.remove(path, true)
    }

    fn remove(&self, path: &str, directory: bool) -> Result<()> {
        let start = Instant::now();
        let result = (|| {
            checked_path(path, false)?;
            let _mutation = self.inner.mutations.lock();
            let mut candidate = self.inner.namespace.read().clone();
            let node = candidate
                .nodes
                .get(path)
                .cloned()
                .ok_or_else(|| Error::NotFound(path.into()))?;
            let is_dir = matches!(&*node.content.read(), Content::Directory);
            if directory && !is_dir {
                return Err(io(
                    std::io::ErrorKind::NotADirectory,
                    "rmdir requires a directory",
                ));
            }
            if !directory && is_dir {
                return Err(io(
                    std::io::ErrorKind::IsADirectory,
                    "unlink requires a file",
                ));
            }
            if directory
                && candidate
                    .nodes
                    .keys()
                    .any(|child| is_descendant(child, path))
            {
                return Err(io(
                    std::io::ErrorKind::DirectoryNotEmpty,
                    "directory is not empty",
                ));
            }
            candidate.nodes.remove(path);
            candidate.ids.remove(&node.id);
            candidate.snapshot.overrides.remove(path);
            candidate.snapshot.tombstones.insert(path.into());
            self.install(candidate, || {})?;
            // Open handles retain the node and OS file descriptor. Windows
            // sharing modes also permit deletion while such handles remain.
            if matches!(&*node.content.read(), Content::Data(_)) {
                let _ = self.inner.data.remove(&data_name(node.id));
            }
            Ok(())
        })();
        self.record(
            if directory { "rmdir" } else { "unlink" },
            path,
            0,
            0,
            0,
            start,
            result.is_ok(),
        );
        result
    }

    pub fn rename(&self, from: &str, to: &str, replace: bool) -> Result<()> {
        let start = Instant::now();
        let result = self.rename_inner(from, to, replace);
        self.record("rename", from, 0, 0, 0, start, result.is_ok());
        result
    }

    fn rename_inner(&self, from: &str, to: &str, replace: bool) -> Result<()> {
        checked_path(from, false)?;
        checked_path(to, false)?;
        let _mutation = self.inner.mutations.lock();
        let mut candidate = self.inner.namespace.read().clone();
        let source = candidate
            .nodes
            .get(from)
            .cloned()
            .ok_or_else(|| Error::NotFound(from.into()))?;
        if from == to {
            return Ok(());
        }
        if is_descendant(to, from) {
            return Err(io(
                std::io::ErrorKind::InvalidInput,
                "cannot rename a directory into its descendant",
            ));
        }
        require_parent(&candidate, to)?;
        let is_directory = matches!(&*source.content.read(), Content::Directory);
        let destination = candidate.nodes.get(to).cloned();
        if let Some(destination) = &destination {
            if !replace {
                return Err(io(
                    std::io::ErrorKind::AlreadyExists,
                    "rename destination exists",
                ));
            }
            let dest_directory = matches!(&*destination.content.read(), Content::Directory);
            if is_directory && !dest_directory {
                return Err(io(
                    std::io::ErrorKind::NotADirectory,
                    "directory destination is a file",
                ));
            }
            if !is_directory && dest_directory {
                return Err(io(
                    std::io::ErrorKind::IsADirectory,
                    "file destination is a directory",
                ));
            }
            if dest_directory && candidate.nodes.keys().any(|child| is_descendant(child, to)) {
                return Err(io(
                    std::io::ErrorKind::DirectoryNotEmpty,
                    "rename destination directory is not empty",
                ));
            }
            candidate.nodes.remove(to);
            candidate.ids.remove(&destination.id);
            candidate.snapshot.overrides.remove(to);
        }
        let moving: Vec<_> = candidate
            .nodes
            .iter()
            .filter(|(path, _)| {
                path.as_str() == from || (is_directory && is_descendant(path, from))
            })
            .map(|(path, node)| (path.clone(), node.clone()))
            .collect();
        let mut updates = Vec::new();
        for (old_path, node) in moving {
            let new_path = format!("{to}{}", &old_path[from.len()..]);
            let record = record_for(&node);
            candidate.nodes.remove(&old_path);
            candidate.snapshot.overrides.remove(&old_path);
            candidate.snapshot.tombstones.insert(old_path);
            candidate.snapshot.tombstones.remove(&new_path);
            candidate
                .snapshot
                .overrides
                .insert(new_path.clone(), record);
            candidate.nodes.insert(new_path.clone(), node.clone());
            updates.push((node, new_path));
        }
        self.install(candidate, || {
            for (node, path) in updates {
                *node.path.write() = path;
            }
        })?;
        if let Some(destination) = destination
            && matches!(&*destination.content.read(), Content::Data(_))
        {
            let _ = self.inner.data.remove(&data_name(destination.id));
        }
        Ok(())
    }

    pub fn set_mode(&self, path: &str, mode: u32) -> Result<()> {
        checked_path(path, true)?;
        checked_mode(mode)?;
        if path.is_empty() {
            return Err(io(
                std::io::ErrorKind::Unsupported,
                "changing root mode is unsupported",
            ));
        }
        let _mutation = self.inner.mutations.lock();
        let mut candidate = self.inner.namespace.read().clone();
        let node = candidate
            .nodes
            .get(path)
            .cloned()
            .ok_or_else(|| Error::NotFound(path.into()))?;
        let mut record = record_for(&node);
        record.mode = mode;
        candidate.snapshot.overrides.insert(path.into(), record);
        self.install(candidate, || {
            node.mode.store(mode, Ordering::Relaxed);
        })
    }

    pub fn set_mode_handle(&self, handle: &Handle, mode: u32) -> Result<()> {
        self.check_handle(handle)?;
        checked_mode(mode)?;
        if handle.node.id == 1 {
            return Err(io(
                std::io::ErrorKind::Unsupported,
                "changing root mode is unsupported",
            ));
        }
        let _mutation = self.inner.mutations.lock();
        let mut candidate = self.inner.namespace.read().clone();
        let path = candidate
            .nodes
            .iter()
            .find_map(|(path, node)| Arc::ptr_eq(node, &handle.node).then_some(path.clone()));
        let Some(path) = path else {
            handle.node.mode.store(mode, Ordering::Relaxed);
            return Ok(());
        };
        let mut record = record_for(&handle.node);
        record.mode = mode;
        candidate.snapshot.overrides.insert(path, record);
        self.install(candidate, || {
            handle.node.mode.store(mode, Ordering::Relaxed);
        })
    }

    pub fn set_mode_id(&self, id: u64, mode: u32) -> Result<()> {
        let node = self.lookup_id(id)?;
        self.set_mode_handle(
            &Handle {
                inner: self.inner.clone(),
                node,
                writable: false,
            },
            mode,
        )
    }

    pub fn metrics(&self) -> OverlayMetrics {
        let snapshot = self.inner.namespace.read().snapshot.clone();
        let mut metrics = snapshot_metrics(&self.inner.root, &self.inner.data, &snapshot)
            .unwrap_or_else(|_| OverlayMetrics {
                overlay_files: snapshot
                    .overrides
                    .values()
                    .filter(|record| matches!(record.content, RecordContent::Data))
                    .count() as u64,
                tombstones: snapshot.tombstones.len() as u64,
                ..Default::default()
            });
        metrics.copy_up_files = self.inner.copy_up_files.load(Ordering::Relaxed);
        metrics.copy_up_bytes = self.inner.copy_up_bytes.load(Ordering::Relaxed);
        metrics.overlay_bytes_written = self.inner.bytes_written.load(Ordering::Relaxed);
        metrics
    }

    pub fn status(root: &Path) -> Result<OverlayMetrics> {
        let root = Directory::open(root, false)?;
        check_root_names(&root)?;
        let _lock = lock_directory(&root, false)?;
        let snapshot = load_snapshot(&root)?;
        let data = root.child("data", false)?;
        validate_data_names(&data)?;
        snapshot_metrics(&root, &data, &snapshot)
    }

    /// Atomically reset only a recognized, inactive overlay. Its lock inode and
    /// base binding remain in place, so an interrupted discard remounts cleanly.
    pub fn discard(root: &Path) -> Result<()> {
        let root = Directory::open(root, false)?;
        check_root_names(&root)?;
        let _lock = lock_directory(&root, false)?;
        let mut snapshot = load_snapshot(&root)?;
        let data = root.child("data", false)?;
        validate_data_names(&data)?;
        // Validate references before changing anything, including symlink bans.
        snapshot_metrics(&root, &data, &snapshot)?;
        snapshot.overrides.clear();
        snapshot.tombstones.clear();
        let bytes = encode_snapshot(&snapshot)?;
        let temp = temp_name(0);
        root.write_new(&temp, &bytes)?;
        root.rename(&temp, STATE)?;
        root.sync()?;
        for name in data.names()? {
            data.remove(&name)?;
        }
        data.sync()?;
        for name in root.names()? {
            if temporary_name(&name) {
                root.remove(&name)?;
            }
        }
        root.sync()?;
        Ok(())
    }

    fn lookup(&self, path: &str) -> Result<Arc<Node>> {
        self.inner
            .namespace
            .read()
            .nodes
            .get(path)
            .cloned()
            .ok_or_else(|| Error::NotFound(path.into()))
    }

    fn lookup_id(&self, id: u64) -> Result<Arc<Node>> {
        self.inner
            .namespace
            .read()
            .ids
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("inode {id}")))
    }

    // Called under mutations lock, with an inode that is not currently a data
    // node. A failed earlier transaction may have left this exact opaque file.
    fn fresh_data(&self, id: u64) -> Result<File> {
        let name = data_name(id);
        match self.inner.data.open_file(&name, true, true) {
            Ok(file) => Ok(file),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                self.inner.data.open_file(&name, false, false)?;
                self.inner.data.remove(&name)?;
                self.inner.data.open_file(&name, true, true)
            }
            Err(error) => Err(error),
        }
    }

    fn check_handle(&self, handle: &Handle) -> Result<()> {
        if !Arc::ptr_eq(&self.inner, &handle.inner) {
            return Err(io(
                std::io::ErrorKind::InvalidInput,
                "handle belongs to another overlay",
            ));
        }
        Ok(())
    }

    fn check_writable(&self, handle: &Handle) -> Result<()> {
        self.check_handle(handle)?;
        if !handle.writable {
            return Err(io(
                std::io::ErrorKind::PermissionDenied,
                "handle was opened read-only",
            ));
        }
        Ok(())
    }

    fn materialize(&self, handle: &Handle, discard_base: bool) -> Result<()> {
        if matches!(&*handle.node.content.read(), Content::Data(_)) {
            return Ok(());
        }
        let _mutation = self.inner.mutations.lock();
        let mut content = handle.node.content.write();
        let Content::Base { source, size } = &*content else {
            if matches!(&*content, Content::Data(_)) {
                return Ok(());
            }
            return Err(io(
                std::io::ErrorKind::IsADirectory,
                "cannot copy up a directory",
            ));
        };
        let name = data_name(handle.node.id);
        let mut file = self.fresh_data(handle.node.id)?;
        let copied = if discard_base { 0 } else { *size };
        let copy_result = (|| {
            let mut offset = 0u64;
            while offset < copied {
                let count = (copied - offset).min(COPY_BUFFER as u64) as usize;
                let bytes = self.inner.base.read_range_untraced(source, offset, count)?;
                if bytes.len() != count {
                    return Err(Error::Corrupt("short immutable file during copy-up".into()));
                }
                file.write_all(&bytes)?;
                offset += bytes.len() as u64;
            }
            file.sync_all()?;
            self.inner.data.sync()?;
            Ok(())
        })();
        if let Err(error) = copy_result {
            drop(file);
            let _ = self.inner.data.remove(&name);
            return Err(error);
        }
        let file = Arc::new(file);
        let mut candidate = self.inner.namespace.read().clone();
        let visible_path = candidate
            .nodes
            .iter()
            .find_map(|(path, node)| Arc::ptr_eq(node, &handle.node).then_some(path.clone()));
        let result = if let Some(path) = visible_path {
            candidate.snapshot.overrides.insert(
                path,
                Record {
                    id: handle.node.id,
                    mode: handle.node.mode.load(Ordering::Relaxed),
                    content: RecordContent::Data,
                },
            );
            self.install(candidate, || {
                *content = Content::Data(file.clone());
            })
        } else {
            // Unlinked handles are still writable but are intentionally absent
            // from the persistent namespace. The next open collects their data.
            *content = Content::Data(file);
            Ok(())
        };
        let installed = matches!(&*content, Content::Data(_));
        if installed {
            self.inner.copy_up_files.fetch_add(1, Ordering::Relaxed);
            self.inner
                .copy_up_bytes
                .fetch_add(copied, Ordering::Relaxed);
        } else {
            let _ = self.inner.data.remove(&name);
        }
        result
    }

    // mutations mutex must be held. A failure after rename still updates the
    // in-memory namespace, then reports the durability error to the caller.
    fn install(&self, candidate: Namespace, apply: impl FnOnce()) -> Result<()> {
        let bytes = encode_snapshot(&candidate.snapshot)?;
        let temp = temp_name(self.inner.temp_sequence.fetch_add(1, Ordering::Relaxed));
        if let Err(error) = self.inner.root.write_new(&temp, &bytes) {
            let _ = self.inner.root.remove(&temp);
            return Err(error);
        }
        if let Err(error) = self.inner.root.rename(&temp, STATE) {
            let _ = self.inner.root.remove(&temp);
            return Err(error);
        }
        {
            let mut namespace = self.inner.namespace.write();
            *namespace = candidate;
            apply();
        }
        self.inner.root.sync()
    }

    fn collect_orphans(&self) -> Result<()> {
        validate_data_names(&self.inner.data)?;
        let expected: BTreeSet<_> = self
            .inner
            .namespace
            .read()
            .snapshot
            .overrides
            .values()
            .filter(|record| matches!(record.content, RecordContent::Data))
            .map(|record| data_name(record.id))
            .collect();
        for name in self.inner.data.names()? {
            if !expected.contains(&name) {
                self.inner.data.remove(&name)?;
            }
        }
        for name in self.inner.root.names()? {
            if temporary_name(&name) {
                self.inner.root.open_file(&name, false, false)?;
                self.inner.root.remove(&name)?;
            }
        }
        self.inner.data.sync()?;
        self.inner.root.sync()
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "matches the shared trace event API"
    )]
    fn record(
        &self,
        operation: &str,
        path: &str,
        offset: u64,
        requested: u64,
        returned: u64,
        start: Instant,
        success: bool,
    ) {
        if let Some(trace) = self.inner.base.trace() {
            trace.record(
                operation,
                path,
                offset,
                requested,
                returned,
                start.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                "bypass",
                "overlay",
                success,
            );
        }
    }
}

fn invalid_json(error: serde_json::Error) -> Error {
    Error::Corrupt(format!("invalid overlay metadata: {error}"))
}

fn entry(node: &Node) -> Result<Entry> {
    let content = node.content.read();
    let (size, is_dir) = match &*content {
        Content::Directory => (0, true),
        Content::Base { size, .. } => (*size, false),
        Content::Data(file) => (file.metadata()?.len(), false),
    };
    Ok(Entry {
        path: node.path.read().clone(),
        id: node.id,
        size,
        mode: node.mode.load(Ordering::Relaxed),
        is_dir,
    })
}

fn record_for(node: &Node) -> Record {
    let content = match &*node.content.read() {
        Content::Directory => RecordContent::Directory,
        Content::Base { source, .. } => RecordContent::Base {
            source: source.clone(),
        },
        Content::Data(_) => RecordContent::Data,
    };
    Record {
        id: node.id,
        mode: node.mode.load(Ordering::Relaxed),
        content,
    }
}

fn is_descendant(path: &str, directory: &str) -> bool {
    path.strip_prefix(directory)
        .is_some_and(|tail| tail.starts_with('/'))
}

fn allocate_id(snapshot: &mut Snapshot) -> Result<u64> {
    let id = snapshot.next_id;
    snapshot.next_id = id.checked_add(1).ok_or_else(|| {
        io(
            std::io::ErrorKind::Other,
            "overlay inode allocator exhausted",
        )
    })?;
    Ok(id)
}

fn require_parent(namespace: &Namespace, path: &str) -> Result<()> {
    let parent = namespace
        .nodes
        .get(parent(path))
        .ok_or_else(|| Error::NotFound(parent(path).into()))?;
    if !matches!(&*parent.content.read(), Content::Directory) {
        return Err(io(
            std::io::ErrorKind::NotADirectory,
            "parent is not a directory",
        ));
    }
    Ok(())
}

fn validate_tree(nodes: &BTreeMap<String, Arc<Node>>) -> Result<()> {
    let mut ids = BTreeSet::new();
    for (path, node) in nodes {
        if !ids.insert(node.id) {
            return Err(Error::Corrupt("duplicate visible inode identity".into()));
        }
        if !path.is_empty() {
            let parent = nodes.get(parent(path)).ok_or_else(|| {
                Error::Corrupt("overlay entry has missing directory parent".into())
            })?;
            if !matches!(&*parent.content.read(), Content::Directory) {
                return Err(Error::Corrupt("overlay entry parent is a file".into()));
            }
        }
    }
    Ok(())
}

fn validate_snapshot(snapshot: &Snapshot) -> Result<()> {
    playsparse_core::parse_hash(&snapshot.base_manifest_blake3)
        .map_err(|_| Error::Corrupt("invalid overlay base identity".into()))?;
    if snapshot.next_id < 2 {
        return Err(Error::Corrupt("invalid overlay inode allocator".into()));
    }
    let mut ids = BTreeSet::new();
    for (path, record) in &snapshot.overrides {
        if !valid_path(path)
            || record.id < 2
            || record.id >= snapshot.next_id
            || record.mode & !0o777 != 0
            || !ids.insert(record.id)
            || snapshot.tombstones.contains(path)
        {
            return Err(Error::Corrupt(
                "invalid/duplicate overlay path, mode, tombstone, or inode".into(),
            ));
        }
        if let RecordContent::Base { source } = &record.content
            && !valid_path(source)
        {
            return Err(Error::Corrupt(
                "invalid immutable base path reference".into(),
            ));
        }
    }
    if snapshot.tombstones.iter().any(|path| !valid_path(path)) {
        return Err(Error::Corrupt("invalid tombstone path".into()));
    }
    Ok(())
}

fn load_snapshot(root: &Directory) -> Result<Snapshot> {
    let bytes = root.read(STATE, MAX_METADATA_BYTES)?;
    let envelope: Envelope = serde_json::from_slice(&bytes).map_err(invalid_json)?;
    if envelope.format != FORMAT || envelope.version != VERSION {
        return Err(Error::Corrupt("unsupported overlay format/version".into()));
    }
    let bytes = serde_json::to_vec(&envelope.snapshot).map_err(invalid_json)?;
    if blake3::hash(&bytes).to_hex().as_str() != envelope.snapshot_blake3 {
        return Err(Error::Corrupt("overlay metadata digest mismatch".into()));
    }
    validate_snapshot(&envelope.snapshot)?;
    Ok(envelope.snapshot)
}

fn encode_snapshot(snapshot: &Snapshot) -> Result<Vec<u8>> {
    validate_snapshot(snapshot)?;
    let digest = blake3::hash(&serde_json::to_vec(snapshot).map_err(invalid_json)?).to_string();
    let bytes = serde_json::to_vec(&Envelope {
        format: FORMAT.into(),
        version: VERSION,
        snapshot: snapshot.clone(),
        snapshot_blake3: digest,
    })
    .map_err(invalid_json)?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        return Err(io(
            std::io::ErrorKind::OutOfMemory,
            "overlay namespace exceeds metadata limit",
        ));
    }
    Ok(bytes)
}

fn temp_name(sequence: u64) -> String {
    format!(".metadata-{:08x}-{sequence:016x}.tmp", std::process::id())
}

fn temporary_name(name: &str) -> bool {
    let Some(body) = name
        .strip_prefix(".metadata-")
        .and_then(|body| body.strip_suffix(".tmp"))
    else {
        return false;
    };
    let Some((pid, sequence)) = body.split_once('-') else {
        return false;
    };
    pid.len() == 8
        && sequence.len() == 16
        && pid
            .bytes()
            .chain(sequence.bytes())
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn object_name(name: &str) -> bool {
    name.strip_suffix(".data").is_some_and(|id| {
        id.len() == 16
            && id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn check_root_names(root: &Directory) -> Result<()> {
    for name in root.names()? {
        if name != STATE && name != LOCK && name != "data" && !temporary_name(&name) {
            return Err(io(
                std::io::ErrorKind::PermissionDenied,
                "overlay root contains unrelated files",
            ));
        }
    }
    Ok(())
}

fn validate_data_names(data: &Directory) -> Result<()> {
    for name in data.names()? {
        if !object_name(&name) {
            return Err(io(
                std::io::ErrorKind::PermissionDenied,
                "overlay data contains unrelated files",
            ));
        }
        data.open_file(&name, false, false)?;
    }
    Ok(())
}

fn lock_directory(root: &Directory, create: bool) -> Result<File> {
    let mut file = match root.open_file(LOCK, create, true) {
        Ok(mut file) if create => {
            file.write_all(LOCK_MARKER)?;
            file.sync_all()?;
            file
        }
        Ok(file) => file,
        Err(Error::Io(error)) if create && error.kind() == std::io::ErrorKind::AlreadyExists => {
            root.open_file(LOCK, false, true)?
        }
        Err(error) => return Err(error),
    };
    file.try_lock().map_err(|error| {
        io(
            std::io::ErrorKind::WouldBlock,
            format!("overlay is active or could not be locked: {error}"),
        )
    })?;
    let mut marker = Vec::new();
    Read::by_ref(&mut file)
        .take(4096)
        .read_to_end(&mut marker)?;
    // A just-created marker was written through this descriptor; rewind before
    // validating it. Existing markers start at offset zero.
    if marker.is_empty() && file.metadata()?.len() == LOCK_MARKER.len() as u64 {
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(0))?;
        file.read_to_end(&mut marker)?;
    }
    if marker != LOCK_MARKER {
        return Err(io(
            std::io::ErrorKind::PermissionDenied,
            "unrecognized overlay lock marker",
        ));
    }
    Ok(file)
}

fn snapshot_metrics(
    root: &Directory,
    data: &Directory,
    snapshot: &Snapshot,
) -> Result<OverlayMetrics> {
    let mut metrics = OverlayMetrics {
        tombstones: snapshot.tombstones.len() as u64,
        metadata_bytes: root.open_file(STATE, false, false)?.metadata()?.len(),
        overlay_allocated_bytes: if cfg!(unix) { Some(0) } else { None },
        ..Default::default()
    };
    for record in snapshot.overrides.values() {
        if matches!(record.content, RecordContent::Data) {
            let file = data.open_file(&data_name(record.id), false, false)?;
            metrics.overlay_files += 1;
            metrics.overlay_logical_bytes = metrics
                .overlay_logical_bytes
                .saturating_add(file.metadata()?.len());
            if let (Some(total), Some(allocated)) = (
                &mut metrics.overlay_allocated_bytes,
                storage::allocated(&file)?,
            ) {
                *total = total.saturating_add(allocated);
            }
        }
    }
    Ok(metrics)
}

fn projected_path(path: &Path) -> Result<std::path::PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut ancestor = absolute.as_path();
    let mut tail = Vec::new();
    while !ancestor.exists() {
        let name = ancestor
            .file_name()
            .ok_or_else(|| io(std::io::ErrorKind::InvalidInput, "invalid overlay root"))?;
        tail.push(name.to_os_string());
        ancestor = ancestor.parent().ok_or_else(|| {
            io(
                std::io::ErrorKind::InvalidInput,
                "overlay root has no existing ancestor",
            )
        })?;
    }
    let mut projected = std::fs::canonicalize(ancestor)?;
    for component in tail.into_iter().rev() {
        projected.push(component);
    }
    Ok(projected)
}

fn read_exact_at(file: &File, mut offset: u64, mut bytes: &mut [u8]) -> Result<()> {
    while !bytes.is_empty() {
        #[cfg(unix)]
        let result = {
            use std::os::unix::fs::FileExt;
            file.read_at(bytes, offset)
        };
        #[cfg(windows)]
        let result = {
            use std::os::windows::fs::FileExt;
            file.seek_read(bytes, offset)
        };
        let count = match result {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            return Err(io(
                std::io::ErrorKind::UnexpectedEof,
                "short overlay data read",
            ));
        }
        offset += count as u64;
        bytes = &mut bytes[count..];
    }
    Ok(())
}

fn write_at(file: &File, mut offset: u64, mut bytes: &[u8]) -> Result<usize> {
    let mut written = 0;
    while !bytes.is_empty() {
        #[cfg(unix)]
        let result = {
            use std::os::unix::fs::FileExt;
            file.write_at(bytes, offset)
        };
        #[cfg(windows)]
        let result = {
            use std::os::windows::fs::FileExt;
            file.seek_write(bytes, offset)
        };
        let count = match result {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) if written != 0 => return Ok(written),
            result => result?,
        };
        if count == 0 {
            if written != 0 {
                return Ok(written);
            }
            return Err(io(
                std::io::ErrorKind::WriteZero,
                "zero-length overlay data write",
            ));
        }
        offset += count as u64;
        bytes = &bytes[count..];
        written += count;
    }
    Ok(written)
}

#[cfg(test)]
mod tests;
