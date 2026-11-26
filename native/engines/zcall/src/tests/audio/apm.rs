use super::*;

#[test]
fn processing_preserves_frame_shape_and_handles_render_reference() {
    let apm = Apm::new(16_000, 1, true, true, false).expect("create Sonora processor");
    let render = vec![500i16; 320];
    apm.process_render(&render);
    let mut capture = vec![500i16; 320];
    apm.process_capture(&mut capture);
    assert_eq!(capture.len(), 320);
}

#[test]
fn processing_handles_partial_tail_without_panicking() {
    let apm = Apm::new(16_000, 1, false, true, true).unwrap();
    let mut capture = vec![100i16; 321];
    let tail = capture[320];
    apm.process_capture(&mut capture);
    assert_eq!(capture[320], tail);
}
