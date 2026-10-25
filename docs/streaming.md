# Live stream to OBS (SRT)

VAR for Trampoline can send one camera over the network to OBS (or any SRT receiver) with low latency. The stream uses SRT (Secure Reliable Transport, an open protocol) and carries H.264 in MPEG-TS. It runs separately from the recording: a missing receiver, network problems or a slow stream never affect recording, live view or review.

The Windows x64 installer, macOS and Linux builds include SRT. **The Windows ARM64 installer doesn't**: GStreamer for Windows on ARM has no SRT plugin. There the Stream menu says that streaming isn't available.

## Setting up OBS (receiver)

1. In OBS: Sources → **+** → **Media Source**, name it e.g. "VAR".
2. Untick **Local File**.
3. **Input:** `srt://0.0.0.0:9000?mode=listener` (any free port; with a passphrase add `&passphrase=<passphrase>`).
4. **Input Format:** `mpegts`.
5. Optional: tick **Use hardware decoding when available**; untick **Restart playback when source becomes active**.

OBS then waits for VAR on port 9000. Windows Firewall on the OBS computer must allow OBS on this network (it asks on first use; allow private networks).

## Starting the stream (VAR)

Stream menu (next to Recording):
- **Camera:** which camera is streamed. Changing it while streaming switches the stream.
- **SRT URL:** `srt://<OBS computer>:9000`, e.g. `srt://192.168.1.20:9000` or `srt://obs-pc.local:9000`.
- **Latency:** SRT receive buffer, default 120 ms. On a good wired network 80–120 ms; on Wi-Fi or with dropouts more (200–500 ms). The larger of the sender's and receiver's latency applies.
- **Bitrate:** default 8 Mbit/s; 6–10 Mbit/s is plenty for 1080p in a local network.
- **Passphrase:** optional encryption (10–79 characters); OBS needs the same passphrase.
- **Start streaming / Stop streaming.**

The menu bar shows "SRT cam N" while the stream is on: green when connected, yellow while connecting or waiting, red after an error. The Stream menu shows the state and the bitrate actually sent. Without a receiver, VAR retries every few seconds; when OBS starts (or comes back), the stream connects by itself. A keyframe is sent every second, so OBS shows the picture within about a second after connecting.

The settings are saved; the stream itself is not started again automatically after a restart of the application. With locked settings, the Stream menu is unavailable (a running stream continues).

## The other way round: OBS connects to VAR

If VAR should wait for OBS instead (e.g. the OBS computer's address isn't known), use `srt://:9000?mode=listener` as SRT URL in VAR and `srt://<VAR laptop>:9000` (without `mode=listener`) as input in OBS. The menu then shows "waiting for receiver" until OBS connects. Windows Firewall on the VAR laptop must then allow VAR for Trampoline (it asks when the stream starts).

## Latency

End-to-end delay is roughly encoding (1–2 frames with hardware encoders) + SRT latency + OBS decoding and buffering. With 120 ms SRT latency expect about 200–300 ms in OBS. Further SRT options can be added to the URL (e.g. `srt://192.168.1.20:9000?latency=80`).
