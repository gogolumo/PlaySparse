use anyhow::{Context, Result, bail};
use playsparse_core::{Error, MAX_READ_BYTES, Manifest};
use playsparse_range::RangeResolver;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::*;
use windows::Win32::Globalization::{CSTR_EQUAL, CSTR_LESS_THAN, CompareStringOrdinal};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, SetConsoleCtrlHandler,
};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::*;
use windows::core::{BOOL, PCWSTR, w};
use winfsp::filesystem::{
    DirBuffer, DirInfo, DirMarker, FileInfo, FileSecurity, FileSystemContext, OpenFileInfo,
    VolumeInfo, WideNameInfo,
};
use winfsp::host::{FileSystemHost, VolumeParams};
use winfsp::{FspError, U16CStr};

// Windows ordinal case folding, rather than Rust Unicode lowercase, defines
// both lookup identity and directory ordering. Colliding names are rejected.
#[derive(Clone, Debug)]
struct WindowsName(Vec<u16>);
impl WindowsName {
    fn new(value: &str) -> Self {
        Self(value.encode_utf16().collect())
    }
}
impl PartialEq for WindowsName {
    fn eq(&self, rhs: &Self) -> bool {
        self.cmp(rhs).is_eq()
    }
}
impl Eq for WindowsName {}
impl PartialOrd for WindowsName {
    fn partial_cmp(&self, rhs: &Self) -> Option<Ordering> {
        Some(self.cmp(rhs))
    }
}
impl Ord for WindowsName {
    fn cmp(&self, rhs: &Self) -> Ordering {
        if self.0.is_empty() || rhs.0.is_empty() {
            return self.0.cmp(&rhs.0);
        }
        // SAFETY: slices are initialized UTF-16 and manifest paths are bounded.
        let comparison = unsafe { CompareStringOrdinal(&self.0, &rhs.0, true) };
        if comparison == CSTR_LESS_THAN {
            Ordering::Less
        } else if comparison == CSTR_EQUAL {
            Ordering::Equal
        } else {
            Ordering::Greater
        }
    }
}

#[derive(Debug)]
struct Node {
    path: String,
    size: u64,
    is_dir: bool,
    id: u64,
}
struct Namespace {
    nodes: BTreeMap<WindowsName, Arc<Node>>,
    children: BTreeMap<WindowsName, Vec<Arc<Node>>>,
    logical_size: u64,
}
impl Namespace {
    fn from_manifest(manifest: &Manifest) -> Result<Self> {
        let mut nodes = BTreeMap::new();
        let mut logical_size = 0u64;
        let entries = std::iter::once((String::new(), 0, true))
            .chain(manifest.directories.iter().map(|p| (p.clone(), 0, true)))
            .chain(
                manifest
                    .files
                    .iter()
                    .map(|f| (f.path.clone(), f.size, false)),
            );
        for (index, (path, size, is_dir)) in entries.enumerate() {
            if size > i64::MAX as u64 {
                bail!("Windows signed file-size limit exceeded: {path}");
            }
            for component in path.split('/').filter(|s| !s.is_empty()) {
                if component.encode_utf16().count() > 255
                    || component.ends_with([' ', '.'])
                    || component.contains(['<', '>', '"', '|', '?', '*'])
                {
                    bail!("file name cannot be represented by this Windows backend: {path}");
                }
            }
            // Transaction paths include the virtual root slash and UTF-16 NUL.
            let path_bytes = (path.encode_utf16().count() + 2) * std::mem::size_of::<u16>();
            if path_bytes > winfsp::constants::FSP_FSCTL_TRANSACT_PATH_SIZEMAX {
                bail!("path exceeds WinFsp's transaction path limit: {path}");
            }
            logical_size = logical_size
                .checked_add(size)
                .context("logical volume size overflow")?;
            let key = WindowsName::new(&path);
            let node = Arc::new(Node {
                path: path.clone(),
                size,
                is_dir,
                id: index as u64 + 1,
            });
            if nodes.insert(key, node).is_some() {
                bail!("case-insensitive Windows name collision: {path}");
            }
        }
        let mut children: BTreeMap<WindowsName, Vec<Arc<Node>>> = BTreeMap::new();
        for node in nodes.values().filter(|n| !n.path.is_empty()) {
            let parent = node.path.rsplit_once('/').map_or("", |(p, _)| p);
            children
                .entry(WindowsName::new(parent))
                .or_default()
                .push(node.clone());
        }
        for list in children.values_mut() {
            list.sort_by(|a, b| {
                WindowsName::new(base_name(&a.path)).cmp(&WindowsName::new(base_name(&b.path)))
            });
        }
        Ok(Self {
            nodes,
            children,
            logical_size,
        })
    }
    fn lookup(&self, path: &str) -> winfsp::Result<Arc<Node>> {
        self.nodes
            .get(&WindowsName::new(path))
            .cloned()
            .ok_or_else(|| STATUS_OBJECT_NAME_NOT_FOUND.into())
    }
}
fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}
fn virtual_path(name: &U16CStr) -> winfsp::Result<String> {
    let path = name
        .to_string()
        .map_err(|_| FspError::from(STATUS_OBJECT_NAME_INVALID))?;
    Ok(path.trim_start_matches('\\').replace('\\', "/"))
}
fn info(node: &Node) -> FileInfo {
    FileInfo {
        file_attributes: if node.is_dir {
            FILE_ATTRIBUTE_DIRECTORY.0
        } else {
            FILE_ATTRIBUTE_READONLY.0
        },
        allocation_size: (node.size + 4095) & !4095,
        file_size: node.size,
        index_number: node.id,
        // Version 1 manifests do not preserve source timestamps.
        creation_time: 132223104000000000,
        last_access_time: 132223104000000000,
        last_write_time: 132223104000000000,
        change_time: 132223104000000000,
        ..FileInfo::default()
    }
}

