# Stream to OBS (SRT)

VAR for Trampoline can send a camera over the network to OBS (or any SRT receiver) with low latency, e.g. as a replay / slow-motion source. The stream uses SRT (Secure Reliable Transport, an open protocol) and carries H.264 in MPEG-TS.

The Windows x64 installer, macOS and Linux builds include SRT. **The Windows ARM64 installer doesn't**: GStreamer for Windows on ARM has no SRT plugin. There the Stream menu says that streaming isn't available.

## What the stream shows

The stream follows the screen:
- **Live:** the live picture of the camera.
- **Review:** the review playback of that camera, exactly as on screen: paused (a still picture), stepping frame by frame, slow motion, reverse. Back to live, the stream shows live again.
- **Camera:** the camera the operator has in full screen (keys 1–4 or a click on the tile); in the split view the camera chosen in the Stream menu.

The stream keeps the size and frame rate of the Stream-menu camera at the start; pictures of other sizes are scaled. While the picture stands still (review paused), the frame is repeated, so OBS always gets a steady stream. The laser pointer trails are not in the stream.

In OBS, a scene with this source becomes the replay: cut to it while the jury reviews, back when done.

## Setting up OBS (receiver)

1. In OBS: Sources → **+** → **Media Source**, name it e.g. "VAR".
2. Untick **Local File**.
3. **Input:** `srt://0.0.0.0:9000?mode=listener` (any free port; with a passphrase add `&passphrase=<passphrase>`). Easiest: enter the SRT URL and passphrase in VAR's Stream menu first and paste the URL from its **Copy to clipboard** here.
4. **Input Format:** `mpegts`.
5. **Network Buffering:** 0 MB (less delay; see *Latency*).
6. Optional: tick **Use hardware decoding when available**; untick **Restart playback when source becomes active**.

OBS then waits for VAR on port 9000. Windows Firewall on the OBS computer must allow OBS on this network (it asks on first use; allow private networks).

## Starting the stream (VAR)

Stream menu (next to Recording):
- **Camera:** the camera streamed in the split view (see above). Can be changed while streaming.
- **SRT URL:** `srt://<OBS computer>:9000`, e.g. `srt://192.168.1.20:9000` or `srt://obs-pc.local:9000`.
- **Latency:** SRT receive buffer, default 120 ms. On a good wired network 50–120 ms; on Wi-Fi or with dropouts more (200–500 ms). The larger of the sender's and receiver's latency applies.
- **Bitrate:** default 8 Mbit/s; 6–10 Mbit/s is plenty for 1080p in a local network.
- **Passphrase:** optional encryption (10–79 characters); OBS needs the same passphrase.
- **In OBS:** the Input to enter in OBS for this SRT URL (the passphrase is shown as `***`). **Copy to clipboard** copies it, with the passphrase if set, to paste into OBS's **Input** field.
- **Start streaming / Stop streaming.**

The menu bar shows "SRT cam N" (with "review" while streaming review playback): green when connected, yellow while connecting or waiting, red after an error. The Stream menu shows the state and the bitrate actually sent. Without a receiver, VAR retries every few seconds; when OBS starts (or comes back), the stream connects by itself. A keyframe is sent every second, so OBS shows the picture within about a second after connecting.

The settings are saved; the stream itself is not started again automatically after a restart of the application. With locked settings, the Stream menu is unavailable (a running stream continues and keeps following the screen).

## The other way round: OBS connects to VAR

If VAR should wait for OBS instead (e.g. the OBS computer's address isn't known), use `srt://:9000?mode=listener` as SRT URL in VAR and `srt://<VAR laptop>:9000` (without `mode=listener`) as input in OBS. The menu then shows "waiting for receiver" until OBS connects. Windows Firewall on the VAR laptop must then allow VAR for Trampoline (it asks when the stream starts).

## Latency

VAR itself adds little: at most 3 frames waiting for the encoder (hardware encoders in low-latency mode, no B-frames), plus the SRT latency. Most of a delay of about a second usually comes from OBS's Media Source buffering. To reduce it:
- OBS Media Source: **Network Buffering** 0 MB; in **FFmpeg Options** `fflags=nobuffer` may help further.
- SRT latency 50–80 ms on a wired network (VAR's Latency setting; OBS's side can be set in its URL, `&latency=80000`, in microseconds for OBS).
- Wired network rather than Wi-Fi.

## Load on the laptop

The stream doesn't wait for the network or the receiver, so these can't hold up the cameras. But it needs a second H.264 encoder session next to the recordings (one per camera) on the same hardware encoder. A laptop whose encoder is just fast enough for the recordings can fall behind with the stream added; the recordings then drop frames. Test on the event laptop: record all cameras and stream for at least 10 minutes and check the debug overlay (dropped frames) and the "resources" lines in the log (memory must stay flat). A development machine with a Snapdragon X handled two 1080p60 recordings, but not two recordings plus the stream.
