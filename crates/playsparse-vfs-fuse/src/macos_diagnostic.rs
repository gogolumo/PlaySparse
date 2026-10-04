//! Installation facts are available even in a binary without the mount feature.
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct MacDiagnostic {
    pub status: &'static str,
    pub binary_support: bool,
    pub driver_installed: bool,
    pub driver_version: Option<String>,
    pub version_supported: bool,
    pub can_attempt_mount: bool,
    pub kernel_approval: &'static str,
    pub mount_test: &'static str,
    pub problems: Vec<&'static str>,
    pub detail: String,
    pub next_steps: Vec<&'static str>,
}

pub(crate) fn supported_version(version: &str) -> bool {
    let parts: Vec<_> = version.split('.').map(str::parse::<u32>).collect();
    matches!(parts.as_slice(), [Ok(5), Ok(minor), Ok(patch)] if (*minor, *patch) >= (3, 3))
}

fn classify(compiled: bool, installed: bool, version: Option<String>) -> MacDiagnostic {
    let supported = version.as_deref().is_some_and(supported_version);
    let mut problems = Vec::new();
    let mut next_steps = Vec::new();
    if !installed {
        problems.push("NOT_INSTALLED");
        next_steps.push("Install official macFUSE 5.3.3+ in the 5.x series, then follow its kernel-backend approval/restart instructions: https://github.com/macfuse/macfuse/wiki/Getting-Started");
    } else if !supported {
        problems.push("UNSUPPORTED_VERSION");
        next_steps.push("Install supported macFUSE 5.3.3+ in the 5.x series; an unreadable version is also blocked. FSKit is unsupported.");
    }
    if !compiled {
        problems.push("NOT_COMPILED");
        next_steps.push("Rebuild: cargo build --locked --release --workspace --features macfuse");
    }
    let ready = problems.is_empty();
    if ready {
        next_steps.push("Run playsparse doctor --mount-test; installation presence does not establish kernel approval or mount capability.");
    }
    MacDiagnostic {
        status: problems.first().copied().unwrap_or("READY_TO_TEST"),
        binary_support: compiled,
        driver_installed: installed,
        driver_version: version,
        version_supported: supported,
        can_attempt_mount: ready,
        kernel_approval: "UNKNOWN",
        mount_test: "NOT RUN",
        problems,
        detail: if ready {
            "Supported macFUSE installation and compiled kernel transport; actual mount capability is unverified. FSKit is unsupported.".into()
        } else {
            "macFUSE mount prerequisites are incomplete; see problems and next_steps.".into()
        },
        next_steps,
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn inspect(compiled: bool) -> MacDiagnostic {
    let installed = std::path::Path::new("/Library/Filesystems/macfuse.fs").is_dir();
    let version = if installed {
        std::process::Command::new("/usr/libexec/PlistBuddy")
            .args([
                "-c",
                "Print :CFBundleVersion",
                "/Library/Filesystems/macfuse.fs/Contents/Info.plist",
            ])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .map(|text| text.trim().to_owned())
    } else {
        None
    };
    classify(compiled, installed, version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_all_missing_prerequisites_without_inventing_approval() {
        let report = classify(false, false, None);
        assert_eq!(report.problems, ["NOT_INSTALLED", "NOT_COMPILED"]);
        assert!(!report.can_attempt_mount);
        assert_eq!(report.kernel_approval, "UNKNOWN");
        assert_eq!(report.mount_test, "NOT RUN");
    }

    #[test]
    fn installation_and_feature_are_independent_gates() {
        let version = Some("5.4.0".into());
        assert_eq!(
            classify(false, true, version.clone()).status,
            "NOT_COMPILED"
        );
        let report = classify(true, true, version);
        assert_eq!(report.status, "READY_TO_TEST");
        assert!(report.can_attempt_mount);
        assert_eq!(report.kernel_approval, "UNKNOWN");
        assert_eq!(classify(true, true, None).status, "UNSUPPORTED_VERSION");
    }

    #[test]
    fn unsupported_transition_releases_and_future_major_are_blocked() {
        for version in ["4.8.3", "5.3.0", "5.3.2", "6.0.0", "5.4.x", "5.4.0.1"] {
            assert_eq!(
                classify(true, true, Some(version.into())).status,
                "UNSUPPORTED_VERSION"
            );
        }
        for version in ["5.3.3", "5.4.0", "5.10.0"] {
            assert!(supported_version(version));
        }
    }
}
