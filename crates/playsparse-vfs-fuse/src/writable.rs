//! Writable FUSE callbacks delegate data and persistence to the shared overlay.
//! The kernel uses ordinary cached IO, so mmap and native execution still work.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use fuser::{
    AccessFlags, BsdFileFlags, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags,
    Generation, INodeNo, LockOwner, MountOption, OpenFlags, RenameFlags, ReplyAttr, ReplyCreate,
    ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite,
    ReplyXattr, Request, TimeOrNow, WriteFlags,
};
use playsparse_core::Error;
use playsparse_overlay::{Entry, Handle, Overlay};
use playsparse_range::{RangeResolver, RuntimeOptions};
#[cfg(test)]
use playsparse_trace::TraceWriter;

type FsResult<T> = std::result::Result<T, Errno>;
const ROOT: u64 = 1;
// Namespace and size change while mounted. Zero TTL makes each new lookup/getattr
// consult the overlay instead of keeping stale sizes or deleted directory entries.
const TTL: Duration = Duration::ZERO;

#[derive(Clone)]
struct OpenFile {
    inode: u64,
    handle: Handle,
    writable: bool,
    append: bool,
}

#[derive(Clone)]
struct DirectoryItem {
    inode: u64,
    name: String,
    kind: FileType,
}

#[derive(Clone)]
enum OpenHandle {
    File(OpenFile),
    Directory {
        inode: u64,
        handle: Handle,
        entries: Arc<Vec<DirectoryItem>>,
    },
}

struct WritableFs {
    resolver: Arc<RangeResolver>,
    overlay: Overlay,
    // The shared read lock covers handle lookup only. No content read, CAS
    // loading, copy-up or file IO runs while it is held.
    handles: RwLock<BTreeMap<u64, OpenHandle>>,
    next_handle: AtomicU64,
    uid: u32,
    gid: u32,
    read_requests: AtomicU64,
    returned_bytes: AtomicU64,
    write_requests: AtomicU64,
    written_bytes: AtomicU64,
    io_errors: AtomicU64,
}

impl WritableFs {
    #[cfg(test)]
    fn open(
        store: &Path,
        cache_bytes: usize,
        overlay: &Path,
        trace: Option<Arc<TraceWriter>>,
    ) -> Result<Self> {
        Self::open_configured(
            store,
            overlay,
            RuntimeOptions {
                cache_bytes,
                trace,
                ..Default::default()
            },
        )
    }

