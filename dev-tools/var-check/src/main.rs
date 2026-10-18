//! `var-check`: analyses recordings and checks them against the requirements.
//!
//! Not part of the delivered application.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use var_check::{CameraReport, Report};

/// Analyse VAR recordings: frames, dropped frames, keyframe interval, bitrate, tracks,
/// segment continuity, index, and synchronisation between cameras.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Recording files (.ts), camera folders (segments) or event folders (one folder
    /// per camera).
    #[arg(required = true)]
    paths: Vec<PathBuf>,

    /// Expected average bitrate in Mbit/s; checked with ±20 % tolerance (REC-008).
    /// Default: 15 for 50 fps and 18 for 60 fps recordings.
    #[arg(long, value_name = "MBIT")]
    bitrate: Option<f64>,

    /// Expected keyframe interval in seconds (AD-4).
    #[arg(long, value_name = "SECONDS", default_value_t = 0.5)]
    gop: f64,

    /// For event folders: find flashes visible to all cameras and check that they are
    /// within ±1 frame (CAP-009). Decodes the video, so it takes a while.
    #[arg(long)]
    sync: bool,

    /// The paths are application log files: evaluate the resource entries of an
    /// endurance run (memory, handles, dropped frames, camera restarts; NFR-001).
    #[arg(long)]
    resources: bool,
}

/// Collects the PASS/FAIL results.
struct Checks {
    all_passed: bool,
}

impl Checks {
    fn check(&mut self, name: &str, passed: bool, detail: String) {
        println!(
            "  {}  {name}: {detail}",
            if passed { "PASS" } else { "FAIL" }
        );
        self.all_passed &= passed;
    }
}

