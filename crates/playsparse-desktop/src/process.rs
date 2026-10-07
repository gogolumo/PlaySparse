//! Current-lifetime ownership only. Numeric process IDs are observation data, never kill targets.
#[cfg(target_os = "macos")]
use anyhow::Context;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    process::{Child, Command, ExitStatus},
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    #[default]
    Launching,
    Running,
    LauncherExitedButGameRunning,
    Exited,
    NeedsAttention,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub pid: u32,
    pub birth: u64,
}
#[derive(Clone, Debug)]
#[cfg_attr(windows, allow(dead_code))]
struct Row {
    identity: Identity,
    parent: u32,
    group: u32,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub lifecycle: Lifecycle,
    pub active_processes: Vec<Identity>,
    pub safe_to_unmount: bool,
    pub root_exit: Option<String>,
    pub limitation: Option<String>,
}
#[cfg_attr(windows, allow(dead_code))]
pub struct LaunchTree {
    child: Child,
    root: Option<Identity>,
    known: BTreeMap<u32, Identity>,
    exit: Option<ExitStatus>,
    external_bundle: bool,
    #[cfg(windows)]
    job: windows_job::Job,
}
impl LaunchTree {
    pub fn prepare(command: &mut Command) {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x00000004);
        }
    }
    pub fn attach(mut child: Child, external_bundle: bool) -> Result<Self> {
        #[cfg(windows)]
        let job = match windows_job::Job::attach(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let root = snapshot().ok().and_then(|rows| {
            rows.into_iter()
                .find(|r| r.identity.pid == child.id())
                .map(|r| r.identity)
        });
        // Reap an immediately exiting root only through its owned handle.
        let exit = child.try_wait()?;
        let mut known = BTreeMap::new();
        if let Some(root) = &root {
            known.insert(root.pid, root.clone());
        }
        Ok(Self {
            child,
            root,
            known,
            exit,
            external_bundle,
            #[cfg(windows)]
            job,
        })
    }
    pub fn observe(&mut self) -> Result<Observation> {
        if self.exit.is_none() {
            self.exit = self.child.try_wait()?;
        }
        #[cfg(windows)]
        {
            let count = self.job.active()?;
            Ok(Observation {
                lifecycle: if count == 0 { Lifecycle::Exited } else if self.exit.is_some() { Lifecycle::LauncherExitedButGameRunning } else { Lifecycle::Running },
                // A Job handle establishes membership, independent of reused numeric PIDs.
                active_processes: self.job.members()?.into_iter().map(|pid| Identity { pid, birth: 0 }).collect(),
                safe_to_unmount: count == 0,
                root_exit: self.exit.map(|s| s.to_string()),
                limitation: Some("Job captures CreateProcess descendants; external broker launches require manual inspection.".into()),
            })
        }
        #[cfg(not(windows))]
        {
            let rows = match snapshot() {
                Ok(rows) => rows,
                Err(e) => {
                    return Ok(Observation {
                        lifecycle: Lifecycle::NeedsAttention,
                        limitation: Some(format!("Process inspection unavailable: {e}")),
                        ..Default::default()
                    });
                }
            };
            let mut active = track(&rows, self.child.id(), self.root.as_ref(), &mut self.known);
            if self.exit.is_none() && !active.iter().any(|p| p.pid == self.child.id()) {
                active.push(self.root.clone().unwrap_or(Identity {
                    pid: self.child.id(),
                    birth: 0,
                }));
            }
            let uncertain = self.external_bundle || self.root.is_none();
            Ok(Observation {
                lifecycle: if !active.is_empty() { if self.exit.is_some() { Lifecycle::LauncherExitedButGameRunning } else { Lifecycle::Running } } else if uncertain { Lifecycle::NeedsAttention } else { Lifecycle::Exited },
                active_processes: active,
                // Unix groups can be escaped via setsid between snapshots. Require explicit closure confirmation.
                safe_to_unmount: false,
                root_exit: self.exit.map(|s| s.to_string()),
                limitation: Some("Unix polling cannot prove absence of children that detach between snapshots. Close all game processes before confirming ordinary unmount. Launch Services apps must be quit normally.".into()),
            })
        }
    }
    pub fn stop_root(&mut self) -> Result<Observation> {
        ensure!(
            !self.external_bundle,
            "Quit the application normally; its Launch Services helper does not own the app"
        );
        if self.child.try_wait()?.is_none() {
            self.child.kill()?;
        }
        self.exit = Some(self.child.wait()?);
        self.observe()
    }
}
// Identity mismatch drops a reused PID. PPID edges require a currently matching parent identity.
#[cfg(any(unix, test))]
fn track(
    rows: &[Row],
    group: u32,
    root: Option<&Identity>,
    known: &mut BTreeMap<u32, Identity>,
) -> Vec<Identity> {
    let group_reused = rows
        .iter()
        .find(|r| r.identity.pid == group)
        .is_some_and(|r| root != Some(&r.identity));
    known.retain(|pid, identity| {
        rows.iter()
            .any(|r| r.identity.pid == *pid && &r.identity == identity)
    });
    loop {
        let before = known.len();
        for row in rows {
            if (!group_reused && row.group == group) || known.contains_key(&row.parent) {
                known.insert(row.identity.pid, row.identity.clone());
            }
        }
        if before == known.len() {
            break;
        }
    }
    known.values().cloned().collect()
}
#[cfg(target_os = "linux")]
fn snapshot() -> Result<Vec<Row>> {
    let mut rows = vec![];
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        use std::os::unix::fs::MetadataExt;
        if entry.metadata()?.uid() != unsafe { libc::geteuid() } {
            continue;
        }
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        match std::fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => {
                if let Some(row) = parse_stat(pid, &stat) {
                    rows.push(row);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(rows)
}
#[cfg(target_os = "linux")]
fn parse_stat(pid: u32, stat: &str) -> Option<Row> {
    let fields: Vec<_> = stat
        .get(stat.rfind(')')? + 2..)?
        .split_whitespace()
        .collect();
    if fields.first() == Some(&"Z") {
        return None;
    }
    Some(Row {
        identity: Identity {
            pid,
            birth: fields.get(19)?.parse().ok()?,
        },
        parent: fields.get(1)?.parse().ok()?,
        group: fields.get(2)?.parse().ok()?,
    })
}
#[cfg(target_os = "macos")]
fn snapshot() -> Result<Vec<Row>> {
    // libproc returns bytes. Reserve slack, then reject a full buffer rather than silently truncating.
    let bytes = unsafe {
        libc::proc_listpids(
            4, /* PROC_UID_ONLY */
            libc::geteuid(),
            std::ptr::null_mut(),
            0,
        )
    };
    ensure!(
        bytes > 0 && bytes < 16 * 1024 * 1024,
        "libproc size unavailable"
    );
    let mut pids = vec![0u32; bytes as usize / 4 + 4096];
    let count = unsafe {
        libc::proc_listpids(
            4, /* PROC_UID_ONLY */
            libc::geteuid(),
            pids.as_mut_ptr().cast(),
            (pids.len() * 4) as i32,
        )
    };
    ensure!(
        count > 0 && (count as usize) < pids.len() * 4,
        "libproc snapshot incomplete"
    );
    let mut rows = vec![];
    for pid in pids
        .into_iter()
        .take(count as usize / 4)
        .filter(|p| *p != 0)
    {
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
        let n = unsafe {
            libc::proc_pidinfo(
                pid as i32,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                std::mem::size_of::<libc::proc_bsdinfo>() as i32,
            )
        };
        if n == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                continue;
            }
            return Err(error).context("libproc process identity unavailable");
        }
        ensure!(
            n as usize == std::mem::size_of::<libc::proc_bsdinfo>(),
            "libproc short identity"
        );
        let info = unsafe { info.assume_init() };
        if info.pbi_status == 5 {
            continue;
        } // SZOMB
        rows.push(Row {
            identity: Identity {
                pid,
                birth: info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec,
            },
            parent: info.pbi_ppid,
            group: info.pbi_pgid,
        });
    }
    Ok(rows)
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn snapshot() -> Result<Vec<Row>> {
    anyhow::bail!("Use OS-owned Job membership on Windows")
}

#[cfg(windows)]
mod windows_job {
    use super::*;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
        System::{Diagnostics::ToolHelp::*, JobObjects::*, Threading::*},
    };
    pub struct Job(usize);
    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0 as HANDLE);
            }
        }
    }
    impl Job {
        pub fn attach(child: &Child) -> Result<Self> {
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            ensure!(
                !handle.is_null(),
                "CreateJobObject failed: {}",
                std::io::Error::last_os_error()
            );
            let job = Self(handle as usize);
            ensure!(
                unsafe { AssignProcessToJobObject(handle, child.as_raw_handle()) } != 0,
                "Job assignment failed: {}",
                std::io::Error::last_os_error()
            );
            let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
            ensure!(snapshot != INVALID_HANDLE_VALUE, "Thread snapshot failed");
            let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
            entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            let mut found = false;
            let mut valid = unsafe { Thread32First(snapshot, &mut entry) } != 0;
            while valid {
                if entry.th32OwnerProcessID == child.id() {
                    let thread =
                        unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                    if !thread.is_null() {
                        let resumed = unsafe { ResumeThread(thread) };
                        unsafe {
                            CloseHandle(thread);
                        }
                        found = resumed != u32::MAX;
                    }
                    break;
                }
                valid = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
            }
            unsafe {
                CloseHandle(snapshot);
            }
            ensure!(found, "Could not resume the owned suspended launch root");
            Ok(job)
        }
        pub fn active(&self) -> Result<u32> {
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
            ensure!(
                unsafe {
                    QueryInformationJobObject(
                        self.0 as HANDLE,
                        JobObjectBasicAccountingInformation,
                        (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                        std::mem::size_of_val(&info) as u32,
                        std::ptr::null_mut(),
                    )
                } != 0,
                "Job accounting unavailable"
            );
            Ok(info.ActiveProcesses)
        }
        pub fn members(&self) -> Result<Vec<u32>> {
            let mut buffer = vec![0usize; 65538];
            ensure!(
                unsafe {
                    QueryInformationJobObject(
                        self.0 as HANDLE,
                        JobObjectBasicProcessIdList,
                        buffer.as_mut_ptr().cast(),
                        (buffer.len() * std::mem::size_of::<usize>()) as u32,
                        std::ptr::null_mut(),
                    )
                } != 0,
                "Job member list unavailable"
            );
            let info = unsafe { &*(buffer.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>()) };
            let count = info.NumberOfProcessIdsInList as usize;
            ensure!(count <= 65536, "Job membership exceeds bound");
            let ids = unsafe { std::slice::from_raw_parts(info.ProcessIdList.as_ptr(), count) };
            Ok(ids.iter().map(|p| *p as u32).collect())
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn row(pid: u32, birth: u64, parent: u32, group: u32) -> Row {
        Row {
            identity: Identity { pid, birth },
            parent,
            group,
        }
    }
    #[test]
    fn reparented_children_and_pid_reuse() {
        let root = Identity { pid: 100, birth: 1 };
        let mut known = BTreeMap::from([(100, root.clone())]);
        assert_eq!(
            track(
                &[
                    row(100, 1, 1, 100),
                    row(101, 2, 100, 100),
                    row(102, 3, 101, 100)
                ],
                100,
                Some(&root),
                &mut known
            )
            .len(),
            3
        );
        assert_eq!(
            track(
                &[row(101, 2, 1, 100), row(102, 3, 1, 100)],
                100,
                Some(&root),
                &mut known
            )
            .len(),
            2
        );
        assert!(
            track(
                &[row(100, 9, 1, 100), row(101, 10, 100, 100)],
                100,
                Some(&root),
                &mut known
            )
            .is_empty()
        );
    }
}
