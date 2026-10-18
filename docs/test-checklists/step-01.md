# Step 1 — Live view of one camera: manual test checklist

Run once per OS with real capture cards. Record the result (✔ / ✘ and notes) in the tables and commit the file. Use a release build (`cargo run --release --bin var`) with the laptop on mains power, as at an event.

**Requirements covered:** CAP-004, CAP-002 (partial: live capture at 50/60 fps), CAP-005 (partial: one camera), CAP-006 (partial), CAP-008 (partial: one camera), VIEW-003 (partial).

## Test setup

| | Windows 11 | macOS 14+ | Ubuntu 26.04 |
|---|---|---|---|
| Tester / date | | | |
| Machine (CPU, GPU) | | | |
| Renderer (Help → About) | | | |
| MJPEG capture card (model) | | | |
| YUY2 capture card (model) | | | |
| HDMI source and output mode (1080p50 / 1080p60) | | | |

## Checks

The debug overlay (top left of the video) shows the mode, capture frame rate and frame count, dropped frames (red if > 0) and display frame rate with skipped frames.

| # | Check | Windows | macOS | Linux |
|---|---|---|---|---|
| 1 | Selecting a device preselects 1080p60 (or 1080p50); the mode list contains only supported formats (MJPEG, YUY2, NV12). | | | |
| 2 | MJPEG card, 1080p50: live view for 10 minutes, dropped = 0, capture ≈ 50 fps. | | | |
| 3 | MJPEG card, 1080p60: live view for 10 minutes, dropped = 0, capture ≈ 60 fps. | | | |
| 4 | YUY2 card, 1080p50: live view for 10 minutes, dropped = 0, capture ≈ 50 fps. | | | |
| 5 | YUY2 card, 1080p60: live view for 10 minutes, dropped = 0, capture ≈ 60 fps. | | | |
| 6 | Colours look correct (no colour cast, black is black, white is white) for MJPEG and YUY2. | | | |
| 7 | Latency test: film a millisecond clock on a screen, photograph screen and live view together; latency ≤ 200 ms. Record the value. | | | |
| 8 | CPU and GPU load during check 3 and 5 (Task Manager / Activity Monitor / `top`), recorded as baseline. | | | |
| 9 | Repeat check 3 with the window behind another window for 5 minutes: dropped = 0. | | | |
| 10 | Unplugging the card shows "Camera disconnected"; plugging it back in restarts the live view without user action. | | | |
| 11 | Switching mode or device restarts the live view with the new mode within about 2 s. | | | |
| 12 | `var --synthetic-cameras --camera synthetic:1` shows the test pattern with a frame counter (top left) and the time (top right). | | | |

## Results

| Check | Measured values (fps, dropped, latency, CPU %, GPU %) |
|---|---|
| | |

## Notes

Record cards that need a mode other than their advertised one, show wrong colours or drop frames, for the compatibility list.
