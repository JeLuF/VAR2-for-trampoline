//! Keeps capture running at full speed when the OS saves power.

/// Opts the process out of Windows power throttling ("efficiency mode", EcoQoS).
///
/// On battery, Windows 11 throttles processes it considers to be in the background
/// (no foreground window, minimised, covered): lower clock speed, efficiency cores
/// and coarse timers. Capture then falls far below the frame rate. At events the
/// laptop runs on mains power, but its battery serves as UPS: after a power failure,
/// recording must continue at full rate even if the window is not in front.
#[cfg(windows)]
pub fn disable_power_throttling() {
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetProcessInformation,
    };

    // Control both policies, enable neither: full execution speed, timer resolution honoured.
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED
            | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        StateMask: 0,
    };
    // SAFETY: `state` is a valid PROCESS_POWER_THROTTLING_STATE of the given size, and
    // GetCurrentProcess returns a pseudo handle that needs no closing.
    let ok = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            (&raw const state).cast(),
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    };
    if ok == 0 {
        tracing::warn!(
            error = %std::io::Error::last_os_error(),
            "cannot disable power throttling"
        );
    } else {
        tracing::info!("power throttling disabled");
    }
}

#[cfg(not(windows))]
pub fn disable_power_throttling() {}

/// Runs the process at above-normal priority, so other programs on the laptop (virus
/// scan, updates, indexing) can't starve capture and recording. Found in the endurance
/// runs: with all CPU cores busy for a minute, capture stalled for more than a second
/// and the watchdog restarted the cameras, interrupting the recording.
#[cfg(windows)]
pub fn raise_priority() {
    use windows_sys::Win32::System::Threading::{
        ABOVE_NORMAL_PRIORITY_CLASS, GetCurrentProcess, SetPriorityClass,
    };
    // SAFETY: pseudo handle of the current process, plain flag.
    let ok = unsafe { SetPriorityClass(GetCurrentProcess(), ABOVE_NORMAL_PRIORITY_CLASS) };
    if ok == 0 {
        tracing::warn!(
            error = %std::io::Error::last_os_error(),
            "cannot raise the process priority"
        );
    } else {
        tracing::info!("process priority above normal");
    }
}

/// Unprivileged processes can't raise their priority on Linux and macOS; nothing to do.
#[cfg(not(windows))]
pub fn raise_priority() {}
