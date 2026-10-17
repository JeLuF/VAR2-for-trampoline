# Video Assisted Referee (VAR) for Trampoline — Software Requirements Specification

| | |
|---|---|
| **Document status** | Draft v0.3 |
| **Date** | 2026-10-16 |
| **Implementation language** | Rust |
| **License** | MIT |

---

## 1. Introduction

### 1.1 Purpose

This document specifies the requirements for a Video Assisted Referee (VAR) application for trampoline competitions. The application records the camera feeds of one jury workplace for the entire duration of an event. Referees can review the recording during the competition, and the full recording is available afterwards for post-event analysis.

### 1.2 Conventions

- **Shall** marks a mandatory requirement, **should** a desired one and **may** an optional one.
- Every requirement has a unique ID (e.g. `REC-003`) and a MoSCoW priority:
  - **M** — Must have
  - **S** — Should have
  - **C** — Could have
  - **W** — Won't have (in this version)
- Every requirement has an acceptance criterion that a tester can verify.

### 1.3 Definitions

| Term | Meaning |
|---|---|
| **Workplace** | One jury station: one PC (laptop) with the cameras attached. |
| **Event** | The period from the operator starting the recording to the operator stopping it, typically one competition day. |
| **Operator** | Person responsible for setup, for starting and stopping the recording, and for locking and unlocking the settings. |
| **Referee** | Person who reviews video and sets markers during the event. |
| **Unlocked mode** | Application state in which all functions and settings are available. The application starts in this mode. |
| **Locked mode** | Application state in which critical functions and settings are unavailable. Only review and marker functions remain. |
| **PIN** | Secret required to switch from locked to unlocked mode. |
| **Live mode** | UI state in which the camera grid shows the live feeds. |
| **Review mode** | UI state in which the camera grid shows recorded video for review. |
| **Marker** | A timestamp set by a referee to allow quick navigation back to a moment. |
| **Segment** | One of several consecutive recording files that together form the continuous recording of a camera. |
| **Wall-clock time** | The local time of day according to the PC's system clock. |

---

## 2. Scope

### 2.1 In scope

- Disciplines: individual trampoline, synchronised trampoline and double mini-trampoline (DMT).
- A single, standalone workplace: one PC with at least two cameras attached via USB capture cards that appear to the OS as webcams.
- Continuous recording of all camera feeds for the whole event.
- Synchronised review of all feeds during the event, including slow motion, frame stepping and markers.
- Making the recording available after the event for post-event analysis.

### 2.2 Out of scope

- Applying or enforcing competition rules. Rules are handled by the superior judges, not by the software.
- Live streaming.
- Scoring and integration with scoring systems.
- Horizontal displacement (HD) and time of flight (ToF) measurement.
- Cloud integration and any network or internet functionality.
- Networking between workplaces. Each workplace is independent of other juries.
- Audio recording.
- Data protection features. Athletes sign a GDPR waiver, and VAR is covered by FIG rules.

### 2.3 Operating environment

- Hardware: laptops (built-in battery serves as UPS).
- Cameras: arbitrary USB capture cards that present themselves as standard video capture devices (UVC / DirectShow / Media Foundation / AVFoundation / V4L2).
- No internet connection is available or required.

---

## 3. Users and access control

Access control is implemented as a lock on the settings, not as user accounts. The Operator sets up the workplace in unlocked mode, then locks the settings. The Referees then work in locked mode. Unlocking is the only action that requires the PIN.

| ID | Requirement | Prio | Acceptance criterion |
|---|---|---|---|
| ROLE-001 | The application shall start in **unlocked mode**. | M | After launch, all functions and settings are available. |
| ROLE-002 | In unlocked mode, the Operator shall be able to use all functions and change all settings (camera selection, formats, storage location, PIN, etc.), subject to CAP-010. | M | All setup options are editable in unlocked mode. |
| ROLE-003 | The Operator shall be able to switch to **locked mode** with a "Lock settings" action. No PIN is required for locking. | M | "Lock settings" switches to locked mode immediately. |
| ROLE-004 | In locked mode, critical functions and settings shall be unavailable. These include at least: all setup settings, starting and stopping the recording, and quitting the application (see SYS-001). | M | In locked mode, each of these is hidden or disabled. |
| ROLE-005 | In locked mode, all review functions (4.5), laser pointer drawing (4.6) and marker functions (4.7) shall remain available. | M | Each of these works in locked mode. |
| ROLE-006 | Switching from locked to unlocked mode ("Unlock settings") shall require the PIN. This is the only action in the application that requires the PIN. | M | Unlocking without the correct PIN is refused; no other action prompts for a PIN. |
| ROLE-007 | The PIN shall be set by the Operator in unlocked mode. Locking shall not be possible until a PIN has been set. | M | "Lock settings" is unavailable without a configured PIN. |
| ROLE-008 | The current mode (locked or unlocked) shall be visibly indicated at all times. | M | Mode indicator visible in all views. |

