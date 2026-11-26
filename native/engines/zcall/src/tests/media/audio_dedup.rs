use super::*;

#[test]
fn stats_expose_legacy_rx_and_tx_audio_packet_totals() {
    let backend = ZrtcMediaBackend::new().unwrap();
    backend.rx_audio_packets.store(17, Ordering::Relaxed);
    backend.tx_audio_packets.store(23, Ordering::Relaxed);
    assert_eq!(backend.stats(), (17, 23));
}

#[test]
fn early_audio_phases_are_only_emitted_for_receive_only_one_to_one_callers() {
    assert_eq!(early_audio_phase(true, false, false, 1), Some("first"));
    assert_eq!(early_audio_phase(true, false, false, 50), Some("connect"));
    assert_eq!(early_audio_phase(true, false, false, 2), None);
    assert_eq!(early_audio_phase(false, false, false, 1), None);
    assert_eq!(early_audio_phase(true, true, false, 1), None);
    assert_eq!(early_audio_phase(true, false, true, 1), None);
    assert_eq!(early_audio_phase(true, false, true, 50), None);
}

#[test]
fn rejects_relay_and_p2p_duplicates_but_evicts_old_entries() {
    let mut dedup = AudioDeduplicator::new(3);
    assert!(dedup.accept(10, 100));
    assert!(!dedup.accept(10, 100));
    assert!(dedup.accept(20, 100));
    assert!(dedup.accept(10, 101));
    assert_eq!(dedup.len(), 3);
    assert!(dedup.accept(30, 1));
    assert_eq!(dedup.len(), 3);
    assert!(dedup.accept(10, 100)); // oldest key was evicted
}