    fn open_configured(store: &Path, overlay: &Path, options: RuntimeOptions) -> Result<Self> {
        let resolver = Arc::new(
            RangeResolver::open_configured(store, options)
                .context("open immutable overlay base")?,
        );
        let overlay = Overlay::open(Arc::clone(&resolver), overlay).context("open overlay")?;
        let entries = overlay.entries()?;
        for entry in &entries {
            if entry.path.split('/').any(|component| component.len() > 255) {
                bail!("unsupported filesystem path {:?}", entry.path);
            }
        }
        // SAFETY: getuid/getgid take no pointers and have no preconditions.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        Ok(Self {
            resolver,
            overlay,
            handles: RwLock::new(BTreeMap::new()),
            next_handle: AtomicU64::new(1),
            uid,
            gid,
            read_requests: AtomicU64::new(0),
            returned_bytes: AtomicU64::new(0),
            write_requests: AtomicU64::new(0),
            written_bytes: AtomicU64::new(0),
            io_errors: AtomicU64::new(0),
        })
    }

    fn entry(&self, inode: INodeNo) -> FsResult<Entry> {
        // Identity resolution lives in the overlay engine. A backend path
        // cache would race with a concurrent rename or replacement.
        match self.overlay.metadata_id(inode.0) {
            Ok(entry) => Ok(entry),
            Err(Error::NotFound(_)) => {
                // Linux can issue getattr without FUSE_GETATTR_FH even for
                // fstat on an unlinked or replaced open inode.
                let handle = self.retained_handle(inode)?.ok_or(Errno::ENOENT)?;
                self.overlay.metadata_handle(&handle).map_err(errno)
            }
            Err(error) => Err(errno(error)),
        }
    }

    fn retained_handle(&self, inode: INodeNo) -> FsResult<Option<Handle>> {
        Ok(self
            .handles
            .read()
            .map_err(|_| Errno::EIO)?
            .values()
            .find_map(|open| match open {
                OpenHandle::File(file) if file.inode == inode.0 => Some(file.handle.clone()),
                OpenHandle::Directory {
                    inode: id, handle, ..
                } if *id == inode.0 => Some(handle.clone()),
                _ => None,
            }))
    }

    fn child_path(&self, parent: INodeNo, name: &OsStr) -> FsResult<String> {
        let parent = self.entry(parent)?;
        if !parent.is_dir {
            return Err(Errno::ENOTDIR);
        }
        let name = name.to_str().ok_or(Errno::EINVAL)?;
        if !playsparse_core::valid_path(name) || name.contains('/') {
            return Err(Errno::EINVAL);
        }
        if name.len() > 255 {
            return Err(Errno::ENAMETOOLONG);
        }
        Ok(if parent.path.is_empty() {
            name.to_owned()
        } else {
            format!("{}/{name}", parent.path)
        })
    }

    fn attributes(&self, entry: &Entry) -> FileAttr {
        FileAttr {
            ino: INodeNo(entry.id),
            size: entry.size,
            blocks: entry.size.div_ceil(512),
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: kind(entry),
            perm: (entry.mode & 0o777) as u16,
            nlink: if entry.is_dir { 2 } else { 1 },
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    fn allocate_handle(&self, handle: OpenHandle) -> FsResult<FileHandle> {
        let id = self
            .next_handle
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| Errno::EMFILE)?;
        self.handles
            .write()
            .map_err(|_| Errno::EIO)?
            .insert(id, handle);
        Ok(FileHandle(id))
    }

    fn handle(&self, inode: INodeNo, fh: FileHandle) -> FsResult<OpenHandle> {
        let handle = self
            .handles
            .read()
            .map_err(|_| Errno::EIO)?
            .get(&fh.0)
            .cloned()
            .ok_or(Errno::EBADF)?;
        let handle_inode = match &handle {
            OpenHandle::File(file) => file.inode,
            OpenHandle::Directory { inode, .. } => *inode,
        };
        if handle_inode != inode.0 {
            return Err(Errno::EBADF);
        }
        Ok(handle)
    }

    fn file(&self, inode: INodeNo, fh: FileHandle) -> FsResult<OpenFile> {
        match self.handle(inode, fh)? {
            OpenHandle::File(file) => Ok(file),
            OpenHandle::Directory { .. } => Err(Errno::EISDIR),
        }
    }

    fn open_file(&self, inode: INodeNo, flags: i32) -> FsResult<FileHandle> {
        let entry = self.entry(inode)?;
        if entry.is_dir {
            return Err(Errno::EISDIR);
        }
        let writable = flags & libc::O_ACCMODE != libc::O_RDONLY;
        if !writable && flags & libc::O_TRUNC != 0 {
            return Err(Errno::EINVAL);
        }
        let handle = self
            .overlay
            .open_file_id(inode.0, writable, flags & libc::O_TRUNC != 0)
            .map_err(errno)?;
        self.allocate_handle(OpenHandle::File(OpenFile {
            inode: entry.id,
            handle,
            writable,
            append: flags & libc::O_APPEND != 0,
        }))
    }

    fn flush_file(&self, inode: INodeNo, fh: FileHandle) -> FsResult<()> {
        self.overlay
            .flush(&self.file(inode, fh)?.handle)
            .map_err(errno)
    }

    fn remove_handle(&self, inode: INodeNo, fh: FileHandle) -> FsResult<OpenHandle> {
        self.handle(inode, fh)?;
        self.handles
            .write()
            .map_err(|_| Errno::EIO)?
            .remove(&fh.0)
            .ok_or(Errno::EBADF)
    }
}

fn kind(entry: &Entry) -> FileType {
    if entry.is_dir {
        FileType::Directory
    } else {
        FileType::RegularFile
    }
}

// statvfs field widths differ between Linux and macOS.
fn stat_u64(value: impl Into<u64>) -> u64 {
    value.into()
}

