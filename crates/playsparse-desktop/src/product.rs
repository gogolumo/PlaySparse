//! Read-only, bounded installation discovery. Candidates never establish compatibility.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LaunchCandidate {
    pub executable: String,
    pub label: String,
    pub kind: String,
    pub rank: u32,
    pub note: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstallationInspection {
    pub source: PathBuf,
    pub title: String,
    pub logical_bytes: u64,
    pub files: usize,
    pub candidates: Vec<LaunchCandidate>,
    pub discovery_truncated: bool,
}
pub fn inspect(source: &Path) -> Result<InstallationInspection> {
    let source = source
        .canonicalize()
        .context("Choose an existing installation folder")?;
    ensure!(source.is_dir(), "Installation must be a directory");
    ensure!(
        !source
            .ancestors()
            .any(|p| p.join("COMMITTED.json").is_file()),
        "Choose the original installation, not a PlaySparse store"
    );
    let mut report = InstallationInspection {
        title: source
            .file_name()
            .context("Cannot inspect filesystem root")?
            .to_string_lossy()
            .into(),
        source: source.clone(),
        logical_bytes: 0,
        files: 0,
        candidates: vec![],
        discovery_truncated: false,
    };
    let mut stack = vec![(source.clone(), 0)];
    let mut entries = 0;
    while let Some((directory, depth)) = stack.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            entries += 1;
            ensure!(
                entries <= 200_000,
                "Installation inspection exceeds 200,000 entries; choose a more specific folder"
            );
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                #[cfg(target_os = "macos")]
                if path.extension().is_some_and(|s| s == "app")
                    && let Ok(executable) = bundle_executable(&path)
                {
                    report.candidates.push(LaunchCandidate { executable: path.strip_prefix(&source)?.to_string_lossy().into(), label: entry.file_name().to_string_lossy().into(), kind: "macos_bundle".into(), rank: 100, note: format!("Bundle executable: {executable}. Uses Launch Services; quit the application itself before unmounting. Signed code may require an APFS shadow.") });
                }
                if depth < 12 {
                    stack.push((path, depth + 1));
                } else {
                    report.discovery_truncated = true;
                }
            } else if metadata.is_file() {
                report.files += 1;
                report.logical_bytes = report
                    .logical_bytes
                    .checked_add(metadata.len())
                    .context("Installation size overflow")?;
                let name = entry.file_name().to_string_lossy().to_lowercase();
                let excluded = [
                    "uninstall",
                    "updater",
                    "crash",
                    "reporter",
                    "redist",
                    "setup",
                    "helper",
                ]
                .iter()
                .any(|s| name.contains(s));
                #[cfg(unix)]
                let executable = {
                    use std::os::unix::fs::PermissionsExt;
                    metadata.permissions().mode() & 0o111 != 0
                };
                #[cfg(windows)]
                let executable = path
                    .extension()
                    .is_some_and(|s| s.eq_ignore_ascii_case("exe"));
                #[cfg(not(any(unix, windows)))]
                let executable = false;
                if executable
                    && !excluded
                    && !path.components().any(|c| c.as_os_str() == "Contents")
                    && report.candidates.len() < 100
                {
                    report.candidates.push(LaunchCandidate { executable: path.strip_prefix(&source)?.to_string_lossy().into(), label: entry.file_name().to_string_lossy().into(), kind: "executable".into(), rank: 80u32.saturating_sub(depth * 5), note: "Discovered executable permission/extension only; explicitly confirm this target. Compatibility is untested.".into() });
                }
            }
        }
    }
    report
        .candidates
        .sort_by(|a, b| b.rank.cmp(&a.rank).then(a.executable.cmp(&b.executable)));
    report.candidates.truncate(30);
    Ok(report)
}
#[cfg(target_os = "macos")]
pub fn bundle_executable(bundle: &Path) -> Result<String> {
    let plist = bundle.join("Contents/Info.plist");
    ensure!(
        !fs::symlink_metadata(&plist)?.is_symlink()
            && plist.canonicalize()?.starts_with(bundle.canonicalize()?),
        "Bundle plist is redirected outside the bundle"
    );
    ensure!(
        fs::metadata(&plist)?.len() <= 1024 * 1024,
        "Bundle plist exceeds limit"
    );
    let output = std::process::Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", "Print :CFBundleExecutable"])
        .arg(plist)
        .output()?;
    ensure!(output.status.success(), "Cannot read bundle executable");
    let executable = String::from_utf8(output.stdout)?.trim().to_owned();
    ensure!(
        !executable.is_empty()
            && !executable.contains(['/', '\\', '\0'])
            && executable != "."
            && executable != "..",
        "Invalid bundle executable"
    );
    let root = bundle.canonicalize()?;
    let binary = bundle
        .join("Contents/MacOS")
        .join(&executable)
        .canonicalize()?;
    ensure!(
        binary.starts_with(root) && binary.is_file(),
        "Bundle executable escapes bundle"
    );
    Ok(executable)
}
