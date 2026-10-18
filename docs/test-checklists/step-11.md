# Step 11 — Laser pointer drawing: manual test checklist

Run once per OS, while recording, on mains power. Record the result (✔ / ✘ and notes) and commit the file.

**Requirements covered:** DRW-001, DRW-002, DRW-003.

Automated: `cargo test -p var-app drawing` (fading, trails removed by themselves).

## Checks

| # | Check | Windows | macOS | Linux |
|---|---|---|---|---|
| 1 | In review, the mouse pointer over a camera is a crosshair; dragging with the left button draws a red glowing trail (DRW-001). | | | |
| 2 | The trail stays fully visible for about 2 s, then fades out within about 1 s; nothing remains (DRW-002). | | | |
| 3 | Drawing works on every camera of the grid and in full screen; a short click still switches to full screen. | | | |
| 4 | Drawing works while paused, while playing at 1×, in reverse and while stepping. | | | |
| 5 | Drawing works in locked mode (ROLE-005). | | | |
| 6 | No drawing in live mode (the live view is not affected). | | | |
| 7 | Changing the window size or switching to full screen: a trail stays on the same spot of the picture. | | | |
| 8 | Drawing during playback at 1×: the UI frame rate in the debug overlay (bottom left) stays at the display rate. | | | |
| 9 | Recordings and `markers.json` contain no drawing data: after drawing, the recording plays back without trails in VLC; `markers.json` is unchanged (DRW-003). | | | |
| 10 | Back to live and review again: earlier trails are gone. | | | |

## Results

| Check | Measured values |
|---|---|
| 1, 2, 8 | Development machine (2 synthetic cameras at 1080p60 YUY2, recording): trail drawn during 1× playback, UI 60 fps at 0.2–0.3 ms per frame; trail faded and gone after 3 s. |
