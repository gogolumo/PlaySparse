//! On-demand FUSE backend with an optional persistent writable overlay.
//!
//! Linux uses fuser's native Rust mount implementation. macOS mounting is an
//! explicit `macfuse` feature because it needs the installed macFUSE driver.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Result, bail};
use serde::Serialize;

#[cfg(any(target_os = "linux", all(target_os = "macos", feature = "macfuse")))]
mod fuse;

#[cfg(any(target_os = "linux", all(target_os = "macos", feature = "macfuse")))]
mod writable;

/// A runtime diagnostic, not evidence that a mount has passed IO tests.
#[derive(Debug, Clone, Serialize)]
pub struct Availability {
    pub backend: &'static str,
    pub available: bool,
    pub detail: String,
}

/// Report whether this binary can attempt a genuine mount on this machine.
pub fn availability() -> Availability {
    #[cfg(any(target_os = "linux", all(target_os = "macos", feature = "macfuse")))]
    {
        fuse::availability()
    }
    #[cfg(all(target_os = "macos", not(feature = "macfuse")))]
    {
        Availability {
            backend: "macFUSE",
            available: false,
            detail: "Mount support was not compiled. Install macFUSE, then rebuild with --features macfuse.".into(),
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Availability {
            backend: "FUSE",
            available: false,
            detail: "This development backend supports Linux and macOS. Use the Windows backend on Windows.".into(),
        }
    }
}

/// Mount a store in the foreground, returning after it is unmounted.
///
/// The mountpoint must already exist and be empty. File content is served from
/// compressed CAS chunks by the range resolver; no files are extracted.
pub fn mount(store: &Path, mountpoint: &Path, cache_bytes: usize) -> Result<()> {
    mount_with_options(store, mountpoint, cache_bytes, None, None)
}

/// Mount the immutable base with an optional persistent writable overlay and
/// bounded asynchronous access trace. Without an overlay the volume is read-only.
pub fn mount_with_options(
    store: &Path,
    mountpoint: &Path,
    cache_bytes: usize,
    overlay: Option<&Path>,
    trace: Option<Arc<playsparse_trace::TraceWriter>>,
) -> Result<()> {
    mount_configured(
        store,
        mountpoint,
        overlay,
        playsparse_range::RuntimeOptions {
            cache_bytes,
            trace,
            ..Default::default()
        },
    )
}

/// Mount with a shared cache/policy/tier configuration, preserving identical
/// resolver behavior for the read-only view and writable overlay fallback.
pub fn mount_configured(
    store: &Path,
    mountpoint: &Path,
    overlay: Option<&Path>,
    options: playsparse_range::RuntimeOptions,
) -> Result<()> {
    #[cfg(any(target_os = "linux", all(target_os = "macos", feature = "macfuse")))]
    {
        fuse::mount(store, mountpoint, overlay, options)
    }
    #[cfg(not(any(target_os = "linux", all(target_os = "macos", feature = "macfuse"))))]
    {
        let _ = (store, mountpoint, overlay, options);
        bail!("{}", availability().detail)
    }
}

/// Unmount using the OS helper. No shell expansion or force/lazy unmount is used.
pub fn unmount(mountpoint: &Path) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use anyhow::Context;
        use std::process::Command;

        let mountpoint = mountpoint.canonicalize().context("resolve mountpoint")?;
        #[cfg(target_os = "linux")]
        let helpers = ["fusermount3", "fusermount"];
        #[cfg(target_os = "macos")]
        let helpers = ["/sbin/umount"];
        for helper in helpers {
            let mut command = Command::new(helper);
            #[cfg(target_os = "linux")]
            command.args(["-u", "--"]);
            command.arg(&mountpoint);
            match command.output() {
                Ok(output) if output.status.success() => return Ok(()),
                Ok(output) => {
                    bail!(
                        "{helper} could not unmount {}: {}",
                        mountpoint.display(),
                        String::from_utf8_lossy(&output.stderr).trim()
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("run unmount helper"),
            }
        }
        bail!("No unmount helper is installed")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = mountpoint;
        bail!("FUSE unmount is supported only on Linux and macOS")
    }
}
