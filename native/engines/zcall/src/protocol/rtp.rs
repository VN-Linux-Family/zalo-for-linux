//! RTP header framing for audio and video.

pub const AUDIO_PT: u8 = 112;
pub const FRAME_SAMPLES: usize = 320; // 20 ms at 16 kHz
pub const TWCC_EXT_ID: u8 = 5;
pub const AUDIO_LEVEL_EXT_ID: u8 = 2;
pub const ABS_SEND_TIME_EXT_ID: u8 = 3;
pub const CAPTURE_NTP_EXT_ID: u8 = 13;
pub const NTP_UNIX_OFFSET: u64 = 2_208_988_800;

/// Read the RFC 6464 one-byte audio-level extension (id 2) from an RTP packet.
/// The extension is part of the authenticated RTP header and remains unencrypted
/// by the SRTP profile used by Zalo.
#[must_use]
pub fn parse_rfc6464_audio_level(rtp: &[u8]) -> Option<u8> {
    if rtp.len() < 16 || rtp[0] >> 6 != 2 || rtp[0] & 0x10 == 0 {
        return None;
    }
    let mut offset = 12usize.checked_add(usize::from(rtp[0] & 0x0f) * 4)?;
    if offset + 4 > rtp.len() || u16::from_be_bytes([rtp[offset], rtp[offset + 1]]) != 0xbede {
        return None;
    }
    let extension_bytes = usize::from(u16::from_be_bytes([rtp[offset + 2], rtp[offset + 3]])) * 4;
    offset += 4;
    let end = offset.checked_add(extension_bytes)?.min(rtp.len());
    while offset < end {
        let header = rtp[offset];
        if header == 0 {
            offset += 1;
            continue;
        }
        let id = header >> 4;
        if id == 15 {
            return None;
        }
        let length = usize::from(header & 0x0f) + 1;
        if id == 2 {
            return (offset + 1 < end).then_some(rtp[offset + 1] & 0x7f);
        }
        offset = offset.checked_add(1 + length)?;
    }
    None
}

pub struct AudioRtpHeader {
    pub marker: bool,
    pub pt: u8,
    pub seq: u16,
    pub ts: u32,
    pub ssrc: u32,
    pub tw_seq: Option<u16>,
}

#[must_use]
pub fn build_audio_rtp_header(
    seq: u16,
    ts: u32,
    ssrc: u32,
    tw_seq: u16,
    marker: bool,
    pt: u8,
) -> [u8; 20] {
    let mut h = [0u8; 20];
    h[0] = 0x90; // V=2, X=1
    h[1] = if marker { 0x80 } else { 0 } | (pt & 0x7f);
    h[2..4].copy_from_slice(&seq.to_be_bytes());
    h[4..8].copy_from_slice(&ts.to_be_bytes());
    h[8..12].copy_from_slice(&ssrc.to_be_bytes());
    h[12..14].copy_from_slice(&0xbedeu16.to_be_bytes()); // RFC 8285 one-byte header ext
    h[14..16].copy_from_slice(&1u16.to_be_bytes()); // 1 word (4 bytes)
    h[16] = (TWCC_EXT_ID << 4) | 1; // id=5, len=2 (encoded as len-1 = 1)
    h[17..19].copy_from_slice(&tw_seq.to_be_bytes());
    h[19] = 0; // padding
    h
}

#[expect(
    clippy::too_many_arguments,
    reason = "The RTP wire-header fields are intentionally passed explicitly to match the protocol layout."
)]
#[must_use]
pub fn build_group_audio_rtp_header(
    seq: u16,
    ts: u32,
    ssrc: u32,
    tw_seq: u16,
    marker: bool,
    pt: u8,
    capture_ms: u64,
    level: u8,
) -> Vec<u8> {
    let mut h = vec![0u8; 12 + 4 + 16];
    h[0] = 0x90;
    h[1] = if marker { 0x80 } else { 0 } | (pt & 0x7f);
    h[2..4].copy_from_slice(&seq.to_be_bytes());
    h[4..8].copy_from_slice(&ts.to_be_bytes());
    h[8..12].copy_from_slice(&ssrc.to_be_bytes());
    h[12..14].copy_from_slice(&0xbedeu16.to_be_bytes());
    h[14..16].copy_from_slice(&4u16.to_be_bytes()); // 4 words (16 bytes)

    h[16] = AUDIO_LEVEL_EXT_ID << 4; // id=2, len=1
    h[17] = level.min(127);
    h[18] = (TWCC_EXT_ID << 4) | 1; // id=5, len=2
    h[19..21].copy_from_slice(&tw_seq.to_be_bytes());
    write_capture_ntp(&mut h, 21, capture_ms);
    h
}

