use super::*;

#[test]
fn builds_and_parses_audio_rtp_header_with_twcc() {
    let header = build_audio_rtp_header(105, 960_320, 987_654_321, 502, true, AUDIO_PT);
    assert_eq!(header.len(), 20);
    let parsed = read_audio_rtp_header(&header).unwrap();
    assert!(parsed.marker);
    assert_eq!(parsed.pt, AUDIO_PT);
    assert_eq!(parsed.seq, 105);
    assert_eq!(parsed.ts, 960_320);
    assert_eq!(parsed.ssrc, 987_654_321);
    assert_eq!(parsed.tw_seq, Some(502));
}

#[test]
fn parses_rfc6464_audio_level_extension_and_rejects_malformed_headers() {
    let mut packet =
        build_group_audio_rtp_header(1, 320, 42, 7, false, AUDIO_PT, 1_700_000_000_000, 43);
    packet.extend_from_slice(&[0xf8, 0xf9]);
    assert_eq!(parse_rfc6464_audio_level(&packet), Some(43));
    assert_eq!(parse_rfc6464_audio_level(&packet[..15]), None);

    let mut no_level = build_audio_rtp_header(1, 320, 42, 7, false, AUDIO_PT).to_vec();
    assert_eq!(parse_rfc6464_audio_level(&no_level), None);
    no_level[0] = 0x10;
    assert_eq!(parse_rfc6464_audio_level(&no_level), None);
}

#[test]
fn group_video_header_matches_short_sfu_extension_layout() {
    let header = build_group_video_rtp_header(
        7,
        9000,
        42,
        8,
        true,
        98,
        false,
        12,
        3,
        1_700_000_000_000,
        1_700_000_000_010,
    );
    assert_eq!(header.len(), 36);
    assert_eq!(header[0], 0x90);
    assert_eq!(header[1], 0x80 | 0x62);
    assert_eq!(u16::from_be_bytes([header[2], header[3]]), 7);
    assert_eq!(u32::from_be_bytes(header[8..12].try_into().unwrap()), 42);
    assert_eq!(u16::from_be_bytes([header[12], header[13]]), 0xbede);
    assert_eq!(header[23], (12 << 4) | 2);
    assert_eq!(header[24], 0x03);
    assert_eq!(u16::from_be_bytes([header[25], header[26]]), 3);
    assert_eq!(header[27], (CAPTURE_NTP_EXT_ID << 4) | 6);
}

#[test]
fn group_video_header_matches_long_sfu_extension_layout() {
    let header = build_group_video_rtp_header(
        9,
        18000,
        77,
        10,
        false,
        97,
        true,
        14,
        5,
        1_700_000_000_000,
        1_700_000_000_020,
    );
    assert_eq!(header.len(), 40);
    assert_eq!(header[23], (14 << 4) | 6);
    assert_eq!(header[24], 0x03);
    assert_eq!(u16::from_be_bytes([header[25], header[26]]), 5);
    assert_eq!(header[31], (CAPTURE_NTP_EXT_ID << 4) | 6);
}