struct Handle {
    node: Arc<Node>,
    directory: DirBuffer,
}
struct CasFilesystem {
    reader: Arc<RangeResolver>,
    namespace: Namespace,
    security: Vec<u8>,
    stop_event: usize,
}
impl CasFilesystem {
    fn security(&self, target: Option<&mut [c_void]>) -> winfsp::Result<u64> {
        let target = target.map(|target| {
            // SAFETY: the FFI descriptor allocation is a mutable byte buffer.
            // This reborrow lasts only for the descriptor copy.
            unsafe {
                std::slice::from_raw_parts_mut(
                    target.as_mut_ptr().cast::<u8>(),
                    std::mem::size_of_val(target),
                )
            }
        });
        copy_security_descriptor(&self.security, target)
    }
}
fn copy_security_descriptor(security: &[u8], target: Option<&mut [u8]>) -> winfsp::Result<u64> {
    if let Some(target) = target {
        if target.len() < security.len() {
            return Err(STATUS_BUFFER_OVERFLOW.into());
        }
        target[..security.len()].copy_from_slice(security);
    }
    Ok(security.len() as u64)
}
impl FileSystemContext for CasFilesystem {
    type FileContext = Handle;

    fn get_security_by_name(
        &self,
        name: &U16CStr,
        descriptor: Option<&mut [c_void]>,
        _resolve: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> winfsp::Result<FileSecurity> {
        let node = self.namespace.lookup(&virtual_path(name)?)?;
        Ok(FileSecurity {
            reparse: false,
            sz_security_descriptor: self.security(descriptor)?,
            attributes: info(&node).file_attributes,
        })
    }
    fn get_security(
        &self,
        _handle: &Handle,
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
    ) -> winfsp::Result<Handle> {
        let node = self.namespace.lookup(&virtual_path(name)?)?;
        let write_access = FILE_WRITE_DATA
            | FILE_APPEND_DATA
            | FILE_WRITE_ATTRIBUTES
            | FILE_WRITE_EA
            | DELETE
            | WRITE_DAC
            | WRITE_OWNER;
        if access & write_access.0 != 0 || options & 0x1000 != 0 {
            // FILE_DELETE_ON_CLOSE
            return Err(STATUS_MEDIA_WRITE_PROTECTED.into());
        }
        if options & 1 != 0 && !node.is_dir {
            return Err(STATUS_NOT_A_DIRECTORY.into());
        }
        if options & 0x40 != 0 && node.is_dir {
            return Err(STATUS_FILE_IS_A_DIRECTORY.into());
        }
        *out.as_mut() = info(&node);
        let normalized: Vec<u16> = format!("\\{}", node.path.replace('/', "\\"))
            .encode_utf16()
            .collect();
        if normalized.len() * 2 <= out.normalized_name_size() as usize {
            out.set_normalized_name(&normalized, None);
        }
        Ok(Handle {
            node,
            directory: DirBuffer::new(),
        })
    }
    fn close(&self, _handle: Handle) {}
    fn get_file_info(&self, handle: &Handle, out: &mut FileInfo) -> winfsp::Result<()> {
        *out = info(&handle.node);
        Ok(())
    }
    fn flush(&self, handle: Option<&Handle>, out: &mut FileInfo) -> winfsp::Result<()> {
        if let Some(handle) = handle {
            *out = info(&handle.node);
        }
        Ok(())
    }
    fn get_volume_info(&self, out: &mut VolumeInfo) -> winfsp::Result<()> {
        out.total_size = self.namespace.logical_size;
        out.free_size = 0;
        out.set_volume_label("PlaySparse");
        Ok(())
    }
    fn read(&self, handle: &Handle, buffer: &mut [u8], offset: u64) -> winfsp::Result<u32> {
        if handle.node.is_dir {
            return Err(STATUS_FILE_IS_A_DIRECTORY.into());
        }
        if buffer.is_empty() {
            return Ok(0);
        }
        if offset >= handle.node.size {
            return Err(STATUS_END_OF_FILE.into());
        }
        let mut copied = 0usize;
        // A driver can request more than the range engine's per-call bound.
        // Split the requested buffer; never hydrate or reconstruct the file.
        for destination in buffer.chunks_mut(MAX_READ_BYTES) {
            let logical_offset = offset
                .checked_add(copied as u64)
                .ok_or_else(|| FspError::from(STATUS_INVALID_PARAMETER))?;
            let data = self
                .reader
                .read_range(&handle.node.path, logical_offset, destination.len())
                .map_err(map_error)?;
            destination[..data.len()].copy_from_slice(&data);
            copied += data.len();
            if data.len() != destination.len() {
                break;
            }
        }
        u32::try_from(copied).map_err(|_| STATUS_INVALID_PARAMETER.into())
    }
    fn read_directory(
        &self,
        handle: &Handle,
        _pattern: Option<&U16CStr>,
        marker: DirMarker<'_>,
        buffer: &mut [u8],
    ) -> winfsp::Result<u32> {
        if !handle.node.is_dir {
            return Err(STATUS_NOT_A_DIRECTORY.into());
        }
        match handle.directory.acquire(marker.is_none(), None) {
            Ok(lock) => {
                let mut entry = DirInfo::<256>::new();
                if !handle.node.path.is_empty() {
                    entry.set_name_raw(&[b'.' as u16][..])?;
                    *entry.file_info_mut() = info(&handle.node);
                    lock.write(&mut entry)?;
                    entry.reset();
                    let parent = handle.node.path.rsplit_once('/').map_or("", |(p, _)| p);
                    entry.set_name_raw(&[b'.' as u16, b'.' as u16][..])?;
                    let parent_node = self.namespace.lookup(parent)?;
                    *entry.file_info_mut() = info(&parent_node);
                    lock.write(&mut entry)?;
                }
                if let Some(children) = self
                    .namespace
                    .children
                    .get(&WindowsName::new(&handle.node.path))
                {
                    for node in children {
                        entry.reset();
                        let name: Vec<u16> = base_name(&node.path).encode_utf16().collect();
                        entry.set_name_raw(name.as_slice())?;
                        *entry.file_info_mut() = info(node);
                        lock.write(&mut entry)?;
                    }
                }
            }
            // Acquire returns STATUS_SUCCESS without a lock if already filled.
            Err(FspError::NTSTATUS(0)) => {}
            Err(error) => return Err(error),
        }
        Ok(handle.directory.read(marker, buffer))
    }
    fn dispatcher_stopped(&self, _normally: bool) {
        // SAFETY: the event is kept open until after host dispatcher teardown.
        unsafe {
            let _ = SetEvent(HANDLE(self.stop_event as *mut c_void));
        }
    }
}
fn map_error(error: Error) -> FspError {
    eprintln!("PlaySparse CAS read failed: {error}");
    match error {
        Error::Io(e) => e.into(),
        Error::NotFound(_) => STATUS_OBJECT_NAME_NOT_FOUND.into(),
        Error::Corrupt(_) | Error::Invalid(_) => STATUS_DATA_ERROR.into(),
        Error::ReadTooLarge => STATUS_INVALID_PARAMETER.into(),
    }
}

fn readonly_security() -> Result<Vec<u8>> {
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let mut length = 0;
    // SAFETY: Windows allocates a self-relative descriptor; copy it before
    // releasing the LocalAlloc allocation. Everyone receives read/execute.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            // AccessCheck consumes file-specific ACE masks; generic GR/GX
            // bits in a supplied descriptor are not mapped by the filesystem.
            w!("O:SYG:SYD:(A;;FRFX;;;WD)"),
            SDDL_REVISION_1,
            &mut descriptor,
            Some(&mut length),
        )?;
        let bytes = std::slice::from_raw_parts(descriptor.0.cast::<u8>(), length as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
        Ok(bytes)
    }
}

