# Installing and testing on the event laptops (Windows)

## Which installer

| Laptop | Installer |
|---|---|
| Intel or AMD processor (almost all laptops) | `var-for-trampoline-<version>-x64.msi` |
| Snapdragon / ARM processor (e.g. Surface Pro X, Copilot+ PCs with Snapdragon X) | `var-for-trampoline-<version>-arm64.msi` |

Settings → System → About → "System type" shows it ("x64-based processor" or "ARM-based processor").

## Installing

Double-click the MSI (administrator rights needed; installs to `C:\Program Files\VAR for Trampoline`). It creates:
- Start menu → VAR for Trampoline → **VAR for Trampoline** and **VAR for Trampoline (test cameras)** (two synthetic test cameras and the debug overlay, for a first test without capture cards),
- a desktop shortcut.

The installer contains everything the application needs, including GStreamer; nothing else has to be installed. An installed GStreamer is not used. Uninstall via Settings → Apps; recordings, configuration and logs stay.

Where things are:
- Recordings: `Videos\VAR` (changeable in the Recording menu).
- Configuration: `%APPDATA%\VAR for Trampoline\config\config.json`.
- Logs: `%LOCALAPPDATA%\VAR for Trampoline\data\logs`, one file per day (Help → About shows the folder and opens it).

The laptop must run on mains power during tests and events.

## Test on the competition hardware

1. **Test cameras:** start "VAR for Trampoline (test cameras)". Both tiles show colour bars with a frame counter and clock. Start recording, press R (review), Space, ←/→, Shift+Space (reverse). Recording menu → *Encoder* shows the encoder in use (should be *hardware*: NVIDIA NVENC, Intel Quick Sync, AMD AMF or Media Foundation).
   F11 (or View → Full screen window) hides the title bar and the taskbar; the setting is kept for the next start.
2. **Real cameras:** start "VAR for Trampoline", right-click each tile → device and mode (1080p50 or 1080p60). Debug overlay (View menu) shows the frame rate and dropped frames per camera.
3. **Short recording:** record 10 minutes while using review now and then; then check the recording in a terminal:

   ```powershell
   cd "C:\Program Files\VAR for Trampoline"
   .\var-check.exe "$env:USERPROFILE\Videos\VAR\<start time>" --sync
   ```

   `--sync` needs a visible flash in all cameras (e.g. a camera flash or switching the hall light); without it, leave it out.
4. **Endurance:** record for several hours with review activity, then evaluate the log:

   ```powershell
   .\var.exe --review-exercise --record-seconds 43200
   .\var-check.exe --resources "<log folder>\var.<date>.log"
   .\var-check.exe "$env:USERPROFILE\Videos\VAR\<start time>"
   ```

   `--review-exercise` performs a random review action every 2 s; `--record-seconds` starts recording as soon as the cameras are live and stops after the time. The cameras selected last are used.
5. **Remote control (optional):** see [remote-control.md](remote-control.md). Windows Firewall asks on first use; allow private networks.
6. **Stream to OBS (optional, x64 only):** set up OBS as in [streaming.md](streaming.md), then Stream menu → SRT URL `srt://<OBS computer>:9000` → Start streaming. Check the delay in OBS and, with the debug overlay, that the cameras drop no frames while streaming and recording.

Please send the log file of each test run (and the `var-check` output) back for evaluation.

## Checklists

The full test lists per function are in [test-checklists](test-checklists/); step 14 lists the final acceptance run.
