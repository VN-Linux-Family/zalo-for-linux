use super::*;

#[test]
fn test_silence() {
    let pcm = vec![0i16; 320];
    assert_eq!(compute_rfc6464_level(&pcm), 127);
}

#[test]
fn test_full_scale() {
    let pcm = vec![32767i16; 320];
    assert_eq!(compute_rfc6464_level(&pcm), 0);
}
