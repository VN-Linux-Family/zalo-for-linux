use super::*;

#[test]
fn opus_voice_frame_encodes_and_decodes_at_call_sample_rate() {
    let pcm = vec![0i16; 320];
    let mut encoder = OpusEncoder::new(16_000, 1, 32_000).expect("create Opus encoder");
    let mut packet = [0u8; 1500];
    let encoded_len = encoder
        .encode(&pcm, &mut packet)
        .expect("encode Opus frame");
    assert!(encoded_len > 0 && encoded_len < packet.len());
    let mut decoder = OpusDecoder::new(16_000, 1).expect("create Opus decoder");
    let mut decoded_pcm = vec![0i16; 640];
    let samples = decoder
        .decode(&packet[..encoded_len], &mut decoded_pcm, false)
        .expect("decode Opus frame");
    assert_eq!(samples, 320);
}

#[test]
fn empty_packet_uses_plc_and_fec_path_is_available() {
    let mut encoder = OpusEncoder::new(16_000, 1, 32_000).unwrap();
    let mut packet = [0u8; 1500];
    let len = encoder.encode(&vec![1200i16; 320], &mut packet).unwrap();
    let mut decoder = OpusDecoder::new(16_000, 1).unwrap();
    let mut output = [0i16; 320];
    assert_eq!(
        decoder.decode(&packet[..len], &mut output, false).unwrap(),
        320
    );
    assert_eq!(decoder.decode(&[], &mut output, false).unwrap(), 320);
    assert_eq!(
        decoder.decode(&packet[..len], &mut output, true).unwrap(),
        320
    );
}
