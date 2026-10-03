//! No-follow creation/opening of a writable directory and each of its ancestors.
use crate::{Error, Result};
use std::{
    fs::{File, OpenOptions},
    path::{Component, Path, PathBuf},
};

#[cfg(windows)]
use std::fs;

pub struct DirectoryAnchor {
    path: PathBuf,
    file: File,
    // On Windows child operations use full paths. Keep every ancestor open
    // without delete sharing so none can be renamed into a path redirect.
    #[cfg(windows)]
    _parents: Vec<File>,
}

fn refused(message: &str) -> Error {
    std::io::Error::new(std::io::ErrorKind::PermissionDenied, message).into()
}

impl DirectoryAnchor {
    pub fn open(path: &Path, create: bool) -> Result<Self> {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        if !absolute.is_absolute()
            || absolute
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(refused(
                "writable directory requires an absolute path without traversal",
            ));
        }
        let mut root = PathBuf::new();
        let mut names = Vec::new();
        for component in absolute.components() {
            match component {
                Component::Prefix(_) | Component::RootDir => root.push(component),
                Component::Normal(name) => names.push(name.to_os_string()),
                Component::CurDir => {}
                Component::ParentDir => unreachable!("parent traversal was rejected"),
            }
        }
        let mut anchor = Self {
            file: open_directory(&root)?,
            path: root,
            #[cfg(windows)]
            _parents: Vec::new(),
        };
        for name in names {
            anchor = anchor.open_child(&name, create)?;
        }
        // Ancestors are locked against rename on Windows, so canonicalization
        // here cannot follow an ancestor substituted after validation. Keep
        // Unix operations anchored to descriptors and never canonicalize a
        // potentially redirected user path after opening it.
        #[cfg(windows)]
        {
            anchor.path = fs::canonicalize(&anchor.path)?;
        }
        Ok(anchor)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    pub fn child(&self, name: &str, create: bool) -> Result<Self> {
        if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', ':', '\0']) {
            return Err(refused("invalid writable directory child name"));
        }
        #[cfg(unix)]
        {
            self.open_child(std::ffi::OsStr::new(name), create)
        }
        #[cfg(windows)]
        {
            // Retain an independent ancestor chain for this child's lifetime.
            Self::open(&self.path.join(name), create)
        }
    }

    #[cfg(unix)]
    fn open_child(&self, name: &std::ffi::OsStr, create: bool) -> Result<Self> {
        use std::os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        };
        let opaque = std::ffi::CString::new(name.as_bytes())
            .map_err(|_| refused("NUL in writable directory path"))?;
        if create {
            // SAFETY: creation is relative to the retained real parent and
            // receives one component. An existing symlink is never followed.
            if unsafe { libc::mkdirat(self.file.as_raw_fd(), opaque.as_ptr(), 0o700) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(error.into());
                }
            }
        }
        // SAFETY: each single-component open is relative to the previous
        // directory descriptor. O_NOFOLLOW applies at every ancestor step.
        let descriptor = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                opaque.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self {
            path: self.path.join(name),
            file: unsafe { File::from_raw_fd(descriptor) },
        })
    }

    #[cfg(windows)]
    fn open_child(mut self, name: &std::ffi::OsStr, create: bool) -> Result<Self> {
        let child = self.path.join(name);
        if create {
            match fs::create_dir(&child) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        let next = open_directory(&child)?;
        let parent = std::mem::replace(&mut self.file, next);
        self._parents.push(parent);
        self.path = child;
        Ok(self)
    }
}

fn open_directory(path: &Path) -> Result<File> {
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
        options.custom_flags(0x02000000 | 0x00200000).share_mode(3);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(refused("reparse point in writable directory path"));
        }
    }
    if !metadata.is_dir() {
        return Err(refused("writable directory must be a real directory"));
    }
    Ok(file)
}
