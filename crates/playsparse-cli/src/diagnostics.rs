#[cfg(unix)]
use anyhow::Context;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{io::Read, path::Path};

const PROBE: &[u8] = b"playsparse-doctor-v1\n";

pub(super) fn probe_read(path: &Path, different_device_from: Option<&Path>) -> Result<()> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(PROBE.len() as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes == PROBE,
        "doctor mount did not return the exact probe bytes"
    );
    #[cfg(unix)]
    if let Some(root) = different_device_from {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            std::fs::metadata(path)?.dev() != std::fs::metadata(root)?.dev(),
            "probe bytes returned without an observed filesystem mount"
        );
    }
    #[cfg(not(unix))]
    ensure!(
        different_device_from.is_none(),
        "POSIX device comparison is unavailable"
    );
    Ok(())
}

pub(super) fn augment(report: &mut Value, requested: bool) -> i32 {
    report["mount_test"] = json!({"status":"NOT RUN","scope":"tiny disposable readonly mount; not physical/game/launcher evidence"});
    let available = report["mount_backend"]["available"].as_bool() == Some(true);
    let exit = if requested && !available {
        report["mount_test"]["status"] = json!("BLOCKED");
        report["mount_test"]["reason"] = report["mount_backend"]["detail"].clone();
        2
    } else if requested {
        report["mount_test"] = mount_test();
        match report["mount_test"]["status"].as_str() {
            Some("PASS") => 0,
            Some("BLOCKED") => 2,
            _ => 1,
        }
    } else {
        0
    };
    if requested {
        let status = report["mount_test"]["status"].clone();
        report["mount_diagnostic"]["mount_test"] = status.clone();
        if status == "PASS" {
            report["mount_diagnostic"]["status"] = json!("MOUNT_TEST_PASSED");
            report["mount_diagnostic"]["kernel_backend"] = json!("AVAILABLE");
        } else if status == "BLOCKED" && available {
            #[cfg(not(unix))]
            {
                report["mount_diagnostic"]["status"] = json!("TEST_UNSUPPORTED");
                report["mount_diagnostic"]["next_steps"] = json!([
                    "Run tools/windows-hardware-validation.ps1 for native Windows mount validation."
                ]);
            }
            #[cfg(unix)]
            {
                report["mount_diagnostic"]["status"] = json!("KERNEL_UNAVAILABLE");
                report["mount_diagnostic"]["kernel_backend"] = json!("BLOCKED");
                report["mount_diagnostic"]["next_steps"] = json!([
                    "Inspect mount_test.provider_log and cleanup results. On macOS approve the macFUSE kernel backend and restart as required by https://github.com/macfuse/macfuse/wiki/Getting-Started. No approval was changed by this test.",
                    "On Linux check /dev/fuse access and mount permission/fusermount."
                ]);
            }
        }
    }
    report["exit_code"] = json!(exit);
    exit
}

pub(super) fn print_human(report: &Value) {
    println!(
        "Version: {}",
        report["version"].as_str().unwrap_or("unknown")
    );
    println!(
        "PlaySparse doctor: {} / {}",
        report["os"].as_str().unwrap_or("unknown"),
        report["arch"].as_str().unwrap_or("unknown")
    );
    println!(
        "{}",
        report["mount_backend"]["detail"]
            .as_str()
            .unwrap_or("Mount backend unavailable")
    );
    if let Some(diagnostic) = report.get("mount_diagnostic") {
        println!(
            "Mount readiness: {}; kernel approval: {}",
            diagnostic["status"].as_str().unwrap_or("UNKNOWN"),
            diagnostic["kernel_approval"].as_str().unwrap_or("UNKNOWN")
        );
        if let Some(steps) = diagnostic["next_steps"].as_array() {
            for step in steps {
                if let Some(step) = step.as_str() {
                    println!("  {step}");
                }
            }
        }
    }
    println!(
        "Actual mount/read/unmount test: {}",
        report["mount_test"]["status"].as_str().unwrap_or("NOT RUN")
    );
    for field in ["reason", "error", "preserved_work"] {
        if let Some(value) = report["mount_test"][field].as_str() {
            println!("{field}: {value}");
        }
    }
    for field in ["temporary_workspace", "store_destination"] {
        let value = &report[field];
        println!(
            "{field}: {} (volume {}), {} bytes available",
            value["location"].as_str().unwrap_or("unknown"),
            value["volume"].as_str().unwrap_or("unknown"),
            value["available_bytes"]
                .as_u64()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "UNKNOWN".into())
        );
        if let Some(error) = value["error"].as_str() {
            println!("  {error}");
        }
    }
}

#[cfg(not(unix))]
fn mount_test() -> Value {
    json!({"status":"BLOCKED","reason":"Use tools/windows-hardware-validation.ps1 for Windows mounted validation; this disposable doctor probe currently supports POSIX.","scope":"no mount attempted"})
}

