//! Logging to console and to a daily rotated log file (NFR-005).
//!
//! The filter can be overridden with the `VAR_LOG` environment variable using the
//! `tracing_subscriber::EnvFilter` syntax, e.g. `VAR_LOG=debug,var_capture=trace`.

use std::fs;
use std::path::Path;

use anyhow::Context;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt::time::ChronoLocal;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

/// Environment variable that overrides the default log filter.
pub const FILTER_ENV: &str = "VAR_LOG";

const DEFAULT_FILTER: &str = "info,wgpu_core=error,wgpu_hal=error,naga=warn";
const MAX_LOG_FILES: usize = 30;

/// Keeps the background log writer alive. Dropping it flushes the log file, so it must
/// be held until the application exits.
#[must_use = "dropping the guard stops writing the log file"]
pub struct LogGuard {
    _worker: WorkerGuard,
}

/// Installs the global logger, writing to stderr and to `var.<date>.log` in `log_dir`.
///
/// Timestamps are local wall-clock time, so log entries can be matched with the
/// wall-clock labels of the recording.
pub fn init(log_dir: &Path) -> anyhow::Result<LogGuard> {
    fs::create_dir_all(log_dir)
        .with_context(|| format!("cannot create log directory {}", log_dir.display()))?;

    let file_appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("var")
        .filename_suffix("log")
        .max_log_files(MAX_LOG_FILES)
        .build(log_dir)
        .context("cannot open log file")?;
    let (file_writer, worker) = tracing_appender::non_blocking(file_appender);

    let filter =
        EnvFilter::try_from_env(FILTER_ENV).unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let timer = ChronoLocal::new("%Y-%m-%d %H:%M:%S%.3f".to_owned());

    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_timer(timer.clone())
                .with_writer(std::io::stderr),
        )
        .with(
            fmt::layer()
                .with_timer(timer)
                .with_ansi(false)
                .with_writer(file_writer),
        )
        .try_init()
        .context("logger already initialised")?;

    install_panic_hook();

    Ok(LogGuard { _worker: worker })
}

/// Logs panics before the default hook prints them, so they end up in the log file.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let thread = thread.name().unwrap_or("<unnamed>");
        tracing::error!(thread, "panic: {info}");
        default_hook(info);
    }));
}
