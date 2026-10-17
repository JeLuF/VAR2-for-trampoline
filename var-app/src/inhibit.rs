//! Keeps the system awake while recording (SYS-002): no sleep, no screen saver.
//!
//! - Windows: `SetThreadExecutionState` with system and display required.
//! - macOS: `caffeinate`, which holds IOPM assertions for display and system sleep,
//!   bound to this process.
//! - Linux: `systemd-inhibit` (logind D-Bus inhibitor for idle and sleep).

use tracing::{info, warn};

/// Prevents sleep and the screen saver while it exists.
pub struct SleepInhibitor {
    #[cfg(not(windows))]
    child: Option<std::process::Child>,
}

impl SleepInhibitor {
    /// Must be created and dropped on the same, long-lived thread (Windows: the
    /// execution state belongs to the thread). The UI thread is used.
    pub fn new() -> Self {
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Power::{
                ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
            };
            // SAFETY: plain flags.
            let previous = unsafe {
                SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED)
            };
            if previous == 0 {
                warn!("cannot prevent sleep: SetThreadExecutionState failed");
            } else {
                info!("sleep and screen saver prevented while recording");
            }
            SleepInhibitor {}
        }
        #[cfg(not(windows))]
        {
            let pid = std::process::id().to_string();
            let mut command = if cfg!(target_os = "macos") {
                let mut command = std::process::Command::new("caffeinate");
                // Display, idle and system sleep; ends with this process.
                command.args(["-d", "-i", "-s", "-w", &pid]);
                command
            } else {
                let mut command = std::process::Command::new("systemd-inhibit");
                command.args([
                    "--what=idle:sleep",
                    "--who=VAR for Trampoline",
                    "--why=Recording",
                    "--mode=block",
                    "sleep",
                    "infinity",
                ]);
                command
            };
            let child = match command
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .spawn()
            {
                Ok(child) => {
                    info!(program = ?command.get_program(), "sleep and screen saver prevented while recording");
                    Some(child)
                }
                Err(e) => {
                    warn!(program = ?command.get_program(), %e, "cannot prevent sleep");
                    None
                }
            };
            SleepInhibitor { child }
        }
    }
}

impl Drop for SleepInhibitor {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Power::{ES_CONTINUOUS, SetThreadExecutionState};
            // SAFETY: plain flags; clears the requirements set in `new`.
            unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
        }
        #[cfg(not(windows))]
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        info!("sleep allowed again");
    }
}
