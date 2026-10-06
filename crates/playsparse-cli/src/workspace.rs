//! Conservative disk budgets before any source-independent workspace is created.
use anyhow::{Context, Result, bail, ensure};
use playsparse_core::Layout;
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};

pub fn projected(path: &Path) -> Result<(PathBuf, PathBuf)> {
    ensure!(
        !path.components().any(|c| matches!(c, Component::ParentDir)),
        "workspace/destination path must not contain '..'"
    );
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut existing = absolute.clone();
    let mut tail = Vec::new();
    while !existing.try_exists()? {
        tail.push(
            existing
                .file_name()
                .context("invalid workspace ancestor")?
                .to_owned(),
        );
        ensure!(existing.pop(), "no existing workspace ancestor");
    }
    ensure!(
        existing.is_dir(),
        "workspace ancestor is not a directory: {}",
        existing.display()
    );
    let existing = existing.canonicalize()?;
    let mut location = existing.clone();
    for component in tail.into_iter().rev() {
        location.push(component);
    }
    Ok((location, existing))
}

#[allow(clippy::unnecessary_cast)]
pub fn inspect(path: &Path) -> Result<Value> {
    let (location, ancestor) = projected(path)?;
    #[cfg(unix)]
    let (available, volume) = {
        use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
        let c = std::ffi::CString::new(ancestor.as_os_str().as_bytes())?;
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
        // SAFETY: live NUL-terminated path and correctly sized output buffer.
        if unsafe { libc::statvfs(c.as_ptr(), stat.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("cannot measure workspace free space");
        }
        let stat = unsafe { stat.assume_init() };
        let available = (stat.f_bavail as u64)
            .checked_mul(stat.f_frsize as u64)
            .context("free space overflow")?;
        let device = std::fs::metadata(&ancestor)?.dev();
        let mut volume = ancestor.clone();
        while let Some(parent) = volume.parent() {
            if std::fs::metadata(parent)?.dev() != device {
                break;
            }
            volume = parent.to_path_buf();
        }
        #[cfg(target_os = "macos")]
        let volume = {
            let mut native = std::mem::MaybeUninit::<libc::statfs>::zeroed();
            // SAFETY: same live path, correctly sized statfs buffer.
            if unsafe { libc::statfs(c.as_ptr(), native.as_mut_ptr()) } != 0 {
                return Err(std::io::Error::last_os_error())
                    .context("cannot identify workspace volume");
            }
            let native = unsafe { native.assume_init() };
            // SAFETY: Darwin statfs returns a NUL-terminated f_mntonname.
            let name = unsafe { std::ffi::CStr::from_ptr(native.f_mntonname.as_ptr()) };
            PathBuf::from(std::ffi::OsStr::from_bytes(name.to_bytes()))
        };
        (available, volume)
    };
    #[cfg(windows)]
    let (available, volume) = {
        let resources = playsparse_vfs_win::system_resources(&ancestor);
        let available = resources["available_disk_bytes"]
            .as_u64()
            .context("cannot measure workspace free space")?;
        let volume = resources["volume_path"]
            .as_str()
            .context("cannot identify workspace volume")?;
        (available, PathBuf::from(volume))
    };
    #[cfg(not(any(unix, windows)))]
    let (available, volume) = {
        bail!("workspace disk inspection unsupported on this platform");
    };
    Ok(
        json!({"location":location,"measurement_path":ancestor,"volume":volume,"available_bytes":available}),
    )
}

pub fn estimate(
    logical: u64,
    entries: u64,
    chunk_size: u32,
    layout: Layout,
    copies: u64,
) -> Result<u64> {
    ensure!(
        (4096..=4 * 1024 * 1024).contains(&chunk_size) && chunk_size.is_power_of_two(),
        "require power-of-two chunk size 4K..4M"
    );
    // CDC's minimum is target/4. Each file can have a final short chunk.
    // No compression/deduplication discount; metadata/rounding and reserve are estimates.
    let chunks = logical
        .div_ceil(u64::from(chunk_size / 4))
        .checked_add(entries)
        .context("chunk budget overflow")?;
    let per_chunk = if layout == Layout::Loose { 8192 } else { 1024 };
    logical
        .checked_add(
            chunks
                .checked_mul(per_chunk)
                .context("object budget overflow")?,
        )
        .and_then(|v| v.checked_add(entries.checked_mul(16384)?))
        .and_then(|v| v.checked_add(64 * 1024 * 1024))
        .and_then(|v| v.checked_mul(copies))
        .context("disk budget overflow")
}

pub fn require(mut report: Value, required: u64, role: &str, suggestion: &str) -> Result<Value> {
    let available = report["available_bytes"]
        .as_u64()
        .context("missing free space measurement")?;
    report["required_estimated_bytes"] = json!(required);
    report["basis"] = json!(
        "conservative uncompressed budget with metadata/rounding reserve; concurrent disk use and unusual filesystem allocation can exceed this estimate"
    );
    if available < required {
        bail!(
            "Insufficient {role} storage\nRequired: ~{:.2} GiB ({required} bytes)\nLocation: {}\nVolume: {}\nAvailable: {:.2} GiB ({available} bytes)\nTry: {suggestion}",
            required as f64 / (1u64 << 30) as f64,
            report["location"].as_str().unwrap_or("unknown"),
            report["volume"].as_str().unwrap_or("unknown"),
            available as f64 / (1u64 << 30) as f64
        );
    }
    Ok(report)
}

pub fn preflight(
    source: &Path,
    location: &Path,
    chunk_size: u32,
    layout: Layout,
    copies: u64,
    role: &str,
) -> Result<Value> {
    let source = source.canonicalize()?;
    ensure!(source.is_dir(), "source must be a directory");
    let report = inspect(location)?;
    let destination = Path::new(report["location"].as_str().context("invalid location")?);
    ensure!(
        !destination.starts_with(&source),
        "workspace/destination must be outside source; no directories created"
    );
    let logical = playsparse_store::directory_bytes(&source)?;
    let (_, entries) = playsparse_store::directory_allocation(&source)?;
    let required = estimate(logical, entries, chunk_size, layout, copies)?;
    let suggestion = if role == "temporary" {
        format!(
            "playsparse analyze {:?} --temp-dir <directory-on-a-volume-with-more-space>",
            source
        )
    } else {
        format!(
            "playsparse pack {:?} <store-on-a-volume-with-more-space>",
            source
        )
    };
    require(report, required, role, &suggestion)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn budget_does_not_assume_compression_and_detects_overflow() {
        let one = estimate(1 << 30, 20, 256 * 1024, Layout::Packs, 1).unwrap();
        assert!(one > 1 << 30);
        assert_eq!(
            estimate(1 << 30, 20, 256 * 1024, Layout::Packs, 2).unwrap(),
            2 * one
        );
        assert!(estimate(u64::MAX, 1, 4096, Layout::Loose, 2).is_err());
        assert!(estimate(1, 1, 0, Layout::Packs, 1).is_err());
    }
    #[test]
    fn insufficient_disk_names_path_volume_and_remedy() {
        let error = require(
            json!({"location":"C:\\Temp","volume":"C:\\","available_bytes":10}),
            100,
            "temporary",
            "--temp-dir D:\\Temp",
        )
        .unwrap_err()
        .to_string();
        for expected in [
            "C:\\Temp",
            "Volume: C:\\",
            "10 bytes",
            "100 bytes",
            "--temp-dir D:\\Temp",
        ] {
            assert!(error.contains(expected), "{error}");
        }
    }
    #[test]
    fn nested_temp_is_refused_before_creating_source_directories() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("new/temp");
        assert!(
            preflight(
                temp.path(),
                &target,
                256 * 1024,
                Layout::Packs,
                2,
                "temporary"
            )
            .is_err()
        );
        assert!(!target.exists());
    }
    #[test]
    fn nonexistent_location_uses_existing_volume_without_creating_it() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("new/temp");
        let report = inspect(&target).unwrap();
        assert!(report["available_bytes"].as_u64().is_some());
        assert!(!target.exists());
    }
}
