use super::*;
use crate::crypto::srtp::{protect_rtp, session_keys};
use crate::protocol::rtp::build_audio_rtp_header;

#[test]
fn recovers_authenticated_packet_after_peer_resets_roc_to_zero() {
    let keys = session_keys(&[0x42; 30]).unwrap();
    let mut sender = RocTracker::default();
    sender.update(65_530, 0);
    sender.update(10, 1);
    let header = build_audio_rtp_header(11, 320, 77, 3, false, 112);
    let protected = protect_rtp(&header, b"opus", &keys, 11, 77, 0);

    let mut receiver = RocTracker {
        last: Some(65_000),
        roc: 2,
    };
    let (clear, roc) = unprotect_with_roc_recovery(&protected, &keys, &mut receiver)
        .expect("authenticate reset-ROC packet");
    assert_eq!(clear.seq, 11);
    assert_eq!(clear.ssrc, 77);
    assert_eq!(clear.opus, b"opus");
    assert_eq!(roc, 0);
    assert_eq!(receiver.roc, 0);
    assert_eq!(receiver.last, Some(11));
}

#[test]
fn guessed_roc_is_tried_before_recovery_candidates_and_rollover_advances() {
    let keys = session_keys(&[0x13; 30]).unwrap();
    let header = build_audio_rtp_header(2, 640, 55, 8, false, 112);
    let protected = protect_rtp(&header, b"next", &keys, 2, 55, 1);
    let mut receiver = RocTracker {
        last: Some(65_530),
        roc: 0,
    };
    let (_, roc) = unprotect_with_roc_recovery(&protected, &keys, &mut receiver).unwrap();
    assert_eq!(roc, 1);
    assert_eq!(receiver.last, Some(2));
    assert_eq!(receiver.roc, 1);
}

#[test]
fn group_receive_tries_authenticated_session_fallback_when_primary_key_is_wrong() {
    let primary = session_keys(&[0x11; 30]).unwrap();
    let fallback = session_keys(&[0x42; 30]).unwrap();
    let header = build_audio_rtp_header(17, 960, 0x1234, 9, false, 112);
    let protected = protect_rtp(&header, b"authenticated fallback", &fallback, 17, 0x1234, 0);
    let (clear, roc, used_fallback, mki_len, tag_len) =
        unprotect_group_with_candidates(&protected, Some(&primary), Some(&fallback), &[0, 1], None)
            .expect("fallback key should authenticate");
    assert_eq!(clear.seq, 17);
    assert_eq!(clear.ssrc, 0x1234);
    assert_eq!(clear.opus, b"authenticated fallback");
    assert_eq!(roc, 0);
    assert!(used_fallback);
    assert_eq!(mki_len, 0);
    assert_eq!(tag_len, 10);
}

#[test]
fn group_receive_prefers_primary_key_when_both_candidates_are_available() {
    let primary = session_keys(&[0x42; 30]).unwrap();
    let fallback = session_keys(&[0x11; 30]).unwrap();
    let header = build_audio_rtp_header(18, 960, 0x1234, 10, false, 112);
    let protected = protect_rtp(&header, b"primary key", &primary, 18, 0x1234, 0);
    let (_, _, used_fallback, mki_len, tag_len) =
        unprotect_group_with_candidates(&protected, Some(&primary), Some(&fallback), &[0], None)
            .expect("primary key should authenticate");
    assert!(!used_fallback);
    assert_eq!(mki_len, 0);
    assert_eq!(tag_len, 10);
}

#[test]
fn group_receive_accepts_only_authenticated_mki_packets() {
    let primary = session_keys(&[0x53; 30]).unwrap();
    let header = build_audio_rtp_header(20, 960, 0x1234, 11, false, 112);
    let protected = protect_rtp(&header, b"group payload with MKI", &primary, 20, 0x1234, 0);
    let tag_start = protected.len() - 10;
    let mut packet = protected[..tag_start].to_vec();
    packet.extend_from_slice(&[0x01, 0x02, 0x03, 0x04]);
    packet.extend_from_slice(&protected[tag_start..]);
    let (clear, roc, fallback, mki_len, tag_len) =
        unprotect_group_with_candidates(&packet, Some(&primary), None, &[0], None)
            .expect("valid MKI packet should authenticate");
    assert_eq!(clear.opus, b"group payload with MKI");
    assert_eq!(roc, 0);
    assert!(!fallback);
    assert_eq!(mki_len, 4);
    assert_eq!(tag_len, 10);

    let last = packet.len() - 1;
    packet[last] ^= 1;
    assert!(unprotect_group_with_candidates(&packet, Some(&primary), None, &[0], None).is_none());
}

#[test]
fn group_receive_discovers_and_caches_nonstandard_mki_and_tag_lengths() {
    let primary = session_keys(&[0x54; 30]).unwrap();
    let header = build_audio_rtp_header(21, 960, 0x1234, 12, false, 112);
    let protected = protect_rtp(&header, b"variable group profile", &primary, 21, 0x1234, 0);
    let tag_start = protected.len() - 10;
    let mut packet = protected[..tag_start].to_vec();
    packet.extend_from_slice(&[0x10, 0x20, 0x30]);
    packet.extend_from_slice(&protected[tag_start..tag_start + 8]);

    let (clear, roc, fallback, mki_len, tag_len) =
        unprotect_group_with_candidates(&packet, Some(&primary), None, &[0], None)
            .expect("variable MKI/tag profile should authenticate");
    assert_eq!(clear.opus, b"variable group profile");
    assert_eq!(roc, 0);
    assert!(!fallback);
    assert_eq!((mki_len, tag_len), (3, 8));

    let (_, _, _, cached_mki, cached_tag) = unprotect_group_with_candidates(
        &packet,
        Some(&primary),
        None,
        &[0],
        Some((mki_len, tag_len)),
    )
    .expect("cached profile should authenticate without profile discovery");
    assert_eq!((cached_mki, cached_tag), (3, 8));
}

#[test]
fn authenticated_old_reordered_packet_does_not_rewind_tracker() {
    let mut tracker = RocTracker {
        last: Some(500),
        roc: 1,
    };
    tracker.update(450, 1);
    assert_eq!(tracker.last, Some(500));
    assert_eq!(tracker.roc, 1);
    tracker.update(350, 1);
    assert_eq!(tracker.last, Some(350));
}

#[test]
fn local_ipv4_signature_is_sorted_and_excludes_loopback() {
    let signature = local_ipv4_signature();
    assert!(!signature.split(',').any(|ip| ip == "127.0.0.1"));
    let mut parts = signature
        .split(',')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let original = parts.clone();
    parts.sort_unstable();
    parts.dedup();
    assert_eq!(parts, original);
}
