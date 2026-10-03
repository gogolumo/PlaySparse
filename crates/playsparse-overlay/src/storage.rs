//! Opaque names, no-follow opens, and directory-relative operations on Unix.
use playsparse_core::{Error, Result};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

pub(crate) struct Directory {
    pub(crate) path: PathBuf,
    _file: File,
}

fn refused(message: &str) -> Error {
    std::io::Error::new(std::io::ErrorKind::PermissionDenied, message).into()
}

pub(crate) fn reject_links(path: &Path) -> Result<()> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut prefix = PathBuf::new();
    for component in absolute.components() {
        if matches!(component, Component::ParentDir) {
            return Err(refused("parent traversal in overlay storage path"));
        }
        prefix.push(component);
        // A Windows prefix (including canonical \\?\C:) is not a complete
        // filesystem path until the RootDir component is appended. Querying
        // that partial prefix fails with ERROR_INVALID_FUNCTION on Windows.
        if matches!(component, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        match fs::symlink_metadata(&prefix) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || is_reparse(&metadata) {
                    return Err(refused("symlink/reparse point in overlay storage path"));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
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

impl Directory {
    pub(crate) fn open(path: &Path, create: bool) -> Result<Self> {
        reject_links(path)?;
        if create {
            fs::create_dir_all(path)?;
        }
        let path = fs::canonicalize(path)?;
        reject_links(&path)?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Retain the directory identity while this overlay is open. Denying
            // delete sharing prevents renaming it through an external handle.
            options.custom_flags(0x02000000 | 0x00200000).share_mode(3);
        }
        let file = options.open(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_dir() || is_reparse(&metadata) {
            return Err(refused("overlay storage must be a real directory"));
        }
        Ok(Self { path, _file: file })
    }

    pub(crate) fn child(&self, name: &str, create: bool) -> Result<Self> {
        checked_name(name)?;
        #[cfg(unix)]
        {
            use std::os::fd::{AsRawFd, FromRawFd};
            let name = std::ffi::CString::new(name).map_err(|_| refused("invalid storage name"))?;
            if create {
                // SAFETY: the retained descriptor anchors this directory; the
                // C string is live. No user path is passed to this operation.
                if unsafe { libc::mkdirat(self._file.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(error.into());
                    }
                }
            }
            // SAFETY: openat never follows the child symlink, and the returned
            // descriptor is uniquely transferred to File below.
            let descriptor = unsafe {
                libc::openat(
                    self._file.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if descriptor < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let file = unsafe { File::from_raw_fd(descriptor) };
            Ok(Self {
                path: self.path.join(name.to_string_lossy().as_ref()),
                _file: file,
            })
        }
        #[cfg(not(unix))]
        {
            Self::open(&self.path.join(name), create)
        }
    }

    pub(crate) fn open_file(&self, name: &str, create_new: bool, write: bool) -> Result<File> {
        checked_name(name)?;
        #[cfg(unix)]
        let file = {
            use std::os::fd::{AsRawFd, FromRawFd};
            let name = std::ffi::CString::new(name).map_err(|_| refused("invalid storage name"))?;
            // Nonblocking open prevents a malicious FIFO from stalling before
            // the regular-file check. Regular files ignore O_NONBLOCK.
            let mut flags = libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
            flags |= if write { libc::O_RDWR } else { libc::O_RDONLY };
            if create_new {
                flags |= libc::O_CREAT | libc::O_EXCL;
            }
            // SAFETY: the retained directory descriptor and name are valid;
            // an exclusively created file has owner-only permissions.
            let descriptor =
                unsafe { libc::openat(self._file.as_raw_fd(), name.as_ptr(), flags, 0o600) };
            if descriptor < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            unsafe { File::from_raw_fd(descriptor) }
        };
        #[cfg(not(unix))]
        let file = {
            reject_links(&self.path)?;
            let mut options = OpenOptions::new();
            options.read(true).write(write).create_new(create_new);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.custom_flags(0x00200000);
            }
            options.open(self.path.join(name))?
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() || is_reparse(&metadata) {
            return Err(refused(
                "overlay object must be a regular file, never a link",
            ));
        }
        Ok(file)
    }

    pub(crate) fn read(&self, name: &str, limit: u64) -> Result<Vec<u8>> {
        let mut file = self.open_file(name, false, false)?;
        if file.metadata()?.len() > limit {
            return Err(Error::Corrupt("overlay metadata exceeds size limit".into()));
        }
        let mut data = Vec::new();
        Read::by_ref(&mut file)
            .take(limit + 1)
            .read_to_end(&mut data)?;
        if data.len() as u64 > limit {
            return Err(Error::Corrupt(
                "overlay metadata grew beyond size limit".into(),
            ));
        }
        Ok(data)
    }

    pub(crate) fn write_new(&self, name: &str, data: &[u8]) -> Result<()> {
        let mut file = self.open_file(name, true, true)?;
        file.write_all(data)?;
        file.sync_all()?;
        Ok(())
    }

    pub(crate) fn rename(&self, from: &str, to: &str) -> Result<()> {
        checked_name(from)?;
        checked_name(to)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let from = std::ffi::CString::new(from).map_err(|_| refused("invalid storage name"))?;
            let to = std::ffi::CString::new(to).map_err(|_| refused("invalid storage name"))?;
            // SAFETY: both names and retained directory descriptors are valid.
            // Replacing a symlink replaces its directory entry, not its target.
            if unsafe {
                libc::renameat(
                    self._file.as_raw_fd(),
                    from.as_ptr(),
                    self._file.as_raw_fd(),
                    to.as_ptr(),
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            reject_links(&self.path)?;
            fs::rename(self.path.join(from), self.path.join(to))?;
            Ok(())
        }
    }

    pub(crate) fn remove(&self, name: &str) -> Result<()> {
        checked_name(name)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let name = std::ffi::CString::new(name).map_err(|_| refused("invalid storage name"))?;
            // SAFETY: unlinkat removes the anchored entry, never a link target.
            if unsafe { libc::unlinkat(self._file.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            reject_links(&self.path)?;
            fs::remove_file(self.path.join(name))?;
            Ok(())
        }
    }

    pub(crate) fn names(&self) -> Result<Vec<String>> {
        // Names are subsequently validated and opened/unlinked relative to our
        // retained descriptor; an externally renamed path cannot cause escape.
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.path)? {
            names.push(
                entry?
                    .file_name()
                    .into_string()
                    .map_err(|_| refused("non-UTF8 entry in overlay storage"))?,
            );
        }
        names.sort();
        Ok(names)
    }

    pub(crate) fn sync(&self) -> Result<()> {
        #[cfg(unix)]
        self._file.sync_all()?;
        Ok(())
    }
}

fn checked_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', ':', '\0']) {
        return Err(refused("invalid opaque overlay storage name"));
    }
    Ok(())
}

pub(crate) fn allocated(file: &File) -> Result<Option<u64>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(Some(file.metadata()?.blocks().saturating_mul(512)))
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Ok(None)
    }
}
