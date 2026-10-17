# Step 10 — Lock mode and system protection: manual test checklist

Run once per OS, while recording, on mains power. Record the result (✔ / ✘ and notes) and commit the file.

**Requirements covered:** ROLE-001 – ROLE-008, SYS-001, SYS-001a, SYS-002, EXP-002, REC-003, NFR-005 (partial: lock and unlock logged).

Automated: `cargo test -p var-app lock` (no locking without PIN, Argon2 hash, wrong PIN refused, pause after 5 wrong PINs, no PIN change while locked).

## Checks

| # | Check | Windows | macOS | Linux |
|---|---|---|---|---|
| 1 | The application starts unlocked, also when it was locked when it ended (ROLE-001). | | | |
| 2 | Without a PIN, "Lock settings" is disabled (ROLE-007). Set PIN… with a PIN of fewer than 4 characters or two different entries is refused. | | | |
| 3 | `config.json` contains only `pin_hash` (`$argon2id$…`), never the PIN. | | | |
| 4 | "Lock settings" locks immediately, without a PIN (ROLE-003). The indicator at the far left shows "LOCKED" (ROLE-008), in live view, review and full screen. | | | |
| 5 | Locked: the Recording and Debug menus are disabled, the camera count can't be changed, right-clicking a camera shows "Settings are locked.", there is no Start/Stop button (ROLE-004, EXP-002). | | | |
| 6 | Locked: review, frame steps, speeds, jumps, markers (M, `,` `.`, Markers menu), full screen and View → Debug overlay work (ROLE-005). Drawing: step 11. | | | |
| 7 | Locked: window close button, Alt+F4 (Windows/Linux), Cmd+Q (macOS), closing from the taskbar / dock leave the application running; a notice "Settings are locked: unlock to quit" appears (SYS-001). | | | |
| 8 | "Unlock settings…" with a wrong PIN: "Wrong PIN.", still locked. After 5 wrong PINs: "Try again in 30 s", even the right PIN is refused during the wait. The right PIN unlocks (ROLE-006). | | | |
| 9 | No other action asks for a PIN (ROLE-006). | | | |
| 10 | Unlocked while recording: closing the window asks "Quit while recording?"; Cancel keeps application and recording running; confirming stops the recording cleanly and quits (SYS-001a). | | | |
| 11 | Stop recording (unlocked) only after confirmation (EXP-002). | | | |
| 12 | 2-hour idle recording on mains power, no input: no sleep, no screen saver, the display stays on (SYS-002). Windows: `powercfg /requests` (as administrator) lists `var.exe` under DISPLAY and SYSTEM. After stopping, the normal power settings apply again. | | | |
| 13 | REC-003: during a recording, do every user action: review and all review functions, markers, full screen, lock and unlock, wrong PIN, quit attempts while locked, quit cancelled while unlocked, minimise and restore the window, open and close all menus. `var-check <recording> --sync` shows no gaps, no dropped frames, cameras in sync. | | | |
| 14 | The log contains "PIN set", "settings locked", "unlock refused: wrong PIN", "settings unlocked", "quit refused: settings are locked", "sleep and screen saver prevented while recording" (NFR-005). | | | |

## Results

| Check | Measured values |
|---|---|
| 2–8, 10, 14 | Development machine (Windows 11 ARM, 2 synthetic cameras, driven by simulated mouse and keyboard input): PIN set, locked during recording, WM_CLOSE and Alt+F4 refused, 2 markers and review in locked mode, wrong PIN refused, unlocked, quit confirmation shown and confirmed. |
| 13 | Same run (77 s): 0 dropped frames on both cameras, no gaps, sync 0.01 frames. |
| 7 (macOS) | Not verified: the default macOS menu is switched off so Cmd+Q reaches the application; check that Cmd+Q is refused when locked and asks for confirmation while recording. |