---

## 4. Functional requirements

### 4.1 Camera setup and capture

| ID | Requirement | Prio | Acceptance criterion |
|---|---|---|---|
| CAP-001 | The application shall support at least **2** cameras simultaneously. | M | Two cameras record and play back concurrently meeting all performance requirements. |
| CAP-002 | The application shall support **1080p at 50 fps** and **1080p at 60 fps** as a minimum. | M | Both modes can be selected and recorded at the full frame rate without dropped frames. |
| CAP-003 | The application shall support any capture device that the OS exposes as a standard video capture device. | M | Tested with at least two different capture card models per OS, covering both MJPEG and YUY2 output. |
| CAP-004 | The application shall handle at least the **MJPEG** and **YUY2** (YUV 4:2:2) pixel formats delivered by the capture devices. | M | Recording works with devices delivering each of these formats. |
| CAP-005 | During setup, the Operator shall be able to choose the capture device for each camera slot from a list of available devices. | M | All connected capture devices are listed and selectable. |
| CAP-006 | During setup, the Operator shall be able to select resolution, frame rate and pixel format per camera from the modes the device offers. | S | Available device modes are listed and selectable. |
| CAP-007 | Capture devices should be identified in a way that remains stable across reconnects and reboots, so the configuration can be restored. | S | After a reboot, the previous configuration maps to the same physical devices. |
| CAP-008 | During setup, the Operator shall be able to check each camera's signal in a live view. | M | Live preview of each selected camera is shown in setup. |
| CAP-009 | The feeds of all cameras shall be synchronised to within **±1 frame** during review. How this is achieved is an implementation detail. | M | With a flash or clapperboard visible to all cameras, the event appears in all feeds within ±1 frame during review. |
| CAP-010 | Camera selection and capture format settings shall not be changeable while a recording is running, even in unlocked mode. | M | These settings are disabled during recording. |

### 4.2 Recording

| ID | Requirement | Prio | Acceptance criterion |
|---|---|---|---|
| REC-001 | The Operator shall start the recording of all camera feeds with a single action at the beginning of the event. | M | One action starts recording on all cameras. |
| REC-002 | The recording shall continue for the entire duration of the event, until the Operator stops it. | M | A 12-hour test recording runs without interruption (see NFR-001). |
| REC-003 | No user action other than the explicit "Stop recording" action (available only in unlocked mode, see EXP-002) shall stop or interrupt the recording. This includes switching to review mode, all review functions, locking and unlocking, and closing or minimising windows. | M | Each of these actions is performed during a test recording; the recording has no gaps. |
| REC-004 | If one camera fails or is disconnected, the recording of all other cameras shall continue without gaps. | M | Unplugging one camera during recording leaves the other recordings continuous. |
| REC-005 | If a failed camera becomes available again, its recording should resume automatically. | S | Replugging the camera resumes its recording without Operator action. |
| REC-006 | A camera failure shall be clearly indicated to the Operator and Referee. | M | A visible alarm appears within 2 s of the failure. |
| REC-006a | The gap in a failed camera's recording does not need to be visualised on the scrub bar. | W | — |
| REC-007 | Encoding shall use hardware acceleration where available. | M | On a machine with a hardware encoder, the hardware encoder is used (verifiable in logs). |
| REC-007a | If no hardware encoder is available, the application shall fall back to software encoding. The software encoder used shall comply with PLT-006 and PLT-007. | M | With hardware encoding disabled or unavailable, recording works using a software encoder (verifiable in logs). |
| REC-007b | The encoder in use (hardware or software) should be shown to the Operator during setup. | S | Setup screen indicates the active encoder. |
| REC-008 | The target quality shall be approximately **15 Mbit/s per 1080p50 camera** (approx. 18 Mbit/s for 1080p60). | S | Measured average bitrate is within ±20 % of the target. |
| REC-009 | The recording may be split into segments, provided that playback across segment boundaries is continuous during review. | M | Scrubbing and playing across a segment boundary shows no gap, stutter or loss of sync. |
| REC-010 | The Operator should be able to move completed segments to another storage location with the OS file manager during a running recording, to free disk space, without interrupting the recording or causing errors in the application. Moved segments are no longer available for review. An in-app function for moving segments is not required. | S | A completed segment is moved in the file manager while recording continues; recording has no gap and the application keeps running without errors. |
| REC-011 | Codec, container and file layout are implementation details, provided REC-009 and EXP-001 are met. | — | — |
| REC-012 | Data loss in case of a crash should be limited by design (e.g. a crash-safe container or segmenting), but no further measures are required. | C | After a forced kill of the application, previously recorded segments remain playable. |
| REC-013 | No audio shall be recorded. | M | Output files contain no audio track. |

