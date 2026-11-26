use super::*;

#[test]
fn reads_transport_wide_sequence_from_rfc8285_extension() {
    let mut rtp = vec![0u8; 20];
    rtp[0] = 0x90;
    rtp[12..14].copy_from_slice(&0xbedeu16.to_be_bytes());
    rtp[14..16].copy_from_slice(&1u16.to_be_bytes());
    rtp[16] = (TWCC_EXT_ID << 4) | 1;
    rtp[17..19].copy_from_slice(&0x1234u16.to_be_bytes());
    assert_eq!(read_tw_seq(&rtp), Some(0x1234));
}

#[test]
fn reports_fractional_and_cumulative_loss() {
    let mut stats = RxStats::new(90_000);
    for (seq, ts) in [(10u16, 0u32), (11, 3000), (13, 9000), (14, 12000)] {
        let mut rtp = vec![0u8; 12];
        rtp[2..4].copy_from_slice(&seq.to_be_bytes());
        rtp[4..8].copy_from_slice(&ts.to_be_bytes());
        rtp[8..12].copy_from_slice(&222u32.to_be_bytes());
        stats.on_packet(&rtp, u64::from(seq) * 33);
    }
    let report = stats.block(1000).unwrap();
    assert_eq!(report.ssrc, 222);
    assert_eq!(report.cumulative_lost, 1);
    assert_eq!(report.fraction_lost, 51);
}

#[test]
fn twcc_build_and_parse_roundtrip_handles_large_and_small_deltas() {
    let mut recorder = TwccRecorder::default();
    for (seq, time) in [(65534u16, 1000u64), (0, 1000), (1, 1200), (2, 900)] {
        recorder.add(seq, time);
    }
    let packets = recorder.build(10, 20);
    assert_eq!(packets.len(), 1);
    let decoded = parse_twcc(&packets[0]).unwrap();
    assert_eq!(decoded.base, 65534);
    assert_eq!(decoded.count, 5);
    assert_eq!(decoded.status, vec![1, 0, 1, 2, 2]);
    assert_eq!(decoded.delta_q, vec![160, 0, 800, -1200]);
}

#[test]
fn nack_tracker_detects_and_retries_bounded_holes() {
    let mut tracker = NackTracker::default();
    assert_eq!(
        tracker.note(22, 10, 0),
        [] as [(u32, std::vec::Vec<u16>); 0]
    );
    let first = tracker.note(22, 13, 10);
    assert_eq!(first, vec![(22, vec![11, 12])]);
    assert_eq!(tracker.poll(49), [] as [(u32, std::vec::Vec<u16>); 0]);
    assert_eq!(tracker.poll(50), vec![(22, vec![11, 12])]);
    assert_eq!(tracker.poll(90), vec![(22, vec![11, 12])]);
}

#[test]
fn builds_nack_compacting_next_sixteen_sequences() {
    let packet = build_nack_many(10, 20, &[100, 101, 104, 116, 120]);
    assert_eq!(u16::from_be_bytes([packet[2], packet[3]]), 4);
    assert_eq!(u16::from_be_bytes([packet[12], packet[13]]), 100);
    assert_eq!(u16::from_be_bytes([packet[14], packet[15]]), 0x8009);
    assert_eq!(u16::from_be_bytes([packet[16], packet[17]]), 120);
    assert_eq!(u16::from_be_bytes([packet[18], packet[19]]), 0x0000);
}

#[test]
fn parses_nack_pid_and_bitmask_with_sequence_wrap() {
    let nack = build_nack_many(20, 10, &[65534, 65535, 0, 2]);
    let parsed = parse_nack(&nack).unwrap();
    assert_eq!(parsed.sender_ssrc, 20);
    assert_eq!(parsed.media_ssrc, 10);
    assert_eq!(parsed.lost_sequences, vec![0, 2, 65534, 65535]);
}

#[test]
fn parses_report_blocks_and_twcc_from_compound_feedback() {
    let block = ReportBlock {
        ssrc: 10,
        fraction_lost: 25,
        cumulative_lost: -2,
        extended_highest_seq: 99,
        jitter: 4,
        lsr: 5,
        dlsr: 6,
    };
    let rr = build_rr(20, &[block]);
    let mut feedback = RtcpFeedback::new(10);
    let events = feedback.on_rtcp(true, &rr, 100);
    assert_eq!(events.report_blocks, vec![block]);

    let mut recorder = TwccRecorder::default();
    recorder.add(1, 1000);
    recorder.add(2, 1010);
    let twcc = recorder.build(20, 10).remove(0);
    let events = feedback.on_rtcp(true, &twcc, 100);
    assert_eq!(events.twcc_feedback.len(), 1);
    assert_eq!(events.twcc_feedback[0].count, 2);
}

#[test]
fn parses_pli_request_for_our_ssrc() {
    let mut feedback = RtcpFeedback::new(10);
    let pli = build_pli(20, 10);
    assert_eq!(feedback.on_rtcp(true, &pli, 100).keyframe_targets, vec![10]);
}
