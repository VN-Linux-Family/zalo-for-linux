use super::*;

fn annex_b_key() -> Vec<u8> {
    vec![
        0, 0, 0, 1, 0x67, 0x64, 0, 0x28, 0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80, 0, 0, 0, 1, 0x65, 1,
        2, 3,
    ]
}

#[test]
fn assembles_complete_key_and_delta_frames() {
    let mut assembler = VideoAssembler::new();
    let key = annex_b_key();
    let frames = assembler.push_packet(1, 0, 90, &key, true, VIDEO_PT_KEY);
    assert_eq!(frames.len(), 1);
    assert!(frames[0].key);
    assert_eq!(frames[0].codec, "avc1.640028");
    assert_eq!(frames[0].data, key);
    let delta = [0, 0, 0, 1, 0x41, 9, 8, 7];
    let frames = assembler.push_packet(2, 0, 180, &delta, true, VIDEO_PT_INTER);
    assert_eq!(frames.len(), 1);
    assert!(!frames[0].key);
    assert_eq!(frames[0].data, delta);
}

#[test]
fn packetizes_and_reassembles_large_annex_b_frame() {
    let data: Vec<u8> = (0..2500).map(|n| (n % 251) as u8).collect();
    let packets = packetize_frame(&data, 1068);
    assert_eq!(packets.len(), 3);
    assert_eq!(&packets[0][..2], &[0x1c, 0x80]);
    assert_eq!(&packets[1][..2], &[0x1c, 0]);
    assert_eq!(&packets[2][..2], &[0x1c, 0x40]);
    let mut assembler = VideoAssembler::new();
    assert!(assembler
        .push_packet(1, 0, 90, &packets[0], false, VIDEO_PT_KEY)
        .is_empty());
    assert!(assembler
        .push_packet(2, 0, 90, &packets[1], false, VIDEO_PT_INTER)
        .is_empty());
    let frames = assembler.push_packet(3, 0, 90, &packets[2], true, VIDEO_PT_INTER);
    assert_eq!(frames.len(), 1);
    assert!(frames[0].key);
    assert_eq!(frames[0].data, data);
}

#[test]
fn group_fragments_accept_fu_indicators_with_nri_bits() {
    for indicator in [0x1c, 0x5c, 0x7c] {
        let mut assembler = VideoAssembler::with_group(true);
        let first = [indicator, 0x80, 0, 0, 0, 1, 0x65, 1];
        let middle = [indicator, 0x00, 2, 3];
        let last = [indicator, 0x40, 4, 5];
        assert!(assembler
            .push_packet(10, 0, 900, &first, false, VIDEO_PT_INTER)
            .is_empty());
        assert!(assembler
            .push_packet(11, 0, 900, &middle, false, VIDEO_PT_INTER)
            .is_empty());
        let frames = assembler.push_packet(12, 0, 900, &last, true, VIDEO_PT_INTER);
        assert_eq!(frames.len(), 1, "indicator={indicator:#04x}");
        assert!(frames[0].key, "indicator={indicator:#04x}");
        assert_eq!(frames[0].data, [0, 0, 0, 1, 0x65, 1, 2, 3, 4, 5]);
    }
}

#[test]
fn completes_a_frame_when_its_middle_packet_arrives_late() {
    let mut assembler = VideoAssembler::new();
    let first = [0x1c, 0x80, 1, 2];
    let middle = [0x1c, 0x00, 3, 4];
    let last = [0x1c, 0x40, 5, 6];
    assert!(assembler
        .push_packet(100, 0, 900, &first, false, VIDEO_PT_KEY)
        .is_empty());
    assert!(assembler
        .push_packet(102, 0, 900, &last, true, VIDEO_PT_INTER)
        .is_empty());
    let frames = assembler.push_packet(101, 0, 900, &middle, false, VIDEO_PT_INTER);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].data, [1, 2, 3, 4, 5, 6]);
}

