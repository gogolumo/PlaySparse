//! Optional read-only detector adapter. External programs are trusted user tools.
use crate::{EngineSignal, GameProfile, invalid, key, text};
use playsparse_core::Result;
use serde_json::Value;
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: usize = 1024 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Generic,
    UniversalModder,
}

#[cfg(unix)]
fn ready<T: std::os::fd::AsRawFd>(pipe: &T) -> std::io::Result<bool> {
    let mut fd = libc::pollfd {
        fd: pipe.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // poll does not consume bytes; one reader owns each anonymous pipe.
    let n = unsafe { libc::poll(&mut fd, 1, 0) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(n > 0 && fd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0)
}
#[cfg(windows)]
fn ready<T: std::os::windows::io::AsRawHandle>(pipe: &T) -> std::io::Result<bool> {
    use windows_sys::Win32::{
        Foundation::{ERROR_BROKEN_PIPE, GetLastError},
        System::Pipes::PeekNamedPipe,
    };
    let mut available = 0;
    let ok = unsafe {
        PeekNamedPipe(
            pipe.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        let e = unsafe { GetLastError() };
        if e == ERROR_BROKEN_PIPE {
            return Ok(true);
        }
        return Err(std::io::Error::from_raw_os_error(e as i32));
    }
    Ok(available > 0)
}
fn drain<T: Read>(
    pipe: &mut T,
    bytes: &mut Vec<u8>,
    is_ready: impl Fn(&T) -> std::io::Result<bool>,
) -> Result<bool> {
    if !is_ready(pipe)? {
        return Ok(false);
    }
    let mut b = [0; 4096];
    let n = pipe.read(&mut b)?;
    if bytes.len() + n > OUTPUT_LIMIT {
        return Err(invalid("scanner output exceeds 1 MiB per stream"));
    }
    bytes.extend_from_slice(&b[..n]);
    Ok(n == 0)
}
#[cfg(windows)]
struct Job(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// Literal argv, bounded pipes and deadline. No shell, stdin, launcher or network calls.
pub fn run(program: &Path, source: &Path, timeout: Duration) -> Result<Vec<u8>> {
    let mut command = Command::new(program);
    command
        .env("UM_NO_UV", "1")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .arg("scan")
        .arg(source)
        .arg("--json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    #[cfg(windows)]
    let _job = {
        use std::{
            mem::{size_of, zeroed},
            os::windows::io::AsRawHandle,
        };
        use windows_sys::Win32::System::JobObjects::*;
        let job = Job(unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) });
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if job.0.is_null()
            || unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            } == 0
            || unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle()) } == 0
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(invalid("cannot contain scanner in Windows job"));
        }
        job
    };
    let result = (|| {
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| invalid("scanner stdout"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| invalid("scanner stderr"))?;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut out_done = false;
        let mut err_done = false;
        let deadline = Instant::now() + timeout;
        loop {
            if Instant::now() >= deadline {
                return Err(invalid("scanner timed out; retry generic mode"));
            }
            if !out_done {
                out_done = drain(&mut stdout, &mut out, ready)?;
            }
            if !err_done {
                err_done = drain(&mut stderr, &mut err, ready)?;
            }
            if let Some(status) = child.try_wait()? {
                // Once the child exits, drain ready data only. Descendants may retain handles.
                if out_done && err_done {
                    if !status.success() {
                        return Err(invalid(format!(
                            "scanner failed with exit {status}; stderr omitted to avoid path leakage"
                        )));
                    }
                    return Ok(out);
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    })();
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
    result
}
fn strings(v: &Value, max: usize) -> Result<Vec<String>> {
    let values = v
        .as_array()
        .ok_or_else(|| invalid("scanner array expected"))?;
    if values.len() > max {
        return Err(invalid("excessive scanner items"));
    }
    values
        .iter()
        .map(|v| {
            v.as_str()
                .filter(|s| text(s, 128))
                .map(str::to_owned)
                .ok_or_else(|| invalid("unsafe scanner label"))
        })
        .collect()
}
fn evidence(v: Option<&Value>, profile: &GameProfile) -> Result<Vec<String>> {
    let Some(v) = v else { return Ok(vec![]) };
    let values = v
        .as_array()
        .ok_or_else(|| invalid("scanner evidence must be array"))?;
    if values.len() > 64 {
        return Err(invalid("excessive evidence"));
    }
    let mut paths = Vec::new();
    for v in values {
        let s = v
            .as_str()
            .filter(|s| s.len() <= 1024)
            .ok_or_else(|| invalid("invalid scanner evidence"))?;
        // Human strings are not persisted. Only actual source-relative paths survive.
        if let Some(f) = profile
            .files
            .iter()
            .find(|f| f.path.eq_ignore_ascii_case(s))
        {
            paths.push(f.path.clone());
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}
fn opt_label(v: Option<&Value>) -> Result<Option<String>> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if text(s, 128) => Ok(Some(s.clone())),
        _ => Err(invalid("invalid scanner metadata label")),
    }
}
pub fn normalize(bytes: &[u8], source: &Path, profile: &mut GameProfile) -> Result<()> {
    if bytes.len() > OUTPUT_LIMIT {
        return Err(invalid("oversized scanner JSON"));
    }
    let v: Value = serde_json::from_slice(bytes).map_err(|_| invalid("malformed scanner JSON"))?;
    if !v.is_object() {
        return Err(invalid("scanner JSON object expected"));
    }
    let reported = v
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("scanner source path missing"))?;
    if Path::new(reported).canonicalize()? != source.canonicalize()? {
        return Err(invalid("scanner source mismatch"));
    }
    let engine = v
        .get("engine")
        .filter(|e| e.is_object())
        .ok_or_else(|| invalid("scanner engine missing"))?;
    let k = engine
        .get("key")
        .and_then(Value::as_str)
        .filter(|k| key(k))
        .ok_or_else(|| invalid("invalid engine key"))?;
    let confidence = engine
        .get("confidence")
        .and_then(Value::as_u64)
        .filter(|c| *c <= 100)
        .ok_or_else(|| invalid("invalid engine confidence"))? as u8;
    // The public label uses the stable key, not arbitrary scanner prose.
    profile.engine.key = k.into();
    profile.engine.label = k.into();
    profile.engine.confidence = Some(confidence);
    profile.engine.version = opt_label(
        engine
            .get("version")
            .or_else(|| engine.get("engine_version")),
    )?;
    profile.engine.evidence_paths = evidence(engine.get("evidence"), profile)?;
    profile.engine.other_signals.clear();
    if let Some(other) = v.get("other_engine_signals") {
        let items = other
            .as_array()
            .filter(|a| a.len() <= 8)
            .ok_or_else(|| invalid("invalid other engine signals"))?;
        for item in items {
            let key = item
                .get("key")
                .and_then(Value::as_str)
                .filter(|k| crate::key(k))
                .ok_or_else(|| invalid("invalid secondary engine"))?;
            profile.engine.other_signals.push(EngineSignal {
                key: key.into(),
                confidence: None,
                evidence_paths: evidence(item.get("evidence"), profile)?,
            });
        }
    }
    profile
        .engine
        .other_signals
        .sort_by(|a, b| a.key.cmp(&b.key));
    profile.game.name = opt_label(v.get("name"))?;
    profile.game.store = opt_label(v.get("store"))?;
    profile.game.appid = opt_label(v.get("appid"))?;
    profile.anti_cheat_detected = if let Some(a) = v.get("anti_cheat") {
        strings(a, 32)?
    } else {
        vec![]
    };
    profile.anti_cheat_detected.sort();
    profile.anti_cheat_detected.dedup();
    if let Some(exes) = v.get("executables") {
        let exes = exes
            .as_object()
            .filter(|o| o.len() <= 64)
            .ok_or_else(|| invalid("invalid executables"))?;
        for (path, exe) in exes {
            let f = profile
                .files
                .iter()
                .find(|f| f.path.eq_ignore_ascii_case(path))
                .ok_or_else(|| invalid("scanner executable absent from source"))?;
            let managed = exe
                .get("managed")
                .and_then(Value::as_bool)
                .ok_or_else(|| invalid("managed flag missing"))?;
            let architecture =
                opt_label(exe.get("arch"))?.ok_or_else(|| invalid("executable arch missing"))?;
            profile.executables.push(crate::Executable {
                path: f.path.clone(),
                managed,
                architecture,
            });
        }
        profile.executables.sort_by(|a, b| a.path.cmp(&b.path));
    }
    profile.scanner_truncated = v
        .get("index_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    profile.scanner_status = "universal-modder".into();
    profile.validate()
}
pub fn apply(profile: &mut GameProfile, source: &Path, mode: Mode, program: &Path) -> Result<()> {
    if mode == Mode::Generic {
        return Ok(());
    }
    match run(program, source, Duration::from_secs(45)) {
        Err(playsparse_core::Error::Io(e))
            if e.kind() == std::io::ErrorKind::NotFound && mode == Mode::Auto =>
        {
            profile.scanner_status = "absent-fallback".into();
            Ok(())
        }
        Err(playsparse_core::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(invalid(
                "universal-modder executable absent; install manually or use --scanner generic",
            ))
        }
        Err(e) => Err(e),
        Ok(bytes) => normalize(&bytes, source, profile),
    }
}
