use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use fuser::{
    AccessFlags, Config, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation,
    INodeNo, LockOwner, MountOption, OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty,
    ReplyEntry, ReplyOpen, ReplyStatfs, ReplyXattr, Request,
};
use playsparse_core::{Error, Manifest};
use playsparse_range::{RangeResolver, RuntimeOptions};

use crate::Availability;

const TTL: Duration = Duration::from_secs(60);
const ROOT: u64 = 1;

#[derive(Debug)]
struct Node {
    path: String,
    parent: u64,
    attr: FileAttr,
    children: BTreeMap<String, u64>,
}

/// Immutable inodes stay valid for the lifetime of the mounted store. Sorted
/// paths make inode assignment and directory cookies deterministic.
#[derive(Debug)]
struct Namespace {
    nodes: BTreeMap<u64, Node>,
    logical_bytes: u64,
}

impl Namespace {
    fn new(manifest: &Manifest, uid: u32, gid: u32) -> Result<Self> {
        Self::from_entries(
            manifest.directories.iter().map(String::as_str),
            manifest
                .files
                .iter()
                .map(|file| (file.path.as_str(), file.size, file.mode)),
            uid,
            gid,
        )
    }

    fn from_entries<'a>(
        directories: impl Iterator<Item = &'a str>,
        files: impl Iterator<Item = (&'a str, u64, u32)>,
        uid: u32,
        gid: u32,
    ) -> Result<Self> {
        let mut entries = BTreeMap::new();
        entries.insert(String::new(), (FileType::Directory, 0, 0o555));
        for directory in directories {
            validate_name(directory)?;
            if entries
                .insert(directory.to_owned(), (FileType::Directory, 0, 0o555))
                .is_some()
            {
                bail!("duplicate directory {directory}");
            }
        }
        let mut logical_bytes = 0u64;
        for (path, size, mode) in files {
            validate_name(path)?;
            if entries
                .insert(path.to_owned(), (FileType::RegularFile, size, mode & 0o555))
                .is_some()
            {
                bail!("duplicate path {path}");
            }
            logical_bytes = logical_bytes
                .checked_add(size)
                .context("logical filesystem size overflow")?;
        }
        let mut by_path = BTreeMap::new();
        let mut nodes = BTreeMap::new();
        for (index, (path, (kind, size, mode))) in entries.into_iter().enumerate() {
            let ino = u64::try_from(index)
                .context("too many inodes")?
                .checked_add(ROOT)
                .context("inode overflow")?;
            by_path.insert(path.clone(), ino);
            nodes.insert(
                ino,
                Node {
                    path,
                    parent: ROOT,
                    attr: FileAttr {
                        ino: INodeNo(ino),
                        size,
                        blocks: size.div_ceil(512),
                        atime: UNIX_EPOCH,
                        mtime: UNIX_EPOCH,
                        ctime: UNIX_EPOCH,
                        crtime: UNIX_EPOCH,
                        kind,
                        perm: mode as u16,
                        nlink: if kind == FileType::Directory { 2 } else { 1 },
                        uid,
                        gid,
                        rdev: 0,
                        blksize: 4096,
                        flags: 0,
                    },
                    children: BTreeMap::new(),
                },
            );
        }
        for (path, &ino) in &by_path {
            if path.is_empty() {
                continue;
            }
            let (parent_path, name) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
            let parent_ino = *by_path
                .get(parent_path)
                .with_context(|| format!("missing parent of {path}"))?;
            let is_directory =
                nodes.get(&ino).context("missing child inode")?.attr.kind == FileType::Directory;
            let parent = nodes.get_mut(&parent_ino).context("missing parent inode")?;
            if parent.attr.kind != FileType::Directory {
                bail!("parent of {path} is a file");
            }
            parent.children.insert(name.to_owned(), ino);
            if is_directory {
                parent.attr.nlink = parent
                    .attr
                    .nlink
                    .checked_add(1)
                    .context("directory link count overflow")?;
            }
            nodes.get_mut(&ino).context("missing inode")?.parent = parent_ino;
        }
        Ok(Self {
            nodes,
            logical_bytes,
        })
    }

