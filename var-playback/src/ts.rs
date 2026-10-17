//! Extracts the H.264 access unit of one frame from its MPEG-TS packets.
//!
//! The segments are written by the application's own muxer (var-record), so only
//! what it writes is handled: PAT/PMT, and one PES per frame on the video PID.

const PACKET_SIZE: usize = 188;
const SYNC_BYTE: u8 = 0x47;
/// Video PID written by the muxer.
const VIDEO_PID: u16 = 0x0100;

/// Appends the elementary stream bytes of the frame in `chunk` (the frame's TS packets
/// as listed in the index) to `out`. Returns false if the chunk is malformed.
pub fn extract_access_unit(chunk: &[u8], out: &mut Vec<u8>) -> bool {
    if !chunk.len().is_multiple_of(PACKET_SIZE) {
        return false;
    }
    for packet in chunk.as_chunks::<PACKET_SIZE>().0 {
        if packet[0] != SYNC_BYTE {
            return false;
        }
        let pid = u16::from(packet[1] & 0x1F) << 8 | u16::from(packet[2]);
        if pid != VIDEO_PID {
            continue; // PAT, PMT
        }
        let adaptation = packet[3] & 0x20 != 0;
        let has_payload = packet[3] & 0x10 != 0;
        if !has_payload {
            continue;
        }
        let start = if adaptation {
            5 + packet[4] as usize
        } else {
            4
        };
        let Some(mut payload) = packet.get(start..) else {
            return false;
        };
        let unit_start = packet[1] & 0x40 != 0;
        if unit_start {
            // PES header: start code, stream id, length, flags, header length.
            if payload.len() < 9 || payload[..3] != [0, 0, 1] {
                return false;
            }
            let header_end = 9 + payload[8] as usize;
            let Some(data) = payload.get(header_end..) else {
                return false;
            };
            payload = data;
        }
        out.extend_from_slice(payload);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use var_record::ts::TsMuxer;

    #[test]
    fn round_trip_with_the_muxer() {
        let mut muxer = TsMuxer::new();
        // An access unit that already starts with an AUD (the muxer adds none) and
        // spans several packets.
        let mut au = vec![0, 0, 0, 1, 0x09, 0xF0, 0, 0, 0, 1, 0x65];
        au.extend((0..1000).map(|i| i as u8));
        for keyframe in [true, false] {
            let mut chunk = Vec::new();
            muxer.write_frame(&mut chunk, &au, 90_000, 90_000, keyframe);
            let mut out = Vec::new();
            assert!(extract_access_unit(&chunk, &mut out));
            assert_eq!(out, au);
        }
    }

    #[test]
    fn rejects_garbage() {
        let mut out = Vec::new();
        assert!(!extract_access_unit(&[0u8; 100], &mut out));
        assert!(!extract_access_unit(&[0u8; 188], &mut out));
    }
}