#[expect(
    clippy::too_many_arguments,
    reason = "The RTP wire-header fields are intentionally passed explicitly to match the protocol layout."
)]
#[must_use]
pub fn build_group_video_rtp_header(
    seq: u16,
    ts: u32,
    ssrc: u32,
    tw_seq: u16,
    marker: bool,
    pt: u8,
    sfu_long: bool,
    sfu_id: u8,
    layer_seq: u16,
    capture_ms: u64,
    send_ms: u64,
) -> Vec<u8> {
    let id = if sfu_id != 0 {
        sfu_id
    } else if sfu_long {
        14
    } else {
        12
    };
    let data_len = if sfu_long { 7 } else { 3 };
    let ext_bytes = 4 + 3 + (1 + data_len) + 8;
    let padded = (ext_bytes + 3) & !3;

    let mut h = vec![0u8; 12 + 4 + padded];
    h[0] = 0x90;
    h[1] = if marker { 0x80 } else { 0 } | (pt & 0x7f);
    h[2..4].copy_from_slice(&seq.to_be_bytes());
    h[4..8].copy_from_slice(&ts.to_be_bytes());
    h[8..12].copy_from_slice(&ssrc.to_be_bytes());
    h[12..14].copy_from_slice(&0xbedeu16.to_be_bytes());
    h[14..16].copy_from_slice(&((padded / 4) as u16).to_be_bytes());

    let mut o = 16;
    h[o] = (ABS_SEND_TIME_EXT_ID << 4) | 2;
    o += 1;
    let abs = (u128::from(send_ms) * 262_144 / 1000 % 0x0100_0000) as u32;
    h[o] = ((abs >> 16) & 0xff) as u8;
    h[o + 1] = ((abs >> 8) & 0xff) as u8;
    h[o + 2] = (abs & 0xff) as u8;
    o += 3;

    h[o] = (TWCC_EXT_ID << 4) | 1;
    o += 1;
    h[o..o + 2].copy_from_slice(&tw_seq.to_be_bytes());
    o += 2;

    h[o] = ((id & 0x0f) << 4) | (if sfu_long { 6 } else { 2 });
    o += 1;
    h[o] = 0x03; // SFU byte 0
    o += 1;
    h[o..o + 2].copy_from_slice(&layer_seq.to_be_bytes());
    o += 2;
    if sfu_long {
        o += 4;
    }

    write_capture_ntp(&mut h, o, capture_ms);
    h
}

fn write_capture_ntp(h: &mut [u8], mut o: usize, capture_ms: u64) {
    h[o] = (CAPTURE_NTP_EXT_ID << 4) | 6;
    o += 1;
    let sec = capture_ms / 1000 + NTP_UNIX_OFFSET;
    let millis = capture_ms % 1000;
    let frac = (u128::from(millis) * 4_294_967_296 / 1000) as u32;
    h[o] = ((sec >> 16) & 0xff) as u8;
    h[o + 1] = ((sec >> 8) & 0xff) as u8;
    h[o + 2] = (sec & 0xff) as u8;
    o += 3;
    h[o..o + 4].copy_from_slice(&frac.to_be_bytes());
}

#[must_use]
pub const fn read_audio_rtp_header(rtp: &[u8]) -> Option<AudioRtpHeader> {
    if rtp.len() < 20 || rtp[0] != 0x90 || u16::from_be_bytes([rtp[12], rtp[13]]) != 0xbede {
        return None;
    }
    let tw_seq = if (rtp[16] >> 4) == TWCC_EXT_ID {
        Some(u16::from_be_bytes([rtp[17], rtp[18]]))
    } else {
        None
    };

    Some(AudioRtpHeader {
        marker: (rtp[1] & 0x80) != 0,
        pt: rtp[1] & 0x7f,
        seq: u16::from_be_bytes([rtp[2], rtp[3]]),
        ts: u32::from_be_bytes([rtp[4], rtp[5], rtp[6], rtp[7]]),
        ssrc: u32::from_be_bytes([rtp[8], rtp[9], rtp[10], rtp[11]]),
        tw_seq,
    })
}

#[cfg(test)]
#[path = "../tests/protocol/rtp.rs"]
mod tests;
