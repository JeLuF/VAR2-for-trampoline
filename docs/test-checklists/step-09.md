# Step 9 — Disk space monitoring: manual test checklist

Run once per OS, on mains power. Record the result (✔ / ✘ and notes) and commit the file.

**Requirements covered:** DSK-001, DSK-002, DSK-003, VIEW-005.

Automated: `cargo test -p var-app disk` (levels, thresholds, dismissing, measured vs. configured rate) and `cargo test -p var-record disk` (free space query).

## Checks

| # | Check | Windows | macOS | Linux |
|---|---|---|---|---|
| 1 | Before recording, the Recording menu shows the free space and the estimated recording time ("configured bitrates"); the menu bar shows "Disk …" (DSK-001). Without a selected camera: "Select a camera" / "Disk … free". | | | |
| 2 | Changing the recording folder to another drive updates the free space within 2 s. | | | |
| 3 | While recording, "Disk …" in the menu bar updates at least once per minute; after 10 s the Recording menu says "measured" (DSK-002, VIEW-005). | | | |
| 4 | Visible in live view, review and full screen, locked and unlocked (VIEW-005). | | | |
| 5 | Accuracy: record 30 min with two real cameras. Note free space at start and end and the estimated rate in the log (`disk estimate … rate_mb_s`). The rate × 30 min must match the actual decrease of free space within ±10 %. | | | |
| 6 | `--simulate-free-gb 20` (two cameras at 1080p50): yellow banner "Disk space low", "Disk" in yellow. `--simulate-free-gb 5`: red banner "Disk almost full", "Disk" in red (DSK-003). Also via Debug → Simulated free disk space. | | | |
| 7 | Thresholds changed in the Recording menu (e.g. 240 / 60 min) take effect immediately and are restored after a restart. The second threshold is always below the first. | | | |
| 8 | "Dismiss" hides the banner; crossing the next threshold shows it again. | | | |
| 9 | The log contains "disk space warning" with level, free space and remaining time when a threshold is crossed (NFR-005). | | | |
| 10 | Recording folder on a disconnected network share: "Disk ?" in red, the UI stays responsive. | | | |

## Results

| Check | Measured values |
|---|---|
| 5 | Development machine (2 synthetic cameras, 150 s): estimated rate 1.344–1.350 MB/s, actually written 201.67 MB in 150 s = 1.344 MB/s (< 0.5 %). The configured estimate (4.64 MB/s) is far too high for synthetic cameras, because the hardware encoder undershoots on a static test image; with real cameras the bitrate is within ±20 % (step 2). |
| 6 | Development machine: 5 GB → yellow banner, about 1 h 01 min; 1 GB → red banner, about 12 min. |