static CONSOLE_EVENT: Mutex<usize> = Mutex::new(0);
unsafe extern "system" fn console_handler(event: u32) -> BOOL {
    if matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT) {
        let handle = CONSOLE_EVENT.lock().unwrap_or_else(|e| e.into_inner());
        if *handle != 0 {
            // SAFETY: registration guard holds this event open.
            unsafe {
                let _ = SetEvent(HANDLE(*handle as *mut c_void));
            }
            return BOOL(1);
        }
    }
    BOOL(0)
}
struct StopEvent(HANDLE);
impl Drop for StopEvent {
    fn drop(&mut self) {
        // SAFETY: this object uniquely owns the open event handle.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
struct ConsoleRegistration;
impl Drop for ConsoleRegistration {
    fn drop(&mut self) {
        unsafe {
            let _ = SetConsoleCtrlHandler(Some(console_handler), false);
        }
        *CONSOLE_EVENT.lock().unwrap_or_else(|e| e.into_inner()) = 0;
    }
}

fn event_name(mountpoint: &Path) -> Result<Vec<u16>> {
    let supplied: Vec<u16> = mountpoint
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let text = mountpoint.to_string_lossy();
    let full = if text.len() == 2 && text.ends_with(':') {
        text.into_owned()
    } else {
        let mut path = vec![0u16; 32768];
        let count =
            unsafe { GetFullPathNameW(PCWSTR(supplied.as_ptr()), Some(&mut path), None) } as usize;
        if count == 0 || count >= path.len() {
            bail!("cannot normalize mountpoint path");
        }
        String::from_utf16(&path[..count]).context("invalid UTF-16 mountpoint")?
    };
    let normalized = full
        .trim_end_matches(['\\', '/'])
        .replace('/', "\\")
        .to_uppercase();
    let digest = blake3::hash(normalized.as_bytes());
    Ok(format!("Local\\PlaySparseUnmount.{}", digest.to_hex())
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect())
}
fn completion_name(mut name: Vec<u16>) -> Vec<u16> {
    name.pop();
    name.extend(".Done".encode_utf16());
    name.push(0);
    name
}

/// Query real Windows resource counters; failed measurements are JSON null.
pub fn system_resources(path: &Path) -> serde_json::Value {
    let mut memory = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    let available_ram = unsafe { GlobalMemoryStatusEx(&mut memory) }
        .ok()
        .map(|_| memory.ullAvailPhys);
    let mut disk_path = path.to_path_buf();
    while !disk_path.exists() && disk_path.pop() {}
    if disk_path.as_os_str().is_empty() {
        disk_path = std::env::current_dir().unwrap_or_else(|_| ".".into());
    }
    if disk_path.is_file() {
        disk_path.pop();
    }
    let wide: Vec<u16> = disk_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut disk_bytes = 0u64;
    let available_disk =
        unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut disk_bytes), None, None) }
            .ok()
            .map(|_| disk_bytes);
    let elevated = (|| -> windows::core::Result<bool> {
        let mut handle = HANDLE::default();
        unsafe {
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle)?;
        }
        let token = StopEvent(handle);
        let mut elevation = TOKEN_ELEVATION::default();
        let mut size = 0;
        unsafe {
            GetTokenInformation(
                token.0,
                TokenElevation,
                Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut size,
            )?;
        }
        Ok(elevation.TokenIsElevated != 0)
    })()
    .ok();
    serde_json::json!({
        "available_ram_bytes": available_ram,
        "available_disk_bytes": available_disk,
        "disk_measurement_path": disk_path,
        "elevated": elevated,
        "winfsp": availability(),
    })
}

