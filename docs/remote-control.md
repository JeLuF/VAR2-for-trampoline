# Remote control (Bitfocus Companion)

VAR for Trampoline can be controlled over the network with a plain text protocol, so a Stream Deck can drive review and markers through [Bitfocus Companion](https://bitfocus.io/companion) and its **Generic TCP/UDP** module (INP-003). A dedicated Companion module is not part of this version (INP-004).

## Enabling

Recording menu → **Remote control**: tick *Companion, TCP port* and set the port (default **7450**). The menu then shows the address to connect to, e.g. `Listening on 192.168.1.20:7450 (all network interfaces)`. The setting is saved; it is off by default. For tests: `--remote-control-port 7450`.

The server listens on **all network interfaces**, so Companion can run on another computer in the same network. On Windows, the firewall asks the first time whether `var.exe` may communicate on networks; allow it for private networks if Companion runs on another computer.

## Security

There is no password: anyone who can reach the port can use the commands below. They are limited to what is allowed in locked mode, review, markers and the view, so nothing can start or stop the recording, change settings or quit the application. Enable the remote control only in a trusted network (the jury's own network, not public Wi-Fi), and disable it when it is not needed. Every connection is logged with the sender's address.

## Protocol

TCP, one command per line (`\n` or `\r\n`), ASCII, case-insensitive. Every line gets a one-line answer: `OK` (accepted), `PONG`, or `ERR <reason>`. Several connections at a time are possible (at most 8). Commands are executed in order, as if the corresponding key were pressed; while a dialog is open in the application (e.g. the PIN prompt) they are ignored, like keys.

| Command | Effect | Key |
|---|---|---|
| `PING` | Answers `PONG` (connection check) | |
| `PLAY` | Play / pause (forwards); in live mode: review from 5 s before live | Space |
| `REVERSE` | Play backwards / pause (0.5× and slower) | Shift + Space |
| `PAUSE` | Pause | |
| `STEP <n>` | `n` frames forward or back (−100 … 100), e.g. `STEP -1` | ← / → |
| `JUMP <s>` | `s` seconds forward or back, e.g. `JUMP -10` | Shift/Ctrl/Alt + ← / → |
| `FASTER`, `SLOWER` | Next speed | + / − |
| `SPEED <x>` | Speed `2`, `1`, `0.5`, `0.25` or `0.1` (also `0.5x`) | |
| `REVIEW` | Review the recording | R |
| `LIVE` | Back to live | L |
| `MARKER` | Set a marker | M |
| `MARKER PREV`, `MARKER NEXT` | Previous / next marker | , / . |
| `CAMERA <n>` | Camera `n` (1–4) full screen / back | 1 – 4 |
| `GRID` | Back to the grid | Esc |

Review functions sent in live mode switch to review first, as the keys do.

## Companion setup

1. Add a connection: **Generic: TCP/UDP** (module `generic-tcp-udp`), protocol TCP, target IP = the address shown in the Recording menu, port 7450.
2. For each button, add the action *Send Command* with e.g. `STEP -1` and the end-of-line character `\n` (option *Command end character*: LF).
3. Useful buttons: `REVIEW`, `LIVE`, `PLAY`, `REVERSE`, `STEP -1`, `STEP 1`, `JUMP -10`, `JUMP -1`, `MARKER`, `MARKER PREV`, `MARKER NEXT`, `SPEED 0.25`, `CAMERA 1`, `CAMERA 2`, `GRID`.

Quick test without Companion, from another computer:

```sh
printf 'PING\nREVIEW\nSTEP -1\n' | nc 192.168.1.20 7450
```

Windows PowerShell:

```powershell
$c = [Net.Sockets.TcpClient]::new("192.168.1.20", 7450); $w = [IO.StreamWriter]::new($c.GetStream()); $w.AutoFlush = $true
$r = [IO.StreamReader]::new($c.GetStream()); $w.WriteLine("PING"); $r.ReadLine()
```
