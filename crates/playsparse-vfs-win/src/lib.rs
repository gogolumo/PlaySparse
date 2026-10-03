//! Windows filesystem backed directly by compressed CAS ranges and an optional
//! persistent writable overlay. The WinFsp driver is required on Windows.
#[cfg(not(windows))]
use playsparse_trace::TraceWriter;
#[cfg(not(windows))]
use std::path::Path;
#[cfg(not(windows))]
use std::sync::Arc;

#[cfg(windows)]
mod backend;
#[cfg(windows)]
pub use backend::{
    availability, mount, mount_configured, mount_with_options, mount_with_overlay,
    system_resources, unmount,
};

#[cfg(not(windows))]
pub fn availability() -> String {
    "WinFsp requires Windows; use the FUSE development backend on this OS".into()
}

#[cfg(not(windows))]
pub fn mount(_store: &Path, _mountpoint: &Path, _cache_bytes: usize) -> anyhow::Result<()> {
    anyhow::bail!("WinFsp mounting requires Windows")
}

#[cfg(not(windows))]
pub fn mount_with_overlay(
    store: &Path,
    mountpoint: &Path,
    cache_bytes: usize,
    overlay: Option<&Path>,
) -> anyhow::Result<()> {
    mount_with_options(store, mountpoint, cache_bytes, overlay, None)
}

#[cfg(not(windows))]
pub fn mount_with_options(
    store: &Path,
    mountpoint: &Path,
    cache_bytes: usize,
    overlay: Option<&Path>,
    trace: Option<Arc<TraceWriter>>,
) -> anyhow::Result<()> {
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

#[cfg(not(windows))]
pub fn mount_configured(
    _store: &Path,
    _mountpoint: &Path,
    _overlay: Option<&Path>,
    _options: playsparse_range::RuntimeOptions,
) -> anyhow::Result<()> {
    anyhow::bail!("WinFsp mounting requires Windows")
}

#[cfg(not(windows))]
pub fn unmount(_mountpoint: &Path) -> anyhow::Result<()> {
    anyhow::bail!("WinFsp unmounting requires Windows")
}

#[cfg(not(windows))]
pub fn system_resources(_path: &Path) -> serde_json::Value {
    serde_json::json!({"available_ram_bytes": null, "available_disk_bytes": null, "elevated": null})
}
