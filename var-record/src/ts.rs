//! Minimal MPEG-TS muxer for one H.264 stream (ISO/IEC 13818-1).
//!
//! Writing the transport stream ourselves gives the exact byte offset of every frame
//! for the segment index (AD-3). Every frame starts in a new TS packet with a PCR;
//! every keyframe is preceded by PAT and PMT, so each GOP can be read on its own.

/// TS packet size.
pub const PACKET_SIZE: usize = 188;
const SYNC_BYTE: u8 = 0x47;
const PAT_PID: u16 = 0x0000;
const PMT_PID: u16 = 0x1000;
pub const VIDEO_PID: u16 = 0x0100;
const PROGRAM_NUMBER: u16 = 1;
const STREAM_TYPE_H264: u8 = 0x1B;
const VIDEO_STREAM_ID: u8 = 0xE0;
/// The PCR runs this far (90 kHz units, 100 ms) ahead of the decoding time.
const PCR_DELAY: u64 = 9_000;
/// Timestamps are 33-bit.
pub const TIMESTAMP_MASK: u64 = (1 << 33) - 1;

/// Access unit delimiter NAL unit (primary_pic_type 7: any slice type).
const AUD: [u8; 6] = [0, 0, 0, 1, 0x09, 0xF0];

/// Muxer state: the continuity counters per PID.
#[derive(Debug, Default)]
pub struct TsMuxer {
    cc_pat: u8,
    cc_pmt: u8,
    cc_video: u8,
}

impl TsMuxer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends the TS packets of one H.264 access unit (Annex B byte stream) to `out`.
    /// Keyframes get PAT and PMT in front and the random access indicator. `pts90`
    /// and `dts90` are 90 kHz timestamps.
    pub fn write_frame(
        &mut self,
        out: &mut Vec<u8>,
        au: &[u8],
        pts90: u64,
        dts90: u64,
        keyframe: bool,
    ) {
        if keyframe {
            self.write_pat(out);
            self.write_pmt(out);
        }

        let pts90 = pts90 & TIMESTAMP_MASK;
        let dts90 = dts90 & TIMESTAMP_MASK;
        let mut pes = Vec::with_capacity(au.len() + 32);
        pes.extend_from_slice(&[0, 0, 1, VIDEO_STREAM_ID, 0, 0]); // length 0: unbounded
        pes.push(0x84); // '10', data_alignment_indicator
        if pts90 == dts90 {
            pes.push(0x80);
            pes.push(5);
            pes.extend_from_slice(&timestamp(0b0010, pts90));
        } else {
            pes.push(0xC0);
            pes.push(10);
            pes.extend_from_slice(&timestamp(0b0011, pts90));
            pes.extend_from_slice(&timestamp(0b0001, dts90));
        }
        // Each access unit must start with an access unit delimiter (13818-1 2.14.1).
        if !starts_with_aud(au) {
            pes.extend_from_slice(&AUD);
        }
        pes.extend_from_slice(au);

        let pcr = dts90.saturating_sub(PCR_DELAY);
        let mut rest = pes.as_slice();
        let mut first = true;
        while !rest.is_empty() {
            let written = write_packet(
                out,
                VIDEO_PID,
                first,
                &mut self.cc_video,
                first.then_some(pcr),
                first && keyframe,
                rest,
            );
            rest = &rest[written..];
            first = false;
        }
    }

    fn write_pat(&mut self, out: &mut Vec<u8>) {
        let mut section = vec![0x00, 0, 0]; // table_id, length filled in below
        section.extend_from_slice(&1u16.to_be_bytes()); // transport_stream_id
        section.push(0xC1); // version 0, current
        section.extend_from_slice(&[0, 0]); // section numbers
        section.extend_from_slice(&PROGRAM_NUMBER.to_be_bytes());
        section.extend_from_slice(&(0xE000 | PMT_PID).to_be_bytes());
        write_psi(out, PAT_PID, &mut self.cc_pat, section);
    }

    fn write_pmt(&mut self, out: &mut Vec<u8>) {
        let mut section = vec![0x02, 0, 0];
        section.extend_from_slice(&PROGRAM_NUMBER.to_be_bytes());
        section.push(0xC1);
        section.extend_from_slice(&[0, 0]);
        section.extend_from_slice(&(0xE000 | VIDEO_PID).to_be_bytes()); // PCR PID
        section.extend_from_slice(&0xF000u16.to_be_bytes()); // no program info
        section.push(STREAM_TYPE_H264);
        section.extend_from_slice(&(0xE000 | VIDEO_PID).to_be_bytes());
        section.extend_from_slice(&0xF000u16.to_be_bytes()); // no ES info
        write_psi(out, PMT_PID, &mut self.cc_pmt, section);
    }
}

