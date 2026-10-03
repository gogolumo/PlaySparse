//! WinFsp callbacks delegate persistence and copy-up to the shared overlay.
use super::*;
use playsparse_overlay::{Entry, Handle as OverlayHandle};
use std::sync::RwLock;

pub(super) struct WritableHandle {
    file: OverlayHandle,
    directory: DirBuffer,
}

pub(super) struct WritableFilesystem {
    overlay: Arc<Overlay>,
    // WinFsp serializes namespace mutations; this lock only protects canonical
    // Windows name lookup, and is never held during payload reads or writes.
    names: RwLock<BTreeMap<WindowsName, String>>,
    security: Vec<u8>,
    stop_event: usize,
    root: std::path::PathBuf,
}

impl WritableFilesystem {
    pub(super) fn new(overlay: Arc<Overlay>, root: &Path, stop_event: usize) -> Result<Self> {
        let filesystem = Self {
            overlay,
            names: RwLock::new(BTreeMap::new()),
            security: writable_security()?,
            stop_event,
            root: root.to_path_buf(),
        };
        filesystem.refresh_names()?;
        Ok(filesystem)
    }

    fn refresh_names(&self) -> Result<()> {
        let mut names = BTreeMap::new();
        names.insert(WindowsName::new(""), String::new());
        for entry in self.overlay.entries()? {
            validate_windows_path(&entry.path)?;
            if entry.size > i64::MAX as u64 {
                bail!("Windows signed file-size limit exceeded: {}", entry.path);
            }
            if entry.path.is_empty() {
                continue;
            }
            if names
                .insert(WindowsName::new(&entry.path), entry.path.clone())
                .is_some()
            {
                bail!("case-insensitive Windows name collision: {}", entry.path);
            }
        }
        *self.names.write().unwrap_or_else(|e| e.into_inner()) = names;
        Ok(())
    }

    fn canonical(&self, supplied: &str) -> winfsp::Result<String> {
        self.names
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&WindowsName::new(supplied))
            .cloned()
            .ok_or_else(|| STATUS_OBJECT_NAME_NOT_FOUND.into())
    }

    fn new_path(&self, supplied: &str) -> winfsp::Result<String> {
        validate_windows_path(supplied).map_err(|_| FspError::from(STATUS_OBJECT_NAME_INVALID))?;
        if supplied.is_empty() {
            return Err(STATUS_ACCESS_DENIED.into());
        }
        let (parent, name) = supplied.rsplit_once('/').unwrap_or(("", supplied));
        let parent = self.canonical(parent)?;
        if !self.overlay.metadata(&parent).map_err(map_error)?.is_dir {
            return Err(STATUS_NOT_A_DIRECTORY.into());
        }
        Ok(if parent.is_empty() {
            name.into()
        } else {
            format!("{parent}/{name}")
        })
    }

    fn entry(&self, handle: &WritableHandle) -> winfsp::Result<Entry> {
        self.overlay.handle_entry(&handle.file).map_err(map_error)
    }

    fn security(&self, target: Option<&mut [c_void]>) -> winfsp::Result<u64> {
        let target = target.map(|target| {
            // SAFETY: WinFsp supplies writable descriptor storage.
            unsafe {
                std::slice::from_raw_parts_mut(
                    target.as_mut_ptr().cast::<u8>(),
                    std::mem::size_of_val(target),
                )
            }
        });
        copy_security_descriptor(&self.security, target)
    }

    fn refresh_after_mutation(&self) -> winfsp::Result<()> {
        self.refresh_names().map_err(|error| {
            eprintln!("PlaySparse overlay namespace refresh failed: {error}");
            FspError::from(STATUS_DATA_ERROR)
        })
    }
}

