use super::*;

#[test]
fn test_compute_level_db_bounds() {
    let silence = vec![0i16; 320];
    assert!(MicLeveler::compute_level_db(&silence) < -90.0);

    let full_scale = vec![32767i16; 320];
    let db = MicLeveler::compute_level_db(&full_scale);
    assert!((db - 0.0).abs() < 0.1);
}

#[test]
fn test_noise_floor_learning_and_speech_gain() {
    let far_active = Arc::new(AtomicBool::new(false));
    let mut leveler = MicLeveler::new(Some(Arc::clone(&far_active)));

    // 1. Feed 25 frames of quiet noise (~ -50 dBFS)
    // Amplitude ~ 100 on 32768 is ~ -50 dBFS
    let noise_frame = vec![103i16; 320];
    for _ in 0..25 {
        let mut f = noise_frame.clone();
        leveler.process(&mut f);
    }
    assert!(leveler.noise_db() > -60.0 && leveler.noise_db() < -45.0);

    // 2. Feed speech frame (~ -30 dBFS, amplitude ~ 1036)
    // Target is -22 dBFS, so gain should increase towards +8 dB
    let speech_frame = vec![1036i16; 320];
    let prev_gain = leveler.gain_db();
    let mut f = speech_frame;
    leveler.process(&mut f);
    assert!(leveler.gain_db() > prev_gain);
    assert!(leveler.gain_db() <= prev_gain + UP_DB + 0.01);
}

#[test]
fn test_far_active_holds_gain() {
    let far_active = Arc::new(AtomicBool::new(false));
    let mut leveler = MicLeveler::new(Some(Arc::clone(&far_active)));

    let noise_frame = vec![103i16; 320];
    for _ in 0..25 {
        let mut f = noise_frame.clone();
        leveler.process(&mut f);
    }

    // Set far_active to true (remote peer is speaking)
    far_active.store(true, Ordering::Relaxed);
    let speech_frame = vec![1036i16; 320];
    let gain_before = leveler.gain_db();
    let mut f = speech_frame;
    leveler.process(&mut f);

    // Gain must be frozen and NOT adapt
    assert_eq!(leveler.gain_db(), gain_before);
}

#[test]
fn test_peak_limiting_does_not_exceed_32000() {
    let mut leveler = MicLeveler::new(None);
    leveler.gain_db = 12.0; // High gain (+12 dB = ~4x)
    leveler.g_prev = 4.0;

    // Input sample with peak 10000 -> 4x would be 40000 (overflows i16 without limit)
    let mut frame = vec![10000i16; 320];
    leveler.process(&mut frame);

    for &s in &frame {
        assert!((-32000..=32000).contains(&s));
    }
}