fn starts_with_aud(au: &[u8]) -> bool {
    let nal = if au.starts_with(&[0, 0, 0, 1]) {
        au.get(4)
    } else if au.starts_with(&[0, 0, 1]) {
        au.get(3)
    } else {
        None
    };
    nal.is_some_and(|b| b & 0x1F == 9)
}

/// Writes a PSI section (without CRC; added here) in one packet.
fn write_psi(out: &mut Vec<u8>, pid: u16, cc: &mut u8, mut section: Vec<u8>) {
    // section_length: bytes after the length field, including the CRC.
    let length = section.len() - 3 + 4;
    section[1] = 0xB0 | ((length >> 8) as u8 & 0x0F);
    section[2] = length as u8;
    let crc = crc32_mpeg2(&section);
    section.extend_from_slice(&crc.to_be_bytes());

    out.extend_from_slice(&[SYNC_BYTE, 0x40 | (pid >> 8) as u8, pid as u8, 0x10 | *cc]);
    *cc = (*cc + 1) & 0x0F;
    out.push(0); // pointer_field
    out.extend_from_slice(&section);
    let fill = PACKET_SIZE - 5 - section.len();
    out.extend(std::iter::repeat_n(0xFF, fill));
}

/// Writes one TS packet with as much of `payload` as fits and returns how much was
/// written. A packet that isn't full is padded with adaptation field stuffing.
fn write_packet(
    out: &mut Vec<u8>,
    pid: u16,
    unit_start: bool,
    cc: &mut u8,
    pcr: Option<u64>,
    random_access: bool,
    payload: &[u8],
) -> usize {
    let needs_flags = pcr.is_some() || random_access;
    let min_adaptation = if needs_flags {
        2 + if pcr.is_some() { 6 } else { 0 }
    } else {
        0
    };
    let space = PACKET_SIZE - 4 - min_adaptation;
    let n = payload.len().min(space);
    let stuffing = space - n;
    let adaptation = if needs_flags {
        min_adaptation + stuffing
    } else {
        stuffing
    };

    let control = if adaptation > 0 { 0x30 } else { 0x10 };
    let pusi = if unit_start { 0x40 } else { 0 };
    out.extend_from_slice(&[
        SYNC_BYTE,
        pusi | (pid >> 8) as u8 & 0x1F,
        pid as u8,
        control | *cc,
    ]);
    *cc = (*cc + 1) & 0x0F;

    if adaptation > 0 {
        out.push((adaptation - 1) as u8);
        if adaptation >= 2 {
            let mut flags = 0u8;
            if random_access {
                flags |= 0x40;
            }
            if pcr.is_some() {
                flags |= 0x10;
            }
            out.push(flags);
            let mut used = 2;
            if let Some(pcr) = pcr {
                out.extend_from_slice(&pcr_bytes(pcr & TIMESTAMP_MASK));
                used += 6;
            }
            out.extend(std::iter::repeat_n(0xFF, adaptation - used));
        }
    }
    out.extend_from_slice(&payload[..n]);
    n
}

/// PTS/DTS field: 4-bit prefix, 33-bit timestamp with marker bits.
fn timestamp(prefix: u8, t: u64) -> [u8; 5] {
    [
        (prefix << 4) | (((t >> 30) & 0x07) as u8) << 1 | 1,
        (t >> 22) as u8,
        (((t >> 15) & 0x7F) as u8) << 1 | 1,
        (t >> 7) as u8,
        ((t & 0x7F) as u8) << 1 | 1,
    ]
}

