use super::super::codec::OpusEncoder;
use super::super::{CHANNELS, FRAME_SAMPLES, SAMPLE_RATE};
use super::*;

fn generate_sine_frame(freq: f32, phase: &mut f32) -> Vec<i16> {
    let mut pcm = vec![0i16; FRAME_SAMPLES];
    let step = 2.0 * std::f32::consts::PI * freq / SAMPLE_RATE as f32;
    for s in &mut pcm {
        *s = (phase.sin() * 15000.0) as i16;
        *phase = (*phase + step) % (2.0 * std::f32::consts::PI);
    }
    pcm
}

#[test]
fn test_jitter_buffer_prebuffering_and_playout() {
    let mut jb = JitterBuffer::new(SAMPLE_RATE as i32, CHANNELS as i32, 10).unwrap();
    jb.set_min_target(3);
    let mut enc = OpusEncoder::new(SAMPLE_RATE as i32, CHANNELS as i32, 32000).unwrap();

    let mut phase = 0.0;
    let mut opus_buf = vec![0u8; 1024];
    let mut out_pcm = vec![0i16; FRAME_SAMPLES];

    // Frame 0: still buffering, pop_frame returns false
    let pcm = generate_sine_frame(440.0, &mut phase);
    let len = enc.encode(&pcm, &mut opus_buf).unwrap();
    jb.push(0, opus_buf[..len].to_vec());
    assert_eq!(jb.state(), JitterState::Buffering);
    assert!(!jb.pop_frame(&mut out_pcm));

    // Frame 1: still buffering
    let pcm = generate_sine_frame(440.0, &mut phase);
    let len = enc.encode(&pcm, &mut opus_buf).unwrap();
    jb.push(1, opus_buf[..len].to_vec());
    assert_eq!(jb.state(), JitterState::Buffering);
    assert!(!jb.pop_frame(&mut out_pcm));

    // Frame 2: reaches min_target (3), transitions to Playing
    let pcm = generate_sine_frame(440.0, &mut phase);
    let len = enc.encode(&pcm, &mut opus_buf).unwrap();
    jb.push(2, opus_buf[..len].to_vec());
    assert_eq!(jb.state(), JitterState::Playing);

    // Now pop_frame succeeds for all 3 frames
    assert!(jb.pop_frame(&mut out_pcm));
    assert!(jb.pop_frame(&mut out_pcm));
    assert!(jb.pop_frame(&mut out_pcm));
}

#[test]
fn test_jitter_buffer_out_of_order_and_plc() {
    let mut jb = JitterBuffer::new(SAMPLE_RATE as i32, CHANNELS as i32, 10).unwrap();
    jb.set_min_target(2);
    let mut enc = OpusEncoder::new(SAMPLE_RATE as i32, CHANNELS as i32, 32000).unwrap();

    let mut phase = 0.0;
    let mut opus_buf = vec![0u8; 1024];

    // Generate frames 0, 1, 3 (frame 2 is missing, simulated packet loss)
    let pcm0 = generate_sine_frame(440.0, &mut phase);
    let len0 = enc.encode(&pcm0, &mut opus_buf).unwrap();
    let frame0 = opus_buf[..len0].to_vec();

    let pcm1 = generate_sine_frame(440.0, &mut phase);
    let len1 = enc.encode(&pcm1, &mut opus_buf).unwrap();
    let frame1 = opus_buf[..len1].to_vec();

    let pcm3 = generate_sine_frame(440.0, &mut phase);
    let len3 = enc.encode(&pcm3, &mut opus_buf).unwrap();
    let frame3 = opus_buf[..len3].to_vec();

    // Push out of order: 1 first, then 0, then 3
    jb.push(1, frame1);
    jb.push(0, frame0);
    jb.push(3, frame3);

    let mut out = vec![0i16; FRAME_SAMPLES];

    // Should pop frame 0
    assert!(jb.pop_frame(&mut out));
    // Should pop frame 1
    assert!(jb.pop_frame(&mut out));
    // Frame 2 is missing -> triggers Packet Loss Concealment (PLC), must still succeed
    assert!(jb.pop_frame(&mut out));
    // Should pop frame 3
    assert!(jb.pop_frame(&mut out));
}