fn errno(error: Error) -> Errno {
    match error {
        Error::NotFound(_) => Errno::ENOENT,
        Error::Invalid(_) | Error::ReadTooLarge => Errno::EINVAL,
        Error::Corrupt(_) => Errno::EIO,
        Error::Io(error) => {
            if let Some(code) = error.raw_os_error() {
                return Errno::from_i32(code);
            }
            match error.kind() {
                std::io::ErrorKind::NotFound => Errno::ENOENT,
                std::io::ErrorKind::AlreadyExists => Errno::EEXIST,
                std::io::ErrorKind::PermissionDenied => Errno::EACCES,
                std::io::ErrorKind::InvalidInput => Errno::EINVAL,
                std::io::ErrorKind::DirectoryNotEmpty => Errno::ENOTEMPTY,
                std::io::ErrorKind::NotADirectory => Errno::ENOTDIR,
                std::io::ErrorKind::IsADirectory => Errno::EISDIR,
                std::io::ErrorKind::WouldBlock => Errno::EBUSY,
                std::io::ErrorKind::StorageFull => Errno::ENOSPC,
                std::io::ErrorKind::Unsupported => Errno::EOPNOTSUPP,
                _ => Errno::EIO,
            }
        }
    }
}

impl Filesystem for WritableFs {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let result = match name.to_str() {
            Some(".") => self.entry(parent),
            Some("..") => self.entry(parent).and_then(|entry| {
                let path = entry.path.rsplit_once('/').map_or("", |(parent, _)| parent);
                self.overlay.metadata(path).map_err(errno)
            }),
            _ => self
                .child_path(parent, name)
                .and_then(|path| self.overlay.metadata(&path).map_err(errno)),
        };
        match result {
            Ok(entry) => reply.entry(&TTL, &self.attributes(&entry), Generation(0)),
            Err(error) => reply.error(error),
        }
    }

    fn getattr(&self, _req: &Request, inode: INodeNo, fh: Option<FileHandle>, reply: ReplyAttr) {
        let result = match fh {
            Some(fh) => match self.handle(inode, fh) {
                Ok(OpenHandle::File(file)) => {
                    self.overlay.metadata_handle(&file.handle).map_err(errno)
                }
                Ok(OpenHandle::Directory { handle, .. }) => {
                    self.overlay.metadata_handle(&handle).map_err(errno)
                }
                Err(error) => Err(error),
            },
            None => self.entry(inode),
        };
        match result {
            Ok(entry) => reply.attr(&TTL, &self.attributes(&entry)),
            Err(error) => reply.error(error),
        }
    }

    fn setattr(
        &self,
        _req: &Request,
        inode: INodeNo,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        fh: Option<FileHandle>,
        crtime: Option<SystemTime>,
        chgtime: Option<SystemTime>,
        bkuptime: Option<SystemTime>,
        flags: Option<BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        // Ownership is the mounting user. Explicit timestamps, BSD attributes,
        // and ownership changes need a later persistent metadata format.
        if uid.is_some_and(|uid| uid != self.uid) || gid.is_some_and(|gid| gid != self.gid) {
            reply.error(Errno::EPERM);
            return;
        }
        if atime.is_some()
            || mtime.is_some()
            || crtime.is_some()
            || chgtime.is_some()
            || bkuptime.is_some()
            || flags.is_some()
        {
            reply.error(Errno::EOPNOTSUPP);
            return;
        }
        let result = (|| {
            let open = fh.map(|fh| self.handle(inode, fh)).transpose()?;
            let mut entry = match &open {
                Some(OpenHandle::File(file)) => {
                    self.overlay.metadata_handle(&file.handle).map_err(errno)?
                }
                Some(OpenHandle::Directory { handle, .. }) => {
                    self.overlay.metadata_handle(handle).map_err(errno)?
                }
                None => self.entry(inode)?,
            };
            if let Some(size) = size {
                if entry.is_dir {
                    return Err(Errno::EISDIR);
                }
                let handle = match &open {
                    Some(OpenHandle::File(file)) if file.writable => file.handle.clone(),
                    Some(OpenHandle::File(_)) => return Err(Errno::EBADF),
                    Some(OpenHandle::Directory { .. }) => return Err(Errno::EISDIR),
                    None => self
                        .overlay
                        .open_file_id(inode.0, true, false)
                        .map_err(errno)?,
                };
                self.overlay.truncate(&handle, size).map_err(errno)?;
                entry = self.overlay.metadata_handle(&handle).map_err(errno)?;
            }
            if let Some(mode) = mode {
                match &open {
                    Some(OpenHandle::File(file)) => {
                        self.overlay
                            .set_mode_handle(&file.handle, mode & 0o777)
                            .map_err(errno)?;
                        entry = self.overlay.metadata_handle(&file.handle).map_err(errno)?;
                    }
                    Some(OpenHandle::Directory { handle, .. }) => {
                        self.overlay
                            .set_mode_handle(handle, mode & 0o777)
                            .map_err(errno)?;
                        entry = self.overlay.metadata_handle(handle).map_err(errno)?;
                    }
                    None => {
                        if let Some(handle) = self.retained_handle(inode)? {
                            self.overlay
                                .set_mode_handle(&handle, mode & 0o777)
                                .map_err(errno)?;
                            entry = self.overlay.metadata_handle(&handle).map_err(errno)?;
                        } else {
                            self.overlay
                                .set_mode_id(inode.0, mode & 0o777)
                                .map_err(errno)?;
                            entry = self.entry(inode)?;
                        }
                    }
                }
            }
            Ok(entry)
        })();
        match result {
            Ok(entry) => reply.attr(&TTL, &self.attributes(&entry)),
            Err(error) => reply.error(error),
        }
    }

    fn open(&self, _req: &Request, inode: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        match self.open_file(inode, flags.0) {
            // Do not keep stale pages across writable opens, while retaining
            // ordinary kernel cached IO rather than using DIRECT_IO.
            Ok(fh) => reply.opened(fh, FopenFlags::empty()),
            Err(error) => reply.error(error),
        }
    }

    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let result = (|| {
            let path = self.child_path(parent, name)?;
            let handle = self
                .overlay
                .create(&path, mode & !umask & 0o777, flags & libc::O_EXCL != 0)
                .map_err(errno)?;
            if flags & libc::O_TRUNC != 0 {
                self.overlay.truncate(&handle, 0).map_err(errno)?;
            }
            let entry = self.overlay.metadata_handle(&handle).map_err(errno)?;
            let fh = self.allocate_handle(OpenHandle::File(OpenFile {
                inode: entry.id,
                handle,
                writable: flags & libc::O_ACCMODE != libc::O_RDONLY,
                append: flags & libc::O_APPEND != 0,
            }))?;
            Ok((entry, fh))
        })();
        match result {
            Ok((entry, fh)) => reply.created(
                &TTL,
                &self.attributes(&entry),
                Generation(0),
                fh,
                FopenFlags::empty(),
            ),
            Err(error) => reply.error(error),
        }
    }

    fn read(
        &self,
        _req: &Request,
        inode: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        self.read_requests.fetch_add(1, Ordering::Relaxed);
        let result = self.file(inode, fh).and_then(|file| {
            self.overlay
                .read(&file.handle, offset, size as usize)
                .map_err(errno)
        });
        match result {
            Ok(bytes) => {
                self.returned_bytes
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                reply.data(&bytes);
            }
            Err(error) => {
                self.io_errors.fetch_add(1, Ordering::Relaxed);
                reply.error(error);
            }
        }
    }

    fn write(
        &self,
        _req: &Request,
        inode: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        self.write_requests.fetch_add(1, Ordering::Relaxed);
        let result = self.file(inode, fh).and_then(|file| {
            if !file.writable {
                return Err(Errno::EBADF);
            }
            self.overlay
                .write(&file.handle, offset, data, file.append)
                .map_err(errno)
        });
        match result {
            Ok(bytes) => {
                self.written_bytes
                    .fetch_add(bytes as u64, Ordering::Relaxed);
                reply.written(bytes as u32);
            }
            Err(error) => {
                self.io_errors.fetch_add(1, Ordering::Relaxed);
                reply.error(error);
            }
        }
    }

    fn flush(
        &self,
        _req: &Request,
        inode: INodeNo,
        fh: FileHandle,
        _owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        match self.flush_file(inode, fh) {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn fsync(
        &self,
        _req: &Request,
        inode: INodeNo,
        fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        match self.flush_file(inode, fh) {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn release(
        &self,
        _req: &Request,
        inode: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        _owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        let result = self
            .remove_handle(inode, fh)
            .and_then(|handle| match handle {
                OpenHandle::File(file) => self.overlay.flush(&file.handle).map_err(errno),
                OpenHandle::Directory { .. } => Err(Errno::EISDIR),
            });
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn mkdir(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        let result = (|| {
            let path = self.child_path(parent, name)?;
            self.overlay
                .mkdir(&path, mode & !umask & 0o777)
                .map_err(errno)?;
            self.overlay.metadata(&path).map_err(errno)
        })();
        match result {
            Ok(entry) => reply.entry(&TTL, &self.attributes(&entry), Generation(0)),
            Err(error) => reply.error(error),
        }
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let result = self
            .child_path(parent, name)
            .and_then(|path| self.overlay.unlink(&path).map_err(errno));
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let result = self
            .child_path(parent, name)
            .and_then(|path| self.overlay.rmdir(&path).map_err(errno));
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        // EXCHANGE and WHITEOUT need distinct overlay transactions.
        #[cfg(target_os = "linux")]
        let (unsupported, replace) = (
            flags.bits() & !RenameFlags::RENAME_NOREPLACE.bits() != 0,
            !flags.contains(RenameFlags::RENAME_NOREPLACE),
        );
        #[cfg(not(target_os = "linux"))]
        let (unsupported, replace) = (!flags.is_empty(), true);
        if unsupported {
            reply.error(Errno::EOPNOTSUPP);
            return;
        }
        let result = (|| {
            let from = self.child_path(parent, name)?;
            let to = self.child_path(newparent, newname)?;
            self.overlay.rename(&from, &to, replace).map_err(errno)
        })();
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn opendir(&self, _req: &Request, inode: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        let result = (|| {
            if flags.0 & libc::O_ACCMODE != libc::O_RDONLY {
                return Err(Errno::EISDIR);
            }
            let entry = self.entry(inode)?;
            if !entry.is_dir {
                return Err(Errno::ENOTDIR);
            }
            let parent = match entry.path.rsplit_once('/') {
                Some((parent, _)) => self.overlay.metadata(parent).map_err(errno)?.id,
                None => ROOT,
            };
            let mut entries = vec![
                DirectoryItem {
                    inode: inode.0,
                    name: ".".into(),
                    kind: FileType::Directory,
                },
                DirectoryItem {
                    inode: parent,
                    name: "..".into(),
                    kind: FileType::Directory,
                },
            ];
            let handle = self.overlay.open_dir_id(inode.0).map_err(errno)?;
            for child in self.overlay.list_handle(&handle).map_err(errno)? {
                let name = child.path.rsplit('/').next().ok_or(Errno::EIO)?.to_owned();
                entries.push(DirectoryItem {
                    inode: child.id,
                    name,
                    kind: kind(&child),
                });
            }
            self.allocate_handle(OpenHandle::Directory {
                inode: inode.0,
                handle,
                entries: Arc::new(entries),
            })
        })();
        match result {
            Ok(fh) => reply.opened(fh, FopenFlags::empty()),
            Err(error) => reply.error(error),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        inode: INodeNo,
        fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let entries = match self.handle(inode, fh) {
            Ok(OpenHandle::Directory { entries, .. }) => entries,
            Ok(OpenHandle::File(_)) => {
                reply.error(Errno::ENOTDIR);
                return;
            }
            Err(error) => {
                reply.error(error);
                return;
            }
        };
        // Snapshot cookies remain stable while a directory is concurrently edited.
        let Ok(skip) = usize::try_from(offset) else {
            reply.ok();
            return;
        };
        for (index, entry) in entries.iter().enumerate().skip(skip) {
            if reply.add(
                INodeNo(entry.inode),
                index as u64 + 1,
                entry.kind,
                &entry.name,
            ) {
                break;
            }
        }
        reply.ok();
    }

    fn releasedir(
        &self,
        _req: &Request,
        inode: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        reply: ReplyEmpty,
    ) {
        match self.remove_handle(inode, fh) {
            Ok(OpenHandle::Directory { .. }) => reply.ok(),
            Ok(OpenHandle::File(_)) => reply.error(Errno::ENOTDIR),
            Err(error) => reply.error(error),
        }
    }

    fn fsyncdir(
        &self,
        _req: &Request,
        inode: INodeNo,
        fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        let result = match self.handle(inode, fh) {
            Ok(OpenHandle::Directory { .. }) => self.overlay.sync().map_err(errno),
            Ok(OpenHandle::File(_)) => Err(Errno::ENOTDIR),
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn access(&self, req: &Request, inode: INodeNo, mask: AccessFlags, reply: ReplyEmpty) {
        let result = self.entry(inode).and_then(|entry| {
            let mode = entry.mode;
            let shift = if req.uid() == self.uid {
                6
            } else if req.gid() == self.gid {
                3
            } else {
                0
            };
            let permissions = mode >> shift;
            let privileged = req.uid() == 0;
            let denied = (!privileged
                && ((mask.contains(AccessFlags::R_OK) && permissions & 4 == 0)
                    || (mask.contains(AccessFlags::W_OK) && permissions & 2 == 0)))
                || (mask.contains(AccessFlags::X_OK)
                    && if privileged {
                        !entry.is_dir && mode & 0o111 == 0
                    } else {
                        permissions & 1 == 0
                    });
            if denied { Err(Errno::EACCES) } else { Ok(()) }
        });
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn statfs(&self, _req: &Request, _inode: INodeNo, reply: ReplyStatfs) {
        // Do not advertise zero free blocks on a writable volume: launchers use
        // statvfs before staging an update. Capacity is the real overlay volume.
        let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        let root = match std::ffi::CString::new(self.overlay.root().as_os_str().as_encoded_bytes())
        {
            Ok(root) => root,
            Err(_) => {
                reply.error(Errno::EIO);
                return;
            }
        };
        // SAFETY: root is NUL terminated and stats points to writable storage.
        if unsafe { libc::statvfs(root.as_ptr(), stats.as_mut_ptr()) } != 0 {
            reply.error(Errno::from_i32(
                std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO),
            ));
            return;
        }
        // SAFETY: successful statvfs initialized stats.
        let stats = unsafe { stats.assume_init() };
        reply.statfs(
            stat_u64(stats.f_blocks),
            stat_u64(stats.f_bfree),
            stat_u64(stats.f_bavail),
            stat_u64(stats.f_files),
            stat_u64(stats.f_ffree),
            u32::try_from(stats.f_frsize).unwrap_or(4096),
            255,
            u32::try_from(stats.f_bsize).unwrap_or(4096),
        );
    }

    fn getxattr(
        &self,
        _req: &Request,
        inode: INodeNo,
        _name: &OsStr,
        _size: u32,
        reply: ReplyXattr,
    ) {
        if let Err(error) = self.entry(inode) {
            reply.error(error);
            return;
        }
        #[cfg(target_os = "linux")]
        reply.error(Errno::from_i32(libc::ENODATA));
        #[cfg(target_os = "macos")]
        reply.error(Errno::from_i32(libc::ENOATTR));
    }

    fn listxattr(&self, _req: &Request, inode: INodeNo, size: u32, reply: ReplyXattr) {
        if let Err(error) = self.entry(inode) {
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
        let sync_error = self.overlay.sync().err().map(|error| error.to_string());
        eprintln!(
            "{}",
            serde_json::json!({
                "event": "fuse_unmounted", "writable": true,
                "read_requests": self.read_requests.load(Ordering::Relaxed),
                "returned_bytes": self.returned_bytes.load(Ordering::Relaxed),
                "write_requests": self.write_requests.load(Ordering::Relaxed),
                "written_bytes": self.written_bytes.load(Ordering::Relaxed),
                "io_errors": self.io_errors.load(Ordering::Relaxed),
                "cache": self.resolver.metrics(), "overlay": self.overlay.metrics(),
                "tiers": self.resolver.store().tier_metrics(),
                "trace": self.resolver.trace().map(|trace| trace.metrics()),
                "sync_error": sync_error,
            })
        );
    }
}

fn overlay_path(store: &Path, mountpoint: &Path, overlay: &Path) -> Result<PathBuf> {
    let store = store.canonicalize().context("resolve store path")?;
    let mountpoint = mountpoint
        .canonicalize()
        .context("resolve mountpoint path")?;
    let overlay = if overlay.exists() {
        overlay.canonicalize().context("resolve overlay path")?
    } else {
        let parent = overlay
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        parent
            .canonicalize()
            .context("resolve existing overlay parent")?
            .join(
                overlay
                    .file_name()
                    .context("overlay needs a directory name")?,
            )
    };
    for (other, label) in [(&store, "store"), (&mountpoint, "mountpoint")] {
        if overlay.starts_with(other) || other.starts_with(&overlay) {
            bail!("overlay and {label} must be separate directory trees");
        }
    }
    Ok(overlay)
}

pub(super) fn mount(
    store: &Path,
    mountpoint: &Path,
    overlay: &Path,
    options: RuntimeOptions,
) -> Result<()> {
    let overlay = overlay_path(store, mountpoint, overlay)?;
    let cache_bytes = options.cache_bytes;
    let fs = WritableFs::open_configured(store, &overlay, options)?;
    let mut config = crate::fuse::mount_config();
    config
        .mount_options
        .retain(|option| *option != MountOption::RO);
    config.mount_options.push(MountOption::RW);
    tracing::info!(store = %store.display(), mountpoint = %mountpoint.display(), overlay = %overlay.display(), cache_bytes, "mounting writable FUSE overlay");
    crate::run_session(fs, mountpoint, &config).context("mount writable FUSE overlay")
}

#[cfg(test)]
mod tests;
