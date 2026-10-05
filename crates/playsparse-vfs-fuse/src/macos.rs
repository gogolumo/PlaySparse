//! macFUSE 5.3.3+ kernel transport via the supported libfuse channel API.
//!
//! fuser 0.18's mount helper calls fuse_mount_compat25, disabled by macFUSE 5.3.
//! Keep fuser's protocol/dispatch engine, but supply a duplicated device fd.
//! FSKit has no device fd and requires a different transport; it is unsupported.
#[cfg(test)]
use fuser::BackgroundSession;
use fuser::{Config, Filesystem, MountOption, Session};
use std::{
    ffi::{CString, c_char, c_int, c_void},
    io,
    os::{
        fd::{FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::Path,
    ptr::{self, NonNull},
};

pub(super) fn runtime_version() -> io::Result<String> {
    let facts = crate::macos_diagnostic::inspect(true);
    if !facts.version_supported {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Install macFUSE 5.3.3 or newer in the 5.x series; this kernel transport requires its public borrowed-fd API",
        ));
    }
    facts
        .driver_version
        .ok_or_else(|| io::Error::other("macFUSE version unavailable"))
}

#[repr(C)]
struct FuseArgs {
    argc: c_int,
    argv: *mut *mut c_char,
    allocated: c_int,
}

#[link(name = "fuse")]
unsafe extern "C" {
    fn fuse_mount(mountpoint: *const c_char, args: *mut FuseArgs) -> *mut c_void;
    fn fuse_chan_fd(channel: *mut c_void) -> c_int;
    fn fuse_chan_destroy(channel: *mut c_void);
    fn fuse_opt_free_args(args: *mut FuseArgs);
}

fn last_error(context: &'static str) -> io::Error {
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(0) {
        io::Error::other(context)
    } else {
        io::Error::new(error.kind(), format!("{context}: {error}"))
    }
}

fn option_string(config: &Config) -> io::Result<String> {
    // Only the options used by this backend are accepted; no implicit FSKit or
    // arbitrary custom option can switch transport beneath the raw fd reader.
    let mut options = Vec::new();
    for option in &config.mount_options {
        let value = match option {
            MountOption::RO => "ro".into(),
            MountOption::RW => "rw".into(),
            MountOption::Exec => "exec".into(),
            MountOption::NoSuid => "nosuid".into(),
            MountOption::NoDev => "nodev".into(),
            MountOption::NoAtime => "noatime".into(),
            MountOption::DefaultPermissions => "default_permissions".into(),
            MountOption::FSName(name) => format!("fsname={name}"),
            MountOption::Subtype(name) => format!("subtype={name}"),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "unsupported macFUSE mount option",
                ));
            }
        };
        if value.contains(['\0', ',']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid macFUSE mount option value",
            ));
        }
        options.push(value);
    }
    if config.acl != fuser::SessionACL::Owner {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "macFUSE mounts currently require the owner ACL",
        ));
    }
    Ok(options.join(","))
}

fn duplicate_fd(fd: c_int) -> io::Result<OwnedFd> {
    // SAFETY: fcntl duplicates a borrowed fd without transferring its ownership.
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        return Err(last_error("duplicate macFUSE device descriptor"));
    }
    // SAFETY: successful F_DUPFD_CLOEXEC returns a fresh, exclusively owned fd.
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

