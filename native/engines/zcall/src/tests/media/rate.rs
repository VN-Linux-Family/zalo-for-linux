use super::*;
use serde_json::json;

#[test]
fn starts_at_server_startup_bitrate_or_nearest_lower_rung() {
    let controller = SendRateController::new(&json!({"startupBitrate":500}), 10);
    assert_eq!(controller.profile().rung, 4);
    assert_eq!(controller.profile().width, 640);
    assert_eq!(controller.profile().height, 360);
}

#[test]
fn uses_default_ladder_when_server_profiles_do_not_fit() {
    let controller = SendRateController::new(&json!({"maxWidth":100,"maxHeight":100}), 10);
    assert_ne!(controller.profile().bitrate, 0);
    assert_eq!(controller.rung_count(), 1);
}

#[test]
fn steps_down_on_loss_and_requires_four_clean_feedback_windows_to_step_up() {
    let mut controller = SendRateController::new(&json!({"startupBitrate":500}), 10);
    let twcc = TwccFeedback {
        sender_ssrc: 20,
        media_ssrc: 10,
        base: 1,
        count: 20,
        reference_time: 100,
        feedback_count: 1,
        status: vec![0; 20],
        delta_q: Vec::new(),
    };
    controller.on_feedback(&[twcc], &[], 3000);
    assert_eq!(controller.tick(3000, true, None).unwrap().rung, 3);
    for second in 1u64..=20 {
        controller.on_sent(100 + second as u16, 3000 + second * 1024);
        let packet = TwccFeedback {
            sender_ssrc: 20,
            media_ssrc: 10,
            base: 100 + second as u16,
            count: 10,
            reference_time: 100 + (second * 16) as u32,
            feedback_count: second as u8,
            status: vec![1; 10],
            delta_q: vec![0; 10],
        };
        controller.on_feedback(&[packet], &[], 3000 + second * 1024);
        let changed = controller.tick(3000 + second * 1024, true, None);
        if second < 20 {
            assert!(changed.is_none());
        } else {
            assert_eq!(changed.unwrap().rung, 4);
        }
    }
}

#[test]
fn ignores_report_blocks_for_another_ssrc() {
    let mut controller = SendRateController::new(&json!({}), 10);
    controller.on_feedback(
        &[],
        &[ReportBlock {
            ssrc: 99,
            fraction_lost: 255,
            ..ReportBlock::default()
        }],
        1000,
    );
    assert!(controller.tick(1000, true, None).is_none());
}
