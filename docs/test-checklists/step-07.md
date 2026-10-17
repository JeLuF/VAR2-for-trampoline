# Step 7 — Slow motion, frame stepping and jump hotkeys: manual test checklist

Run once per OS, first with synthetic cameras (burned-in frame counter), then with two real capture cards, while recording, on mains power. Record the result (✔ / ✘ and notes) and commit the file.

**Requirements covered:** REV-003, REV-004, REV-006, INP-001 (partial: review functions).

Automated: `cargo test --release -p var-playback --test stepping` (exact steps across keyframes and a segment boundary, 100 backward steps at key-repeat pace, speeds, jumps). On the development machine: backward step max 13 ms, speeds within ±2.3 %.

## Checks

| # | Check | Windows | macOS | Linux |
|---|---|---|---|---|
| 1 | Synthetic cameras: each press of → / ← changes the frame counter by exactly 1 on all cameras, also across a keyframe (every 25/30 frames) and a segment boundary (`--segment-seconds 10`). | | | |
| 2 | Holding ← steps back continuously and smoothly over several seconds; every step responds within 100 ms (no visible hesitation). | | | |
| 3 | Speeds 2×, 1×, 0.5×, 0.25×, 0.1× (buttons and + / −): measured with a stopwatch against the burned-in clock or frame counter over 20 s of recording time, within ±5 %. | | | |
| 4 | Shift / Ctrl (Cmd) / Alt + ← / → move the position by 1 / 10 / 60 s on all cameras (time display and burned-in clock). | | | |
| 5 | ← or Shift + ← in live mode switches to review at the live edge and steps back. | | | |
| 6 | Help menu lists all shortcuts; each works as described. | | | |
| 7 | Full-screen camera: steps follow that camera's frames. | | | |
| 8 | Two real cameras at 1080p50/60: checks 1–4 with the flash or a filmed millisecond clock. | | | |

## Results

| Check | Measured values |
|---|---|
| | |