#[cfg(unix)]
fn mount_present(path: &Path) -> Result<bool> {
    // Read the OS mount table, never stat a potentially disconnected FUSE root.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let mut bytes = Vec::new();
        std::fs::File::open("/proc/self/mountinfo")?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= 1024 * 1024,
            "mount table exceeds doctor inspection limit"
        );
        for line in bytes.split(|byte| *byte == b'\n') {
            if let Some(field) = line.split(|byte| *byte == b' ').nth(4) {
                let mut decoded = Vec::new();
                let mut i = 0;
                while i < field.len() {
                    if field[i] == b'\\'
                        && i + 3 < field.len()
                        && field[i + 1..i + 4]
                            .iter()
                            .all(|byte| (b'0'..=b'7').contains(byte))
                    {
                        decoded.push(
                            (field[i + 1] - b'0') * 64 + (field[i + 2] - b'0') * 8 + field[i + 3]
                                - b'0',
                        );
                        i += 4;
                    } else {
                        decoded.push(field[i]);
                        i += 1;
                    }
                }
                if decoded == path.as_os_str().as_bytes() {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
    #[cfg(target_os = "macos")]
    {
        use std::{
            process::{Command, Stdio},
            thread,
            time::{Duration, Instant},
        };
        let output = tempfile::NamedTempFile::new()?;
        let mut child = Command::new("/sbin/mount")
            .stdout(output.reopen()?)
            .stderr(Stdio::null())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = child.try_wait()? {
                ensure!(status.success(), "could not inspect OS mount table");
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("OS mount table inspection timed out");
            }
            thread::sleep(Duration::from_millis(20));
        }
        let mut bytes = Vec::new();
        output
            .reopen()?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= 1024 * 1024,
            "mount table exceeds doctor inspection limit"
        );
        let marker = format!(" on {} (", path.display());
        Ok(String::from_utf8(bytes)?
            .lines()
            .any(|line| line.contains(&marker)))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    anyhow::bail!(
        "doctor mount-table inspection is unsupported on this POSIX platform: {}",
        path.display()
    )
}

#[cfg(unix)]
fn mount_test() -> Value {
    use std::{
        fs,
        process::{Child, Command, Stdio},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };
    let mut report = json!({"status":"FAIL","scope":"tiny disposable readonly mount; not physical/game/launcher evidence","commands":[],"cleanup":"NOT RUN"});
    let mut work = None;
    let mut provider: Option<Child> = None;
    let interrupted = Arc::new(AtomicBool::new(false));
    let signal_flag = interrupted.clone();
    let result: Result<()> = (|| {
        ctrlc::set_handler(move || {
            signal_flag.store(true, Ordering::Relaxed);
        })
        .context("install doctor interruption handler")?;
        let temp = tempfile::Builder::new()
            .prefix("playsparse-doctor-")
            .tempdir()?;
        let root = temp.path().canonicalize()?;
        work = Some(temp);
        let source = root.join("source");
        let store = root.join("store");
        let mounted = root.join("mounted");
        fs::create_dir(&source)?;
        fs::create_dir(&mounted)?;
        fs::write(source.join("probe.txt"), PROBE)?;
        playsparse_store::pack_directory(
            &source,
            &store,
            &playsparse_store::PackOptions::default(),
        )?;
        let exe = std::env::current_exe()?;
        let log = fs::File::create(root.join("provider.log"))?;
        report["commands"]
            .as_array_mut()
            .unwrap()
            .push(json!([exe, "mount", store, mounted, "--cache", "1M"]));
        provider = Some(
            Command::new(&exe)
                .arg("mount")
                .arg(&store)
                .arg(&mounted)
                .args(["--cache", "1M"])
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()?,
        );
        let started = Instant::now();
        let mut read_passed = false;
        while started.elapsed() < Duration::from_secs(15) && !interrupted.load(Ordering::Relaxed) {
            if provider
                .as_mut()
                .context("missing doctor provider")?
                .try_wait()?
                .is_some()
            {
                break;
            }
            let command = vec![
                exe.as_os_str().to_os_string(),
                "doctor-probe-read".into(),
                mounted.join("probe.txt").into_os_string(),
                "--different-device-from".into(),
                root.clone().into_os_string(),
            ];
            let mut reader = Command::new(&exe)
                .args(&command[1..])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            report["commands"]
                .as_array_mut()
                .unwrap()
                .push(json!(command));
            let read_deadline = Instant::now() + Duration::from_secs(1);
            loop {
                if let Some(status) = reader.try_wait()? {
                    read_passed = status.success();
                    break;
                }
                if Instant::now() >= read_deadline || interrupted.load(Ordering::Relaxed) {
                    let _ = reader.kill();
                    let _ = reader.wait();
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            if read_passed {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        report["elapsed_seconds"] = json!(started.elapsed().as_secs_f64());
        report["read_exact_probe"] = json!(read_passed);
        if !read_passed {
            report["status"] = json!(if interrupted.load(Ordering::Relaxed) {
                "FAIL"
            } else {
                "BLOCKED"
            });
            anyhow::bail!(
                "{}",
                if interrupted.load(Ordering::Relaxed) {
                    "doctor interrupted; cleanup attempted"
                } else {
                    "mount provider could not serve the probe; inspect its log and driver/mount permissions"
                }
            );
        }
        // The bounded probe child checked both exact bytes and filesystem device.
        report["status"] = json!("PASS");
        Ok(())
    })();
    if let Err(error) = result {
        report["error"] = json!(format!("{error:#}"));
    }
    if let Some(temp) = work {
        // tempfile may report /var/... while macOS mount tables expose the
        // canonical /private/var/... path. Use the canonical root during
        // cleanup so mount detection addresses the same filesystem path that
        // was used when the provider was started.
        let root = temp
            .path()
            .canonicalize()
            .unwrap_or_else(|_| temp.path().to_path_buf());
        let mounted = root.join("mounted");
        let observed = mount_present(&mounted);
        let mounted_now = observed.as_ref().copied().unwrap_or(true);
        let mut cleanup_ok = observed.is_ok();
        if mounted_now {
            if let Ok(exe) = std::env::current_exe() {
                report["commands"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!([exe, "unmount", mounted]));
                match Command::new(exe)
                    .arg("unmount")
                    .arg(&mounted)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    Ok(mut child) => {
                        let until = Instant::now() + Duration::from_secs(10);
                        loop {
                            match child.try_wait() {
                                Ok(Some(status)) => {
                                    cleanup_ok = status.success();
                                    break;
                                }
                                Ok(None) if Instant::now() < until => {
                                    thread::sleep(Duration::from_millis(20))
                                }
                                _ => {
                                    let _ = child.kill();
                                    let _ = child.wait();
                                    cleanup_ok = false;
                                    break;
                                }
                            }
                        }
                    }
                    Err(_) => cleanup_ok = false,
                }
            } else {
                cleanup_ok = false;
            }
        }
        if let Some(mut child) = provider {
            let until = Instant::now() + Duration::from_secs(5);
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        report["provider_exit_code"] = json!(status.code());
                        if report["status"] == "PASS" && !status.success() {
                            report["status"] = json!("FAIL");
                            report["error"] = json!("mount provider failed during teardown");
                        }
                        break;
                    }
                    Ok(None) if Instant::now() < until => thread::sleep(Duration::from_millis(20)),
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        cleanup_ok = false;
                        break;
                    }
                }
            }
        }
        if mount_present(&mounted).unwrap_or(true) {
            cleanup_ok = false;
        }
        report["provider_log"] = json!(fs::File::open(root.join("provider.log")).ok().and_then(
            |file| {
                let mut bytes = String::new();
                file.take(65536).read_to_string(&mut bytes).ok()?;
                Some(bytes)
            }
        ));
        report["cleanup"] = json!(if cleanup_ok { "PASS" } else { "FAIL" });
        if !cleanup_ok {
            report["status"] = json!("FAIL");
            report["preserved_work"] = json!(temp.keep());
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocked_probe_never_runs_when_prerequisites_are_missing() {
        let mut report = json!({"mount_backend":{"available":false,"detail":"not compiled"}});
        assert_eq!(augment(&mut report, true), 2);
        assert_eq!(report["mount_test"]["status"], "BLOCKED");
        assert_eq!(report["mount_test"]["reason"], "not compiled");
    }
    #[test]
    fn passive_diagnostics_do_not_claim_mount_approval() {
        let mut report = json!({"mount_backend":{"available":true}});
        assert_eq!(augment(&mut report, false), 0);
        assert_eq!(report["mount_test"]["status"], "NOT RUN");
    }
    #[test]
    fn probe_requires_exact_bytes_and_refuses_longer_or_shorter_content() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), PROBE).unwrap();
        probe_read(file.path(), None).unwrap();
        std::fs::write(file.path(), [PROBE, b"extra"].concat()).unwrap();
        assert!(probe_read(file.path(), None).is_err());
        std::fs::write(file.path(), b"short").unwrap();
        assert!(probe_read(file.path(), None).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn exact_bytes_on_an_ordinary_directory_cannot_pass_mount_probe() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("probe.txt");
        std::fs::write(&file, PROBE).unwrap();
        assert!(probe_read(&file, Some(temp.path())).is_err());
        assert!(!mount_present(temp.path()).unwrap());
    }
}