#[test]
fn test_jitter_buffer_starvation_and_rebuffering() {
    let mut jb = JitterBuffer::new(SAMPLE_RATE as i32, CHANNELS as i32, 10).unwrap();
    jb.set_min_target(2);
    let mut enc = OpusEncoder::new(SAMPLE_RATE as i32, CHANNELS as i32, 32000).unwrap();

    let mut phase = 0.0;
    let mut opus_buf = vec![0u8; 1024];
    let mut out = vec![0i16; FRAME_SAMPLES];

    let pcm = generate_sine_frame(440.0, &mut phase);
    let len = enc.encode(&pcm, &mut opus_buf).unwrap();
    jb.push(0, opus_buf[..len].to_vec());
    jb.push(1, opus_buf[..len].to_vec());

    // Pop frames 0 and 1
    assert!(jb.pop_frame(&mut out));
    assert!(jb.pop_frame(&mut out));

    // Next 5 missing frames -> PLC 1..5 -> true
    for _ in 0..5 {
        assert!(jb.pop_frame(&mut out));
    }

    // 6th missing frame -> exceeds PLC limit -> starvation!
    // Should return false and transition back to Buffering state
    assert!(!jb.pop_frame(&mut out));
    assert_eq!(jb.state(), JitterState::Buffering);
    assert_eq!(jb.stats().underrun_count, 1);

    // New packets arrive: must buffer min_target (2) before resuming playout
    jb.push(50, opus_buf[..len].to_vec());
    assert_eq!(jb.state(), JitterState::Buffering);
    assert!(!jb.pop_frame(&mut out));

    jb.push(51, opus_buf[..len].to_vec());
    assert_eq!(jb.state(), JitterState::Playing);
    assert!(jb.pop_frame(&mut out));
    assert!(jb.pop_frame(&mut out));
}

#[test]
fn test_jitter_buffer_overflow_pruning_advances_next_idx() {
    let mut jb = JitterBuffer::new(SAMPLE_RATE as i32, CHANNELS as i32, 10).unwrap();
    jb.set_min_target(3);
    let mut enc = OpusEncoder::new(SAMPLE_RATE as i32, CHANNELS as i32, 32000).unwrap();

    let mut phase = 0.0;
    let mut opus_buf = vec![0u8; 1024];
    let mut out = vec![0i16; FRAME_SAMPLES];

    // Push 15 packets in a burst (exceeding max_depth = 10)
    for seq in 100..115 {
        let pcm = generate_sine_frame(440.0, &mut phase);
        let len = enc.encode(&pcm, &mut opus_buf).unwrap();
        jb.push(seq, opus_buf[..len].to_vec());
    }

    // Popping must decode real frames immediately with zero PLC frames!
    assert!(jb.pop_frame(&mut out));
    assert_eq!(jb.stats().plc_frames, 0);
    assert_eq!(jb.stats().played_frames, 1);
}

#[test]
fn test_jitter_buffer_backward_sequence_reset_and_ssrc_change() {
    let mut jb = JitterBuffer::new(SAMPLE_RATE as i32, CHANNELS as i32, 10).unwrap();
    jb.set_min_target(2);
    let mut enc = OpusEncoder::new(SAMPLE_RATE as i32, CHANNELS as i32, 32000).unwrap();

    let mut phase = 0.0;
    let mut opus_buf = vec![0u8; 1024];
    let mut out = vec![0i16; FRAME_SAMPLES];

    let pcm = generate_sine_frame(440.0, &mut phase);
    let len = enc.encode(&pcm, &mut opus_buf).unwrap();
    let pkt = opus_buf[..len].to_vec();

    jb.push_with_ssrc(5000, 111, pkt.clone());
    jb.push_with_ssrc(5001, 111, pkt.clone());
    assert!(jb.pop_frame(&mut out));

    // Simulate network handover where sequence resets backward to 10
    jb.push_with_ssrc(10, 111, pkt.clone());
    jb.push_with_ssrc(11, 111, pkt);
    assert_eq!(jb.state(), JitterState::Playing);
    assert!(jb.pop_frame(&mut out));
    assert!(jb.pop_frame(&mut out));
    assert_eq!(jb.stats().late_dropped, 0);
}

#[test]
fn test_jitter_buffer_multiframe_40ms_opus() {
    let mut jb = JitterBuffer::new(SAMPLE_RATE as i32, CHANNELS as i32, 10).unwrap();
    jb.set_min_target(2);
    let mut enc = OpusEncoder::new(SAMPLE_RATE as i32, CHANNELS as i32, 32000).unwrap();

    let mut phase = 0.0;
    let mut opus_buf = vec![0u8; 1024];
    let mut out = vec![0i16; FRAME_SAMPLES];

    // Encode 40ms (640 samples) Opus packets
    let mut pcm40 = generate_sine_frame(440.0, &mut phase);
    pcm40.extend(generate_sine_frame(440.0, &mut phase));
    let len = enc.encode(&pcm40, &mut opus_buf).unwrap();
    let pkt40 = opus_buf[..len].to_vec();

    jb.push(1, pkt40.clone());
    jb.push(2, pkt40);

    // Two 40ms packets must yield four 20ms pop_frame calls with zero PLC!
    assert!(jb.pop_frame(&mut out));
    assert!(jb.pop_frame(&mut out));
    assert!(jb.pop_frame(&mut out));
    assert!(jb.pop_frame(&mut out));
    assert_eq!(jb.stats().plc_frames, 0);
    assert_eq!(jb.stats().played_frames, 4);
}