/// PCR with base `base` (90 kHz) and extension 0.
fn pcr_bytes(base: u64) -> [u8; 6] {
    [
        (base >> 25) as u8,
        (base >> 17) as u8,
        (base >> 9) as u8,
        (base >> 1) as u8,
        (((base & 1) as u8) << 7) | 0x7E,
        0,
    ]
}

/// CRC-32/MPEG-2: polynomial 0x04C11DB7, initial 0xFFFFFFFF, not reflected.
fn crc32_mpeg2(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_check_value() {
        assert_eq!(crc32_mpeg2(b"123456789"), 0x0376_E6E7);
    }

    #[test]
    fn timestamp_encoding() {
        // 0x1_2345_6789 (33 bits) with PTS-only prefix.
        let t = timestamp(0b0010, 0x1_2345_6789);
        let decoded = (u64::from(t[0] >> 1) & 0x07) << 30
            | u64::from(t[1]) << 22
            | u64::from(t[2] >> 1) << 15
            | u64::from(t[3]) << 7
            | u64::from(t[4] >> 1);
        assert_eq!(decoded, 0x1_2345_6789);
        assert_eq!(t[0] >> 4, 0b0010);
        assert!(t[0] & 1 == 1 && t[2] & 1 == 1 && t[4] & 1 == 1);
    }

    fn packets(out: &[u8]) -> Vec<&[u8]> {
        assert_eq!(out.len() % PACKET_SIZE, 0);
        out.chunks(PACKET_SIZE).collect()
    }

    fn pid(p: &[u8]) -> u16 {
        u16::from(p[1] & 0x1F) << 8 | u16::from(p[2])
    }

    #[test]
    fn keyframe_has_tables_pcr_and_random_access() {
        let mut muxer = TsMuxer::new();
        let mut out = Vec::new();
        let au = vec![0x55; 1000];
        muxer.write_frame(&mut out, &au, 90_000, 90_000, true);

        let p = packets(&out);
        assert!(p.iter().all(|p| p[0] == SYNC_BYTE));
        assert_eq!(
            (pid(p[0]), pid(p[1]), pid(p[2])),
            (PAT_PID, PMT_PID, VIDEO_PID)
        );
        // First video packet: unit start, adaptation with random access + PCR.
        assert_eq!(p[2][1] & 0x40, 0x40);
        assert_eq!(p[2][3] & 0x30, 0x30);
        assert_eq!(p[2][5], 0x50);
        // PES start code follows the adaptation field.
        let af_len = p[2][4] as usize;
        assert_eq!(
            &p[2][5 + af_len..5 + af_len + 4],
            &[0, 0, 1, VIDEO_STREAM_ID]
        );
        // Continuity counter increments per video packet.
        let ccs: Vec<u8> = p[2..].iter().map(|p| p[3] & 0x0F).collect();
        assert_eq!(ccs, (0..ccs.len() as u8).collect::<Vec<_>>());

        // Payload bytes add up: PES header (14) + AUD (6) + AU.
        let payload: usize = p[2..]
            .iter()
            .map(|p| {
                let start = if p[3] & 0x20 != 0 {
                    5 + p[4] as usize
                } else {
                    4
                };
                PACKET_SIZE - start
            })
            .sum();
        assert_eq!(payload, 14 + AUD.len() + au.len());
    }

    #[test]
    fn delta_frame_has_no_tables() {
        let mut muxer = TsMuxer::new();
        let mut out = Vec::new();
        muxer.write_frame(&mut out, &[0, 0, 0, 1, 0x09, 0xF0, 1, 2, 3], 1, 1, false);
        let p = packets(&out);
        assert_eq!(p.len(), 1);
        assert_eq!(pid(p[0]), VIDEO_PID);
        assert_eq!(p[0][5], 0x10); // PCR only
    }

    #[test]
    fn every_payload_length_fills_whole_packets() {
        let mut muxer = TsMuxer::new();
        for len in 150..400 {
            let mut out = Vec::new();
            muxer.write_frame(&mut out, &vec![7u8; len], 0, 0, false);
            assert_eq!(out.len() % PACKET_SIZE, 0, "length {len}");
        }
    }
}
