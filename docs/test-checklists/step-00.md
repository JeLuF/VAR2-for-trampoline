# Step 0 — Application skeleton with device list: manual test checklist

Run once per OS with real capture cards. Record the result (✔ / ✘ and notes) in the tables and commit the file.

**Requirements covered:** PLT-002, PLT-005, PLT-001 (partial: CI), CAP-005 (partial: enumeration), NFR-003 (partial), NFR-005 (partial: logging).

## Test setup

| | Windows 11 | macOS 14+ | Ubuntu 26.04 |
|---|---|---|---|
| Tester / date | | | |
| Machine | | | |
| GStreamer version (Help → About) | | | |
| Renderer (Help → About) | | | |
| Capture card 1 (model, MJPEG/YUY2) | | | |
| Capture card 2 (model, MJPEG/YUY2) | | | |

## Checks

| # | Check | Windows | macOS | Linux |
|---|---|---|---|---|
| 1 | CI is green on all three OSes (link to run). | | | |
| 2 | `var` opens the main window with the setup screen; UI text is English. | | | |
| 3 | Every connected capture card is listed once, with name, API and ID. | | | |
| 4 | Each card shows its 1080p50 and/or 1080p60 modes, marked "✔ 1080p50/60". | | | |
| 5 | MJPEG and YUY2 modes are both listed where the card offers them. | | | |
| 6 | Unplugging a card removes it from the list within 2 s. | | | |
| 7 | Plugging it back in adds it within 2 s, with the same modes. | | | |
| 8 | `var --synthetic-cameras 2` lists two synthetic cameras with 1080p50/60 in MJPEG and YUY2. | | | |
| 9 | Without the flag, no synthetic camera is listed. | | | |
| 10 | The log file (folder in Help → About) contains application start, the GStreamer version, the renderer, each connected device, and the connect/disconnect events from checks 6–7, with local timestamps. | | | |
| 11 | Closing the window ends the process, and "application exited" is logged. | | | |

## Notes

Record any card that is listed with unexpected modes, appears twice, or is missing, for the compatibility list (risk "capture cards behave differently per OS").
