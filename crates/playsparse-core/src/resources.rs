//! Current-process CPU and peak resident/working-set memory, not system totals.

pub fn process_resources() -> (Option<f64>, Option<u64>) {
    #[cfg(unix)]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: getrusage writes a valid, correctly sized initialized buffer.
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
            return (None, None);
        }
        let usage = unsafe { usage.assume_init() };
        let cpu = usage.ru_utime.tv_sec as f64
            + usage.ru_utime.tv_usec as f64 / 1e6
            + usage.ru_stime.tv_sec as f64
            + usage.ru_stime.tv_usec as f64 / 1e6;
        #[cfg(target_os = "macos")]
        let rss = usage.ru_maxrss as u64;
        #[cfg(not(target_os = "macos"))]
        let rss = (usage.ru_maxrss as u64).saturating_mul(1024);
        (Some(cpu), Some(rss))
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::FILETIME,
            System::{
                ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
                Threading::{GetCurrentProcess, GetProcessTimes},
            },
        };
        // SAFETY: the pseudo-handle is borrowed, valid for this process and must
        // not be closed. All output buffers have the exact documented ABI.
        unsafe {
            let handle = GetCurrentProcess();
            let (mut created, mut exited, mut kernel, mut user) = (
                std::mem::zeroed::<FILETIME>(),
                std::mem::zeroed::<FILETIME>(),
                std::mem::zeroed::<FILETIME>(),
                std::mem::zeroed::<FILETIME>(),
            );
            let ticks = |time: FILETIME| {
                (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
            };
            let cpu = if GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user)
                != 0
            {
                ticks(kernel)
                    .checked_add(ticks(user))
                    .map(|ticks| ticks as f64 / 10_000_000.0)
            } else {
                None
            };
            let mut counters = std::mem::zeroed::<PROCESS_MEMORY_COUNTERS>();
            counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
            let rss = if GetProcessMemoryInfo(handle, &mut counters, counters.cb) != 0 {
                Some(counters.PeakWorkingSetSize as u64)
            } else {
                None
            };
            (cpu, rss)
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        (None, None)
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    #[test]
    fn native_process_measurements_are_present_and_monotonic() {
        let before = super::process_resources();
        let mut memory = vec![0u8; 1024 * 1024];
        for (i, byte) in memory.iter_mut().enumerate() {
            *byte = i as u8;
        }
        std::hint::black_box(memory);
        let after = super::process_resources();
        assert!(after.0.unwrap() >= before.0.unwrap());
        assert!(after.1.unwrap() > 0);
        assert!(after.1.unwrap() >= before.1.unwrap());
    }
}
