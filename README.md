# VAR for Trampoline

Video Assisted Referee application for trampoline competitions: records the camera feeds of one jury workplace for a whole event, with synchronised review while recording.

- Requirements: [var-trampoline-requirements.md](var-trampoline-requirements.md)
- Implementation plan: [var-trampoline-implementation-plan.md](var-trampoline-implementation-plan.md)
- Manual test checklists: [docs/test-checklists/](docs/test-checklists/)

**Status:** step 13 (hardening, endurance, remote control) — all functions of the SRS; packaging and the final acceptance runs (step 14) are next. The playback engine of step 5 is included, but only for the running recording: reviewing finished recordings is not a use case.

## Operation

- View → Cameras sets the number of cameras (1–4; 1 fills the window, 2 side by side, 3–4 in a 2×2 grid). It can't be changed while recording.
- Right-click a camera to choose its device and mode; click it (or press 1–4) for full screen, Esc returns to the grid.
- The Recording menu holds folder, segment length and encoder; Start/Stop and the status of all cameras are in the menu bar.
- A camera that fails (error, or no video for 1 second) shows a red alarm in its tile and in the menu bar. Its pipeline is stopped without affecting the other camera, and it restarts automatically when the device is back; while recording, its recording continues in a new segment.
- While recording, **Review** (menu bar) switches the grid to the recorded video of all cameras, synchronised by capture time; **Back to live** returns. The review bar has play/pause (Space), the wall-clock time of the shown frame, and a scrub bar over the whole recording labelled with the time of day. Recorded video is available for review about 0.5 s after capture. Review reads the segment files only and has no influence on the recording.
- In review: frame steps forward and backward, speeds 2×, 1×, 0.5×, 0.25× and 0.1×, jumps of ±1, ±10 and ±60 s (buttons on both sides of the frame steps, or Shift / Ctrl / Alt + ← →). Review functions used in live mode switch to review first (e.g. ← shows the last frames one by one).
- Reverse slow motion: **Reverse** (Shift + Space) plays backwards at 0.5×, 0.25× or 0.1×; it stops at the start of the recording. 2× and 1× are forwards only. While playing, playback waits briefly for a camera that is still decoding (at most 1 s), so frames aren't skipped and the cameras stay together.
- Laser pointer: in review, dragging with the mouse over a camera draws a red trail that fades out after 3 seconds (adjustable in View → Laser pointer fades after, 0.5–30 s; saved). It is only shown, never recorded or saved; a click still switches to full screen.
- Markers: M sets a marker at the current time (live) or the shown moment (review); `,` and `.` jump to the previous and next marker; the Markers menu lists them with time of day; the scrub bar shows them. They are saved in `markers.json` in the recording folder and can't be edited or deleted.
- Disk space: the menu bar shows the remaining recording time ("Disk 3 h 20 min"); the Recording menu shows the free space as well. Before recording it is estimated from the configured bitrates of the selected cameras, while recording from the data actually written in the last minute. Below 2 h a yellow, below 30 min a red banner warns; both thresholds can be set in the Recording menu. A dismissed banner reappears when the next threshold is crossed.
- Lock mode: the application starts unlocked. After a PIN has been set (mode menu at the far left → Set PIN…), "Lock settings" locks: the Recording and Debug menus, camera selection, the number of cameras, start/stop and quitting (window close, Alt+F4, Cmd+Q, Quit) are unavailable; review, markers and full screen stay available. "Unlock settings…" asks for the PIN — the only place where a PIN is asked for. After 5 wrong PINs, unlocking pauses for 30 s. The PIN is stored only as an Argon2 hash; the mode indicator ("Unlocked" / "LOCKED") is always visible.
- Quitting while recording (unlocked) needs confirmation. While recording, the computer doesn't go to sleep and the screen saver doesn't start.
- F1 (or Help → Keyboard shortcuts) shows all keyboard shortcuts over the video.
- View → Full screen window (F11) hides the window's title bar and the Windows taskbar; the application's menu bar with the recording status stays. The setting is saved, so the application starts in full screen again. With locked settings, full screen can be switched on but not off.
- Help → About shows the version, GStreamer version, encoder, renderer, and the log and configuration folders.
- Application icon: `assets/var.ico` (embedded in `var.exe` on Windows, used by the installer) and `assets/var-256.png` (window icon), drawn by `packaging/icon/make-icon.ps1`.
- Remote control for Bitfocus Companion / Stream Deck: Recording menu → Remote control (off by default). A text protocol over TCP (default port 7450, all network interfaces) for review, marker and view functions; nothing can start or stop the recording, change settings or quit. See [docs/remote-control.md](docs/remote-control.md).
- The application runs at above-normal process priority (Windows), so other programs can't stall capture. Resource use and frame counters are logged every minute for endurance checks. No internet or network access is needed (NFR-002); only the remote control, when enabled, listens on the network.
- Cameras, modes, folder, segment length, disk warning thresholds, remote control and the PIN hash are saved automatically and restored on the next start (`config.json` in the per-user configuration folder).

