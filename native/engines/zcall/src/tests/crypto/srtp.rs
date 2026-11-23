use super::*;
use crate::protocol::rtp::build_audio_rtp_header;

#[test]
fn hmac_sha1_matches_rfc_2202_test_case_1() {
    let key = [0x0b; 20];
    let digest = hmac_sha1(&key, &[b"Hi There"]);
    assert_eq!(
        digest,
        [
            0xb6, 0x17, 0x31, 0x86, 0x55, 0x05, 0x72, 0x64, 0xe2, 0x8b, 0xc0, 0xb6, 0xfb, 0x37,
            0x8c, 0x8e, 0xf1, 0x46, 0xbe, 0x00,
        ]
    );
}

#[test]
fn hmac_sha1_matches_rfc_2202_multiblock_test_case_2() {
    let key = [0xaa; 20];
    let data = [0xdd; 50];
    let digest = hmac_sha1(&key, &[&data]);
    assert_eq!(
        digest,
        [
            0x12, 0x5d, 0x73, 0x42, 0xb9, 0xac, 0x11, 0xcd, 0x91, 0xa3, 0x9a, 0xf4, 0x8a, 0xa1,
            0x7b, 0x4f, 0x63, 0xf1, 0x75, 0xd3,
        ]
    );
}

#[test]
fn derives_session_keys_with_rfc3711_lengths_and_rejects_short_master() {
    let keys = session_keys(&[0x42; 30]).unwrap();
    assert_eq!(keys.enc.len(), 16);
    assert_eq!(keys.auth.len(), 20);
    assert_eq!(keys.salt.len(), 14);
    assert!(session_keys(&[0x42; 29]).is_err());
}

#[test]
fn derives_master_from_the_first_thirty_session_id_bytes() {
    let session = b"012345678901234567890123456789extra";
    let master = master_from_sess_id(session).unwrap();
    assert_eq!(&master, b"012345678901234567890123456789");
    assert!(master_from_sess_id(b"short").is_err());
}

#[test]
fn derives_a_deterministic_packet_iv_that_changes_with_sequence() {
    let keys = session_keys(&[0x42; 30]).unwrap();
    let first = packet_iv(&keys.salt, 0x1234_5678, 100, 0);
    let repeated = packet_iv(&keys.salt, 0x1234_5678, 100, 0);
    let next = packet_iv(&keys.salt, 0x1234_5678, 101, 0);
    assert_eq!(first.len(), 16);
    assert_eq!(first, repeated);
    assert_ne!(first, next);
}

#[test]
fn protects_and_unprotects_an_rtp_payload() {
    let keys = session_keys(&[0x42; 30]).unwrap();
    let ssrc = 0xdead_beefu32;
    let seq = 1234u16;
    let mut header = [0u8; 12];
    header[0] = 0x80;
    header[1] = 112;
    header[2..4].copy_from_slice(&seq.to_be_bytes());
    header[4..8].copy_from_slice(&960_000u32.to_be_bytes());
    header[8..12].copy_from_slice(&ssrc.to_be_bytes());
    let original = b"Testing Opus Audio Frame Payload 1234567890!@#$%^&*()";

    let protected = protect_rtp(&header, original, &keys, seq, ssrc, 0);
    assert_eq!(protected.len(), header.len() + original.len() + 10);
    let clear = unprotect_rtp(&protected, &keys, 0).unwrap();
    assert_eq!(clear.seq, seq);
    assert_eq!(clear.ssrc, ssrc);
    assert_eq!(clear.opus, original);
}

#[test]
fn matches_legacy_node_srtp_wire_vector() {
    let keys = session_keys(&[0x42; 30]).unwrap();
    let header = hex_bytes("80620001000ea600deadbeef");
    let payload = b"legacy-rust-interoperability-vector-2026";
    let expected = hex_bytes(
        "80620001000ea600deadbeef5ca2531becdc8fa53bea26bab555b5ec67760afb4a1ed1e9dea5f60b8eb9ee00a93fc55ccfc99b72e3951717b28bee25c620",
    );
    let protected = protect_rtp(&header, payload, &keys, 1, 0xdead_beef, 0);
    assert_eq!(
        protected, expected,
        "Rust SRTP must match legacy Node byte-for-byte"
    );
    let clear = unprotect_rtp(&expected, &keys, 0).expect("legacy packet authenticates");
    assert_eq!(clear.seq, 1);
    assert_eq!(clear.ssrc, 0xdead_beef);
    assert_eq!(clear.opus, payload);
}

fn hex_bytes(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks(2)
        .map(|pair| {
            assert_eq!(pair.len(), 2);
            let digit = |b: u8| match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                _ => panic!("invalid hex digit"),
            };
            digit(pair[0]) * 16 + digit(pair[1])
        })
        .collect()
}

