use super::*;
use crate::audio::codec::OpusEncoder;

fn encoded_silence() -> Vec<u8> {
    let pcm = [0i16; FRAME_SAMPLES];
    let mut encoder = OpusEncoder::new(16_000, 1, 24_000).unwrap();
    let mut encoded_packet = [0u8; 256];
    let length = encoder.encode(&pcm, &mut encoded_packet).unwrap();
    encoded_packet[..length].to_vec()
}

#[test]
fn keeps_sequence_rollover_state_per_member() {
    let mut mixer = GroupAudioMixer::new();
    let opus = encoded_silence();
    assert!(mixer.push_packet(10, 65534, 0, opus.clone()));
    assert!(mixer.push_packet(20, 7, 0, opus.clone()));
    assert_eq!(mixer.candidate_roc(10, 0), 1);
    assert_eq!(mixer.candidate_roc(20, 8), 0);
    assert!(mixer.candidate_rocs(10, 0).contains(&1));
    assert!(mixer.candidate_rocs(10, 0).contains(&0));
    assert!(mixer.push_packet(10, 0, 1, opus));
    assert_eq!(mixer.candidate_roc(10, 1), 1);
}

#[test]
fn mixes_multiple_member_decoders_into_one_frame() {
    let mut mixer = GroupAudioMixer::new();
    let opus = encoded_silence();
    for seq in 0..3 {
        assert!(mixer.push_packet(100, seq, 0, opus.clone()));
        assert!(mixer.push_packet(200, seq, 0, opus.clone()));
    }
    let mut output = [123i16; FRAME_SAMPLES];
    assert_eq!(mixer.mix_frame(&mut output), 2);
    assert!(output.iter().all(|sample| sample.abs() <= 2));
}

#[test]
fn rejects_invalid_ssrc_and_empty_payload() {
    let mut mixer = GroupAudioMixer::new();
    assert!(!mixer.push_packet(0, 1, 0, vec![1]));
    assert!(!mixer.push_packet(10, 1, 0, Vec::new()));
    assert_eq!(mixer.member_count(), 0);
}

#[test]
fn refuses_to_mix_undersized_output() {
    let mut mixer = GroupAudioMixer::new();
    assert_eq!(mixer.mix_frame(&mut [0i16; FRAME_SAMPLES - 1]), 0);
}