## Recording layout

```
<recording folder>/
└─ 2026-10-16_09-00-00/                 one folder per recording (event), named after its start
   ├─ Camera 1 - <device name>/
   │  ├─ 0001_09-00-00.ts               MPEG-TS segment (H.264, no audio), plays in VLC
   │  ├─ 0001_09-00-00.idx              index: per frame wall-clock time, keyframe flag, byte offset
   │  ├─ 0002_09-10-00.ts
   │  └─ …
   └─ Camera 2 - <device name>/
```

A new segment starts on a keyframe after the segment length (default 10 minutes). Completed segments can be moved to another drive during the recording. The timestamps of all cameras share one clock, so the segments of different cameras are synchronised.

## Workspace

| Crate | Purpose |
|---|---|
| `var-app` | The application (binary `var`): eframe/egui UI with the wgpu renderer |
| `var-core` | Paths, logging, drop detection, index format, colorimetry |
| `var-capture` | Device enumeration, capture pipelines, synthetic camera |
| `var-record` | Encoding, segment writer, index, free disk space |
| `var-playback` | Index reader, decoders, sync player (from step 5) |
| `var-control` | Remote control server and protocol for Bitfocus Companion |
| `dev-tools/var-check` | Development tool that analyses recordings (from step 2) |

## Building

Needs a current stable Rust toolchain (edition 2024) and GStreamer 1.24 or later, including the development files.

### Windows 11

1. Install Visual Studio 2022 Build Tools with the "Desktop development with C++" workload.
2. Install Rust with [rustup](https://rustup.rs/) (MSVC toolchain).
3. Install the GStreamer **MSVC** package for your architecture from <https://gstreamer.freedesktop.org/download/>, with the development files. On Windows on ARM, use the `arm64` package.
4. Put GStreamer's `bin` folder (which contains `pkg-config.exe`) on `PATH` and point `PKG_CONFIG_PATH` to its `lib\pkgconfig` folder. Either once, permanently for your user (then restart the terminal or VS Code):

   ```powershell
   $gst = "$env:LOCALAPPDATA\Programs\gstreamer\1.0\msvc_arm64"   # or msvc_x86_64; system-wide installs: C:\Program Files\gstreamer\1.0\...
   [Environment]::SetEnvironmentVariable("PKG_CONFIG_PATH", "$gst\lib\pkgconfig", "User")
   [Environment]::SetEnvironmentVariable("Path", "$gst\bin;" + [Environment]::GetEnvironmentVariable("Path", "User"), "User")
   ```

   or per terminal session with `. .\scripts\dev-env.ps1`, which finds the installation itself. If PowerShell refuses to run scripts, first allow it for the current session only: `Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass`.

On Windows, the UI renders with DX12; set `WGPU_BACKEND=vulkan` to try Vulkan instead.

Windows installer (MSI with the GStreamer runtime): install the WiX Toolset 5 (`dotnet tool install --global wix --version 5.0.2`, needs the .NET SDK), then

```powershell
powershell -ExecutionPolicy Bypass -File packaging\windows\build-msi.ps1 -Arch x64     # Intel/AMD laptops
powershell -ExecutionPolicy Bypass -File packaging\windows\build-msi.ps1 -Arch arm64   # Snapdragon laptops
```

Each needs the Rust target (`rustup target add x86_64-pc-windows-msvc` / `aarch64-pc-windows-msvc`), the Visual Studio compilers for the architecture and GStreamer MSVC with development files for the architecture; x64 can be built on an ARM64 machine and vice versa. The MSI is written to `target\msi`. It contains `var.exe`, `var-check.exe`, the GStreamer plugins the application uses and exactly the DLLs they load (found with `dumpbin`), the Visual C++ runtime, and the licence texts of all bundled components; the build fails if a GPL library would be bundled or an imported DLL is missing. When GStreamer is installed next to `var.exe`, the application uses only that runtime, not a GStreamer installed elsewhere.

Installing and testing on the event laptops: [docs/windows-installation.md](docs/windows-installation.md).

### Releases (GitHub)

Set the version in `Cargo.toml` and run `cargo update --workspace` (updates `Cargo.lock` to the new version; the workflow does it too if forgotten), commit both, then create a release on GitHub with a tag of the same version (e.g. `0.2.1` or `v0.2.1`). Publishing it starts the workflow [.github/workflows/release.yml](.github/workflows/release.yml), which builds the Windows installer (x64) and the Linux and macOS binaries, runs smoke tests (installer contents, start without an installed GStreamer, quick unit tests) and attaches the files to the release, after about 20–30 minutes. A tag that doesn't match `Cargo.toml` stops the build. There is no build for each commit; "Run workflow" on the Actions page builds without a release (files under the run's artifacts). The long tests (recording, review, endurance) run locally only, see below. The ARM64 installer is built locally (`-Arch arm64`).

