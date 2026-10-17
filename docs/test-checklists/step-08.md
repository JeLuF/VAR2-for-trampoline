# Step 8 — Markers: manual test checklist

Run once per OS, while recording, on mains power. Record the result (✔ / ✘ and notes) and commit the file.

**Requirements covered:** MRK-001 – MRK-007, REV-008, INP-001 (partial: marker functions).

## Checks

| # | Check | Windows | macOS | Linux |
|---|---|---|---|---|
| 1 | M in live mode sets a marker at the current time; a notice "Marker n set at …" appears (MRK-001). | | | |
| 2 | M in review mode sets a marker at the shown moment. | | | |
| 3 | `,` / `.` jump to the previous / next marker; all cameras show the marked moment (with the synthetic camera: the same frame counter as when the marker was set) (MRK-003). | | | |
| 4 | The Markers menu lists all markers with time of day; clicking one jumps there, also from live mode (MRK-004). | | | |
| 5 | The scrub bar shows all markers (REV-008). | | | |
| 6 | There is no way to edit or delete a marker (MRK-005). | | | |
| 7 | `markers.json` in the recording folder contains every marker immediately after it was set (open it during the recording) (MRK-006, MRK-007). | | | |
| 8 | Setting 100 markers quickly during a recording causes no gaps (`var-check`). Automated run on the development machine: 100 markers, 0 dropped frames. | | | |
| 9 | Kill the application during a recording: `markers.json` contains all markers set before. | | | |

## Results

| Check | Measured values |
|---|---|
| | |