#[test]
fn reorders_complete_frames_by_extended_sequence_number() {
    let mut assembler = VideoAssembler::new();
    assert_eq!(
        assembler
            .push_packet(10, 0, 100, &annex_b_key(), true, VIDEO_PT_KEY)
            .len(),
        1
    );
    let second = [0, 0, 0, 1, 0x41, 2];
    let third = [0, 0, 0, 1, 0x41, 3];
    assert!(assembler
        .push_packet(12, 0, 300, &third, true, VIDEO_PT_INTER)
        .is_empty());
    let frames = assembler.push_packet(11, 0, 200, &second, true, VIDEO_PT_INTER);
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].timestamp, 200);
    assert_eq!(frames[1].timestamp, 300);
}

#[test]
fn ignores_duplicate_and_late_packets_after_a_frame_was_emitted() {
    let mut assembler = VideoAssembler::new();
    let key = annex_b_key();
    assert_eq!(
        assembler
            .push_packet(10, 0, 100, &key, true, VIDEO_PT_KEY)
            .len(),
        1
    );
    assert!(assembler
        .push_packet(10, 0, 100, &key, true, VIDEO_PT_KEY)
        .is_empty());
    assert!(assembler
        .push_packet(9, 0, 90, &key, true, VIDEO_PT_KEY)
        .is_empty());
}

#[test]
fn handles_sequence_rollover_using_the_srtp_roc() {
    let mut assembler = VideoAssembler::new();
    assert_eq!(
        assembler
            .push_packet(u16::MAX, 0, 100, &annex_b_key(), true, VIDEO_PT_KEY)
            .len(),
        1
    );
    let delta = [0, 0, 0, 1, 0x41, 9];
    assert_eq!(
        assembler
            .push_packet(0, 1, 200, &delta, true, VIDEO_PT_INTER)
            .len(),
        1
    );
}

#[test]
fn group_assembler_drops_svc_nals_and_caches_sps_pps() {
    let mut assembler = VideoAssembler::with_group(true);
    let mut key = annex_b_key();
    key.extend_from_slice(&[0, 0, 0, 1, 0x6e, 1, 2]);
    let frames = assembler.push_packet(1, 0, 100, &key, true, VIDEO_PT_INTER);
    assert_eq!(frames.len(), 1);
    assert!(frames[0].key);
    assert_eq!(frames[0].codec, "avc1.640028");
    assert!(!frames[0]
        .data
        .windows(5)
        .any(|window| window == [0, 0, 0, 1, 0x6e]));
    let idr_only = [0, 0, 0, 1, 0x65, 4, 5, 6];
    let frames = assembler.push_packet(2, 0, 200, &idr_only, true, VIDEO_PT_INTER);
    assert_eq!(frames.len(), 1);
    assert!(frames[0].key);
    assert!(frames[0].data.starts_with(&annex_b_key()[..16]));
}

#[test]
fn first_delta_is_dropped_and_requests_a_keyframe() {
    let mut assembler = VideoAssembler::new();
    let delta = [0, 0, 0, 1, 0x41, 9];
    assert!(assembler
        .push_packet(1, 0, 100, &delta, true, VIDEO_PT_INTER)
        .is_empty());
    assert!(assembler.take_keyframe_request());
    assert!(!assembler.take_keyframe_request());
    let frames = assembler.push_packet(2, 0, 200, &annex_b_key(), true, VIDEO_PT_KEY);
    assert_eq!(frames.len(), 1);
    assert!(frames[0].key);
}

#[test]
fn group_gap_drops_deltas_until_the_next_keyframe() {
    let mut assembler = VideoAssembler::with_group(true);
    let key = annex_b_key();
    assert_eq!(
        assembler
            .push_packet(1, 0, 100, &key, true, VIDEO_PT_INTER)
            .len(),
        1
    );
    let delta = [0, 0, 0, 1, 0x41, 9];
    assert!(assembler
        .push_packet(4, 0, 400, &delta, true, VIDEO_PT_INTER)
        .is_empty());
    std::thread::sleep(REORDER_WINDOW + Duration::from_millis(10));
    assert!(assembler
        .push_packet(5, 0, 500, &delta, true, VIDEO_PT_INTER)
        .is_empty());
    assert!(assembler.take_keyframe_request());
    let frames = assembler.push_packet(6, 0, 600, &key, true, VIDEO_PT_INTER);
    assert_eq!(frames.len(), 1);
    assert!(frames[0].key);
}
