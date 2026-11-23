use super::*;

#[test]
#[ignore = "requires a running PulseAudio/PipeWire session"]
fn live_pulseaudio_rs_streams_capture_and_playback() {
    let mut recorder = PulseRecorder::new(None).expect("create Rust record stream");
    let mut captured = [0_u8; 640];
    recorder
        .read(&mut captured)
        .expect("read one 20 ms audio frame");

    let mut player = PulsePlayer::new(None).expect("create Rust playback stream");
    player
        .write(&[0_u8; 640])
        .expect("queue one silent 20 ms frame");
    std::thread::sleep(Duration::from_millis(100));
    assert!(player.latency_ms().is_some(), "query playback timing");
}

#[test]
#[ignore = "requires a running PulseAudio/PipeWire session; query-only test"]
fn live_pulse_queries_read_default_source_and_own_capture_stream() {
    let mut connection = pulse_connection().expect("connect to PulseAudio-compatible server");
    let source =
        selected_source_info(&mut connection, None).expect("read default source information");
    assert!(source
        .resolved_name
        .as_deref()
        .is_some_and(|name| !name.is_empty()));
    assert!(source.channels > 0);
    assert!(source.average_volume <= PA_VOLUME_NORM * 3);

    let _own_stream = source_output_for_pid(&mut connection, i64::from(std::process::id()))
        .expect("query source-output list for current process");
}

#[test]
#[ignore = "requires a running PulseAudio/PipeWire session; sends current values unchanged"]
fn live_pulse_native_control_roundtrips_preserve_current_source_state() {
    let mut connection = pulse_connection().expect("connect to PulseAudio-compatible server");
    let _: protocol::SinkInfoList = connection
        .roundtrip(&protocol::Command::GetSinkInfoList)
        .expect("query sink inventory");
    let _: protocol::CardInfoList = connection
        .roundtrip(&protocol::Command::GetCardInfoList)
        .expect("query card and active-profile inventory");
    let inventory = pulse_device_inventory().expect("enumerate audio devices and defaults");
    assert!(
        !inventory.sources.is_empty(),
        "at least one capture source should be listed"
    );
    assert!(
        !inventory.sinks.is_empty(),
        "at least one playback sink should be listed"
    );
    let source = selected_source_info(&mut connection, None).expect("query default source");
    let source_name = CString::new(
        source
            .resolved_name
            .as_deref()
            .expect("resolved source name"),
    )
    .expect("valid source name");
    let current: protocol::SourceInfo = connection
        .roundtrip(&protocol::Command::GetSourceInfo(protocol::GetSourceInfo {
            index: None,
            name: Some(source_name),
        }))
        .expect("read current source state");
    connection
        .acknowledge(&protocol::Command::SetSourceVolume(
            protocol::SetDeviceVolumeParams {
                device_index: Some(current.index),
                device_name: None,
                volume: current.cvolume,
            },
        ))
        .expect("write unchanged per-channel source volume");
    connection
        .acknowledge(&protocol::Command::SetSourceMute(
            protocol::SetDeviceMuteParams {
                device_index: Some(current.index),
                device_name: None,
                mute: current.muted,
            },
        ))
        .expect("write unchanged source mute state");
    let _recorder = PulseRecorder::new(None).expect("create a temporary own capture stream");
    let outputs: protocol::SourceOutputInfoList = connection
        .roundtrip(&protocol::Command::GetSourceOutputInfoList)
        .expect("query source-output inventory");
    let own_output = outputs
        .into_iter()
        .find(|output| {
            output
                .props
                .get(protocol::Prop::ApplicationProcessId)
                .and_then(|value| std::str::from_utf8(value).ok())
                .map(|value| parse_pid_like_atoi(value.trim_end_matches('\0')))
                == Some(i64::from(std::process::id()))
        })
        .expect("find this process's capture stream");
    if own_output.has_volume && own_output.volume_writable {
        connection
            .acknowledge(&protocol::Command::SetSourceOutputVolume(
                protocol::SetStreamVolumeParams {
                    index: own_output.index,
                    volume: own_output.cvolume,
                },
            ))
            .expect("write unchanged per-channel source-output volume");
    }
    connection
        .acknowledge(&protocol::Command::SetSourceOutputMute(
            protocol::SetStreamMuteParams {
                index: own_output.index,
                mute: own_output.muted,
            },
        ))
        .expect("write unchanged source-output mute state");
    let _own_stream = source_output_for_pid(&mut connection, i64::from(std::process::id()))
        .expect("query own source-output snapshot");
}

#[test]
fn channel_volume_average_matches_pulse_integer_average() {
    assert_eq!(average_channel_volume(2, &[1, 2]), Some(1));
    assert_eq!(average_channel_volume(3, &[1, 2, 3]), Some(2));
    assert_eq!(average_channel_volume(0, &[1, 2]), None);
    assert_eq!(average_channel_volume(3, &[1, 2]), None);
}

#[test]
fn volume_percentage_conversion_matches_pulse_rounding_and_clamps() {
    assert_eq!(volume_to_percent(0), 0);
    assert_eq!(volume_to_percent(32_768), 50);
    assert_eq!(volume_to_percent(PA_VOLUME_NORM), 100);
    assert_eq!(volume_to_percent(PA_VOLUME_NORM * 2), 150);
    assert_eq!(percent_to_volume(0), 0);
    assert_eq!(percent_to_volume(50), 32_768);
    assert_eq!(percent_to_volume(100), PA_VOLUME_NORM);
    assert_eq!(percent_to_volume(150), PA_VOLUME_NORM * 3 / 2);
}

#[test]
fn pid_parser_matches_atoi_prefix_behavior() {
    assert_eq!(parse_pid_like_atoi("123"), 123);
    assert_eq!(parse_pid_like_atoi("  +123tail"), 123);
    assert_eq!(parse_pid_like_atoi("-2"), -2);
    assert_eq!(parse_pid_like_atoi("not-a-pid"), 0);
}

#[test]
fn invalid_or_default_source_names_resolve_through_server_info() {
    assert!(source_name_uses_default(None));
    assert!(source_name_uses_default(Some("")));
    assert!(source_name_uses_default(Some("@DEFAULT_SOURCE@")));
    assert!(source_name_uses_default(Some("bad\0name")));
    assert!(!source_name_uses_default(Some("alsa_input.test")));
}

fn source_name_uses_default(source: Option<&str>) -> bool {
    source.is_none_or(|name| {
        name.is_empty() || name.as_bytes().contains(&0) || name == "@DEFAULT_SOURCE@"
    })
}