#[test]
fn matches_legacy_node_srtp_large_video_packet_tag() {
    let keys = session_keys(&[0x42; 30]).unwrap();
    let header = hex_bytes("80620001000ea600deadbeef");
    let payload = (0..1100)
        .map(|i| ((i * 37 + 11) & 0xff) as u8)
        .collect::<Vec<_>>();
    let protected = protect_rtp(&header, &payload, &keys, 1, 0xdead_beef, 0);
    assert_eq!(
        &protected[protected.len() - 10..],
        &hex_bytes("ee932972b32a73924f19")
    );
    let clear = unprotect_rtp(&protected, &keys, 0).expect("large legacy packet authenticates");
    assert_eq!(clear.opus, payload);
}

#[test]
fn unprotects_rfc3711_mki_before_the_authentication_tag() {
    let keys = session_keys(&[0x6a; 30]).unwrap();
    let seq = 321u16;
    let ssrc = 0x1357_2468u32;
    let mut header = [0u8; 12];
    header[0] = 0x80;
    header[1] = 98;
    header[2..4].copy_from_slice(&seq.to_be_bytes());
    header[4..8].copy_from_slice(&9000u32.to_be_bytes());
    header[8..12].copy_from_slice(&ssrc.to_be_bytes());
    let clear_payload = b"group-video-with-mki";
    let protected = protect_rtp(&header, clear_payload, &keys, seq, ssrc, 0);

    for (mki, tag_len) in [(&[0x01][..], 10usize), (&[0x01, 0x02, 0x03, 0x04][..], 10)] {
        let tag_start = protected.len() - 10;
        let mut packet = protected[..tag_start].to_vec();
        packet.extend_from_slice(mki);
        packet.extend_from_slice(&protected[tag_start..]);
        let result = unprotect_rtp_with_profile(&packet, &keys, 0, mki.len(), tag_len)
            .expect("MKI packet should authenticate");
        assert_eq!(result.seq, seq);
        assert_eq!(result.ssrc, ssrc);
        assert_eq!(result.opus, clear_payload);
        assert!(unprotect_rtp(&packet, &keys, 0).is_none());
    }
}

#[test]
fn rejects_mki_packet_with_modified_ciphertext_or_tag() {
    let keys = session_keys(&[0x6b; 30]).unwrap();
    let seq = 322u16;
    let ssrc = 0x1357_2469u32;
    let mut header = [0u8; 12];
    header[0] = 0x80;
    header[1] = 98;
    header[2..4].copy_from_slice(&seq.to_be_bytes());
    header[8..12].copy_from_slice(&ssrc.to_be_bytes());
    let protected = protect_rtp(&header, b"authenticated", &keys, seq, ssrc, 0);
    let tag_start = protected.len() - 10;
    let mut packet = protected[..tag_start].to_vec();
    packet.extend_from_slice(&[0xaa, 0xbb]);
    packet.extend_from_slice(&protected[tag_start..]);
    let mut damaged = packet.clone();
    damaged[12] ^= 0x80;
    assert!(unprotect_rtp_with_profile(&damaged, &keys, 0, 2, 10).is_none());
    let last = packet.len() - 1;
    packet[last] ^= 1;
    assert!(unprotect_rtp_with_profile(&packet, &keys, 0, 2, 10).is_none());
}

#[test]
fn rejects_tampered_ciphertext_and_authentication_tag() {
    let keys = session_keys(&[0x42; 30]).unwrap();
    let ssrc = 0x1122_3344u32;
    let seq = 55u16;
    let mut header = [0u8; 12];
    header[0] = 0x80;
    header[1] = 112;
    header[2..4].copy_from_slice(&seq.to_be_bytes());
    header[4..8].copy_from_slice(&1000u32.to_be_bytes());
    header[8..12].copy_from_slice(&ssrc.to_be_bytes());
    let protected = protect_rtp(&header, b"Authentic audio data", &keys, seq, ssrc, 0);

    let mut payload_tampered = protected.clone();
    payload_tampered[15] ^= 0xff;
    assert!(unprotect_rtp(&payload_tampered, &keys, 0).is_none());

    let mut tag_tampered = protected;
    let last = tag_tampered.len() - 1;
    tag_tampered[last] ^= 1;
    assert!(unprotect_rtp(&tag_tampered, &keys, 0).is_none());
}

#[test]
fn authenticates_rtp_packets_after_sequence_rollover() {
    let keys = session_keys(&[0x51; 30]).unwrap();
    let seq = 3u16;
    let header = build_audio_rtp_header(seq, 960, 0x1122_3344, 77, false, 112);
    let protected = protect_rtp(
        &header,
        b"opus-frame-after-rollover",
        &keys,
        seq,
        0x1122_3344,
        1,
    );

    assert!(unprotect_rtp(&protected, &keys, 0).is_none());
    let clear = unprotect_rtp(&protected, &keys, 1).expect("correct rollover counter");
    assert_eq!(clear.seq, seq);
    assert_eq!(clear.ssrc, 0x1122_3344);
    assert_eq!(clear.opus, b"opus-frame-after-rollover");
}