### macOS 14 or later

```sh
brew install rustup gstreamer pkgconf
rustup default stable
```

### Ubuntu 26.04

```sh
sudo apt install build-essential pkg-config curl \
  libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
  libxkbcommon-dev libwayland-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

The user running the application needs access to the video devices (member of the `video` group).

## Running

```sh
cargo run --release --bin var
```

Debug options:

| Option | Effect |
|---|---|
| `--synthetic-cameras [N]` | Adds N (default 1) synthetic test cameras to the device list |
| `--all-device-providers` | Lists devices from all GStreamer providers, not only the OS's capture API (Media Foundation, AVFoundation, V4L2) |
| `--camera ID` | Selects the device with this ID at start, in its default mode (1080p60, else 1080p50). Repeat for camera 2. Synthetic cameras: `synthetic:1`, … |
| `--mode FORMAT:WxH@FPS` | Mode for the `--camera` at the same position, e.g. `YUY2:1920x1080@50` |
| `--segment-seconds N` | Segment length in seconds instead of 10 minutes |
| `--config FILE` | Configuration file to use instead of the per-user one (keeps tests from changing the real configuration) |
| `--debug-overlay` | Shows the debug overlay (also in the View menu) |
| `--review-exercise` | While recording, performs a random review action every 2 seconds (review, seek, play/pause, full screen, back to live), to test that review doesn't affect the recording |
| `--unplug-cycle N` | Unplugs and replugs synthetic camera 2 every N seconds (camera failure test). The Debug menu does the same by hand. |
| `--simulate-free-gb N` | Behaves as if the recording drive had only N GB free, minus what is recorded (disk warning test). The Debug menu offers 50, 20, 5 and 1 GB. |
| `--resource-log-seconds N` | Logs resource use (memory, CPU, handles) and frame counters every N seconds instead of 60 |
| `--remote-control-port PORT` | Enables the remote control on this port (saved, like the menu setting) |
| `--force-software-encoder` | Records with OpenH264 instead of a hardware encoder |
| `--record-seconds N` | Starts recording as soon as camera and encoder are ready and stops after N seconds (test recordings) |
| `--recording-dir DIR` | Recording folder (default: `VAR` in the user's videos folder) |
| `--log-dir DIR` | Writes log files to DIR |

Logs go to the console and to a daily log file `var.<date>.log`; Help → About shows the folder. The environment variable `VAR_LOG` overrides the log filter, e.g. `VAR_LOG=debug`.

## Tests and checks

```sh
cargo test --workspace          # headless tests, no capture hardware needed
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo deny check licenses       # licence check (cargo install cargo-deny)
```

Analysing recordings with `var-check` (exit code 1 if a check fails). It takes a segment file, a camera folder (all segments: index, continuity across segment boundaries, dropped frames, bitrate, keyframes, tracks) or a recording folder (all cameras). `--sync` finds flashes visible to all cameras and checks that they are within ±1 frame; it decodes the video, so it takes a while.

```sh
cargo run --release --bin var-check -- PATH [--bitrate MBIT] [--sync]
```

Example: a one-hour test recording with two capture cards at 1080p50, then the check including the flash test:

```sh
var --camera "<ID 1>" --camera "<ID 2>" --mode MJPEG:1920x1080@50 --mode YUY2:1920x1080@50 --record-seconds 3600
var-check "<recording folder>/<start time>" --sync
```

The synthetic camera shows a white frame every 5 seconds on the shared clock, so `var-check --sync` also works with synthetic cameras.

Capture soak test (synthetic camera, 1080p60 MJPEG and YUY2, prints captured and dropped frames; not run by default):

```sh
VAR_SOAK_SECS=600 cargo test --release -p var-capture --test camera soak -- --ignored --nocapture
```

Memory over repeated camera restarts (synthetic and connected cameras, 15 restarts each; memory must stay flat):

```sh
cargo test --release -p var-capture --test restart_memory -- --ignored --nocapture
```

Endurance run (NFR-001): record with scripted review activity, then evaluate the log (memory stable within ±5 % after the first hour, handles, dropped frames, camera restarts, recording interruptions) and the recording:

```sh
var --camera "<ID 1>" --camera "<ID 2>" --review-exercise --record-seconds 43200
var-check --resources "<log folder>/var.<date>.log"
var-check "<recording folder>/<start time>"
```

Don't use the computer for anything else during the run.

Review at full load (two cameras at 1080p60 recording while being reviewed at 1× and in reverse; prints decoder throughput, lag of the shown frames, waits and skipped frames; not run by default):

```sh
cargo test --release -p var-playback --test load -- --ignored --nocapture
```

Run performance tests on mains power, as at an event (the laptop's battery only serves as UPS). On battery, power-saving settings can reduce performance considerably.

## Licence

MIT, see [LICENSE](LICENSE).