**Storage estimate:** at 15 Mbit/s, one camera needs approx. 6.75 GB per hour. Two cameras recording 12 hours at 1080p50 need approx. 162 GB; at 1080p60, approx. 195 GB.

### 4.3 Disk space monitoring

| ID | Requirement | Prio | Acceptance criterion |
|---|---|---|---|
| DSK-001 | Before starting the recording, the application shall show the free disk space and the estimated remaining recording time. | M | Both values are shown in setup. |
| DSK-002 | During recording, the application shall continuously display the estimated remaining recording time. | M | Value is visible during recording and updates at least once per minute. |
| DSK-003 | The application shall warn the Operator when the estimated remaining recording time falls below configurable thresholds (default: 2 h and 30 min). | M | Warnings are shown when the thresholds are crossed. |

### 4.4 Live view and display layout

| ID | Requirement | Prio | Acceptance criterion |
|---|---|---|---|
| VIEW-001 | The main screen shall show all cameras in a grid. | M | All cameras visible simultaneously. |
| VIEW-002 | The user shall be able to show any single camera full-screen and return to the grid. | M | Toggle per camera works in live and review mode. |
| VIEW-003 | In live mode, the grid shall show the live view of all cameras, both during setup and during recording. | M | Live feeds are shown in both states. |
| VIEW-004 | In review mode, the grid shall show the recorded video instead of the live view. No live view is shown in review mode. | M | Entering review mode switches the grid to recorded video; no live feed is visible. |
| VIEW-005 | A recording status indicator shall be visible at all times during recording, showing at least: recording active, elapsed time, per-camera health and remaining disk time. | M | Indicator visible in all modes, locked and unlocked. |
| VIEW-006 | Output to a second display is not part of this version. | W | — |
| VIEW-007 | The current wall-clock time of the displayed frame shall be shown during review. | S | Timestamp is shown and matches the scrub bar. |

### 4.5 Review

| ID | Requirement | Prio | Acceptance criterion |
|---|---|---|---|
| REV-001 | During the running recording, Referees shall be able to review the recorded video of all cameras in parallel and in sync. | M | All cameras play back simultaneously, synchronised per CAP-009. |
| REV-002 | Recorded video shall be available for review within **1 second** of capture (5 seconds is acceptable). | M | Measured delay between an event in the live feed and its availability in review is ≤ 1 s (hard limit 5 s). |
| REV-003 | Playback speeds shall be: **2×, 1×, 0.5×, 0.25×, 0.1×**. | M | All speeds selectable; playback speed measured within ±5 %. |
| REV-004 | Single-frame stepping shall be available both **forwards (next frame)** and **backwards (previous frame)**. | M | Each step moves exactly one frame on all cameras simultaneously. |
| REV-005 | Slow-motion playback in reverse should be available. | C | Reverse playback at 0.5× and slower plays smoothly. |
| REV-006 | Hotkeys shall allow jumping forwards and backwards by **1 s, 10 s and 60 s**. | M | Each hotkey moves the playback position by the stated amount on all cameras. |
| REV-007 | A scrub bar shall allow navigating the entire recording. It shall be labelled with **wall-clock time**, not elapsed recording time. | M | Scrub bar covers the full recording; labels match the PC's system clock time at capture. |
| REV-007a | If the recording runs past midnight, the scrub bar labels shall continue as plain time of day (23:59, 0:00, 0:01, …). No day indicator is required. Daylight-saving changes need not be handled, as no competition runs at the time of the switch. | S | A recording spanning midnight shows continuous labels from 23:59 to 0:00 and navigation across midnight works. |
| REV-008 | The scrub bar shall display all markers. | M | All markers are visible on the scrub bar. |
| REV-009 | A single action shall return from review mode to live mode. | M | One action switches back to the live grid. |
| REV-010 | Review shall have no influence on the recording (see REC-003). | M | Covered by REC-003 tests. |

### 4.6 Laser pointer drawing

| ID | Requirement | Prio | Acceptance criterion |
|---|---|---|---|
| DRW-001 | In review mode, the user shall be able to draw on the video with the mouse, similar to a laser pointer. | S | Drawing with the mouse produces visible trails on the video. |
| DRW-002 | Drawn trails shall fade out automatically after a few seconds (default approx. 3 s). | S | Trails disappear without user action. |
| DRW-003 | Drawings shall be a display overlay only and shall not be recorded or saved. | S | Recordings and exports contain no drawings. |

### 4.7 Markers

