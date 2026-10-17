//! Resource use of this process (memory, CPU time, handles, threads), for the
//! endurance monitoring (NFR-001): sampled and logged periodically, so a 12-hour run
//! can be checked for growing memory or leaking handles afterwards.

use std::time::Duration;

/// One sample of the process's resource use. Values an OS doesn't provide are `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Resources {
    /// Physical memory in use (working set / resident set), bytes.
    pub resident_bytes: u64,
    /// Memory private to the process (commit charge / anonymous memory), bytes.
    pub private_bytes: Option<u64>,
    /// CPU time used so far, user and system, all threads.
    pub cpu_time: Duration,
    /// Open handles (Windows) or file descriptors (Linux).
    pub handles: Option<u64>,
    pub threads: Option<u64>,
}

/// Samples the resource use of this process. `None` if the OS refuses.
pub fn sample() -> Option<Resources> {
    imp::sample()
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessHandleCount, GetProcessTimes,
    };

    pub fn sample() -> Option<Resources> {
        // SAFETY: the pseudo handle of the current process needs no closing; all out
        // pointers point to valid, correctly sized structures.
        unsafe {
            let process = GetCurrentProcess();
            let mut memory: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
            memory.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
            if GetProcessMemoryInfo(
                process,
                &mut memory as *mut _ as *mut PROCESS_MEMORY_COUNTERS,
                memory.cb,
            ) == 0
            {
                return None;
            }
            let zero = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
            let cpu_time =
                if GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) != 0
                {
                    // 100 ns units.
                    let ticks = |t: FILETIME| {
                        (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
                    };
                    Duration::from_nanos((ticks(kernel) + ticks(user)) * 100)
                } else {
                    Duration::ZERO
                };
            let mut handles = 0u32;
            let handles =
                (GetProcessHandleCount(process, &mut handles) != 0).then_some(u64::from(handles));
            Some(Resources {
                resident_bytes: memory.WorkingSetSize as u64,
                private_bytes: Some(memory.PrivateUsage as u64),
                cpu_time,
                handles,
                threads: None,
            })
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;

    pub fn sample() -> Option<Resources> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let field = |name: &str| {
            status
                .lines()
                .find_map(|line| line.strip_prefix(name))
                .and_then(|rest| rest.split_whitespace().next()?.parse::<u64>().ok())
        };
        let resident_bytes = field("VmRSS:")? * 1024;
        let private_bytes = field("RssAnon:").map(|kb| kb * 1024);
        let threads = field("Threads:");

        // Fields 14 and 15 of /proc/self/stat (after the command in parentheses):
        // user and system time in clock ticks.
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        let after_command = &stat[stat.rfind(')')? + 2..];
        let fields: Vec<&str> = after_command.split_whitespace().collect();
        let ticks: u64 =
            fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?;
        // SAFETY: plain query.
        let per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
        let cpu_time = Duration::from_nanos(ticks * 1_000_000_000 / per_second);

        let handles = std::fs::read_dir("/proc/self/fd")
            .ok()
            .map(|dir| dir.count() as u64);
        Some(Resources {
            resident_bytes,
            private_bytes,
            cpu_time,
            handles,
            threads,
        })
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    pub fn sample() -> Option<Resources> {
        let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = size_of::<libc::proc_taskinfo>() as libc::c_int;
        // SAFETY: `info` is a valid buffer of `size` bytes.
        let written = unsafe {
            libc::proc_pidinfo(
                libc::getpid(),
                libc::PROC_PIDTASKINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        if written != size {
            return None;
        }
        Some(Resources {
            resident_bytes: info.pti_resident_size,
            private_bytes: None,
            cpu_time: Duration::from_nanos(info.pti_total_user + info.pti_total_system),
            handles: None,
            threads: Some(info.pti_threadnum as u64),
        })
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod imp {
    use super::*;

    pub fn sample() -> Option<Resources> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_this_process() {
        let first = sample().expect("resource sample");
        assert!(first.resident_bytes > 1_000_000, "{first:?}");
        if cfg!(any(windows, target_os = "linux")) {
            assert!(first.handles.unwrap() > 0);
        }
        // Burn some CPU: the CPU time grows.
        let start = std::time::Instant::now();
        let mut x = 0u64;
        while start.elapsed() < Duration::from_millis(200) {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
        }
        std::hint::black_box(x);
        let second = sample().unwrap();
        assert!(second.cpu_time > first.cpu_time, "{first:?} {second:?}");
    }
}