    fn node(&self, ino: INodeNo) -> std::result::Result<&Node, Errno> {
        self.nodes.get(&ino.0).ok_or(Errno::ENOENT)
    }

    fn directory(&self, ino: INodeNo) -> std::result::Result<&Node, Errno> {
        let node = self.node(ino)?;
        if node.attr.kind != FileType::Directory {
            return Err(Errno::ENOTDIR);
        }
        Ok(node)
    }

    fn lookup(&self, parent: INodeNo, name: &OsStr) -> std::result::Result<&Node, Errno> {
        let directory = self.directory(parent)?;
        let name = name.to_str().ok_or(Errno::ENOENT)?;
        let ino = match name {
            "." => parent.0,
            ".." => directory.parent,
            name => *directory.children.get(name).ok_or(Errno::ENOENT)?,
        };
        self.node(INodeNo(ino))
    }
}

fn validate_name(path: &str) -> Result<()> {
    if !playsparse_core::valid_path(path) || path.split('/').any(|name| name.len() > 255) {
        bail!("unsupported filesystem path {path:?}");
    }
    Ok(())
}

struct StoreFs {
    resolver: Arc<RangeResolver>,
    namespace: Namespace,
    read_requests: AtomicU64,
    requested_bytes: AtomicU64,
    returned_bytes: AtomicU64,
    read_errors: AtomicU64,
}

impl StoreFs {
    #[cfg(test)]
    fn open(store: &Path, cache_bytes: usize) -> Result<Self> {
        Self::open_configured(
            store,
            RuntimeOptions {
                cache_bytes,
                ..Default::default()
            },
        )
    }

    fn open_configured(store: &Path, options: RuntimeOptions) -> Result<Self> {
        let resolver = Arc::new(
            RangeResolver::open_configured(store, options).context("open immutable store")?,
        );
        // Only the mounting user can access the filesystem (fuser's default ACL).
        // SAFETY: getuid/getgid have no pointer arguments or preconditions.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        let namespace = Namespace::new(resolver.manifest(), uid, gid)?;
        Ok(Self {
            resolver,
            namespace,
            read_requests: AtomicU64::new(0),
            requested_bytes: AtomicU64::new(0),
            returned_bytes: AtomicU64::new(0),
            read_errors: AtomicU64::new(0),
        })
    }

    fn open_file(&self, ino: INodeNo, flags: OpenFlags) -> std::result::Result<&Node, Errno> {
        let node = self.namespace.node(ino)?;
        if node.attr.kind == FileType::Directory {
            return Err(Errno::EISDIR);
        }
        if flags.0 & libc::O_ACCMODE != libc::O_RDONLY
            || flags.0 & (libc::O_TRUNC | libc::O_APPEND | libc::O_CREAT) != 0
        {
            return Err(Errno::EROFS);
        }
        Ok(node)
    }

    fn read_file(
        &self,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: usize,
    ) -> std::result::Result<Vec<u8>, Errno> {
        let node = self.namespace.node(ino)?;
        if node.attr.kind == FileType::Directory {
            return Err(Errno::EISDIR);
        }
        if fh.0 != ino.0 {
            return Err(Errno::EBADF);
        }
        self.resolver
            .read_range(&node.path, offset, size)
            .map_err(|error| {
                tracing::error!(path = %node.path, offset, size, %error, "FUSE range read failed");
                match error {
                    Error::NotFound(_) => Errno::ENOENT,
                    Error::ReadTooLarge => Errno::EINVAL,
                    _ => Errno::EIO,
                }
            })
    }
}

impl Filesystem for StoreFs {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        match self.namespace.lookup(parent, name) {
            Ok(node) => reply.entry(&TTL, &node.attr, Generation(0)),
            Err(error) => reply.error(error),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.namespace.node(ino) {
            Ok(node) => reply.attr(&TTL, &node.attr),
            Err(error) => reply.error(error),
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        match self.open_file(ino, flags) {
            // Keep kernel caching enabled: ordinary mmap and exec page faults
            // become FUSE read requests. DIRECT_IO would obstruct mmap support.
            Ok(_) => reply.opened(FileHandle(ino.0), FopenFlags::FOPEN_KEEP_CACHE),
            Err(error) => reply.error(error),
        }
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        self.read_requests.fetch_add(1, Ordering::Relaxed);
        self.requested_bytes
            .fetch_add(u64::from(size), Ordering::Relaxed);
        match self.read_file(ino, fh, offset, size as usize) {
            Ok(bytes) => {
                self.returned_bytes
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                reply.data(&bytes);
            }
            Err(error) => {
                self.read_errors.fetch_add(1, Ordering::Relaxed);
                reply.error(error);
            }
        }
    }

