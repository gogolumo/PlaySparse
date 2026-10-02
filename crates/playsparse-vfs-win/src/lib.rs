//! Read-only Windows filesystem backed directly by compressed CAS ranges.
//! The WinFsp driver is required on Windows; no hydrated backing files are made.
use std::path::Path;

#[cfg(windows)]
mod backend;
#[cfg(windows)]
pub use backend::{availability, mount, system_resources, unmount};

#[cfg(not(windows))]
pub fn availability() -> String {
    "WinFsp requires Windows; use the FUSE development backend on this OS".into()
}

#[cfg(not(windows))]
pub fn mount(_store: &Path, _mountpoint: &Path, _cache_bytes: usize) -> anyhow::Result<()> {
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
