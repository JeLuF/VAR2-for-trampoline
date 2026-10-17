# Step 13 — Hardening, endurance and control interface: manual test checklist

Run once per OS, on mains power, with two real cameras (capture cards). Record the result (✔ / ✘ and notes) and commit the file.

**Requirements covered:** NFR-001, NFR-002, NFR-004, NFR-005, REC-002, INP-001, INP-002, INP-003.

Automated:
- `cargo test -p var-app shortcuts`: every review and marker function has a shortcut; the help overlay is generated from the same table that handles the keys; no key is bound twice; modifiers take precedence.
- `cargo test -p var-control`: protocol (valid and invalid commands), server over TCP, listening on all interfaces.
- `cargo test -p var-app remote`: enabling starts and disabling stops the server.
- `cargo test -p var-check resources`: evaluation of the resource log.

## Endurance (NFR-001, NFR-004, REC-002)

```sh
var --camera "<ID 1>" --camera "<ID 2>" --review-exercise --record-seconds 43200
var-check --resources "<log folder>/var.<date>.log" ["<log of the next day>"]
var-check "<recording folder>/<start time>" --sync
```

| # | Check | Windows | macOS | Linux |
|---|---|---|---|---|
| 1 | 12 hours with two real cameras at 1080p50/60 and `--review-exercise` (a random review action every 2 s): no crash. | | | |
| 2 | `var-check --resources`: memory at the end within ±5 % of the value after the first hour; handles not growing. | | | |
| 3 | `var-check --resources`: no dropped frames, no camera failure, no recording interruption. | | | |
| 4 | `var-check <recording> --sync`: no gaps at segment boundaries, no dropped frames, cameras in sync. | | | |
| 5 | The log contains start, stop, camera failures (if any), lock/unlock, disk warnings (NFR-005). | | | |
| 6 | Optional: during the run, unplug a camera for 10 s once: one interruption, recording of the other camera unaffected, resumption logged. | | | |

## Keyboard and help (INP-001, INP-002)

| # | Check | Windows | macOS | Linux |
|---|---|---|---|---|
| 7 | F1 and Help → Keyboard shortcuts open the overlay; F1, Esc or × close it. | | | |
| 8 | Every shortcut in the overlay does what it says. | | | |

## Remote control (INP-003)

See [docs/remote-control.md](../remote-control.md).

| # | Check | Windows | macOS | Linux |
|---|---|---|---|---|
| 9 | Off by default: nothing listens on port 7450 (`netstat -an`). | | | |
| 10 | Recording menu → Remote control on: the menu shows `Listening on <IP>:7450`; the port is open on all interfaces (`0.0.0.0:7450`). | | | |
| 11 | From another computer (Companion Generic TCP, or `nc`): `PING` → `PONG`; `REVIEW`, `STEP -1`, `JUMP -10`, `REVERSE`, `MARKER`, `CAMERA 1`, `GRID`, `LIVE` work like the keys. | | | |
| 12 | `STOP`, `QUIT` and anything unknown answer `ERR …` and change nothing; there is no way to start or stop the recording or change settings remotely. | | | |
| 13 | Works in locked mode; commands are ignored while the PIN dialog is open. | | | |
| 14 | Off again: the port is closed, connected clients are disconnected. Setting and port survive a restart. | | | |
| 15 | Connections are logged with the sender's address. | | | |

## No network needed (NFR-002)

| # | Check | Windows | macOS | Linux |
|---|---|---|---|---|
| 16 | With all network adapters disabled (flight mode, cable unplugged): start, record, review, markers, lock, disk monitoring work; the remote control (if on) still listens and reports its port. | | | |

Code review on the development machine: the only network code is the remote control (`var-control`); no Rust crate in the application does network access (`cargo tree`: only `url` via egui's link handling, unused); the bundled GStreamer plugins contain no network sources or sinks.

## Results

| Check | Measured values |
|---|---|
| 1–4 | Development machine (Windows 11 ARM, Snapdragon X, mains power), **2 hours** (not 12), 2 synthetic cameras YUY2 1080p50, `--review-exercise`, with compiles and tests running in parallel part of the time: no crash, no camera failure, no recording interruption; 12 segments per camera, boundaries continuous, every index correct. Memory peak 2349 MB (1:00–1:29) → 2366 MB (1:29–1:58), +0.7 %; mean 1983 → 2037 MB; handles 1759 → 1680. CPU 41 % of all cores. **Dropped: 11 and 8 single frames of 360 000** (0.003 %), several at the same moment on both cameras (system-wide stalls, partly during compiles). To be repeated with real capture cards, 12 hours, nothing else running. |
| 7 | F1 overlay shows all 14 shortcut lines. |
| 10–12, 15 | Remote control on port 7450, connected from the machine's LAN address (10.1.0.139): `PING` → `PONG`, `CAMERA 2` → full screen, `GRID` → grid, `STOP` → `ERR unknown command STOP`, `SPEED 3` → `ERR …`; connection logged with address. Windows Firewall asks on first use. |

## Fixes from the endurance runs

- **Process priority above normal (Windows):** in the first run, a full CPU load from other programs (compiling) stalled capture for more than a second; the watchdog then restarted the cameras and the recording was interrupted (18 camera failures in 13 minutes). With the higher priority, the second run had none, also while compiling.
- **Review decoder recreated after an error:** after a camera restart, the recording continues in a new segment from a new encoder instance; the review decoder couldn't renegotiate (`not-negotiated`) and stayed broken, which caused a storm of errors. Now each error replaces the decoder; review continues after a short hiccup.
- **MSI:** the `playback` plugin (decodebin, used by review since step 6) was missing from the installer.
- **Endurance evaluation:** memory goes up and down by several hundred MB with review (frame caches), so `var-check --resources` compares peak and mean over 30-minute windows instead of single values.
