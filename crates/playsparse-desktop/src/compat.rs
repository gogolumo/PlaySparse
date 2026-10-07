//! Evidence records are data, never executable recipes or compatibility predictions.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::Path;
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Status {
    #[default]
    Unknown,
    Detected,
    Tested,
    Compatible,
    CompatibleWithLimitations,
    Unsupported,
    Broken,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub level: String,
    pub receipt: String,
    pub commit: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub status: Status,
    pub game_identifier: String,
    pub platform: String,
    pub os: String,
    pub architecture: String,
    pub launch_target: String,
    pub launcher_type: String,
    pub filesystem_behaviour: String,
    pub case_sensitivity: String,
    pub anti_cheat: String,
    pub native_code: String,
    pub overlay_behaviour: String,
    pub limitations: Vec<String>,
    pub tested_version: Option<String>,
    pub test_date: Option<String>,
    pub evidence: Vec<Evidence>,
}
impl Record {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= 64 * 1024,
            "Compatibility profile exceeds 64 KiB"
        );
        let record: Self = serde_json::from_slice(bytes)?;
        ensure!(
            record.limitations.len() <= 64 && record.evidence.len() <= 64,
            "Too many compatibility entries"
        );
        for value in [
            record.game_identifier.as_str(),
            &record.platform,
            &record.os,
            &record.architecture,
            &record.launch_target,
            &record.launcher_type,
            &record.filesystem_behaviour,
            &record.case_sensitivity,
            &record.anti_cheat,
            &record.native_code,
            &record.overlay_behaviour,
        ] {
            ensure!(
                value.len() <= 4096 && !value.chars().any(char::is_control),
                "Invalid compatibility text"
            );
        }
        ensure!(
            record.launch_target.is_empty() || super::relative_executable(&record.launch_target),
            "Profile launch target traversal"
        );
        for limitation in &record.limitations {
            ensure!(
                limitation.len() <= 4096 && !limitation.chars().any(char::is_control),
                "Invalid limitation"
            );
        }
        for evidence in &record.evidence {
            ensure!(
                [
                    "CI simulation",
                    "hosted native test",
                    "physical hardware validation",
                    "commercial game compatibility"
                ]
                .contains(&evidence.level.as_str()),
                "Unknown evidence level"
            );
            ensure!(
                evidence.receipt.len() <= 4096 && !evidence.receipt.chars().any(char::is_control),
                "Invalid evidence reference"
            );
            ensure!(
                evidence.commit.len() == 40
                    && evidence.commit.bytes().all(|b| b.is_ascii_hexdigit()),
                "Evidence requires full commit SHA"
            );
        }
        if matches!(
            record.status,
            Status::Tested
                | Status::Compatible
                | Status::CompatibleWithLimitations
                | Status::Broken
        ) {
            ensure!(
                !record.evidence.is_empty()
                    && record.test_date.is_some()
                    && record.tested_version.is_some(),
                "Tested status requires evidence, version and date"
            );
        }
        Ok(record)
    }
}
/// Bounded hashing, rejects every symlink component and external hardlinks on Unix.
/// The writable mount is rechecked at launch; this does not eliminate replacement between check and exec.
pub fn fingerprint(root: &Path, relative: &str) -> Result<String> {
    ensure!(
        super::relative_executable(relative),
        "Invalid executable path"
    );
    let mut path = root.to_path_buf();
    for component in Path::new(relative).components() {
        path.push(component);
        ensure!(
            !std::fs::symlink_metadata(&path)?.is_symlink(),
            "Executable symlink rejected"
        );
    }
    #[cfg(target_os = "macos")]
    if path.is_dir() {
        path = path
            .join("Contents/MacOS")
            .join(super::product::bundle_executable(&path)?);
    }
    let mut file = std::fs::File::open(&path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= 512 * 1024 * 1024,
        "Invalid or oversized executable"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(metadata.nlink() == 1, "Hardlinked executable rejected");
    }
    let mut hash = blake3::Hasher::new();
    std::io::copy(&mut file, &mut hash)?;
    Ok(hash.finalize().to_hex().to_string())
}
