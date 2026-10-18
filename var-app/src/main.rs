// No console window for release builds on Windows; the log file has everything.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod disk;
mod drawing;
mod inhibit;
mod live;
mod lock;
mod markers;
mod monitor;
mod recording;
mod remote;
mod review;
mod setup;
mod shortcuts;
mod video;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::Parser;
use tracing::{error, info};
use var_capture::WatcherOptions;

/// VAR for Trampoline: records all camera feeds of a jury workplace and provides
/// synchronised review during the event.
#[derive(Debug, Parser)]
#[command(name = "var", version)]
struct Args {
    /// Add synthetic test cameras to the device list (debug). Without a value: one camera.
    #[arg(long, value_name = "N", num_args = 0..=1, default_missing_value = "1", default_value_t = 0)]
    synthetic_cameras: u32,

    /// List capture devices from all GStreamer device providers, not only the OS's
    /// capture API (debug).
    #[arg(long)]
    all_device_providers: bool,

    /// Select the device with this ID at start, in its default mode (debug). Repeat for
    /// the next camera slot (up to 4). Synthetic cameras have the IDs `synthetic:1`,
    /// `synthetic:2`, …
    #[arg(long = "camera", value_name = "ID", num_args = 1, action = clap::ArgAction::Append)]
    cameras: Vec<String>,

    /// Mode for the `--camera` at the same position instead of its default, as
    /// FORMAT:WIDTHxHEIGHT@FPS, e.g. `YUY2:1920x1080@50` or `MJPEG:1920x1080@60` (debug).
    #[arg(long = "mode", value_name = "MODE", requires = "cameras")]
    modes: Vec<String>,

    /// Segment length in seconds instead of the default 10 minutes (debug).
    #[arg(long, value_name = "SECONDS")]
    segment_seconds: Option<u64>,

    /// Don't use hardware encoders; record with the software fallback (debug).
    #[arg(long)]
    force_software_encoder: bool,

    /// Start recording as soon as camera and encoder are ready, and stop after this
    /// many seconds (debug, for test recordings).
    #[arg(long, value_name = "SECONDS")]
    record_seconds: Option<u64>,

    /// Recording folder [default: VAR in the user's videos folder].
    #[arg(long, value_name = "DIR")]
    recording_dir: Option<PathBuf>,

    /// Configuration file [default: config.json in the per-user configuration directory].
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Show the debug overlay (frame rates, dropped frames).
    #[arg(long)]
    debug_overlay: bool,

    /// Unplug and replug synthetic camera 2 at this interval in seconds, to test camera
    /// failures (debug; needs `--synthetic-cameras 2`).
    #[arg(long, value_name = "SECONDS")]
    unplug_cycle: Option<u64>,

    /// While recording, perform a random review action every 2 seconds (switch to
    /// review, seek, play/pause, full screen, back to live), to test that review has no
    /// influence on the recording (debug).
    #[arg(long)]
    review_exercise: bool,

    /// Behave as if the recording drive had only this many gigabytes free, to test the
    /// disk space warnings (debug). Recorded data is subtracted.
    #[arg(long, value_name = "GB")]
    simulate_free_gb: Option<f64>,

    /// Log resource use (memory, CPU, handles) and frame counters every N seconds
    /// [default: 60].
    #[arg(long, value_name = "SECONDS")]
    resource_log_seconds: Option<u64>,

    /// Enable the remote control for Bitfocus Companion on this TCP port, listening on
    /// all network interfaces (also in the Recording menu; off by default).
    #[arg(long, value_name = "PORT")]
    remote_control_port: Option<u16>,

    /// Directory for log files [default: per-user application data directory].
    #[arg(long, value_name = "DIR")]
    log_dir: Option<PathBuf>,
}

