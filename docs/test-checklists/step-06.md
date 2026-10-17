# Step 6 — Review while recording: manual test checklist

Includes the playback engine planned for step 5, applied to the running recording only (reviewing finished recordings is not a use case). Run once per OS with two real capture cards, on mains power. Record the result (✔ / ✘ and notes) and commit the file.

**Requirements covered:** REV-001, REV-002, REV-009, REV-010, VIEW-002, VIEW-004, REC-003 (partial: review actions), REC-010; from step 5: CAP-009, REC-009, REV-007, REV-007a, VIEW-007, REV-001.

## Test setup

| | Windows 11 | macOS 14+ | Ubuntu 26.04 |
|---|---|---|---|
| Tester / date | | | |
| Machine (CPU, GPU) | | | |
| Review decoder (debug overlay in review) | | | |

## Checks

| # | Check | Windows | macOS | Linux |
|---|---|---|---|---|
| 1 | "Review" is only available while recording; it switches all tiles to recorded video, marked "(review)"; no live view is visible (VIEW-004). | | | |
| 2 | "Back to live" (menu bar or review bar) returns to the live grid with one click (REV-009). | | | |
| 3 | Latency test: a millisecond clock filmed by a camera; switch to review, seek to the live edge: the clock time shown in review is at most 1 s behind the real time (REV-002). Record the value. | | | |
| 4 | Flash test in review: seek to a flash; it appears on all cameras in the same frame or ±1 frame (CAP-009). | | | |
| 5 | The scrub bar covers the whole recording, labelled with time of day; clicking and dragging moves all cameras; the time shown matches the clock burned into the synthetic camera or filmed by a camera (REV-007, VIEW-007). | | | |
| 6 | Play/pause (button and Space): playback runs at real speed; at the live edge it follows the recording. | | | |
| 7 | Playback across segment boundaries (e.g. `--segment-seconds 30`): no visible stutter, no loss of sync (REC-009). | | | |
| 8 | Full screen (click, keys 1–4, Esc) works in review mode (VIEW-002). | | | |
| 9 | 1-hour recording with continuous review use (`--review-exercise`, or by hand): `var-check` reports no dropped frames and no gaps for all cameras (REV-010, REC-003). | | | |
| 10 | Moving a completed segment to another drive during review: no error; jumping into it shows "No video at this time" for that camera only; the recording continues without gap (REC-010). | | | |
| 11 | Camera failure during review: the menu bar shows the alarm; the failed camera's tile shows "No video at this time" for the time of the failure. | | | |
| 12 | Recording across midnight (or with the system clock set to 23:55): scrub bar labels continue 23:59 → 00:00 (REV-007a). | | | |

## Results

| Check | Measured values |
|---|---|
| | |