fn writable_info(entry: &Entry) -> FileInfo {
    let mut out = info(&Node {
        path: entry.path.clone(),
        size: entry.size,
        is_dir: entry.is_dir,
        id: entry.id,
    });
    out.file_attributes = if entry.is_dir {
        FILE_ATTRIBUTE_DIRECTORY.0
            | if entry.mode & 0o222 == 0 {
                FILE_ATTRIBUTE_READONLY.0
            } else {
                0
            }
    } else {
        FILE_ATTRIBUTE_ARCHIVE.0
            | if entry.mode & 0o222 == 0 {
                FILE_ATTRIBUTE_READONLY.0
            } else {
                0
            }
    };
    out
}

fn fill_open(entry: &Entry, out: &mut OpenFileInfo) {
    *out.as_mut() = writable_info(entry);
    let normalized: Vec<u16> = format!("\\{}", entry.path.replace('/', "\\"))
        .encode_utf16()
        .collect();
    if normalized.len() * 2 <= out.normalized_name_size() as usize {
        out.set_normalized_name(&normalized, None);
    }
}

impl FileSystemContext for WritableFilesystem {
    type FileContext = WritableHandle;

    fn get_security_by_name(
        &self,
        name: &U16CStr,
        descriptor: Option<&mut [c_void]>,
        _resolve: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> winfsp::Result<FileSecurity> {
        let path = self.canonical(&virtual_path(name)?)?;
        let entry = self.overlay.metadata(&path).map_err(map_error)?;
        Ok(FileSecurity {
            reparse: false,
            sz_security_descriptor: self.security(descriptor)?,
            attributes: writable_info(&entry).file_attributes,
        })
    }

    fn get_security(
        &self,
        _handle: &WritableHandle,
        descriptor: Option<&mut [c_void]>,
    ) -> winfsp::Result<u64> {
        self.security(descriptor)
    }

    fn open(
        &self,
        name: &U16CStr,
        options: u32,
        access: u32,
        out: &mut OpenFileInfo,
    ) -> winfsp::Result<WritableHandle> {
        let path = self.canonical(&virtual_path(name)?)?;
        let entry = self.overlay.metadata(&path).map_err(map_error)?;
        if options & 1 != 0 && !entry.is_dir {
            return Err(STATUS_NOT_A_DIRECTORY.into());
        }
        if options & 0x40 != 0 && entry.is_dir {
            return Err(STATUS_FILE_IS_A_DIRECTORY.into());
        }
        if options & 0x1000 != 0 {
            if entry.path.is_empty() {
                return Err(STATUS_ACCESS_DENIED.into());
            }
            if !entry.is_dir && entry.mode & 0o222 == 0 {
                return Err(STATUS_CANNOT_DELETE.into());
            }
            if entry.is_dir
                && !self
                    .overlay
                    .list(&entry.path)
                    .map_err(map_error)?
                    .is_empty()
            {
                return Err(STATUS_DIRECTORY_NOT_EMPTY.into());
            }
        }
        let write = access & (FILE_WRITE_DATA | FILE_APPEND_DATA).0 != 0;
        if write && entry.mode & 0o222 == 0 {
            return Err(STATUS_ACCESS_DENIED.into());
        }
        let file = if entry.is_dir {
            self.overlay.open_dir(&path)
        } else {
            self.overlay.open_file(&path, write, false)
        }
        .map_err(map_error)?;
        fill_open(&self.overlay.handle_entry(&file).map_err(map_error)?, out);
        Ok(WritableHandle {
            file,
            directory: DirBuffer::new(),
        })
    }

    fn create(
        &self,
        name: &U16CStr,
        options: u32,
        _access: u32,
        attributes: u32,
        _descriptor: Option<&[c_void]>,
        _allocation_size: u64,
        extra: Option<&[u8]>,
        reparse: bool,
        out: &mut OpenFileInfo,
    ) -> winfsp::Result<WritableHandle> {
        if reparse || extra.is_some() {
            return Err(STATUS_NOT_SUPPORTED.into());
        }
        let allowed = (FILE_ATTRIBUTE_NORMAL
            | FILE_ATTRIBUTE_ARCHIVE
            | FILE_ATTRIBUTE_READONLY
            | FILE_ATTRIBUTE_DIRECTORY)
            .0;
        if attributes & !allowed != 0 {
            return Err(STATUS_NOT_SUPPORTED.into());
        }
        let supplied = virtual_path(name)?;
        if self.canonical(&supplied).is_ok() {
            return Err(STATUS_OBJECT_NAME_COLLISION.into());
        }
        let path = self.new_path(&supplied)?;
        let directory = options & 1 != 0;
        if !directory && attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0 {
            return Err(STATUS_INVALID_PARAMETER.into());
        }
        let mode = if directory {
            0o755
        } else if attributes & FILE_ATTRIBUTE_READONLY.0 != 0 {
            0o444
        } else {
            0o644
        };
        let file = if directory {
            self.overlay.mkdir(&path, mode).map_err(map_error)?;
            self.overlay.open_dir(&path).map_err(map_error)?
        } else {
            self.overlay.create(&path, mode, true).map_err(map_error)?
        };
        self.refresh_after_mutation()?;
        fill_open(&self.overlay.handle_entry(&file).map_err(map_error)?, out);
        Ok(WritableHandle {
            file,
            directory: DirBuffer::new(),
        })
    }

    fn close(&self, _handle: WritableHandle) {}

    fn cleanup(&self, handle: &WritableHandle, _name: Option<&U16CStr>, flags: u32) {
        // FspCleanupDelete is 1. Cleanup cannot report an error in Windows;
        // validation happens in set_delete, and persistence failures are logged.
        let result = (|| -> winfsp::Result<()> {
            if flags & 1 != 0 {
                let entry = self.entry(handle)?;
                if entry.is_dir {
                    self.overlay.rmdir(&entry.path)
                } else {
                    self.overlay.unlink(&entry.path)
                }
                .map_err(map_error)?;
                self.refresh_after_mutation()?;
            }
            self.overlay.flush(&handle.file).map_err(map_error)
        })();
        if let Err(error) = result {
            eprintln!("PlaySparse overlay cleanup failed: {error:?}");
        }
    }

    fn set_delete(
        &self,
        handle: &WritableHandle,
        _name: &U16CStr,
        delete: bool,
    ) -> winfsp::Result<()> {
        if !delete {
            return Ok(());
        }
        let entry = self.entry(handle)?;
        if entry.path.is_empty() {
            return Err(STATUS_ACCESS_DENIED.into());
        }
        if entry.mode & 0o222 == 0 && !entry.is_dir {
            return Err(STATUS_CANNOT_DELETE.into());
        }
        if entry.is_dir
            && !self
                .overlay
                .list_handle(&handle.file)
                .map_err(map_error)?
                .is_empty()
        {
            return Err(STATUS_DIRECTORY_NOT_EMPTY.into());
        }
        Ok(())
    }

    fn get_file_info(&self, handle: &WritableHandle, out: &mut FileInfo) -> winfsp::Result<()> {
        *out = writable_info(&self.entry(handle)?);
        Ok(())
    }

    fn flush(&self, handle: Option<&WritableHandle>, out: &mut FileInfo) -> winfsp::Result<()> {
        if let Some(handle) = handle {
            self.overlay.flush(&handle.file).map_err(map_error)?;
            *out = writable_info(&self.entry(handle)?);
        } else {
            self.overlay.flush_all().map_err(map_error)?;
        }
        Ok(())
    }

    fn get_volume_info(&self, out: &mut VolumeInfo) -> winfsp::Result<()> {
        let free = system_resources(&self.root)["available_disk_bytes"]
            .as_u64()
            .unwrap_or(0);
        let logical = self
            .overlay
            .entries()
            .map_err(map_error)?
            .iter()
            .fold(0u64, |size, entry| size.saturating_add(entry.size));
        out.total_size = logical.saturating_add(free);
        out.free_size = free;
        out.set_volume_label("PlaySparse");
        Ok(())
    }

    fn read(&self, handle: &WritableHandle, buffer: &mut [u8], offset: u64) -> winfsp::Result<u32> {
        let entry = self.entry(handle)?;
        if entry.is_dir {
            return Err(STATUS_FILE_IS_A_DIRECTORY.into());
        }
        if buffer.is_empty() {
            return Ok(0);
        }
        if offset >= entry.size {
            return Err(STATUS_END_OF_FILE.into());
        }
        let mut copied = 0usize;
        for destination in buffer.chunks_mut(MAX_READ_BYTES) {
            let data = self
                .overlay
                .read(
                    &handle.file,
                    offset
                        .checked_add(copied as u64)
                        .ok_or_else(|| FspError::from(STATUS_INVALID_PARAMETER))?,
                    destination.len(),
                )
                .map_err(map_error)?;
            destination[..data.len()].copy_from_slice(&data);
            copied += data.len();
            if data.len() < destination.len() {
                break;
            }
        }
        u32::try_from(copied).map_err(|_| STATUS_INVALID_PARAMETER.into())
    }

    fn write(
        &self,
        handle: &WritableHandle,
        buffer: &[u8],
        offset: u64,
        append: bool,
        constrained: bool,
        out: &mut FileInfo,
    ) -> winfsp::Result<u32> {
        let entry = self.entry(handle)?;
        if entry.is_dir {
            return Err(STATUS_FILE_IS_A_DIRECTORY.into());
        }
        let buffer = if constrained {
            if append || offset >= entry.size {
                *out = writable_info(&entry);
                return Ok(0);
            }
            &buffer[..buffer
                .len()
                .min((entry.size - offset).min(usize::MAX as u64) as usize)]
        } else {
            buffer
        };
        let start = if append { entry.size } else { offset };
        if start
            .checked_add(buffer.len() as u64)
            .is_none_or(|end| end > i64::MAX as u64)
        {
            return Err(STATUS_FILE_TOO_LARGE.into());
        }
        let mut written = 0usize;
        for chunk in buffer.chunks(MAX_READ_BYTES) {
            let count = self
                .overlay
                .write(
                    &handle.file,
                    offset
                        .checked_add(written as u64)
                        .ok_or_else(|| FspError::from(STATUS_INVALID_PARAMETER))?,
                    chunk,
                    append,
                )
                .map_err(map_error)?;
            written += count;
            if count != chunk.len() {
                break;
            }
        }
        *out = writable_info(&self.entry(handle)?);
        u32::try_from(written).map_err(|_| STATUS_INVALID_PARAMETER.into())
    }

    fn set_file_size(
        &self,
        handle: &WritableHandle,
        size: u64,
        allocation: bool,
        out: &mut FileInfo,
    ) -> winfsp::Result<()> {
        let entry = self.entry(handle)?;
        if entry.is_dir {
            return Err(STATUS_FILE_IS_A_DIRECTORY.into());
        }
        if size > i64::MAX as u64 {
            return Err(STATUS_FILE_TOO_LARGE.into());
        }
        // Allocation growth is advisory; this sparse filesystem does not promise
        // reserved physical space. Reducing allocation truncates logical data.
        if !allocation || size < entry.size {
            self.overlay
                .truncate(&handle.file, size)
                .map_err(map_error)?;
        }
        *out = writable_info(&self.entry(handle)?);
        Ok(())
    }

    fn overwrite(
        &self,
        handle: &WritableHandle,
        attributes: u32,
        replace_attributes: bool,
        _allocation: u64,
        extra: Option<&[u8]>,
        out: &mut FileInfo,
    ) -> winfsp::Result<()> {
        if extra.is_some() {
            return Err(STATUS_NOT_SUPPORTED.into());
        }
        if replace_attributes {
            self.set_basic_info(handle, attributes, 0, 0, 0, 0, out)?;
        }
        self.set_file_size(handle, 0, false, out)
    }

    fn set_basic_info(
        &self,
        handle: &WritableHandle,
        attributes: u32,
        created: u64,
        accessed: u64,
        written: u64,
        changed: u64,
        out: &mut FileInfo,
    ) -> winfsp::Result<()> {
        // V1 CAS does not retain timestamps; explicit timestamp changes must
        // return an error rather than claim they persisted after remount.
        if [created, accessed, written, changed]
            .iter()
            .any(|time| *time != 0)
        {
            return Err(STATUS_NOT_SUPPORTED.into());
        }
        let entry = self.entry(handle)?;
        if attributes != u32::MAX {
            let allowed = (FILE_ATTRIBUTE_NORMAL
                | FILE_ATTRIBUTE_ARCHIVE
                | FILE_ATTRIBUTE_READONLY
                | FILE_ATTRIBUTE_DIRECTORY)
                .0;
            if attributes & !allowed != 0
                || (!entry.is_dir && attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0)
                || (entry.is_dir && attributes & FILE_ATTRIBUTE_ARCHIVE.0 != 0)
                || (!entry.is_dir && attributes & FILE_ATTRIBUTE_ARCHIVE.0 == 0)
            {
                return Err(STATUS_NOT_SUPPORTED.into());
            }
            let mode = if attributes & FILE_ATTRIBUTE_READONLY.0 != 0 {
                entry.mode & !0o222
            } else {
                entry.mode | 0o200
            };
            self.overlay
                .set_mode_handle(&handle.file, mode)
                .map_err(map_error)?;
        }
        *out = writable_info(&self.entry(handle)?);
        Ok(())
    }

    fn rename(
        &self,
        handle: &WritableHandle,
        _name: &U16CStr,
        new_name: &U16CStr,
        replace: bool,
    ) -> winfsp::Result<()> {
        let from = self.entry(handle)?.path;
        let supplied = virtual_path(new_name)?;
        let to = self.new_path(&supplied)?;
        if let Ok(existing) = self.canonical(&to) {
            // A case-only rename changes preserved spelling without replacing
            // the source with itself. Other collisions obey replace-if-exists.
            if existing != from && !replace {
                return Err(STATUS_OBJECT_NAME_COLLISION.into());
            }
            if existing != from {
                self.overlay
                    .rename(&from, &existing, replace)
                    .map_err(map_error)?;
            } else {
                self.overlay.rename(&from, &to, false).map_err(map_error)?;
            }
        } else {
            self.overlay
                .rename(&from, &to, replace)
                .map_err(map_error)?;
        }
        self.refresh_after_mutation()
    }

    fn read_directory(
        &self,
        handle: &WritableHandle,
        _pattern: Option<&U16CStr>,
        marker: DirMarker<'_>,
        buffer: &mut [u8],
    ) -> winfsp::Result<u32> {
        let directory = self.entry(handle)?;
        if !directory.is_dir {
            return Err(STATUS_NOT_A_DIRECTORY.into());
        }
        match handle.directory.acquire(marker.is_none(), None) {
            Ok(lock) => {
                let mut record = DirInfo::<256>::new();
                if !directory.path.is_empty() {
                    record.set_name_raw(&[b'.' as u16][..])?;
                    *record.file_info_mut() = writable_info(&directory);
                    lock.write(&mut record)?;
                    record.reset();
                    record.set_name_raw(&[b'.' as u16, b'.' as u16][..])?;
                    let parent = directory
                        .path
                        .rsplit_once('/')
                        .map_or("", |(parent, _)| parent);
                    *record.file_info_mut() =
                        writable_info(&self.overlay.metadata(parent).map_err(map_error)?);
                    lock.write(&mut record)?;
                }
                let mut children = self.overlay.list_handle(&handle.file).map_err(map_error)?;
                children.sort_by(|left, right| {
                    WindowsName::new(base_name(&left.path))
                        .cmp(&WindowsName::new(base_name(&right.path)))
                });
                for entry in children {
                    record.reset();
                    let name: Vec<u16> = base_name(&entry.path).encode_utf16().collect();
                    record.set_name_raw(name.as_slice())?;
                    *record.file_info_mut() = writable_info(&entry);
                    lock.write(&mut record)?;
                }
            }
            Err(FspError::NTSTATUS(0)) => {}
            Err(error) => return Err(error),
        }
        Ok(handle.directory.read(marker, buffer))
    }

    fn dispatcher_stopped(&self, _normally: bool) {
        // SAFETY: the mount guard retains the event until dispatcher teardown.
        unsafe {
            let _ = SetEvent(HANDLE(self.stop_event as *mut c_void));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use playsparse_store::{PackOptions, pack_directory};

    fn fixture() -> (
        tempfile::TempDir,
        Arc<RangeResolver>,
        Arc<Overlay>,
        WritableFilesystem,
    ) {
        winfsp::winfsp_init().expect("Windows callback tests require installed WinFsp");
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(source.join("Assets")).unwrap();
        std::fs::write(source.join("Assets/base.dat"), b"base").unwrap();
        let store = temp.path().join("store");
        pack_directory(&source, &store, &PackOptions::default()).unwrap();
        let reader = Arc::new(RangeResolver::open(&store, 65536).unwrap());
        let root = temp.path().join("overlay");
        let overlay = Arc::new(Overlay::open(reader.clone(), &root).unwrap());
        let filesystem = WritableFilesystem::new(overlay.clone(), &root, 0).unwrap();
        (temp, reader, overlay, filesystem)
    }

    fn name(path: &str) -> winfsp::U16CString {
        winfsp::U16CString::from_str(path).unwrap()
    }

    fn open_info() -> OpenFileInfo {
        // SAFETY: this repr(C) FFI value consists of integer metadata, a raw
        // nullable pointer and an integer length. Zero length prevents writes
        // to the unused normalized-name pointer in these callback tests.
        unsafe { std::mem::zeroed() }
    }

    #[test]
    fn update_rename_delete_handles_preserve_base_and_remount() {
        let (temp, reader, overlay, filesystem) = fixture();
        let handle = filesystem
            .open(
                &name("\\assets\\BASE.dat"),
                0x40,
                (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0,
                &mut open_info(),
            )
            .unwrap();
        let mut info = FileInfo::default();
        assert_eq!(
            filesystem
                .write(&handle, b"Z", 1, false, false, &mut info)
                .unwrap(),
            1
        );
        assert_eq!(
            filesystem
                .write(&handle, b"!", 0, true, false, &mut info)
                .unwrap(),
            1
        );
        filesystem
            .set_file_size(&handle, 8, false, &mut info)
            .unwrap();
        assert_eq!(info.file_size, 8);
        filesystem
            .rename(
                &handle,
                &name("\\Assets\\base.dat"),
                &name("\\Assets\\patched.dat"),
                false,
            )
            .unwrap();
        filesystem.flush(Some(&handle), &mut info).unwrap();
        let mut bytes = [0; 8];
        assert_eq!(filesystem.read(&handle, &mut bytes, 0).unwrap(), 8);
        assert_eq!(&bytes, b"bZse!\0\0\0");
        assert_eq!(reader.read_range("Assets/base.dat", 0, 4).unwrap(), b"base");
        assert_eq!(
            std::fs::read(temp.path().join("source/Assets/base.dat")).unwrap(),
            b"base"
        );
        filesystem.close(handle);
        drop(filesystem);
        drop(overlay);
        let overlay = Arc::new(Overlay::open(reader, &temp.path().join("overlay")).unwrap());
        let filesystem = WritableFilesystem::new(overlay, &temp.path().join("overlay"), 0).unwrap();
        assert!(filesystem.canonical("Assets/base.dat").is_err());
        let handle = filesystem
            .open(
                &name("\\assets\\PATCHED.dat"),
                0x40,
                FILE_GENERIC_READ.0 | DELETE.0,
                &mut open_info(),
            )
            .unwrap();
        filesystem
            .set_delete(&handle, &name("\\Assets\\patched.dat"), true)
            .unwrap();
        assert!(filesystem.canonical("Assets/patched.dat").is_ok());
        filesystem.cleanup(&handle, Some(&name("\\Assets\\patched.dat")), 1);
        assert!(filesystem.canonical("Assets/patched.dat").is_err());
        assert_eq!(filesystem.read(&handle, &mut bytes, 0).unwrap(), 8);
        assert_eq!(&bytes, b"bZse!\0\0\0");
    }

    #[test]
    fn directory_handles_follow_ancestor_rename_and_reject_nonempty_delete() {
        let (_temp, _reader, overlay, filesystem) = fixture();
        let directory = filesystem
            .create(
                &name("\\New"),
                1,
                FILE_GENERIC_READ.0,
                FILE_ATTRIBUTE_DIRECTORY.0,
                None,
                0,
                None,
                false,
                &mut open_info(),
            )
            .unwrap();
        let file = filesystem
            .create(
                &name("\\new\\file.dat"),
                0x40,
                FILE_GENERIC_WRITE.0,
                FILE_ATTRIBUTE_NORMAL.0,
                None,
                0,
                None,
                false,
                &mut open_info(),
            )
            .unwrap();
        assert!(
            matches!(filesystem.set_delete(&directory, &name("\\New"), true), Err(FspError::NTSTATUS(code)) if code == STATUS_DIRECTORY_NOT_EMPTY.0)
        );
        filesystem
            .rename(&directory, &name("\\New"), &name("\\Moved"), false)
            .unwrap();
        assert_eq!(filesystem.entry(&file).unwrap().path, "Moved/file.dat");
        assert_eq!(
            overlay.list_handle(&directory.file).unwrap()[0].path,
            "Moved/file.dat"
        );
        filesystem.cleanup(&file, Some(&name("\\Moved\\file.dat")), 1);
        filesystem
            .set_delete(&directory, &name("\\Moved"), true)
            .unwrap();
        filesystem.cleanup(&directory, Some(&name("\\Moved")), 1);
        assert!(filesystem.canonical("Moved").is_err());
    }

    #[test]
    fn constrained_writes_and_unsupported_metadata_are_explicit() {
        let (_temp, _reader, _overlay, filesystem) = fixture();
        let handle = filesystem
            .open(
                &name("\\Assets\\base.dat"),
                0x40,
                FILE_GENERIC_WRITE.0,
                &mut open_info(),
            )
            .unwrap();
        let mut info = FileInfo::default();
        assert_eq!(
            filesystem
                .write(&handle, b"abcdef", 3, false, true, &mut info)
                .unwrap(),
            1
        );
        assert_eq!(info.file_size, 4);
        assert_eq!(
            filesystem
                .write(&handle, b"x", 0, true, true, &mut info)
                .unwrap(),
            0
        );
        assert!(
            matches!(filesystem.set_basic_info(&handle, u32::MAX, 1, 0, 0, 0, &mut info), Err(FspError::NTSTATUS(code)) if code == STATUS_NOT_SUPPORTED.0)
        );
        assert!(
            matches!(filesystem.create(&name("\\NUL.txt"), 0x40, FILE_GENERIC_WRITE.0, FILE_ATTRIBUTE_NORMAL.0, None, 0, None, false, &mut open_info()), Err(FspError::NTSTATUS(code)) if code == STATUS_OBJECT_NAME_INVALID.0)
        );
    }
}
