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

#[cfg(all(target_os = "macos", feature = "macfuse"))]
mod macos;

#[cfg(any(target_os = "macos", test))]
mod macos_diagnostic;

#[cfg(target_os = "macos")]
pub fn macos_diagnostic() -> impl Serialize {
    macos_diagnostic::inspect(cfg!(feature = "macfuse"))
}

#[cfg(any(target_os = "linux", all(target_os = "macos", feature = "macfuse")))]
fn run_session<FS: fuser::Filesystem>(
    filesystem: FS,
    path: &Path,
    config: &fuser::Config,
) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        fuser::mount(filesystem, path, config)
    }
    #[cfg(target_os = "macos")]
    {
        macos::mount(filesystem, path, config)
    }
}

#[cfg(all(test, target_os = "linux"))]
fn spawn_session<FS: fuser::Filesystem>(
    filesystem: FS,
    path: &Path,
    config: &fuser::Config,
) -> std::io::Result<fuser::BackgroundSession> {
    fuser::spawn_mount(filesystem, path, config)
}
#[cfg(all(test, target_os = "macos", feature = "macfuse"))]
fn spawn_session<FS: fuser::Filesystem>(
    filesystem: FS,
    path: &Path,
    config: &fuser::Config,
) -> std::io::Result<macos::MountedSession> {
    macos::spawn(filesystem, path, config)
}

/// A runtime diagnostic, not evidence that a mount has passed IO tests.
#[derive(Debug, Clone, Serialize)]
pub struct Availability {
    pub backend: &'static str,
    pub available: bool,
    pub detail: String,
}

/// Report whether this binary can attempt a genuine mount on this machine.
pub fn availability() -> Availability {
    #[cfg(target_os = "linux")]
    {
        fuse::availability()
    }
    #[cfg(target_os = "macos")]
    {
        let facts = macos_diagnostic::inspect(cfg!(feature = "macfuse"));
        Availability {
            backend: "macFUSE",
            available: facts.can_attempt_mount,
            detail: format!(
                "{}: {} {}",
                facts.status,
                facts.detail,
                facts.next_steps.join(" ")
            ),
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
        {
            for helper in ["fusermount3", "fusermount"] {
                let output = match Command::new(helper)
                    .args(["-u", "--"])
                    .arg(&mountpoint)
                    .output()
                {
                    Ok(output) => output,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error).context("run unmount helper"),
                };

                if output.status.success() {
                    return Ok(());
                }

                bail!(
                    "{helper} could not unmount {}: {}",
                    mountpoint.display(),
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }

            bail!("No unmount helper is installed")
        }

        #[cfg(target_os = "macos")]
        {
            // Use Disk Arbitration on macOS. A raw unmount(2) can detach the
            // filesystem without notifying macFUSE's userspace channel in the
            // way required for the blocked FUSE reader to terminate cleanly.
            let output = Command::new("/usr/sbin/diskutil")
                .arg("unmount")
                .arg(&mountpoint)
                .output()
                .context("run diskutil unmount")?;

            if output.status.success() {
                return Ok(());
            }

            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);

            bail!(
                "diskutil could not unmount {}: {}{}",
                mountpoint.display(),
                stderr.trim(),
                if stderr.trim().is_empty() {
                    stdout.trim()
                } else {
                    ""
                }
            );
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = mountpoint;
        bail!("FUSE unmount is supported only on Linux and macOS")
    }
}