struct Channel {
    pointer: NonNull<c_void>,
    mountpoint: CString,
}
impl Channel {
    fn open(mountpoint: &Path, config: &Config) -> io::Result<(Self, OwnedFd)> {
        runtime_version()?;
        let mountpoint = mountpoint.canonicalize()?;
        let mountpoint = CString::new(mountpoint.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "mountpoint contains NUL"))?;
        let values = [
            CString::new("playsparse").unwrap(),
            CString::new("-o").unwrap(),
            CString::new(option_string(config)?).unwrap(),
        ];
        let mut argv: Vec<_> = values
            .iter()
            .map(|value| value.as_ptr().cast_mut())
            .collect();
        argv.push(ptr::null_mut());
        let mut args = FuseArgs {
            argc: values.len() as c_int,
            argv: argv.as_mut_ptr(),
            allocated: 0,
        };
        // SAFETY: all arguments remain alive for the synchronous call. libfuse
        // owns any replacement argument array, which fuse_opt_free_args frees.
        let pointer = unsafe { fuse_mount(mountpoint.as_ptr(), &mut args) };
        let error = last_error("macFUSE channel mount failed");
        unsafe { fuse_opt_free_args(&mut args) };
        let channel = Self {
            pointer: NonNull::new(pointer).ok_or(error)?,
            mountpoint,
        };
        // SAFETY: channel retains the original owning reference. This call may
        // wait for asynchronous mount setup and returns a borrowed device fd.
        let fd = unsafe { fuse_chan_fd(channel.pointer.as_ptr()) };
        if fd < 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "macFUSE kernel device unavailable; use approved macFUSE 5.3.3+ kernel backend. FSKit is unsupported: {}",
                    io::Error::last_os_error()
                ),
            ));
        }
        let owned = duplicate_fd(fd)?;
        Ok((channel, owned))
    }
    fn unmount(&self) -> io::Result<()> {
        // Use the ordinary OS operation, not fuse_unmount's version-dependent
        // channel ownership. No force/lazy unmount and no security-policy change.
        let result = unsafe { libc::unmount(self.mountpoint.as_ptr(), 0) };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOENT)) {
            Ok(()) // A user may have already unmounted the volume.
        } else {
            Err(error)
        }
    }
}
impl Drop for Channel {
    fn drop(&mut self) {
        if let Err(error) = self.unmount() {
            eprintln!("macFUSE ordinary unmount failed: {error}");
        }
        // SAFETY: exactly one original fuse_mount reference is released here.
        // macFUSE 5.3.3+ callbacks retain their own references while active.
        unsafe { fuse_chan_destroy(self.pointer.as_ptr()) };
    }
}

pub(super) fn mount<FS: Filesystem>(
    filesystem: FS,
    path: &Path,
    config: &Config,
) -> io::Result<()> {
    let (_channel, fd) = Channel::open(path, config)?;
    Session::from_fd(filesystem, fd, config.acl, config.clone())?.run()
}

#[cfg(test)]
pub(super) struct MountedSession {
    channel: Channel,
    session: Option<BackgroundSession>,
}
#[cfg(test)]
impl MountedSession {
    pub(super) fn umount_and_join(mut self) -> io::Result<()> {
        self.channel.unmount()?;
        self.session.take().unwrap().join()
    }
}
#[cfg(test)]
pub(super) fn spawn<FS: Filesystem>(
    filesystem: FS,
    path: &Path,
    config: &Config,
) -> io::Result<MountedSession> {
    let (channel, fd) = Channel::open(path, config)?;
    let session = Session::from_fd(filesystem, fd, config.acl, config.clone())?.spawn()?;
    Ok(MountedSession {
        channel,
        session: Some(session),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macos_diagnostic::supported_version;
    use std::os::fd::AsRawFd;
    #[test]
    fn duplicate_preserves_channel_owned_descriptor() {
        let original = tempfile::tempfile().unwrap();
        let fd = original.as_raw_fd();
        let duplicate = duplicate_fd(fd).unwrap();
        assert_ne!(duplicate.as_raw_fd(), fd);
        assert_ne!(
            unsafe { libc::fcntl(duplicate.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
        drop(duplicate);
        assert_ne!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
        assert!(duplicate_fd(-1).is_err());
    }
    #[test]
    fn rejects_transport_switch_and_option_injection() {
        let mut config = crate::fuse::mount_config();
        assert!(
            option_string(&config)
                .unwrap()
                .contains("default_permissions")
        );
        config
            .mount_options
            .push(MountOption::CUSTOM("backend=fskit".into()));
        assert!(option_string(&config).is_err());
        config.mount_options.pop();
        config
            .mount_options
            .push(MountOption::FSName("name,allow_other".into()));
        assert!(option_string(&config).is_err());
    }
    #[test]
    fn requires_supported_channel_ownership_and_descriptor_api() {
        for version in [
            "4.8.3", "4.9.0", "5.2.0", "5.3.0", "5.3.2", "6.0.0", "", "5.4.x",
        ] {
            assert!(!supported_version(version), "{version}");
        }
        for version in ["5.3.3", "5.4.0", "5.10.0"] {
            assert!(supported_version(version), "{version}");
        }
    }
}