fn main() -> ExitCode {
    var_core::runtime::suppress_error_dialogs();
    var_core::runtime::use_bundled_gstreamer();
    let args = Args::parse();
    let log_dir = args
        .log_dir
        .clone()
        .unwrap_or_else(var_core::paths::log_dir);

    let _log_guard = match var_core::logging::init(&log_dir) {
        Ok(guard) => guard,
        Err(err) => {
            eprintln!("error: {err:#}");
            return ExitCode::FAILURE;
        }
    };
    info!(version = var_core::VERSION, ?args, log_dir = %log_dir.display(), "application starting");

    let result = run(args, log_dir);
    match &result {
        Ok(()) => info!("application exited"),
        Err(err) => error!("application failed: {err:#}"),
    }
    if result.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn run(args: Args, log_dir: PathBuf) -> anyhow::Result<()> {
    var_capture::init()?;
    var_capture::raise_priority();

    let options = app::Options {
        watcher: WatcherOptions {
            synthetic_cameras: args.synthetic_cameras,
            all_providers: args.all_device_providers,
        },
        cameras: args.cameras,
        modes: args.modes,
        force_software_encoder: args.force_software_encoder,
        record_for: args.record_seconds.map(std::time::Duration::from_secs),
        recording_dir: args.recording_dir,
        segment_length: args.segment_seconds.map(std::time::Duration::from_secs),
        debug_overlay: args.debug_overlay,
        unplug_cycle: args.unplug_cycle.map(std::time::Duration::from_secs),
        review_exercise: args.review_exercise,
        simulated_free: args.simulate_free_gb.map(|gb| (gb.max(0.0) * 1e9) as u64),
        remote_control_port: args.remote_control_port,
        resource_log_interval: args
            .resource_log_seconds
            .map_or(monitor::DEFAULT_INTERVAL, |s| {
                std::time::Duration::from_secs(s.max(1))
            }),
        config_path: args.config.unwrap_or_else(var_core::paths::config_file),
        log_dir,
    };

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title(var_core::APP_NAME)
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([800.0, 500.0]);
    // Window and taskbar icon (the same as embedded in var.exe on Windows).
    match eframe::icon_data::from_png_bytes(include_bytes!("../../assets/var-256.png")) {
        Ok(icon) => viewport = viewport.with_icon(icon),
        Err(e) => error!(%e, "cannot load the application icon"),
    }
    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: wgpu_configuration(),
        viewport,
        event_loop_builder: macos_without_default_menu(),
        ..Default::default()
    };

    eframe::run_native(
        var_core::APP_NAME,
        native_options,
        Box::new(move |cc| Ok(Box::new(app::VarApp::new(cc, options)))),
    )
    .map_err(|err| anyhow::anyhow!("{err}"))
    .context("cannot open the application window")
}

/// macOS: no default application menu. Its "Quit" (Cmd+Q) terminates the process
/// without a close request, so locked mode could not prevent it (SYS-001). The
/// application handles Cmd+Q itself, like closing the window.
#[cfg(target_os = "macos")]
fn macos_without_default_menu() -> Option<eframe::EventLoopBuilderHook> {
    use winit::platform::macos::EventLoopBuilderExtMacOS;
    Some(Box::new(|builder| {
        builder.with_default_menu(false);
    }))
}

#[cfg(not(target_os = "macos"))]
fn macos_without_default_menu() -> Option<eframe::EventLoopBuilderHook> {
    None
}

/// Graphics backends: DX12 on Windows (native, and the API the hardware video decoders
/// share textures with), wgpu's primary backends elsewhere (Metal, Vulkan).
/// The `WGPU_BACKEND` environment variable overrides this, e.g. `WGPU_BACKEND=vulkan`.
fn wgpu_configuration() -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};

    let default_backends = if cfg!(target_os = "windows") {
        wgpu::Backends::DX12
    } else {
        wgpu::Backends::PRIMARY
    };
    let mut setup = WgpuSetupCreateNew::default();
    setup.instance_descriptor.backends = wgpu::Backends::from_env().unwrap_or(default_backends);

    WgpuConfiguration {
        wgpu_setup: WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}