    fn flush(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        _lock_owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        if fh.0 == ino.0 && self.namespace.node(ino).is_ok() {
            reply.ok();
        } else {
            reply.error(Errno::EBADF);
        }
    }

    fn release(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        if fh.0 == ino.0 {
            reply.ok();
        } else {
            reply.error(Errno::EBADF);
        }
    }

    fn opendir(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        match self.namespace.directory(ino) {
            Ok(_) if flags.0 & libc::O_ACCMODE == libc::O_RDONLY => {
                reply.opened(FileHandle(ino.0), FopenFlags::empty())
            }
            Ok(_) => reply.error(Errno::EROFS),
            Err(error) => reply.error(error),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let directory = match self.namespace.directory(ino) {
            Ok(directory) => directory,
            Err(error) => {
                reply.error(error);
                return;
            }
        };
        if fh.0 != ino.0 {
            reply.error(Errno::EBADF);
            return;
        }
        // Cookies index [".", "..", children...] and name the next position.
        // skip() is bounded by this directory's size even for a huge cookie.
        let entries = [(ino.0, "."), (directory.parent, "..")].into_iter().chain(
            directory
                .children
                .iter()
                .map(|(name, &child)| (child, name.as_str())),
        );
        let Ok(skip) = usize::try_from(offset) else {
            reply.ok();
            return;
        };
        for (index, (child, name)) in entries.enumerate().skip(skip) {
            let Some(node) = self.namespace.nodes.get(&child) else {
                reply.error(Errno::EIO);
                return;
            };
            if reply.add(INodeNo(child), index as u64 + 1, node.attr.kind, name) {
                break;
            }
        }
        reply.ok();
    }

    fn releasedir(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        reply: ReplyEmpty,
    ) {
        if fh.0 == ino.0 {
            reply.ok();
        } else {
            reply.error(Errno::EBADF);
        }
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        // Logical view size: shared/compressed physical blocks cannot be
        // assigned unambiguously to individual virtual files.
        reply.statfs(
            self.namespace.logical_bytes.div_ceil(4096),
            0,
            0,
            self.namespace.nodes.len() as u64,
            0,
            4096,
            255,
            4096,
        );
    }

    fn access(&self, req: &Request, ino: INodeNo, mask: AccessFlags, reply: ReplyEmpty) {
        let node = match self.namespace.node(ino) {
            Ok(node) => node,
            Err(error) => {
                reply.error(error);
                return;
            }
        };
        if mask.contains(AccessFlags::W_OK) {
            reply.error(Errno::EROFS);
            return;
        }
        let shift = if req.uid() == node.attr.uid {
            6
        } else if req.gid() == node.attr.gid {
            3
        } else {
            0
        };
        let permissions = node.attr.perm >> shift;
        let privileged = req.uid() == 0;
        let read_denied = mask.contains(AccessFlags::R_OK) && !privileged && permissions & 4 == 0;
        let execute_denied = mask.contains(AccessFlags::X_OK)
            && (if privileged {
                node.attr.perm & 0o111 == 0
            } else {
                permissions & 1 == 0
            });
        if read_denied || execute_denied {
            reply.error(Errno::EACCES);
        } else {
            reply.ok();
        }
    }

    fn getxattr(&self, _req: &Request, ino: INodeNo, _name: &OsStr, _size: u32, reply: ReplyXattr) {
        if let Err(error) = self.namespace.node(ino) {
            reply.error(error);
            return;
        }
        #[cfg(target_os = "linux")]
        reply.error(Errno::from_i32(libc::ENODATA));
        #[cfg(target_os = "macos")]
        reply.error(Errno::from_i32(libc::ENOATTR));
    }

    fn listxattr(&self, _req: &Request, ino: INodeNo, size: u32, reply: ReplyXattr) {
        if let Err(error) = self.namespace.node(ino) {
            reply.error(error);
            return;
        }
        if size == 0 {
            reply.size(0);
        } else {
            reply.data(&[]);
        }
    }

    fn destroy(&mut self) {
        self.resolver.stop_prefetch();
        // A complete machine-readable line also works when no log subscriber
        // has been installed. These counters measure actual kernel callbacks;
        // the kernel page cache can satisfy reads without invoking this backend.
        eprintln!(
            "{}",
            serde_json::json!({
                "event": "fuse_unmounted", "read_requests": self.read_requests.load(Ordering::Relaxed),
                "requested_bytes": self.requested_bytes.load(Ordering::Relaxed),
                "returned_bytes": self.returned_bytes.load(Ordering::Relaxed),
                "read_errors": self.read_errors.load(Ordering::Relaxed),
                "cache": self.resolver.metrics(),
                "tiers": self.resolver.store().tier_metrics(),
                "trace": self.resolver.trace().map(|trace| trace.metrics()),
            })
        );
    }
}

pub(super) fn mount_config() -> Config {
    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::RO,
        MountOption::Exec,
        MountOption::NoSuid,
        MountOption::NoDev,
        MountOption::NoAtime,
        MountOption::DefaultPermissions,
        MountOption::FSName("playsparse".into()),
        MountOption::Subtype("playsparse".into()),
    ];
    config.n_threads =
        Some(std::thread::available_parallelism().map_or(4, |n| n.get().clamp(2, 16)));
    config
}

fn validate_mountpoint(store: &Path, mountpoint: &Path) -> Result<(PathBuf, PathBuf)> {
    let store = store.canonicalize().context("resolve store path")?;
    let mountpoint = mountpoint
        .canonicalize()
        .context("resolve existing mountpoint")?;
    if !mountpoint.is_dir() {
        bail!("mountpoint must be an existing directory");
    }
    if store.starts_with(&mountpoint) || mountpoint.starts_with(&store) {
        bail!("store and mountpoint must be separate directory trees");
    }
    if fs::read_dir(&mountpoint)
        .context("inspect mountpoint")?
        .next()
        .transpose()?
        .is_some()
    {
        bail!("mountpoint must be empty");
    }
    Ok((store, mountpoint))
}

pub(super) fn mount(
    store: &Path,
    mountpoint: &Path,
    overlay: Option<&Path>,
    options: RuntimeOptions,
) -> Result<()> {
    let (store, mountpoint) = validate_mountpoint(store, mountpoint)?;
    if let Some(overlay) = overlay {
        return crate::writable::mount(&store, &mountpoint, overlay, options);
    }
    let cache_bytes = options.cache_bytes;
    let fs = StoreFs::open_configured(&store, options)?;
    tracing::info!(store = %store.display(), mountpoint = %mountpoint.display(), cache_bytes, "mounting FUSE store");
    fuser::mount(fs, &mountpoint, &mount_config()).context("mount FUSE store")
}

pub(super) fn availability() -> Availability {
    #[cfg(target_os = "linux")]
    {
        match fs::OpenOptions::new().read(true).write(true).open("/dev/fuse") {
            Ok(_) => Availability { backend: "FUSE", available: true, detail: "Linux /dev/fuse can be opened; mounting still requires mount permission or fusermount3.".into() },
            Err(error) => Availability { backend: "FUSE", available: false, detail: format!("Cannot open /dev/fuse: {error}. Install/enable FUSE and grant access to its device.") },
        }
    }
    #[cfg(target_os = "macos")]
    {
        let installed = Path::new("/Library/Filesystems/macfuse.fs").exists();
        Availability {
            backend: "macFUSE",
            available: installed,
            detail: if installed {
                "macFUSE installation found; the driver must be approved and loaded for mounting."
                    .into()
            } else {
                "macFUSE is not installed at /Library/Filesystems/macfuse.fs.".into()
            },
        }
    }
}

#[cfg(test)]
mod tests;
