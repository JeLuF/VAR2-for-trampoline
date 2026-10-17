# Step 2 — Recording one camera: manual test checklist

Run once per OS with real capture cards, on mains power. Record the result (✔ / ✘ and notes) in the tables and commit the file. Use a release build.

**Requirements covered:** CAP-002, REC-007, REC-007a, REC-007b, REC-008, REC-013, REC-001 (partial: one camera), EXP-001 (partial: format), EXP-002 (partial: confirmation), PLT-007 (partial: encoder choice), NFR-004 (partial).

Test recordings can be made with the GUI or with
`var --camera "<ID>" --mode <FORMAT:WxH@FPS> --record-seconds 600` (device ID: tooltip of the device in the camera's right-click menu, or the log). Check each file with `var-check <file>`.

## Test setup

| | Windows 11 | macOS 14+ | Ubuntu 26.04 |
|---|---|---|---|
| Tester / date | | | |
| Machine (CPU, GPU) | | | |
| Encoder shown in setup (hardware) | | | |
| MJPEG capture card (model) | | | |
| YUY2 capture card (model) | | | |

## Checks

| # | Check | Windows | macOS | Linux |
|---|---|---|---|---|
| 1 | The Recording menu shows a hardware encoder; the log contains "encoder selected" with the same encoder (REC-007, REC-007b). | | | |
| 2 | MJPEG card, 1080p50, 10 minutes: `var-check` passes (0 dropped, 15 Mbit/s ±20 %, keyframe every 0.5 s, no audio). | | | |
| 3 | MJPEG card, 1080p60, 10 minutes: `var-check` passes (18 Mbit/s ±20 %). | | | |
| 4 | YUY2 card, 1080p50, 10 minutes: `var-check` passes. | | | |
| 5 | YUY2 card, 1080p60, 10 minutes: `var-check` passes. | | | |
| 6 | The recordings of checks 2–5 play in VLC from start to end, including seeking. | | | |
| 7 | Live view continues without stutter while recording (debug overlay: display ≈ capture fps; no visible stutter). | | | |
| 8 | `--force-software-encoder`: the Recording menu shows OpenH264 (software); a 10-minute 1080p60 recording passes `var-check` (REC-007a). Record the CPU load. | | | |
| 9 | "Stop recording" asks for confirmation; "Cancel" keeps recording; confirming stops it (EXP-002). | | | |
| 10 | Closing the window while recording asks for confirmation; "Cancel" keeps the application and recording running. Also try Alt+F4 (Windows) and Cmd+Q (macOS) (risk: quit interception). | | | |
| 11 | Device and mode can't be changed while recording (CAP-010). | | | |
| 12 | Killing the process during a recording (Task Manager / `kill -9`): the file plays in VLC up to the kill (REC-012). | | | |
| 13 | A recording folder that can't be written (e.g. read-only) shows an error at start; live view continues. | | | |
| 14 | CPU and GPU load during checks 3 and 5, recorded. | | | |

## Results

| Check | Measured values (frames, dropped, bitrate, CPU %, GPU %) |
|---|---|
| | |

## Notes

The debug overlay and `var-check` show frames that arrive late without a frame being lost ("late") separately from dropped frames. Many late frames point to an unstable USB connection or a busy machine.