pub fn availability() -> String {
    match winfsp::winfsp_init() {
        Ok(_) => "WinFsp DLL loaded; driver mounting still requires a runtime mount test".into(),
        Err(error) => {
            format!("WinFsp unavailable ({error:?}); install WinFsp 2.1+ with its driver")
        }
    }
}
/// Serve the CAS directly until Ctrl+C or an unmount command signals shutdown.
pub fn mount(store: &Path, mountpoint: &Path, cache_bytes: usize) -> Result<()> {
    let _init = winfsp::winfsp_init()
        .context("WinFsp is not installed; install WinFsp 2.1+ including its driver")?;
    let reader = Arc::new(RangeResolver::open(store, cache_bytes)?);
    let namespace = Namespace::from_manifest(reader.manifest())?;
    let name = event_name(mountpoint)?;
    let stop = StopEvent(unsafe { CreateEventW(None, true, false, PCWSTR(name.as_ptr()))? });
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        bail!("a PlaySparse mount already owns this mountpoint");
    }
    let done_name = completion_name(name);
    let done = StopEvent(unsafe { CreateEventW(None, true, false, PCWSTR(done_name.as_ptr()))? });
    {
        let mut console = CONSOLE_EVENT.lock().unwrap_or_else(|e| e.into_inner());
        if *console != 0 {
            bail!("only one WinFsp mount per host process is supported");
        }
        *console = stop.0.0 as usize;
    }
    if let Err(error) = unsafe { SetConsoleCtrlHandler(Some(console_handler), true) } {
        *CONSOLE_EVENT.lock().unwrap_or_else(|e| e.into_inner()) = 0;
        return Err(error.into());
    }
    let _console = ConsoleRegistration;
    let filesystem = CasFilesystem {
        reader: reader.clone(),
        namespace,
        security: readonly_security()?,
        stop_event: stop.0.0 as usize,
    };
    let mut volume = VolumeParams::new();
    volume
        .sector_size(512)
        .sectors_per_allocation_unit(8)
        .max_component_length(255)
        .filesystem_name("PlaySparse")
        .read_only_volume(true)
        .case_sensitive_search(false)
        .case_preserved_names(true)
        .unicode_on_disk(true)
        .persistent_acls(false)
        .file_info_timeout(u32::MAX)
        .pass_query_directory_pattern(false)
        .irp_timeout(60000)
        .irp_capacity(1000);
    let mut host: FileSystemHost<CasFilesystem> = FileSystemHost::new(volume, filesystem)?;
    host.mount(mountpoint).context(
        "mount failed (choose an unused drive letter or supported directory mountpoint)",
    )?;
    host.start_with_threads(0)
        .context("WinFsp dispatcher failed to start")?;
    eprintln!(
        "PlaySparse mounted {} from compressed CAS; Ctrl+C or playsparse unmount stops it",
        mountpoint.display()
    );
    eprintln!(
        "WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos; https://github.com/winfsp/winfsp"
    );
    let wait = unsafe { WaitForSingleObject(stop.0, INFINITE) };
    host.unmount();
    host.stop();
    drop(host);
    unsafe {
        SetEvent(done.0)?;
    }
    eprintln!("PlaySparse cache metrics: {:?}", reader.metrics());
    if wait == WAIT_FAILED {
        bail!("waiting for unmount failed: {:?}", unsafe {
            GetLastError()
        });
    }
    Ok(())
}
pub fn unmount(mountpoint: &Path) -> Result<()> {
    let name = event_name(mountpoint)?;
    let event = StopEvent(
        unsafe { OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(name.as_ptr())) }
            .context("no PlaySparse host owns this mountpoint in the current Windows session")?,
    );
    let done_name = completion_name(name);
    let done = StopEvent(
        unsafe {
            OpenEventW(
                SYNCHRONIZATION_SYNCHRONIZE,
                false,
                PCWSTR(done_name.as_ptr()),
            )
        }
        .context("mount host is still initializing or has exited")?,
    );
    unsafe {
        SetEvent(event.0)?;
    }
    match unsafe { WaitForSingleObject(done.0, 60_000) } {
        WAIT_OBJECT_0 => {}
        WAIT_TIMEOUT => bail!("mount host did not finish unmounting within 60 seconds"),
        _ => bail!("waiting for mount teardown failed: {:?}", unsafe {
            GetLastError()
        }),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use playsparse_core::{Chunker, Layout};
    use playsparse_store::{PackOptions, pack_directory};

    fn fixture() -> (tempfile::TempDir, Arc<RangeResolver>) {
        winfsp::winfsp_init().expect("Windows callback tests require an installed WinFsp DLL");
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(source.join("Assets")).unwrap();
        std::fs::write(
            source.join("Assets/World.dat"),
            (0..20000).map(|i| (i / 4096) as u8).collect::<Vec<_>>(),
        )
        .unwrap();
        let store = temp.path().join("store");
        pack_directory(
            &source,
            &store,
            &PackOptions {
                chunk_size: 4096,
                chunker: Chunker::Fixed,
                layout: Layout::Packs,
                ..Default::default()
            },
        )
        .unwrap();
        let reader = Arc::new(RangeResolver::open(&store, 65536).unwrap());
        (temp, reader)
    }
    #[test]
    fn case_insensitive_lookup_retains_source_path() {
        let (_temp, reader) = fixture();
        let names = Namespace::from_manifest(reader.manifest()).unwrap();
        assert_eq!(
            names.lookup("assets/world.DAT").unwrap().path,
            "Assets/World.dat"
        );
        assert!(names.lookup("assets/missing").is_err());
        assert!(names.lookup("").unwrap().is_dir);
    }
    #[test]
    fn reject_case_collisions_before_mount() {
        let (_temp, reader) = fixture();
        let mut manifest = reader.manifest().clone();
        let mut duplicate = manifest.files[0].clone();
        duplicate.path = "assets/world.dat".into();
        manifest.files.push(duplicate);
        assert!(Namespace::from_manifest(&manifest).is_err());
    }
    #[test]
    fn read_callback_only_loads_intersecting_objects() {
        let (_temp, reader) = fixture();
        let filesystem = CasFilesystem {
            namespace: Namespace::from_manifest(reader.manifest()).unwrap(),
            reader: reader.clone(),
            security: vec![],
            stop_event: 0,
        };
        let handle = Handle {
            node: filesystem.namespace.lookup("assets/world.dat").unwrap(),
            directory: DirBuffer::new(),
        };
        let mut bytes = [0; 2];
        assert_eq!(filesystem.read(&handle, &mut bytes, 4095).unwrap(), 2);
        assert_eq!(bytes, [0, 1]);
        assert_eq!(reader.metrics().decompressions, 2);
        let mut tail = [0; 4];
        assert_eq!(filesystem.read(&handle, &mut tail, 19998).unwrap(), 2);
        assert_eq!(&tail[..2], &[4, 4]);
        assert_eq!(filesystem.read(&handle, &mut [], u64::MAX).unwrap(), 0);
        assert!(
            matches!(filesystem.read(&handle, &mut bytes, 20000), Err(FspError::NTSTATUS(code)) if code == STATUS_END_OF_FILE.0)
        );
    }

    #[test]
    fn metadata_keeps_offsets_above_eight_gib() {
        let node = Node {
            path: "large.dat".into(),
            size: (10u64 << 30) + 17,
            is_dir: false,
            id: 42,
        };
        let metadata = info(&node);
        assert_eq!(metadata.file_size, (10u64 << 30) + 17);
        assert_eq!(metadata.allocation_size, (10u64 << 30) + 4096);
        assert_eq!(metadata.index_number, 42);
    }

    #[test]
    fn undersized_security_buffer_returns_overflow_without_modification() {
        let descriptor = [1, 2, 3, 4];
        assert_eq!(copy_security_descriptor(&descriptor, None).unwrap(), 4);
        let mut short = [0xaa; 3];
        assert!(
            matches!(copy_security_descriptor(&descriptor, Some(&mut short)), Err(FspError::NTSTATUS(code)) if code == STATUS_BUFFER_OVERFLOW.0)
        );
        assert_eq!(short, [0xaa; 3]);
        let mut exact = [0; 4];
        assert_eq!(
            copy_security_descriptor(&descriptor, Some(&mut exact)).unwrap(),
            4
        );
        assert_eq!(exact, descriptor);
    }

    #[test]
    fn reject_paths_that_exceed_driver_transaction_limit() {
        let (_temp, reader) = fixture();
        let mut manifest = reader.manifest().clone();
        manifest.directories.clear();
        let mut file = manifest.files[0].clone();
        // Each component fits, but the whole path plus slash/NUL does not.
        file.path = vec!["x".repeat(255); 4].join("/");
        manifest.files = vec![file];
        assert!(Namespace::from_manifest(&manifest).is_err());
    }
}