fn main() -> ExitCode {
    // Installed with the application: use its GStreamer, not another one.
    var_core::runtime::suppress_error_dialogs();
    var_core::runtime::use_bundled_gstreamer();
    let args = Args::parse();
    let mut checks = Checks { all_passed: true };

    if args.resources {
        let paths: Vec<&Path> = args.paths.iter().map(PathBuf::as_path).collect();
        if let Err(err) = check_resources(&paths, &mut checks) {
            println!("  ERROR  {err:#}");
            checks.all_passed = false;
        }
        return if checks.all_passed {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }

    for path in &args.paths {
        let result = if path.is_file() {
            check_file(path, &args, &mut checks)
        } else {
            match var_check::camera_dirs(path) {
                Ok(cameras) if !cameras.is_empty() => check_event(&cameras, &args, &mut checks),
                _ => check_camera(path, &args, &mut checks).map(|_| ()),
            }
        };
        if let Err(err) = result {
            println!("{}\n  ERROR  {err:#}\n", path.display());
            checks.all_passed = false;
        }
    }

    if checks.all_passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn check_file(path: &Path, args: &Args, checks: &mut Checks) -> anyhow::Result<()> {
    println!("{}", path.display());
    let content = var_check::read_file(path)?;
    let report = var_check::analyze(path)?;
    print_report(&report);
    if let Ok((_, records)) = var_check::read_index(path) {
        let problems = var_check::check_index(path, &content, &records);
        print_problems(&problems);
        checks.check(
            "index",
            problems.is_empty(),
            format!("{} records", records.len()),
        );
    }
    check_report(&report, args, checks);
    println!();
    Ok(())
}

fn check_camera(dir: &Path, args: &Args, checks: &mut Checks) -> anyhow::Result<CameraReport> {
    println!("{}  (camera folder)", dir.display());
    let camera = var_check::analyze_camera(dir)?;

    for segment in &camera.segments {
        let r = &segment.report;
        let index = match &segment.index_problems {
            None => "no index".to_owned(),
            Some(p) if p.is_empty() => "index ok".to_owned(),
            Some(p) => format!("INDEX: {} problem(s)", p.len()),
        };
        println!(
            "  {}  {} frames, {:.1} s, {:.1} MB, {index}",
            segment.path.file_name().unwrap().to_string_lossy(),
            r.frames,
            r.duration,
            r.file_size as f64 / 1e6
        );
        if let Some(problems) = &segment.index_problems {
            print_problems(problems);
        }
    }
    for number in &camera.missing_segments {
        println!("  segment {number:04} missing (moved or deleted)");
    }
    println!("  Whole recording (from the index):");
    print_report(&camera.combined);

    let index_ok = camera
        .segments
        .iter()
        .all(|s| s.index_problems.as_ref().is_some_and(|p| p.is_empty()));
    checks.check(
        "index of every segment",
        index_ok,
        format!("{} segment(s)", camera.segments.len()),
    );
    let discontinuous: Vec<_> = camera
        .boundaries
        .iter()
        .filter(|b| !b.is_continuous())
        .collect();
    for b in &discontinuous {
        println!(
            "  boundary after {}: {:.2} frames",
            b.after, b.interval_frames
        );
    }
    checks.check(
        "segment boundaries continuous (REC-009)",
        discontinuous.is_empty(),
        format!(
            "{} boundaries, {} with gap or overlap",
            camera.boundaries.len(),
            discontinuous.len()
        ),
    );
    check_report(&camera.combined, args, checks);
    println!();
    Ok(camera)
}

fn check_event(cameras: &[PathBuf], args: &Args, checks: &mut Checks) -> anyhow::Result<()> {
    for dir in cameras {
        if let Err(err) = check_camera(dir, args, checks) {
            println!("  ERROR  {err:#}\n");
            checks.all_passed = false;
        }
    }
    if args.sync {
        println!(
            "Synchronisation (flashes, reference: {})",
            cameras[0].display()
        );
        let offsets = var_check::sync(cameras)?;
        for o in &offsets {
            println!(
                "  {}  {}: {:+.1} ms ({:+.2} frames)",
                format_wall(o.reference_wall_us),
                o.camera.file_name().unwrap().to_string_lossy(),
                o.offset_us as f64 / 1000.0,
                o.offset_frames
            );
        }
        let worst = offsets
            .iter()
            .map(|o| o.offset_frames.abs())
            .fold(0.0, f64::max);
        checks.check(
            "cameras in sync within ±1 frame (CAP-009)",
            !offsets.is_empty() && worst <= 1.0,
            if offsets.is_empty() {
                "no flash found in all cameras".to_owned()
            } else {
                format!("{} flash(es), worst {worst:.2} frames", offsets.len())
            },
        );
        println!();
    }
    Ok(())
}

/// Endurance run (NFR-001, NFR-004) from the application's resource log.
fn check_resources(paths: &[&Path], checks: &mut Checks) -> anyhow::Result<()> {
    let report = var_check::resources::resource_log(paths)?;
    println!("Resource log: {} entries", report.samples.len());
    if report.samples.is_empty() {
        anyhow::bail!("no resource entries (application older than step 13?)");
    }
    println!(
        "  Duration         {}",
        format_duration(report.duration_s())
    );
    let cpu: Vec<f64> = report
        .samples
        .iter()
        .filter_map(|s| s.cpu_percent)
        .collect();
    if !cpu.is_empty() {
        println!(
            "  CPU              mean {:.1} %, max {:.1} % of all cores",
            cpu.iter().sum::<f64>() / cpu.len() as f64,
            cpu.iter().copied().fold(0.0, f64::max)
        );
    }
    if let Some(mean) = report.memory_mean_mb() {
        println!("  Memory           mean {mean:.0} MB after 10 min");
    }
    if let Some((first, last, max)) = report.handles_range() {
        println!("  Handles          {first:.0} → {last:.0}, max {max:.0}");
    }
    for (slot, camera) in &report.cameras {
        println!(
            "  Camera {slot}         {} frames captured, {} dropped, {} restart(s), {} recording interruption(s)",
            camera.captured,
            camera.dropped,
            camera.generations.saturating_sub(1),
            camera.interruptions
        );
    }
    println!(
        "  Log              {} error(s), {} warning(s), {} camera failure(s)",
        report.errors, report.warnings, report.camera_failures
    );

    match report.memory_windows() {
        Some(memory) => {
            let window =
                |(from, to): (f64, f64)| format!("{}–{}", format_minutes(from), format_minutes(to));
            println!(
                "  Memory windows   {}: peak {:.0} MB, mean {:.0} MB  →  {}: peak {:.0} MB, mean {:.0} MB ({:+.1} %)",
                window(memory.reference),
                memory.reference_peak,
                memory.reference_mean,
                window(memory.last),
                memory.last_peak,
                memory.last_mean,
                memory.mean_change_percent()
            );
            let after_first_hour = memory.reference.0 >= 3600.0;
            checks.check(
                if after_first_hour {
                    "memory stable after the first hour, peak ±5 % (NFR-001)"
                } else {
                    "memory stable, peak ±5 % (run shorter than 1.5 h)"
                },
                memory.peak_change_percent().abs() <= 5.0,
                format!(
                    "peak {:.0} → {:.0} MB ({:+.1} %)",
                    memory.reference_peak,
                    memory.last_peak,
                    memory.peak_change_percent()
                ),
            );
        }
        None => println!("  Memory           run too short to compare"),
    }
    let dropped: u64 = report.cameras.values().map(|c| c.dropped).sum();
    checks.check(
        "no dropped frames (NFR-004)",
        dropped == 0,
        format!("{dropped} dropped"),
    );
    let interruptions: u64 = report.cameras.values().map(|c| c.interruptions).sum();
    checks.check(
        "recording not interrupted (REC-002)",
        interruptions == 0 && report.camera_failures == 0,
        format!(
            "{interruptions} interruption(s), {} camera failure(s)",
            report.camera_failures
        ),
    );
    Ok(())
}

fn print_problems(problems: &[String]) {
    for p in problems {
        println!("    {p}");
    }
}

fn print_report(report: &Report) {
    let fps = report.declared_fps.unwrap_or(report.measured_fps);
    println!("  File size        {:.1} MB", report.file_size as f64 / 1e6);
    println!("  Tracks           {}", report.tracks.join(" | "));
    println!(
        "  Duration         {:.2} s ({})",
        report.duration,
        format_duration(report.duration)
    );
    println!(
        "  Frames           {} ({} keyframes), {:.3} fps measured, {}",
        report.frames,
        report.keyframes,
        report.measured_fps,
        report
            .declared_fps
            .map_or("not declared".to_owned(), |f| format!(
                "{f:.3} fps declared"
            ))
    );
    println!(
        "  Keyframe interval max {} frames, average {:.1} frames ({:.2} s)",
        report.max_gop,
        report.average_gop,
        report.average_gop / fps
    );
    println!(
        "  Bitrate          {:.2} Mbit/s video, {:.2} Mbit/s file",
        report.video_bitrate / 1e6,
        report.file_bitrate / 1e6
    );
    for (position, missing) in report.gaps.iter().take(20) {
        println!(
            "  Gap at {}: {missing} frame(s) missing",
            format_duration(*position)
        );
    }
    if report.gaps.len() > 20 {
        println!("  … {} more gaps", report.gaps.len() - 20);
    }
}

fn check_report(report: &Report, args: &Args, checks: &mut Checks) {
    let fps = report.declared_fps.unwrap_or(report.measured_fps);
    let expected_bitrate = args.bitrate.unwrap_or(if fps > 55.0 { 18.0 } else { 15.0 });
    let file_mbit = report.file_bitrate / 1e6;
    checks.check(
        "video track",
        report.video_tracks == 1,
        format!("{} video track(s)", report.video_tracks),
    );
    checks.check(
        "no audio (REC-013)",
        report.audio_tracks == 0,
        format!("{} audio track(s)", report.audio_tracks),
    );
    checks.check(
        "no dropped frames (NFR-004)",
        report.dropped_frames == 0,
        format!(
            "{} dropped ({} late frames without loss)",
            report.dropped_frames, report.late_frames
        ),
    );
    checks.check(
        "bitrate ±20 % (REC-008)",
        (file_mbit - expected_bitrate).abs() <= 0.2 * expected_bitrate,
        format!("{file_mbit:.2} Mbit/s, expected {expected_bitrate} Mbit/s"),
    );
    checks.check(
        "keyframe interval (AD-4)",
        report.max_gop as f64 <= (args.gop * fps).round() + 0.5,
        format!(
            "max {} frames, expected ≤ {}",
            report.max_gop,
            (args.gop * fps).round()
        ),
    );
}

/// `h:mm` from seconds.
fn format_minutes(seconds: f64) -> String {
    let minutes = (seconds / 60.0).round() as u64;
    format!("{}:{:02}", minutes / 60, minutes % 60)
}

fn format_duration(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    format!(
        "{}:{:02}:{:02}.{:03}",
        total / 3600,
        total / 60 % 60,
        total % 60,
        ((seconds.fract()) * 1000.0) as u64
    )
}

/// Local time of a wall-clock time, `HH:MM:SS.mmm`.
fn format_wall(wall_us: i64) -> String {
    use chrono::{Local, TimeZone};
    Local.timestamp_micros(wall_us).single().map_or_else(
        || wall_us.to_string(),
        |t| t.format("%H:%M:%S%.3f").to_string(),
    )
}