| ID | Requirement | Prio | Acceptance criterion |
|---|---|---|---|
| MRK-001 | Referees shall be able to set a marker at the current time with a hotkey, both in live mode and in review mode. | M | Hotkey creates a marker at the current live time or the current playback position. |
| MRK-002 | Markers shall be set while the recording is running without affecting it. | M | Covered by REC-003 tests. |
| MRK-003 | The user shall be able to jump to the next and previous marker with hotkeys. | M | Hotkeys move playback to the adjacent marker on all cameras. |
| MRK-004 | The user should be able to select a marker from a list, showing its wall-clock time, to jump to it. | S | Selecting a marker in the list jumps to it. |
| MRK-005 | Markers cannot be edited or deleted. | M | No edit or delete function exists. |
| MRK-006 | Markers shall be persisted to disk as they are created, so they survive an application restart. | S | After restarting the application, all markers are present. |
| MRK-007 | Markers may be stored or exported as a JSON file alongside the recording. | C | A JSON file containing the markers exists after the event. |
| MRK-008 | Marker annotations, labels and scoring integration are out of scope for this version. | W | — |

### 4.8 Input devices

| ID | Requirement | Prio | Acceptance criterion |
|---|---|---|---|
| INP-001 | All review and marker functions shall be operable with keyboard shortcuts. | M | Each function in 4.5 and 4.7 has a shortcut. |
| INP-002 | The keyboard shortcuts shall be documented in the UI (e.g. a help overlay). | S | Help overlay lists all shortcuts. |
| INP-003 | The architecture should allow control by an Elgato Stream Deck via Bitfocus Companion in a later version. | S | An external control interface (e.g. a local API) is designed for this. |
| INP-004 | Stream Deck / Bitfocus Companion integration is not part of this version. | W | — |

### 4.9 Ending the event and post-event export

| ID | Requirement | Prio | Acceptance criterion |
|---|---|---|---|
| EXP-001 | At the end of the event, the complete recording of each camera shall be available in a common video format that plays in standard players (e.g. VLC) without special software. | M | Recordings play in VLC on all three supported OSes. |
| EXP-002 | Stopping the recording shall only be possible in unlocked mode and shall require a confirmation. | M | Stop is unavailable in locked mode; in unlocked mode it is executed only after confirmation. |
| EXP-003 | Time synchronisation between the files of different cameras does not need to be persisted. | W | — |

### 4.10 System behaviour

| ID | Requirement | Prio | Acceptance criterion |
|---|---|---|---|
| SYS-001 | In locked mode, quitting the application shall not be possible by any means the application can intercept (including Alt+F4, Cmd+Q, window close and menu actions). | M | Each quit method is ignored in locked mode; the application keeps running. |
| SYS-001a | In unlocked mode, quitting the application while a recording is running shall require a confirmation. | S | A confirmation dialog appears; cancelling keeps the application and recording running. |
| SYS-002 | During recording, the application shall prevent OS sleep and the screen saver. | M | System does not sleep or start the screen saver during a 2-hour idle test recording. |
| SYS-003 | Automatic restart after a crash is not required. | W | — |

---

## 5. Non-functional requirements

| ID | Requirement | Prio | Acceptance criterion |
|---|---|---|---|
| NFR-001 | The application shall run stably for at least **12 hours** of continuous recording with simultaneous intermittent review. | M | 12-hour test without crash, dropped frames or unbounded memory growth. |
| NFR-002 | The application shall not require internet or network access. | M | All functions work with networking disabled. |
| NFR-003 | The UI language shall be English. | M | — |
| NFR-004 | No frames shall be dropped at the configured frame rate under normal load. | M | Frame counters show no drops during the 12-hour test. |
| NFR-005 | The application shall log operational events (start, stop, camera failures, lock and unlock, warnings) to a log file. | S | Log contains these events with timestamps. |

---

## 6. Platforms, packaging and licensing

| ID | Requirement | Prio | Acceptance criterion |
|---|---|---|---|
| PLT-001 | Supported operating systems: **Windows 11**, **macOS 14 or later**, **Ubuntu 26.04 LTS**. | M | All acceptance tests pass on each OS. |
| PLT-002 | The application shall be implemented in **Rust**. | M | — |
| PLT-003 | Windows shall be distributed as an **MSI installer**. | M | MSI installs and uninstalls cleanly on Windows 11. |
| PLT-004 | macOS and Linux shall be distributed as source code only, with build instructions. | M | Build instructions produce a working application on both OSes. |
| PLT-005 | The application's source code shall be licensed under the **MIT** license. | M | — |
| PLT-006 | Third-party components (e.g. media frameworks, codec libraries) may use any license that permits distribution with the application, including proprietary licenses. The Windows MSI shall include the license texts and notices of all bundled components. | M | License notices of all bundled components are present in the installed application. |
| PLT-007 | Bundling a GPL-licensed component in the Windows MSI should be avoided, because the distributed binary would then have to be offered under the GPL. | S | Dependency license review before release. |

---

## 7. Open points

None at present.
