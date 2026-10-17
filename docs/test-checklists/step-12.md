# Step 12 — Reverse slow motion: manual test checklist

Run once per OS, first with synthetic cameras (burned-in frame counter), then with two real capture cards, while recording, on mains power. Record the result (✔ / ✘ and notes) and commit the file.

**Requirements covered:** REV-005.

Automated:
- `cargo test --release -p var-playback --test reverse`: reverse at 0.5×, 0.25× and 0.1× on two cameras (720p50) shows every frame exactly once, backwards, across keyframes and a segment boundary, at the speed ±5 %; stops at the start of the recording; no reverse faster than 0.5×.
- `cargo test --release -p var-playback --test load -- --ignored --nocapture`: two cameras at 1080p60 recording and reviewed (1× and reverse 0.5×).

## Checks

| # | Check | Windows | macOS | Linux |
|---|---|---|---|---|
| 1 | **Reverse** (or Shift + Space) plays backwards; the button shows **Pause**; again pauses. Starting reverse at 1× or 2× switches to 0.5×. | | | |
| 2 | Synthetic cameras: the frame counter decreases by exactly 1 per shown frame, on all cameras at the same time, across a keyframe (every 30 frames at 60 fps) and a segment boundary (`--segment-seconds 10`). | | | |
| 3 | Speeds 0.5×, 0.25×, 0.1× in reverse, measured with a stopwatch against the burned-in clock over 10 s of recording time: within ±5 %. | | | |
| 4 | + / − in reverse: 0.5× is the fastest; clicking 1× or 2× continues forwards. | | | |
| 5 | Space while playing backwards switches to forwards; Shift + Space while playing forwards switches to backwards. | | | |
| 6 | Reverse stops (pauses) at the start of the recording. | | | |
| 7 | Smooth on the slowest supported machine with two real cameras at 1080p50/60: no visible stalls after the first fraction of a second. | | | |
| 8 | Reverse has no influence on the recording: `var-check <recording> --sync` shows no gaps (REC-003). | | | |

## Results

| Check | Measured values |
|---|---|
| 2, 3 | Development machine (Windows 11 ARM, Snapdragon X, automated test, 720p50): every frame shown on both cameras, 0.5× → 0.497–0.500×, 0.25× → 0.249–0.250×, 0.1× → 0.100×. |
| 7 | Same machine, 2 × 1080p60 YUY2 recording and reviewed: reverse 0.5× showed every frame on both cameras (162 of 162); one wait of about 0.5 s at the start while the previous GOP is decoded. App: frame counter 458 → 396 in 2 s. |

## Notes from the development machine

- One hardware decoder (D3D12) decodes about 230 fps at 1080p when idle, but only about 80–90 fps while two 1080p60 streams are being encoded: encoding and decoding share the GPU's video engine. Two cameras at 1080p60 at 1× need 120 fps of decoding. When decoding can't keep up, playback now waits for the cameras (at most 1 s at a time) instead of letting them drift apart, so 1× can become slightly slower than real time; measured 0.975× under full load, 1.00× without recording load.
- The results varied between runs on this laptop (some runs slower by a factor of 2–3); to be checked on the event laptops (step 13 endurance runs).
