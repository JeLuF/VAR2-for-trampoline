# Step 4 — Camera failure, status bar and configuration: manual test checklist

Run once per OS with two real capture cards, on mains power. Record the result (✔ / ✘ and notes) in the tables and commit the file. Use a release build (Windows: the MSI from the CI artifact, see check 8).

**Requirements covered:** REC-004, REC-005, REC-006, CAP-006, CAP-007, CAP-010, VIEW-005 (partial: without disk time), REC-002 (partial), NFR-003, PLT-003 (partial: smoke build).

## Test setup

| | Windows 11 | macOS 14+ | Ubuntu 26.04 |
|---|---|---|---|
| Tester / date | | | |
| Machine | | | |
| Capture card 1 / 2 (model, USB port) | | | |

## Checks

| # | Check | Windows | macOS | Linux |
|---|---|---|---|---|
| 1 | During a recording, unplug camera 2 for 30 s: its tile shows "CAMERA FAILURE … Not recording" with a red frame, and the menu bar shows "Cam 2 NOT RECORDING" within 2 s (REC-006). Record the time from the log ("camera failed"). | | | |
| 2 | Replug camera 2: live view and recording resume within 5 s without operator action (REC-005). Record the time from the log ("camera recording resumed"). | | | |
| 3 | `var-check` on the recording: camera 1 continuous (no dropped frames, all boundaries continuous); camera 2 has a new segment after the failure (REC-004). | | | |
| 4 | Repeat checks 1–2 twenty times in one session: memory (Task Manager / `top`) stays flat, no camera stays in "restarting", the log shows 20 resumptions. | | | |
| 5 | HDMI source of a capture card switched off (card stays connected): alarm within 2 s; switching the source on again resumes the recording. | | | |
| 6 | Close and restart the application: both cameras and their modes, the recording folder and the segment length are restored (configuration). | | | |
| 7 | Reboot the machine (cards on the same USB ports): the cameras are restored and mapped to the same physical cards (CAP-007). Also try the cards in swapped ports and note the result. | | | |
| 8 | Windows: install the MSI on a clean Windows 11 machine without GStreamer; the application starts from the start menu, shows the cameras and records (PLT-003 smoke test). Uninstall removes it. | | | |
| 9 | Device and mode can't be changed while recording (right-click menu locked) (CAP-010). | | | |

## Results

| Check | Measured values (alarm time, resume time, memory before/after) |
|---|---|
| | |

## Notes

- Device IDs: Windows uses the device path (contains the USB serial number or, for devices without one, the USB port); Linux the USB bus path; macOS the AVFoundation unique ID. A card without serial number plugged into another USB port is a different device, and must be selected again.
- Without capture hardware, failures can be simulated with synthetic cameras: `var --synthetic-cameras 2 --camera synthetic:1 --camera synthetic:2 --unplug-cycle 20 --record-seconds 600`, or the Debug menu.
