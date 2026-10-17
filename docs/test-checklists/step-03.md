# Step 3 — Two cameras, segments and timestamps: manual test checklist

Run once per OS with two real capture cards, on mains power. Record the result (✔ / ✘ and notes) in the tables and commit the file. Use a release build.

**Requirements covered:** CAP-001, CAP-005, CAP-008, CAP-009 (partial: capture timestamps), REC-001, REC-009 (partial: files), REC-010 (partial: writer tolerates moved segments), REC-012, VIEW-001, VIEW-002 (partial: live), VIEW-003.

Test recordings: GUI, or `var --camera "<ID 1>" --camera "<ID 2>" --mode … --mode … --record-seconds N [--segment-seconds N]`. Check with `var-check "<recording folder>" [--sync]`.

## Test setup

| | Windows 11 | macOS 14+ | Ubuntu 26.04 |
|---|---|---|---|
| Tester / date | | | |
| Machine (CPU, GPU) | | | |
| Capture card 1 (model, format) | | | |
| Capture card 2 (model, format) | | | |
| Flash source (camera flash, LED) | | | |

## Checks

| # | Check | Windows | macOS | Linux |
|---|---|---|---|---|
| 1 | Right-clicking a camera tile selects device and mode for that camera; a device used by one camera is disabled for the other; while recording the menu is locked (CAP-005, CAP-001, CAP-010). | | | |
| 1a | The menu bar shows the recording status (REC, elapsed time, bitrate and segment per camera) and Start/Stop; the Recording menu contains folder, segment length and encoder. | | | |
| 2 | The live grid shows both cameras side by side, in setup and while recording (VIEW-001, VIEW-003). | | | |
| 3 | Clicking a camera or pressing its number (1–4) shows it full screen; clicking again or Esc returns to the grid (VIEW-002). | | | |
| 3a | View → Cameras: 1, 2, 3 and 4 cameras give a full tile, side by side, and a 2×2 grid; the setting is locked while recording and restored after a restart. | | | |
| 3b | With 3 and 4 cameras at the event resolution (1080p50/60), a 10-minute recording passes `var-check` (no dropped frames). Record CPU load and memory. | | | |
| 4 | One "Start recording" records both cameras into one recording folder with a subfolder per camera (REC-001). | | | |
| 5 | Flash test: fire a flash visible to both cameras several times during a recording; `var-check --sync` reports every flash within ±1 frame (CAP-009). Record the offsets. | | | |
| 6 | 1-hour recording at 1080p50 (default 10-minute segments): `var-check` passes the index, boundary and dropped-frame checks for both cameras. | | | |
| 7 | Every segment of check 6 plays in VLC; playing segments one after the other shows no jump. | | | |
| 8 | Killing the application during a recording: all completed segments and the partial last segment play in VLC; `var-check` passes the index check (REC-012). | | | |
| 9 | Moving a completed segment to another drive with the file manager during a recording: no error in the application, the recording continues; `var-check` reports the segment as missing and finds no gap otherwise (REC-010). | | | |
| 10 | The debug overlay shows 0 dropped frames for both cameras during check 6. Display fps and skipped frames recorded (see notes). | | | |

## Results

| Check | Measured values |
|---|---|
| | |

## Notes

- "Display" fps in the debug overlay is the rate at which the live view shows new frames. A 50 fps camera on a 60 Hz screen shows some frames skipped; this affects only the live view, not the recording. On the development machine (Snapdragon X Plus) the UI runs at 60 fps with two 1080p50 cameras, 0.3 ms CPU per UI frame, and shows 42–47 of 50 frames per second per camera.
- Up to 4 cameras: on the development machine (Snapdragon X Plus, Qualcomm hardware encoder via Media Foundation) 4 × 720p50 and 2 × 1080p50 record without loss, 4 × 1080p50 do not: the hardware encoder can't encode 4 × 1080p50 at once (live view alone runs without loss). Encoder throughput limits the number of cameras per machine; check 3b finds the limit of the event laptops.
- Built-in laptop cameras often vary their frame rate (low light, image processing); use capture cards for the frame-drop checks.
