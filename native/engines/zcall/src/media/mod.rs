//! Media transport backend implementing ZRTP over UDP with SRTP and Opus audio.

pub mod rate;

use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::audio::backend::{
    get_source_volume, probe_audio_hardware, pulse_get_source_info, pulse_normalize_own_stream,
    set_source_mute, set_source_volume,
};
use crate::audio::{
    clip_guard::{apply_soft_limiter, ClipGuard},
    compute_rfc6464_level,
    group::GroupAudioMixer,
    leveler::MicLeveler,
    Apm, JitterBuffer, OpusEncoder, PulsePlayer, PulseRecorder, FRAME_BYTES, FRAME_SAMPLES,
    SAMPLE_RATE,
};
use crate::crypto::srtp::{
    hmac_sha1, master_from_sess_id, parse_rtp_header_len, protect_rtp, session_keys, sha1,
    unprotect_rtp, unprotect_rtp_with_profile, SrtpKeys,
};
use crate::media::rate::{RateTotals, SendRateController, VideoProfile};
use crate::protocol::group::{
    build_group_init_zrtp_packet, build_group_video_quality_config,
    build_group_video_quality_request, build_share_init_zrtp_packet, build_share_zavi_ping_packet,
    group_video_plan as parse_group_video_plan, parse_room_updated, select_group_video_quality,
    GroupInitLayout, GroupVideoLayer, GroupVideoPlan, GroupVideoQualityRequest,
};
use crate::protocol::rtcp::{build_nack_many, NackTracker, RtcpFeedback};
use crate::protocol::rtp::{
    build_audio_rtp_header, build_group_audio_rtp_header, build_group_video_rtp_header,
    parse_rfc6464_audio_level,
};

use crate::protocol::zrtp::{
    build_change_address_packet, build_end_call_packet, build_init_zrtp_packet, build_ping_packet,
    build_req_forward_packet, build_zavi_ping_packet, msg_types, p2p, parse_server_reply,
    wrap_media_packet,
};

use crate::video::{packetize_frame, VideoAssembler, VideoFrame, VIDEO_PT_INTER, VIDEO_PT_KEY};

const GROUP_SRTP_PROFILE_DISCOVERY_LIMIT: u8 = 4;

struct PlayoutWriteContext<'a> {
    player: &'a mut Option<PulsePlayer>,
    apm: Option<&'a Apm>,
    far_active: &'a AtomicBool,
    last_far_speech: &'a mut Instant,
    speaker_fault_reported: &'a mut bool,
    media_events: &'a Mutex<VecDeque<GroupMediaEvent>>,
    last_reopen: &'a mut Instant,
    write_block_ms: &'a mut u64,
    write_count: &'a mut u32,
    max_write_ms: &'a mut u64,
    non_blocking_streak: &'a mut u32,
}

impl PlayoutWriteContext<'_> {
    fn write_frame(
        &mut self,
        pcm_out: &[i16; FRAME_SAMPLES],
        silence: &[u8],
        has_frame: bool,
        muted: bool,
    ) -> bool {
        let Some(output) = self.player.as_mut() else {
            return false;
        };
        let started = Instant::now();
        let result = if muted || !has_frame {
            if !muted {
                self.far_active.store(false, Ordering::Relaxed);
            }
            output.write(silence)
        } else {
            if let Some(apm) = self.apm {
                apm.process_render(pcm_out);
            }
            let sum_sq: i64 = pcm_out
                .iter()
                .map(|&sample| i64::from(sample) * i64::from(sample))
                .sum();
            let mean_sq = sum_sq / i64::try_from(FRAME_SAMPLES).unwrap_or(i64::MAX);
            if mean_sq > 100_000 {
                *self.last_far_speech = Instant::now();
                self.far_active.store(true, Ordering::Relaxed);
            }
            output.write(bytemuck::cast_slice(pcm_out))
        };
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(()) => {
                *self.write_block_ms += elapsed_ms;
                *self.write_count = self.write_count.saturating_add(1);
                *self.max_write_ms = (*self.max_write_ms).max(elapsed_ms);
                if elapsed_ms == 0 {
                    *self.non_blocking_streak = self.non_blocking_streak.saturating_add(1);
                } else {
                    *self.non_blocking_streak = 0;
                }
                true
            }
            Err(error) => {
                crate::zcall_log!("[zcall-rust] Pulse playback write failed; reopening: {error}");
                if !*self.speaker_fault_reported {
                    if let Ok(mut events) = self.media_events.lock() {
                        events.push_back(GroupMediaEvent::AudioFault("speaker".into()));
                    }
                    *self.speaker_fault_reported = true;
                }
                *self.player = None;
                *self.last_reopen = Instant::now();
                false
            }
        }
    }
}

fn log_playout_diagnostics(
    last_diag: &mut Instant,
    jitter_buffer: Option<&Arc<Mutex<JitterBuffer>>>,
    player: Option<&PulsePlayer>,
    mic_volume: u8,
    write_block_ms: &mut u64,
    write_count: &mut u32,
    max_write_ms: &mut u64,
) {
    if last_diag.elapsed() < Duration::from_secs(5) {
        return;
    }
    let jitter_diag = jitter_buffer
        .and_then(|jitter| {
            jitter.lock().ok().map(|jitter| {
                let stats = jitter.stats();
                format!(
                    "rx={} play={} q={}/{} plc={} late_drop={} under={}",
                    stats.rx_packets,
                    stats.played_frames,
                    jitter.queue_len(),
                    jitter.min_target(),
                    stats.plc_frames,
                    stats.late_dropped,
                    stats.underrun_count
                )
            })
        })
        .unwrap_or_else(|| "group mixer".into());
    let average_write = if *write_count > 0 {
        *write_block_ms / u64::from(*write_count)
    } else {
        0
    };
    let latency = player.and_then(PulsePlayer::latency_ms).unwrap_or_default();
    crate::zcall_log!(
        "[zcall-rust] playout: [{jitter_diag} write={}ms max={}ms pa_lat={}ms] mic={}% ",
        average_write,
        *max_write_ms,
        latency,
        mic_volume
    );
    *last_diag = Instant::now();
    *write_block_ms = 0;
    *write_count = 0;
    *max_write_ms = 0;
}

fn open_playback_device(
    speaker_device: Option<&str>,
    media_events: &Mutex<VecDeque<GroupMediaEvent>>,
    speaker_fault_reported: &mut bool,
) -> Option<PulsePlayer> {
    match PulsePlayer::new(speaker_device) {
        Ok(player) => Some(player),
        Err(error) => {
            crate::zcall_log!("[zcall-rust] Pulse playback open failed: {error}");
            if !*speaker_fault_reported {
                if let Ok(mut events) = media_events.lock() {
                    events.push_back(GroupMediaEvent::AudioFault("speaker".into()));
                }
                *speaker_fault_reported = true;
            }
            None
        }
    }
}

fn reopen_playback_if_needed(
    player: &mut Option<PulsePlayer>,
    speaker_device: Option<&str>,
    last_reopen: &mut Instant,
    non_blocking_streak: &mut u32,
) {
    if player.is_none() && last_reopen.elapsed() >= Duration::from_millis(500) {
        *last_reopen = Instant::now();
        if let Ok(reopened) = PulsePlayer::new(speaker_device) {
            crate::zcall_log!("[zcall-rust] Pulse playback stream recovered");
            *player = Some(reopened);
            *non_blocking_streak = 0;
        }
    }
}

fn playout_has_frame(
    params: &Mutex<Option<MediaSessionParams>>,
    jitter_buffer: Option<&Arc<Mutex<JitterBuffer>>>,
    group_audio: &Mutex<GroupAudioMixer>,
    pcm_out: &mut [i16; FRAME_SAMPLES],
) -> bool {
    let is_group = params
        .lock()
        .ok()
        .and_then(|params| {
            params
                .as_ref()
                .map(|params| params.kind == MediaKind::Group)
        })
        .unwrap_or(false);
    if is_group {
        group_audio
            .lock()
            .is_ok_and(|mut mixer| mixer.mix_frame(pcm_out) > 0)
    } else {
        jitter_buffer
            .and_then(|jitter| {
                jitter
                    .lock()
                    .ok()
                    .map(|mut jitter| jitter.pop_frame(pcm_out))
            })
            .unwrap_or(false)
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_playout_worker(
    running: Arc<AtomicBool>,
    audio_enabled: Arc<AtomicBool>,
    devices: Arc<Mutex<(Option<String>, Option<String>)>>,
    device_generation: Arc<AtomicU64>,
    speaker_off: Arc<AtomicBool>,
    params: Arc<Mutex<Option<MediaSessionParams>>>,
    jitter_buffer: Option<Arc<Mutex<JitterBuffer>>>,
    group_audio: Arc<Mutex<GroupAudioMixer>>,
    apm: Option<Arc<Apm>>,
    far_active: Arc<AtomicBool>,
    mic_volume: Arc<AtomicU8>,
    media_events: Arc<Mutex<VecDeque<GroupMediaEvent>>>,
) {
    let _ = thread::Builder::new()
        .name("zcall-playout".into())
        .spawn(move || {
            let mut speaker_device: Option<String> = None;
            let mut player: Option<PulsePlayer> = None;
            let mut speaker_fault_reported = false;
            let mut active_generation = u64::MAX;
            let mut last_reopen = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
            let mut last_far_speech = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
            let mut last_diag = Instant::now();
            let mut write_block_ms = 0u64;
            let mut write_count = 0u32;
            let mut max_write_ms = 0u64;
            let mut non_blocking_streak = 0u32;
            let mut pcm_out = [0i16; FRAME_SAMPLES];
            let silence = vec![0u8; FRAME_BYTES];
            let tick = Duration::from_millis(20);

            while running.load(Ordering::Relaxed) {
                if !audio_enabled.load(Ordering::Acquire) {
                    player = None;
                    active_generation = u64::MAX;
                    far_active.store(false, Ordering::Relaxed);
                    thread::sleep(tick);
                    continue;
                }
                let loop_start = Instant::now();
                let generation = device_generation.load(Ordering::Acquire);
                if generation != active_generation {
                    speaker_device = devices.lock().map_or(None, |devices| devices.1.clone());
                    player = open_playback_device(
                        speaker_device.as_deref(),
                        &media_events,
                        &mut speaker_fault_reported,
                    );
                    active_generation = generation;
                    non_blocking_streak = 0;
                    last_reopen = Instant::now();
                }

                reopen_playback_if_needed(
                    &mut player,
                    speaker_device.as_deref(),
                    &mut last_reopen,
                    &mut non_blocking_streak,
                );

                let has_frame =
                    playout_has_frame(&params, jitter_buffer.as_ref(), &group_audio, &mut pcm_out);

                let muted = speaker_off.load(Ordering::Relaxed);
                let write_ok = PlayoutWriteContext {
                    player: &mut player,
                    apm: apm.as_deref(),
                    far_active: &far_active,
                    last_far_speech: &mut last_far_speech,
                    speaker_fault_reported: &mut speaker_fault_reported,
                    media_events: &media_events,
                    last_reopen: &mut last_reopen,
                    write_block_ms: &mut write_block_ms,
                    write_count: &mut write_count,
                    max_write_ms: &mut max_write_ms,
                    non_blocking_streak: &mut non_blocking_streak,
                }
                .write_frame(&pcm_out, &silence, has_frame, muted);

                if last_far_speech.elapsed() >= Duration::from_millis(300) {
                    far_active.store(false, Ordering::Relaxed);
                }

                log_playout_diagnostics(
                    &mut last_diag,
                    jitter_buffer.as_ref(),
                    player.as_ref(),
                    mic_volume.load(Ordering::Relaxed).min(100),
                    &mut write_block_ms,
                    &mut write_count,
                    &mut max_write_ms,
                );

                // pa_simple_write follows the device's hardware clock. Sleep only
                // when no device is available or a virtual sink returns immediately.
                if !write_ok || non_blocking_streak > 6 {
                    let elapsed = loop_start.elapsed();
                    if elapsed < tick {
                        thread::sleep(tick.checked_sub(elapsed).unwrap());
                    }
                }
            }
        });
}

fn ring_ping_interval() -> Duration {
    let value = std::env::var("ZCALL_PING_MS").ok();
    ring_ping_interval_from(value.as_deref())
}

fn ring_ping_interval_from(value: Option<&str>) -> Duration {
    let millis = value
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (20..=60_000).contains(value))
        .unwrap_or(6_000);
    Duration::from_millis(millis)
}

fn handle_group_share_incoming(
    share_state: &Arc<Mutex<Option<GroupSharePeer>>>,
    events: &Arc<Mutex<VecDeque<GroupMediaEvent>>>,
    active: &Arc<AtomicBool>,
    packet: &[u8],
    from: SocketAddr,
) -> bool {
    let server = share_state
        .lock()
        .ok()
        .and_then(|peer| peer.as_ref().map(|peer| peer.server));
    if server != Some(from) {
        return false;
    }
    if let Some(reply) = parse_server_reply(packet) {
        if reply.cmd == crate::protocol::zrtp::commands::END_CALL {
            let is_share = share_state
                .lock()
                .ok()
                .and_then(|peer| peer.as_ref().map(|peer| peer.uid == reply.uid))
                .unwrap_or(false);
            if is_share {
                if let Ok(mut queue) = events.lock() {
                    queue.push_back(GroupMediaEvent::ShareFailed(
                        "group share peer ended by the SFU".into(),
                    ));
                }
                active.store(false, Ordering::Release);
            }
        } else if matches!(
            reply.cmd,
            crate::protocol::zrtp::commands::INIT_CALL_CALLER
                | crate::protocol::zrtp::commands::INIT_CALL_CALLEE
        ) {
            if let Ok(mut guard) = share_state.lock() {
                if let Some(peer) = guard.as_mut() {
                    if peer.uid != reply.uid || peer.token != 0 {
                        return true;
                    }
                    if reply.result == Some(0) && reply.server_token.is_some_and(|token| token != 0)
                    {
                        peer.token = reply.server_token.unwrap_or(0);
                        peer.started_at = Some(Instant::now());
                        peer.last_ping =
                            Instant::now().checked_sub(Duration::from_secs(6)).unwrap();
                        if let Ok(mut queue) = events.lock() {
                            queue.push_back(GroupMediaEvent::ShareReady);
                        }
                    } else {
                        if let Ok(mut queue) = events.lock() {
                            queue.push_back(GroupMediaEvent::ShareFailed(format!(
                                "share InitZRTP refused: {:?}",
                                reply.result
                            )));
                        }
                        peer.active.store(false, Ordering::Release);
                    }
                }
            }
        }
    } else if packet.first() == Some(&msg_types::VIDEO_FEC) && packet.len() > 5 {
        let target_uid = share_state
            .lock()
            .ok()
            .and_then(|peer| peer.as_ref().map(|peer| peer.uid));
        if target_uid.is_some_and(|uid| rtcp_requests_keyframe(&packet[5..], uid)) {
            if let Ok(mut queue) = events.lock() {
                queue.push_back(GroupMediaEvent::ShareKeyframeRequested);
            }
        }
    }
    true
}

fn collect_group_share_outbound(
    share_state: &Arc<Mutex<Option<GroupSharePeer>>>,
    events: &Arc<Mutex<VecDeque<GroupMediaEvent>>>,
    init_packet: &[u8],
    init_tries: &mut u8,
    last_init: &mut Instant,
) -> Vec<(Vec<u8>, SocketAddr)> {
    let mut outbound = Vec::new();
    if let Ok(mut guard) = share_state.lock() {
        if let Some(peer) = guard.as_mut() {
            if peer.token == 0 && last_init.elapsed() >= Duration::from_secs(1) {
                if *init_tries >= 3 {
                    if let Ok(mut queue) = events.lock() {
                        queue.push_back(GroupMediaEvent::ShareFailed(
                            "group share peer received no InitZRTP response".into(),
                        ));
                    }
                    peer.active.store(false, Ordering::Release);
                } else {
                    *init_tries += 1;
                    *last_init = Instant::now();
                    outbound.push((init_packet.to_vec(), peer.server));
                }
            } else if peer.token != 0 && peer.last_ping.elapsed() >= Duration::from_secs(6) {
                let ping = build_share_zavi_ping_packet(
                    peer.uid,
                    peer.token,
                    peer.control_seq,
                    &peer.sess_id,
                );
                peer.control_seq = peer.control_seq.wrapping_add(1);
                outbound.push((ping, peer.server));
                if peer.width > 0 && peer.height > 0 {
                    let layer = GroupVideoLayer {
                        id: 0,
                        bitrate_kbps: peer.max_shr_screen_br,
                        width: peer.width,
                        height: peer.height,
                    };
                    let config = build_group_video_quality_config(
                        peer.uid,
                        peer.token,
                        peer.control_seq,
                        &[layer],
                    );
                    peer.control_seq = peer.control_seq.wrapping_add(1);
                    outbound.push((config, peer.server));
                }
                peer.last_ping = Instant::now();
            }
            for _ in 0..8 {
                let Some(packet) = peer.queue.pop_front() else {
                    break;
                };
                outbound.push((packet, peer.server));
            }
        }
    }
    outbound
}

fn group_share_worker(
    socket: &Arc<UdpSocket>,
    share_state: &Arc<Mutex<Option<GroupSharePeer>>>,
    events: &Arc<Mutex<VecDeque<GroupMediaEvent>>>,
    running: &Arc<AtomicBool>,
    active: &Arc<AtomicBool>,
    init_packet: &[u8],
) {
    let mut init_tries = 1u8;
    let mut last_init = Instant::now();
    let mut buf = [0u8; 4096];
    while running.load(Ordering::Acquire) && active.load(Ordering::Acquire) {
        match socket.recv_from(&mut buf) {
            Ok((len, from)) if len > 0 => {
                if !handle_group_share_incoming(share_state, events, active, &buf[..len], from) {
                    continue;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => {
                if let Ok(mut queue) = events.lock() {
                    queue.push_back(GroupMediaEvent::ShareFailed(format!(
                        "group share socket receive failed: {error}"
                    )));
                }
                break;
            }
            _ => {}
        }
        for (packet, server) in collect_group_share_outbound(
            share_state,
            events,
            init_packet,
            &mut init_tries,
            &mut last_init,
        ) {
            let _ = socket.send_to(&packet, server);
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn rtcp_requests_keyframe(packet: &[u8], target_uid: u32) -> bool {
    let mut offset = 0usize;
    while offset + 12 <= packet.len() {
        if packet[offset] >> 6 != 2 {
            return false;
        }
        let length =
            (u16::from_be_bytes([packet[offset + 2], packet[offset + 3]]) as usize + 1) * 4;
        if length < 12 || offset + length > packet.len() {
            return false;
        }
        let format = packet[offset] & 0x1f;
        if packet[offset + 1] == 206 && format == 1 {
            let uid =
                u32::from_be_bytes(packet[offset + 8..offset + 12].try_into().unwrap_or([0; 4]));
            if uid == target_uid {
                return true;
            }
        } else if packet[offset + 1] == 206 && format == 4 && length >= 16 {
            let uid = u32::from_be_bytes(
                packet[offset + 12..offset + 16]
                    .try_into()
                    .unwrap_or([0; 4]),
            );
            if uid == target_uid {
                return true;
            }
        }
        offset += length;
    }
    false
}

fn group_event_from_reply(
    reply: &crate::protocol::zrtp::ServerReply,
    local_uid: u32,
) -> Option<GroupMediaEvent> {
    use crate::protocol::zrtp::commands;

    match reply.cmd {
        commands::ROOM_UPDATED if reply.uid == local_uid => {
            let mut members = parse_room_updated(&reply.payload)?;
            members.retain(|uid| *uid != 0 && *uid != local_uid);
            Some(GroupMediaEvent::Roster(members))
        }
        commands::END_CALL if reply.uid == local_uid && matches!(reply.sub_cmd, 1 | 2) => {
            Some(GroupMediaEvent::PeerEnd(reply.sub_cmd))
        }
        commands::END_CALL if reply.uid != 0 && reply.uid != local_uid => {
            Some(GroupMediaEvent::MemberLeft(reply.uid, reply.sub_cmd))
        }
        _ => None,
    }
}

/// Authenticated RFC 3711 rollover tracking with recovery for a peer that
/// restarts its RTP sequence/ROC after a network handover.
#[derive(Debug, Clone, Default)]
struct RocTracker {
    last: Option<u16>,
    roc: u32,
}

impl RocTracker {
    const fn guess(&self, seq: u16) -> u32 {
        let Some(last) = self.last else {
            return self.roc;
        };
        if seq < last && last.wrapping_sub(seq) > 0x8000 {
            self.roc.wrapping_add(1)
        } else if seq > last && seq.wrapping_sub(last) > 0x8000 && self.roc > 0 {
            self.roc - 1
        } else {
            self.roc
        }
    }

    fn candidates(&self, seq: u16) -> Vec<u32> {
        let guessed = self.guess(seq);
        let mut values = vec![guessed, 0, guessed.wrapping_add(1)];
        if guessed > 0 {
            values.push(guessed - 1);
        }
        let mut seen = HashSet::new();
        values.retain(|value| seen.insert(*value));
        values
    }

    /// Update only on forward movement or a large backwards jump. Call this
    /// only after SRTP authentication succeeds.
    fn update(&mut self, seq: u16, roc: u32) {
        let Some(last) = self.last else {
            self.last = Some(seq);
            self.roc = roc;
            return;
        };
        let previous = (u64::from(self.roc) << 16) | u64::from(last);
        let next = (u64::from(roc) << 16) | u64::from(seq);
        if next > previous || previous - next > 100 {
            self.last = Some(seq);
            self.roc = roc;
        }
    }
}

// Diagnostic-only compatibility probe. It never accepts/decrypts a packet;
// it reports whether the live tag matches common RTP header/tag/ROC layouts.
fn probe_srtp_auth_layout(rtp: &[u8], keys: &SrtpKeys) -> String {
    if rtp.len() < 16 {
        return "packet-too-short".into();
    }
    let Some(header_len) = parse_rtp_header_len(rtp) else {
        return "invalid-rtp-header".into();
    };
    // Diagnostic-only probe for the SFU's `srtpNoSeq` mode. Try the standard
    // SRTP authenticated region first, then explicitly test whether the peer
    // excludes RTP sequence bytes, zeroes them, or omits ROC. A match here is
    // reported only; it is never accepted as media until a matching strict
    // unprotect path and a regression test are added.
    let header = &rtp[..header_len];
    let mut header_without_seq = Vec::with_capacity(header_len.saturating_sub(2));
    header_without_seq.extend_from_slice(&header[..2]);
    header_without_seq.extend_from_slice(&header[4..]);
    let mut header_zero_seq = header.to_vec();
    header_zero_seq[2..4].fill(0);

    for tag_len in (4usize..=20).rev() {
        if rtp.len() < header_len + tag_len {
            continue;
        }
        let tag_start = rtp.len() - tag_len;
        let tag = &rtp[tag_start..];
        for mki_len in 0..=16 {
            if tag_start < header_len + mki_len {
                continue;
            }
            let body_end = tag_start - mki_len;
            let body = &rtp[header_len..body_end];
            for roc in [Some(0u32), Some(1), Some(2), Some(3), None] {
                let roc_big_endian = roc.map(u32::to_be_bytes);
                let roc_little_endian = roc.map(u32::to_le_bytes);
                let check = |mode: &str, digest: [u8; 20]| -> Option<String> {
                    (digest[..tag_len] == *tag).then(|| {
                        format!(
                            "match:mode={mode}:roc={}:header={header_len}:mki={mki_len}:tag={tag_len}",
                            roc.map_or_else(|| "none".into(), |value| value.to_string())
                        )
                    })
                };
                if let Some(roc_bytes) = roc_big_endian.as_ref() {
                    if let Some(result) = check(
                        "standard-be",
                        hmac_sha1(&keys.auth, &[header, body, roc_bytes]),
                    ) {
                        return result;
                    }
                    if let Some(result) = check(
                        "without-rtp-seq-be",
                        hmac_sha1(&keys.auth, &[&header_without_seq, body, roc_bytes]),
                    ) {
                        return result;
                    }
                    if let Some(result) = check(
                        "zero-rtp-seq-be",
                        hmac_sha1(&keys.auth, &[&header_zero_seq, body, roc_bytes]),
                    ) {
                        return result;
                    }
                    if let Some(result) = check(
                        "body-plus-roc-be",
                        hmac_sha1(&keys.auth, &[body, roc_bytes]),
                    ) {
                        return result;
                    }
                }
                if let Some(roc_bytes) = roc_little_endian.as_ref() {
                    if let Some(result) = check(
                        "standard-le",
                        hmac_sha1(&keys.auth, &[header, body, roc_bytes]),
                    ) {
                        return result;
                    }
                }
                if roc.is_none() {
                    if let Some(result) =
                        check("without-roc", hmac_sha1(&keys.auth, &[header, body]))
                    {
                        return result;
                    }
                    if let Some(result) = check(
                        "without-rtp-seq-and-roc",
                        hmac_sha1(&keys.auth, &[&header_without_seq, body]),
                    ) {
                        return result;
                    }
                    if let Some(result) = check(
                        "zero-rtp-seq-and-roc",
                        hmac_sha1(&keys.auth, &[&header_zero_seq, body]),
                    ) {
                        return result;
                    }
                    if let Some(result) = check("body-only", hmac_sha1(&keys.auth, &[body])) {
                        return result;
                    }
                }
            }
        }
    }
    format!("no-known-auth-layout:parsed_header={header_len}")
}

fn unprotect_with_roc_recovery(
    rtp: &[u8],
    keys: &SrtpKeys,
    tracker: &mut RocTracker,
) -> Option<(crate::crypto::srtp::UnprotectedRtp, u32)> {
    if rtp.len() < 4 {
        return None;
    }
    let seq = u16::from_be_bytes([rtp[2], rtp[3]]);
    for roc in tracker.candidates(seq) {
        if let Some(clear) = unprotect_rtp(rtp, keys, roc) {
            tracker.update(seq, roc);
            return Some((clear, roc));
        }
    }
    None
}

const fn group_camera_advertised(video_enabled: bool, camera_enabled: bool) -> bool {
    video_enabled && camera_enabled
}

fn looks_like_group_h264_payload(payload: &[u8]) -> bool {
    payload.starts_with(&[0, 0, 1])
        || payload.starts_with(&[0, 0, 0, 1])
        // H.264 FU-A indicator: NAL type 28 is the low five bits;
        // the NRI bits vary with the source stream (commonly 0x7c/0x5c).
        || (payload.len() >= 2
            && payload[0] & 0x1f == 28
            && payload[1] & 0x1f != 0
            && payload[1] & 0x1f <= 23)
}

fn unprotect_group_with_candidates(
    rtp: &[u8],
    primary: Option<&SrtpKeys>,
    fallback: Option<&SrtpKeys>,
    rocs: &[u32],
    profile: Option<(usize, usize)>,
) -> Option<(crate::crypto::srtp::UnprotectedRtp, u32, bool, usize, usize)> {
    // Keep the standard HMAC-SHA1-80/no-MKI profile first. RFC 3711 makes
    // both MKI and authentication-tag lengths configurable, so discover the
    // negotiated profile on the first authenticated group packet rather
    // than assuming a short hard-coded list. The caller caches the returned
    // profile for all subsequent audio/video packets in this session.
    const TAG_LENGTHS: [usize; 6] = [10, 8, 12, 4, 16, 20];
    for (candidate, is_fallback) in [(primary, false), (fallback, true)] {
        let Some(candidate) = candidate else {
            continue;
        };
        for roc in rocs.iter().copied() {
            if let Some((mki_len, tag_len)) = profile {
                if let Some(clear) =
                    unprotect_rtp_with_profile(rtp, candidate, roc, mki_len, tag_len)
                {
                    return Some((clear, roc, is_fallback, mki_len, tag_len));
                }
                continue;
            }
            for tag_len in TAG_LENGTHS {
                for mki_len in 0..=16 {
                    if let Some(clear) =
                        unprotect_rtp_with_profile(rtp, candidate, roc, mki_len, tag_len)
                    {
                        return Some((clear, roc, is_fallback, mki_len, tag_len));
                    }
                }
            }
        }
    }
    None
}

struct AudioDeduplicator {
    seen: HashSet<(u32, u32)>,
    order: VecDeque<(u32, u32)>,
    capacity: usize,
}

impl AudioDeduplicator {
    fn new(capacity: usize) -> Self {
        Self {
            seen: HashSet::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Return true for the first authenticated copy of this SSRC/extended seq.
    fn accept(&mut self, ssrc: u32, extended_seq: u32) -> bool {
        let key = (ssrc, extended_seq);
        if !self.seen.insert(key) {
            return false;
        }
        self.order.push_back(key);
        while self.order.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
        true
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.seen.len()
    }
}

fn stop_p2p_path(
    socket: &UdpSocket,
    selected_server: Option<(SocketAddr, u32)>,
    p2p_path: &Mutex<Option<SocketAddr>>,
    p2p_enabled: &AtomicBool,
    params: &MediaSessionParams,
    notify_peer: bool,
) -> bool {
    p2p_enabled.store(false, Ordering::Release);
    let old_path = p2p_path.lock().ok().and_then(|mut path| path.take());
    if notify_peer {
        for seq in 0..3u32 {
            if let Some(peer) = old_path {
                let signal = p2p::signal(params.role == MediaRole::Callee, params.call_id, 2, seq);
                let _ = socket.send_to(&signal, peer);
            }
            if let Some((server, token)) = selected_server {
                let request = build_req_forward_packet(params.uid, token, seq, 2, &[]);
                let _ = socket.send_to(&request, server);
            }
        }
    }
    old_path.is_some()
}

fn send_change_address(
    socket: &UdpSocket,
    selected_server: Option<(SocketAddr, u32)>,
    params: &MediaSessionParams,
    sequence: u32,
) {
    let Some((server, token)) = selected_server else {
        return;
    };
    let change = build_change_address_packet(params.uid, token, sequence, &params.sess_id);
    let _ = socket.send_to(&change, server);

    let ping = build_ping_packet(params.uid, token, sequence, &params.sess_id);
    let _ = socket.send_to(&ping, server);

    if params.kind != MediaKind::Group {
        let request = build_req_forward_packet(params.uid, token, 0, 8, &[4, 0, 0, 0, 0, 0, 0, 0]);
        let _ = socket.send_to(&request, server);
    }
}

fn local_ipv4_addresses() -> Vec<std::net::Ipv4Addr> {
    let Ok(addresses) = nix::ifaddrs::getifaddrs() else {
        return Vec::new();
    };

    addresses
        .filter_map(|interface| {
            interface
                .address
                .as_ref()
                .and_then(|address| address.as_sockaddr_in())
                .map(nix::sys::socket::SockaddrIn::ip)
        })
        .filter(|address| !address.is_loopback() && !address.is_unspecified())
        .collect()
}

fn local_ipv4_signature() -> String {
    let mut addresses = local_ipv4_addresses()
        .into_iter()
        .map(|address| address.to_string())
        .collect::<Vec<_>>();
    addresses.sort();
    addresses.dedup();
    addresses.join(",")
}

fn read_plain_rtp_payload(rtp: &[u8]) -> Option<(u16, u32, Vec<u8>)> {
    if rtp.len() < 12 || rtp[0] >> 6 != 2 {
        return None;
    }
    let header_len = parse_rtp_header_len(rtp)?;
    let mut end = rtp.len();
    if rtp[0] & 0x20 != 0 {
        let padding = *rtp.last()? as usize;
        if padding == 0 || padding > end.saturating_sub(header_len) {
            return None;
        }
        end -= padding;
    }
    if end <= header_len {
        return None;
    }
    Some((
        u16::from_be_bytes([rtp[2], rtp[3]]),
        u32::from_be_bytes([rtp[8], rtp[9], rtp[10], rtp[11]]),
        rtp[header_len..end].to_vec(),
    ))
}

fn accepts_relay_source(selected: Option<SocketAddr>, from: SocketAddr, is_group: bool) -> bool {
    // The legacy group receiver accepted SFU media/control datagrams without
    // pinning them to the InitZRTP source endpoint. Relays may use another
    // port or host for downlink; SRTP authenticates encrypted media. Preserve
    // strict endpoint pinning for one-to-one calls.
    selected == Some(from) || is_group
}

fn media_debug_reset() {
    if let Some(path) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("zcall-media-debug.log")))
    {
        let _ = std::fs::File::create(path);
    }
}

fn media_debug(message: &str) {
    if let Some(path) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("zcall-media-debug.log")))
    {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            use std::io::Write;
            let _ = writeln!(file, "{message}");
        }
    }
}

fn next_group_control_sequence(sequence: &AtomicU32) -> u32 {
    sequence.fetch_add(1, Ordering::Relaxed)
}

const fn media_receive_timed_out(last_rx_ms: u64, now_ms: u64) -> bool {
    last_rx_ms > 0 && now_ms.saturating_sub(last_rx_ms) >= MEDIA_TIMEOUT_MS
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_default(|duration| duration.as_millis() as u64)
}

pub struct CandidateServer {
    pub addr: SocketAddr,
    pub token: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupMediaEvent {
    Roster(Vec<u32>),
    MemberLeft(u32, u8),
    PeerEnd(u8),
    ShareReady,
    ShareKeyframeRequested,
    ShareFailed(String),
    MemberAudio(u32),
    MemberVideo(u32),
    MemberSpeaking(u32, bool),
    AudioFault(String),
    EarlyAudio(&'static str),
}

const SPEAK_DBOV: u8 = 50;
const SPEAK_WINDOW: usize = 10;
const SPEAK_ON_FRAMES: usize = 4;
const SPEAK_HOLD: Duration = Duration::from_millis(600);
const MEDIA_TIMEOUT_MS: u64 = 20_000;

const fn early_audio_phase(
    is_caller: bool,
    is_group: bool,
    audio_enabled: bool,
    authenticated_packets: u32,
) -> Option<&'static str> {
    if !is_caller || is_group || audio_enabled {
        return None;
    }
    match authenticated_packets {
        1 => Some("first"),
        50 => Some("connect"),
        _ => None,
    }
}

#[derive(Default)]
struct MemberSpeakingTracker {
    history: VecDeque<bool>,
    active: bool,
    last_loud: Option<Instant>,
}

impl MemberSpeakingTracker {
    fn observe(&mut self, level: u8, now: Instant) -> Option<bool> {
        let loud = level <= SPEAK_DBOV;
        self.history.push_back(loud);
        if self.history.len() > SPEAK_WINDOW {
            self.history.pop_front();
        }
        if loud {
            self.last_loud = Some(now);
        }
        let next = if self.active {
            self.last_loud
                .is_some_and(|last| now.duration_since(last) < SPEAK_HOLD)
        } else {
            self.history.iter().filter(|sample| **sample).count() >= SPEAK_ON_FRAMES
        };
        if next == self.active {
            return None;
        }
        self.active = next;
        Some(next)
    }

    fn expire(&mut self, now: Instant) -> bool {
        if self.active
            && self
                .last_loud
                .is_some_and(|last| now.duration_since(last) >= SPEAK_HOLD)
        {
            self.active = false;
            self.last_loud = None;
            self.history.clear();
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaRole {
    Caller,
    Callee,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    OneToOne,
    Group,
}

fn spawn_p2p_bind_worker(
    bind_params: Option<MediaSessionParams>,
    bind_socket: Arc<UdpSocket>,
    bind_running: Arc<AtomicBool>,
    bind_path: Arc<Mutex<Option<SocketAddr>>>,
    bind_enabled: Arc<AtomicBool>,
    bind_seq: Arc<AtomicU16>,
) {
    let Some(bind_params) = bind_params.filter(|params| !params.p2p_candidates.is_empty()) else {
        return;
    };
    thread::Builder::new()
        .name("zcall-p2p-bind".into())
        .spawn(move || {
            let mut attempt_started = Instant::now();
            let mut last_send = Instant::now()
                .checked_sub(Duration::from_millis(100))
                .unwrap();
            let mut was_enabled = false;
            while bind_running.load(Ordering::Relaxed) {
                let path_is_up = bind_path.lock().is_ok_and(|path| path.is_some());
                let enabled = bind_enabled.load(Ordering::Acquire);
                if !enabled {
                    was_enabled = false;
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                if !was_enabled {
                    // P2P_BIND_GIVE_UP_MS = 15 s. Explicit subCmd 3
                    // or a fresh start signal begins another bind window.
                    attempt_started = Instant::now();
                    last_send = Instant::now()
                        .checked_sub(Duration::from_millis(100))
                        .unwrap();
                    was_enabled = true;
                }
                if path_is_up {
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                if attempt_started.elapsed() >= Duration::from_secs(15) {
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                let interval = if attempt_started.elapsed() < Duration::from_secs(1) {
                    Duration::from_millis(100)
                } else {
                    Duration::from_millis(400)
                };
                if last_send.elapsed() >= interval {
                    last_send = Instant::now();
                    let sequence = bind_seq.fetch_add(1, Ordering::Relaxed);
                    for (index, candidate) in bind_params.p2p_candidates.iter().enumerate() {
                        let packet = p2p::bind(
                            bind_params.role == MediaRole::Callee,
                            bind_params.call_id,
                            sequence,
                            index as u32,
                        );
                        let _ = bind_socket.send_to(&packet, candidate);
                    }
                }
                thread::sleep(Duration::from_millis(25));
            }
        })
        .ok();
}

#[derive(Default)]
struct AudioAuthDiagnostics {
    reported: bool,
    failed_reported: bool,
}

#[derive(Default)]
struct VideoAuthDiagnostics {
    reported: bool,
    failed_reported: bool,
    frame_reported: bool,
}

#[derive(Clone, Copy)]
struct AudioRtpSource {
    from: SocketAddr,
    direct: bool,
}

#[derive(Clone, Copy)]
struct VideoRtpDeliveryContext {
    source: AudioRtpSource,
    is_group: bool,
    is_share: bool,
    payload_type: u8,
    marker: bool,
}

struct DecodedVideoRtp {
    sequence: u16,
    ssrc: u32,
    roc: u32,
    payload: Vec<u8>,
}

struct VideoRtpDecode {
    packet: Option<DecodedVideoRtp>,
    fallback_key_used: bool,
    auth_profile: (usize, usize),
}

struct AudioTransmitResources {
    tx_running: Arc<AtomicBool>,
    tx_audio_packets: Arc<AtomicU64>,
    tx_socket: Arc<UdpSocket>,
    tx_keys: Arc<Mutex<Option<SrtpKeys>>>,
    tx_params: Arc<Mutex<Option<MediaSessionParams>>>,
    tx_selected: Arc<Mutex<Option<(SocketAddr, u32)>>>,
    tx_muted: Arc<AtomicBool>,
    tx_twcc: Arc<AtomicU16>,
    tx_devices: Arc<Mutex<(Option<String>, Option<String>)>>,
    tx_device_generation: Arc<AtomicU64>,
    tx_rtcp: Arc<Mutex<Option<RtcpFeedback>>>,
    tx_p2p_path: Arc<Mutex<Option<SocketAddr>>>,
    tx_rate: Arc<Mutex<Option<SendRateController>>>,
    tx_last_video: Arc<AtomicU64>,
    tx_pending_profile: Arc<Mutex<Option<VideoProfile>>>,
    tx_apm: Option<Arc<Apm>>,
    tx_far_active: Arc<AtomicBool>,
    tx_mic_volume: Arc<AtomicU8>,
    tx_mic_volume_dirty: Arc<AtomicBool>,
    tx_mic_manual: Arc<AtomicBool>,
    tx_pending_mic_volume: Arc<Mutex<Option<u8>>>,
    tx_mic_level: Arc<AtomicU8>,
    tx_mic_level_dirty: Arc<AtomicBool>,
    tx_audio_enabled: Arc<AtomicBool>,
    tx_media_events: Arc<Mutex<VecDeque<GroupMediaEvent>>>,
    last_ping: Arc<Mutex<Instant>>,
    tx_group_plan: Arc<Mutex<Option<GroupVideoPlan>>>,
    tx_group_wanted: Arc<Mutex<HashMap<u32, u8>>>,
    tx_group_control_seq: Arc<AtomicU32>,
}

struct AudioTransmitWorker {
    resources: AudioTransmitResources,
    recorder: Option<PulseRecorder>,
    mic_fault_reported: bool,
    clip_guard: ClipGuard,
    guard_device: Option<String>,
    leveler: MicLeveler,
    mic_gain_enabled: bool,
    last_volume_reported: u8,
    active_device_generation: u64,
    last_mic_reopen: Instant,
    last_level_at: Instant,
    encoder: Option<OpusEncoder>,
    opus_buf: Vec<u8>,
    pcm_buf: Vec<u8>,
    seq: u16,
    audio_roc: u32,
    ts: u32,
    ping_seq: u32,
    last_twcc: Instant,
    last_report: Instant,
    audio_packets: u32,
    audio_octets: u32,
}

impl AudioTransmitWorker {
    fn new(resources: AudioTransmitResources) -> Self {
        let initial_microphone = resources
            .tx_devices
            .lock()
            .map_or(None, |devices| devices.0.clone());
        let mut clip_guard =
            ClipGuard::new(initial_microphone.clone(), resources.tx_mic_volume.clone())
                .with_controls(
                    Some(resources.tx_far_active.clone()),
                    resources.tx_mic_manual.clone(),
                );
        clip_guard.initialize_device();
        let leveler = MicLeveler::new(Some(resources.tx_far_active.clone()));
        let mic_gain_enabled = std::env::var("ZCALL_MIC_GAIN").map_or(true, |value| value != "0");
        let last_volume_reported = resources.tx_mic_volume.load(Ordering::Relaxed);
        let encoder = match OpusEncoder::new(SAMPLE_RATE.cast_signed(), 1, 32000) {
            Ok(encoder) => Some(encoder),
            Err(err) => {
                crate::zcall_log!("[zcall-rust] Opus encoder unavailable: {err}");
                None
            }
        };
        Self {
            resources,
            recorder: None,
            mic_fault_reported: false,
            clip_guard,
            guard_device: initial_microphone,
            leveler,
            mic_gain_enabled,
            last_volume_reported,
            active_device_generation: u64::MAX,
            last_mic_reopen: Instant::now().checked_sub(Duration::from_secs(1)).unwrap(),
            last_level_at: Instant::now()
                .checked_sub(Duration::from_millis(100))
                .unwrap(),
            encoder,
            opus_buf: vec![0u8; 1024],
            pcm_buf: vec![0u8; FRAME_BYTES],
            seq: 0,
            audio_roc: 0,
            ts: 0,
            ping_seq: 0,
            last_twcc: Instant::now(),
            last_report: Instant::now(),
            audio_packets: 0,
            audio_octets: 0,
        }
    }

    fn run(mut self) {
        while self.resources.tx_running.load(Ordering::Relaxed) {
            self.apply_pending_mic_volume();

            self.refresh_microphone();

            if let Some((pcm_s16, level)) = self.capture_pcm_frame() {
                self.encode_and_send_audio(&pcm_s16, level);
            }

            // Zalo's send-side bandwidth estimator depends on regular
            // transport-wide feedback. Audio RTCP carries TWCC + SR + SDES;
            // video RTCP carries RR + SDES when we have received video.
            self.send_twcc_feedback();
            self.send_periodic_rtcp_reports();

            // Send 6-second keepalive ping
            self.send_keepalive();
        }
    }

    fn apply_pending_mic_volume(&mut self) {
        if let Ok(mut pending) = self.resources.tx_pending_mic_volume.lock() {
            if let Some(percent) = pending.take() {
                if percent > 0 {
                    let _ = set_source_mute(self.guard_device.as_deref(), false);
                    pulse_normalize_own_stream();
                }
                self.clip_guard.mark_manual_write();
                if set_source_volume(self.guard_device.as_deref(), percent).is_ok() {
                    let actual = get_source_volume(self.guard_device.as_deref())
                        .map_or(percent, |value| value.min(100));
                    self.resources
                        .tx_mic_volume
                        .store(actual, Ordering::Relaxed);
                    crate::zcall_log!("[zcall-rust] mic volume: {actual}%");
                } else {
                    self.resources
                        .tx_mic_volume
                        .store(percent, Ordering::Relaxed);
                    crate::zcall_log!("[zcall-rust] warning: failed to apply manual mic volume");
                }
                self.last_volume_reported = self.resources.tx_mic_volume.load(Ordering::Relaxed);
                self.resources
                    .tx_mic_volume_dirty
                    .store(true, Ordering::Release);
            }
        }
    }

    fn refresh_microphone(&mut self) {
        let device_generation = self.resources.tx_device_generation.load(Ordering::Acquire);
        if device_generation != self.active_device_generation {
            let microphone = self
                .resources
                .tx_devices
                .lock()
                .map_or(None, |devices| devices.0.clone());
            let audio_enabled = self.resources.tx_audio_enabled.load(Ordering::Acquire);
            let switch_result = if audio_enabled {
                PulseRecorder::new(microphone.as_deref()).map(Some)
            } else if pulse_get_source_info(microphone.as_deref()).is_some() {
                Ok(None)
            } else {
                Err("selected microphone source could not be resolved".to_owned())
            };

            match switch_result {
                Ok(new_recorder) => {
                    self.recorder = new_recorder;
                    if audio_enabled {
                        // The capture stream must exist before normalizing its
                        // source-output volume/mute state in PulseAudio/PipeWire.
                        pulse_normalize_own_stream();
                    }
                    if microphone != self.guard_device {
                        self.clip_guard.set_device(microphone.clone());
                        self.clip_guard.initialize_device();
                        self.guard_device = microphone;
                        self.resources.tx_mic_manual.store(false, Ordering::Release);
                        self.leveler = MicLeveler::new(Some(self.resources.tx_far_active.clone()));
                        self.last_volume_reported =
                            self.resources.tx_mic_volume.load(Ordering::Relaxed);
                        self.resources
                            .tx_mic_volume_dirty
                            .store(true, Ordering::Release);
                    }
                    self.mic_fault_reported = false;
                }
                Err(error) => {
                    crate::zcall_log!(
                        "[zcall-rust] microphone switch rejected; keeping previous device: {error}"
                    );
                    // Roll back the requested selection as well as retaining the
                    // previous recorder and ClipGuard target.
                    if let Ok(mut devices) = self.resources.tx_devices.lock() {
                        devices.0.clone_from(&self.guard_device);
                    }
                    if !self.mic_fault_reported {
                        if let Ok(mut events) = self.resources.tx_media_events.lock() {
                            events.push_back(GroupMediaEvent::AudioFault("mic".into()));
                        }
                        self.mic_fault_reported = true;
                    }
                }
            }
            self.active_device_generation = device_generation;
            self.last_mic_reopen = Instant::now();
        }
        if self.recorder.is_none()
            && self.resources.tx_audio_enabled.load(Ordering::Acquire)
            && self.last_mic_reopen.elapsed() >= Duration::from_millis(500)
        {
            self.last_mic_reopen = Instant::now();
            let microphone = self
                .resources
                .tx_devices
                .lock()
                .map_or(None, |devices| devices.0.clone());
            match PulseRecorder::new(microphone.as_deref()) {
                Ok(reopened) => {
                    crate::zcall_log!("[zcall-rust] Pulse microphone stream recovered");
                    self.recorder = Some(reopened);
                    pulse_normalize_own_stream();
                }
                Err(error) => {
                    if !self.mic_fault_reported {
                        if let Ok(mut events) = self.resources.tx_media_events.lock() {
                            events.push_back(GroupMediaEvent::AudioFault("mic".into()));
                        }
                        self.mic_fault_reported = true;
                    }
                    crate::zcall_log!("[zcall-rust] Pulse microphone reopen failed: {error}");
                }
            }
        }
        if self.clip_guard.sync_os_volume() {
            if let Ok(reopened) = PulseRecorder::new(None) {
                self.recorder = Some(reopened);
                pulse_normalize_own_stream();
                self.clip_guard.initialize_device();
                self.leveler = MicLeveler::new(Some(self.resources.tx_far_active.clone()));
                self.last_volume_reported = self.resources.tx_mic_volume.load(Ordering::Relaxed);
                self.resources
                    .tx_mic_volume_dirty
                    .store(true, Ordering::Release);
            }
        }
    }

    fn send_twcc_feedback(&mut self) {
        if self.last_twcc.elapsed() >= Duration::from_millis(50) {
            self.last_twcc = Instant::now();
            let selected = *self.resources.tx_selected.lock().unwrap();
            if let Some((server, token)) = selected {
                let reports = self
                    .resources
                    .tx_rtcp
                    .lock()
                    .ok()
                    .and_then(|mut feedback| feedback.as_mut().map(RtcpFeedback::twcc_packets))
                    .unwrap_or_default();
                for report in reports {
                    let packet = wrap_media_packet(msg_types::AUDIO_FEC, token, &report);
                    let _ = self.resources.tx_socket.send_to(&packet, server);
                    if let Some(peer) = self
                        .resources
                        .tx_p2p_path
                        .lock()
                        .ok()
                        .and_then(|path| *path)
                    {
                        if let Some(params) = self
                            .resources
                            .tx_params
                            .lock()
                            .ok()
                            .and_then(|params| params.clone())
                        {
                            let direct = p2p::wrap(
                                params.role == MediaRole::Callee,
                                params.call_id,
                                p2p::AUDIO_RTCP,
                                &report,
                            );
                            let _ = self.resources.tx_socket.send_to(&direct, peer);
                        }
                    }
                }
            }
        }
    }

    fn send_periodic_rtcp_reports(&mut self) {
        if self.last_report.elapsed() >= Duration::from_secs(1) {
            self.last_report = Instant::now();
            let selected = *self.resources.tx_selected.lock().unwrap();
            if let Some((server, token)) = selected {
                let reports = self.resources.tx_rtcp.lock().ok().and_then(|mut feedback| {
                    let feedback = feedback.as_mut()?;
                    let now = unix_now_ms();
                    let downlink_loss = feedback.downlink_audio_loss();
                    let audio =
                        feedback.audio_report(now, self.ts, self.audio_packets, self.audio_octets);
                    let video = feedback.video_report(now);
                    Some((audio, video, downlink_loss))
                });
                if let Some((audio_report, video_report, downlink_loss)) = reports {
                    let now = unix_now_ms();
                    let sending_video = now
                        .saturating_sub(self.resources.tx_last_video.load(Ordering::Relaxed))
                        < 1_500;
                    let changed = self
                        .resources
                        .tx_rate
                        .lock()
                        .ok()
                        .and_then(|mut controller| {
                            controller.as_mut().and_then(|controller| {
                                controller.tick(now, sending_video, downlink_loss)
                            })
                        });
                    if let Some(profile) = changed {
                        if let Ok(mut pending) = self.resources.tx_pending_profile.lock() {
                            *pending = Some(profile);
                        }
                    }
                    let packet = wrap_media_packet(msg_types::AUDIO_FEC, token, &audio_report);
                    let _ = self.resources.tx_socket.send_to(&packet, server);
                    if let Some(peer) = self
                        .resources
                        .tx_p2p_path
                        .lock()
                        .ok()
                        .and_then(|path| *path)
                    {
                        if let Some(params) = self
                            .resources
                            .tx_params
                            .lock()
                            .ok()
                            .and_then(|params| params.clone())
                        {
                            let direct = p2p::wrap(
                                params.role == MediaRole::Callee,
                                params.call_id,
                                p2p::AUDIO_RTCP,
                                &audio_report,
                            );
                            let _ = self.resources.tx_socket.send_to(&direct, peer);
                        }
                    }
                    if let Some(video_report) = video_report {
                        let packet = wrap_media_packet(msg_types::VIDEO_FEC, token, &video_report);
                        let _ = self.resources.tx_socket.send_to(&packet, server);
                        if let Some(peer) = self
                            .resources
                            .tx_p2p_path
                            .lock()
                            .ok()
                            .and_then(|path| *path)
                        {
                            if let Some(params) = self
                                .resources
                                .tx_params
                                .lock()
                                .ok()
                                .and_then(|params| params.clone())
                            {
                                let direct = p2p::wrap(
                                    params.role == MediaRole::Callee,
                                    params.call_id,
                                    p2p::VIDEO_RTCP,
                                    &video_report,
                                );
                                let _ = self.resources.tx_socket.send_to(&direct, peer);
                            }
                        }
                    }
                }
            }
        }
    }

    fn send_keepalive(&mut self) {
        if self.resources.last_ping.lock().unwrap().elapsed() >= Duration::from_secs(6) {
            *self.resources.last_ping.lock().unwrap() = Instant::now();
            let (server_opt, p_opt) = {
                let s = *self.resources.tx_selected.lock().unwrap();
                let p = self.resources.tx_params.lock().unwrap().as_ref().map(|p| {
                    (
                        p.uid,
                        p.sess_id.clone(),
                        p.kind == MediaKind::Group,
                        p.video_enabled,
                    )
                });
                (s, p)
            };
            if let (Some((server, token)), Some((uid, sess_id, is_group, video_on))) =
                (server_opt, p_opt)
            {
                let ping_pkt = if is_group {
                    let group_camera_enabled =
                        !std::env::var("ZCALL_GROUP_CAMERA").is_ok_and(|value| value == "0");
                    // The camera state is independent of SRTP mode. The
                    // encrypted group path still sends camera RTP; gating
                    // this bit on `group_plain` made the SFU advertise us
                    // as camera-off while packets were being transmitted.
                    let ping_video_on = group_camera_advertised(video_on, group_camera_enabled);
                    let group_seq =
                        next_group_control_sequence(&self.resources.tx_group_control_seq);
                    media_debug(&format!(
                        "periodic-zavi-ping seq={group_seq} video_on={ping_video_on}"
                    ));
                    build_zavi_ping_packet(uid, token, group_seq, &sess_id, ping_video_on)
                } else {
                    self.ping_seq = self.ping_seq.wrapping_add(1);
                    build_ping_packet(uid, token, self.ping_seq, &sess_id)
                };
                let _ = self.resources.tx_socket.send_to(&ping_pkt, server);

                if is_group {
                    let plan = self
                        .resources
                        .tx_group_plan
                        .lock()
                        .ok()
                        .and_then(|plan| plan.clone());
                    if let Some(plan) = plan.filter(|plan| !plan.send.is_empty()) {
                        let group_seq =
                            next_group_control_sequence(&self.resources.tx_group_control_seq);
                        let packet =
                            build_group_video_quality_config(uid, token, group_seq, &plan.send);
                        let _ = self.resources.tx_socket.send_to(&packet, server);
                    }
                    let entries = self
                        .resources
                        .tx_group_wanted
                        .lock()
                        .map_or_default(|wanted| {
                            wanted
                                .iter()
                                .map(|(uid, quality_id)| GroupVideoQualityRequest {
                                    uid: *uid,
                                    quality_id: if std::env::var("ZCALL_GROUP_QUALITY")
                                        .is_ok_and(|value| value == "0")
                                    {
                                        0xff
                                    } else {
                                        *quality_id
                                    },
                                })
                                .collect::<Vec<_>>()
                        });
                    if !entries.is_empty() {
                        let group_seq =
                            next_group_control_sequence(&self.resources.tx_group_control_seq);
                        let packet =
                            build_group_video_quality_request(uid, token, group_seq, &entries);
                        let _ = self.resources.tx_socket.send_to(&packet, server);
                    }
                }
            }
        }
    }

    fn capture_pcm_frame(&mut self) -> Option<(Vec<i16>, u8)> {
        let has_recorder = self.recorder.is_some();
        if !has_recorder {
            thread::sleep(Duration::from_millis(10));
            return None;
        }
        let read_ok = self
            .recorder
            .as_mut()
            .is_some_and(|recorder| recorder.read(&mut self.pcm_buf).is_ok());
        if !read_ok {
            if !self.mic_fault_reported {
                if let Ok(mut events) = self.resources.tx_media_events.lock() {
                    events.push_back(GroupMediaEvent::AudioFault("mic".into()));
                }
                self.mic_fault_reported = true;
            }
            self.recorder = None;
            self.last_mic_reopen = Instant::now();
            thread::sleep(Duration::from_millis(5));
            return None;
        }

        let mut pcm_s16: Vec<i16> = self
            .pcm_buf
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| i16::from_le_bytes(*pair))
            .collect();
        if self.resources.tx_muted.load(Ordering::Relaxed) || self.clip_guard.is_os_muted() {
            pcm_s16.fill(0);
        } else {
            // ClipGuard measures unprocessed hardware PCM before APM.
            self.clip_guard.feed(&pcm_s16);
            if let Some(apm) = self.resources.tx_apm.as_ref() {
                apm.process_capture(&mut pcm_s16);
            }
            if self.mic_gain_enabled {
                self.leveler.process(&mut pcm_s16);
            }
        }
        apply_soft_limiter(&mut pcm_s16);

        let level = compute_rfc6464_level(&pcm_s16);
        if self.last_level_at.elapsed() >= Duration::from_millis(100) {
            self.resources.tx_mic_level.store(level, Ordering::Relaxed);
            self.resources
                .tx_mic_level_dirty
                .store(true, Ordering::Release);
            self.last_level_at = Instant::now();
        }
        let current_volume = self.resources.tx_mic_volume.load(Ordering::Relaxed);
        if current_volume != self.last_volume_reported {
            self.last_volume_reported = current_volume;
            self.resources
                .tx_mic_volume_dirty
                .store(true, Ordering::Release);
        }
        Some((pcm_s16, level))
    }

    fn encode_and_send_audio(&mut self, pcm_s16: &[i16], level: u8) {
        let encoded_len = {
            let Some(encoder) = self.encoder.as_mut() else {
                return;
            };
            let Ok(encoded_len) = encoder.encode(pcm_s16, &mut self.opus_buf) else {
                return;
            };
            encoded_len
        };
        let opus_data = self.opus_buf[..encoded_len].to_vec();
        self.send_audio_packet(&opus_data, encoded_len, level);
    }

    fn send_audio_packet(&mut self, opus_data: &[u8], encoded_len: usize, level: u8) {
        let (server_opt, p_opt, k_opt) = {
            let selected = *self.resources.tx_selected.lock().unwrap();
            let params = self
                .resources
                .tx_params
                .lock()
                .unwrap()
                .as_ref()
                .map(|params| {
                    (
                        params.uid,
                        params.send_udp,
                        params.call_id,
                        params.role == MediaRole::Caller,
                        params.kind == MediaKind::Group,
                    )
                });
            let keys = self
                .resources
                .tx_keys
                .lock()
                .unwrap()
                .as_ref()
                .map(|keys| (keys.enc, keys.auth, keys.salt));
            (selected, params, keys)
        };
        let (Some((server, token)), Some((uid, send_udp, call_id, is_caller, is_group))) =
            (server_opt, p_opt)
        else {
            return;
        };
        if !send_udp || (!is_group && k_opt.is_none()) {
            return;
        }

        let tw_seq = self.resources.tx_twcc.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut controller) = self.resources.tx_rate.lock() {
            if let Some(controller) = controller.as_mut() {
                controller.on_sent(tw_seq, unix_now_ms());
            }
        }
        let rtp_header = if is_group {
            build_group_audio_rtp_header(
                self.seq,
                self.ts,
                uid,
                tw_seq,
                false,
                112,
                unix_now_ms(),
                level,
            )
        } else {
            build_audio_rtp_header(self.seq, self.ts, uid, tw_seq, false, 112).to_vec()
        };
        let media_payload = if let Some((enc_k, auth_k, salt_k)) = k_opt {
            let keys = SrtpKeys {
                enc: enc_k,
                auth: auth_k,
                salt: salt_k,
            };
            protect_rtp(&rtp_header, opus_data, &keys, self.seq, uid, self.audio_roc)
        } else {
            let mut plain = rtp_header;
            plain.extend_from_slice(opus_data);
            plain
        };
        let media_packet = wrap_media_packet(msg_types::AUDIO_RTP, token, &media_payload);
        let _ = self.resources.tx_socket.send_to(&media_packet, server);
        if !is_group {
            if let Some(peer) = self
                .resources
                .tx_p2p_path
                .lock()
                .ok()
                .and_then(|path| *path)
            {
                let direct = p2p::wrap(!is_caller, call_id, p2p::AUDIO, &media_payload);
                let _ = self.resources.tx_socket.send_to(&direct, peer);
            }
        }
        self.audio_packets = self.audio_packets.wrapping_add(1);
        self.resources
            .tx_audio_packets
            .fetch_add(1, Ordering::Relaxed);
        self.audio_octets = self.audio_octets.wrapping_add(encoded_len as u32);
        self.seq = self.seq.wrapping_add(1);
        if self.seq == 0 {
            self.audio_roc = self.audio_roc.wrapping_add(1);
        }
        self.ts = self.ts.wrapping_add(320);
    }
}

#[derive(Default)]
struct GroupLayoutAttempt {
    responded: HashSet<SocketAddr>,
    accepted: Vec<(SocketAddr, u32, Option<String>)>,
}

struct ReceiveWorkerSetup {
    running: Arc<AtomicBool>,
    socket: Arc<UdpSocket>,
    keys: Arc<Mutex<Option<SrtpKeys>>>,
    selected_server: Arc<Mutex<Option<(SocketAddr, u32)>>>,
    params: Arc<Mutex<Option<MediaSessionParams>>>,
    jitter_buffer: Option<Arc<Mutex<JitterBuffer>>>,
    group_audio: Arc<Mutex<GroupAudioMixer>>,
}

struct ReceiveWorker {
    rx_running: Arc<AtomicBool>,
    rx_audio_packets: Arc<AtomicU64>,
    rx_socket: Arc<UdpSocket>,
    rx_keys: Arc<Mutex<Option<SrtpKeys>>>,
    rx_fallback_keys: Arc<Mutex<Option<SrtpKeys>>>,
    rx_selected: Arc<Mutex<Option<(SocketAddr, u32)>>>,
    video_frames: Arc<Mutex<VecDeque<VideoFrame>>>,
    rx_params: Arc<Mutex<Option<MediaSessionParams>>>,
    rx_rtcp: Arc<Mutex<Option<RtcpFeedback>>>,
    rx_nack: Arc<Mutex<NackTracker>>,
    rx_keyframe_request: Arc<AtomicBool>,
    rx_p2p_path: Arc<Mutex<Option<SocketAddr>>>,
    rx_p2p_enabled: Arc<AtomicBool>,
    rx_video_history: VideoHistory,
    rx_group_events: Arc<Mutex<VecDeque<GroupMediaEvent>>>,
    rx_share_peer: Arc<Mutex<Option<GroupSharePeer>>>,
    rx_remote_share_uid: Arc<AtomicU32>,
    rx_send_rate: Arc<Mutex<Option<SendRateController>>>,
    rx_last_media_rx_ms: Arc<AtomicU64>,
    rx_audio_enabled: Arc<AtomicBool>,
    rx_tx_audio_packets: Arc<AtomicU64>,
    rx_last_video_sent_ms: Arc<AtomicU64>,
    rx_tx_video_packets: Arc<AtomicU64>,
    rx_jitter_buffer: Option<Arc<Mutex<JitterBuffer>>>,
    rx_group_audio: Arc<Mutex<GroupAudioMixer>>,
    video_assemblers: HashMap<u32, VideoAssembler>,
    audio_roc: RocTracker,
    video_rocs: HashMap<u32, RocTracker>,
    last_video_pli: HashMap<u32, Instant>,
    audio_dedup: AudioDeduplicator,
    member_audio_seen: HashSet<u32>,
    member_speaking: HashMap<u32, MemberSpeakingTracker>,
    last_audio_rx: Instant,
    last_p2p_rx: Option<Instant>,
    last_recovery: Instant,
    local_ip_sig: String,
    next_network_check: Instant,
    change_address_retries: VecDeque<(Instant, &'static str)>,
    change_addr_seq: u32,
    relay_audio_streak: u32,
    rx_audio_second: u32,
    early_audio_rx_count: u32,
    group_rx_audio_packets: u64,
    group_rx_video_packets: u64,
    group_rx_audio_decoded: u64,
    group_rx_video_decoded: u64,
    group_rx_audio_bad: u64,
    group_rx_video_bad: u64,
    rx_video_packets: u64,
    rx_video_decoded: u64,
    rx_video_bad: u64,
    rx_video_first: u8,
    group_rx_other_host: u64,
    group_rx_alt_port: u64,
    group_rx_last_summary: Instant,
    group_rx_audio_first: u8,
    group_rx_video_first: u8,
    group_audio_srtp_profile: Option<(usize, usize)>,
    group_video_srtp_profile: Option<(usize, usize)>,
    group_audio_srtp_profile_discovery_attempts: u8,
    group_video_srtp_profile_discovery_attempts: u8,
    audio_auth: AudioAuthDiagnostics,
    video_auth: VideoAuthDiagnostics,
}

impl ReceiveWorker {
    fn run(mut self) {
        let mut buf = [0u8; 2048];

        while self.rx_running.load(Ordering::Relaxed) {
            if let Ok((len, from)) = self.rx_socket.recv_from(&mut buf) {
                let pkt = &buf[..len];
                if pkt.is_empty() {
                    continue;
                }
                if self.process_packet(pkt, from) {
                    continue;
                }
            }
            let recovery_now = Instant::now();
            self.check_network_recovery(recovery_now);

            self.expire_member_speaking();

            self.send_change_address_retries(recovery_now);

            self.log_receive_summary();

            self.poll_pending_video_nacks();
        }
    }
    fn handle_audio_rtp_down(
        &mut self,
        payload: &[u8],
        from: SocketAddr,
        direct: bool,
        params_snapshot: Option<&MediaSessionParams>,
    ) -> bool {
        if payload.len() < 12 {
            return false;
        }
        // The relay and P2P path carry identical SRTP sequence spaces. Decode
        // first, then deliver each SSRC/ROC/sequence exactly once.
        let rtp_body = payload;
        let seq = u16::from_be_bytes([rtp_body[2], rtp_body[3]]);
        let ssrc = u32::from_be_bytes([rtp_body[8], rtp_body[9], rtp_body[10], rtp_body[11]]);
        let local_uid = params_snapshot.map_or_default(|params| params.uid);
        let is_group = params_snapshot.is_some_and(|params| params.kind == MediaKind::Group);
        if is_group {
            self.handle_group_audio_rtp(
                rtp_body,
                AudioRtpSource { from, direct },
                params_snapshot,
                ssrc,
                seq,
                local_uid,
            )
        } else {
            self.handle_peer_audio_rtp(rtp_body, from, direct, params_snapshot)
        }
    }

    fn handle_video_down(
        &mut self,
        payload: &[u8],
        from: SocketAddr,
        direct: bool,
        params_snapshot: Option<&MediaSessionParams>,
    ) -> bool {
        if payload.len() < 12 {
            return false;
        }
        let rtp_body = payload;
        let ssrc = u32::from_be_bytes([rtp_body[8], rtp_body[9], rtp_body[10], rtp_body[11]]);
        if params_snapshot
            .is_some_and(|params| params.kind == MediaKind::Group && params.uid == ssrc)
            && !std::env::var("ZCALL_CAM_LOOPBACK").is_ok_and(|value| value == "1")
        {
            return true;
        }
        let local_share_uid = self
            .rx_share_peer
            .lock()
            .ok()
            .and_then(|peer| peer.as_ref().map(|peer| peer.uid));
        let is_local_share = local_share_uid == Some(ssrc);
        let is_share = is_local_share || self.rx_remote_share_uid.load(Ordering::Acquire) == ssrc;
        if is_local_share && !std::env::var("ZCALL_SHARE_LOOPBACK").is_ok_and(|value| value == "1")
        {
            return true;
        }
        let marker = (rtp_body[1] & 0x80) != 0;
        let pt = rtp_body[1] & 0x7f;
        let is_group_video = params_snapshot.is_some_and(|params| params.kind == MediaKind::Group);
        if is_group_video || pt == VIDEO_PT_KEY || pt == VIDEO_PT_INTER {
            let decoded = self.decode_video_rtp(rtp_body, ssrc, is_share, is_group_video);
            self.record_video_auth_result(rtp_body, ssrc, pt, marker, is_group_video, &decoded);
            if let Some(packet) = decoded.packet {
                self.deliver_video_rtp(
                    &packet,
                    rtp_body,
                    VideoRtpDeliveryContext {
                        source: AudioRtpSource { from, direct },
                        is_group: is_group_video,
                        is_share,
                        payload_type: pt,
                        marker,
                    },
                );
            }
        }
        false
    }

    fn check_network_recovery(&mut self, recovery_now: Instant) {
        if recovery_now >= self.next_network_check {
            self.next_network_check = recovery_now + Duration::from_millis(500);
            let current_signature = local_ipv4_signature();
            let network_changed =
                !current_signature.is_empty() && current_signature != self.local_ip_sig;
            if !current_signature.is_empty() {
                self.local_ip_sig = current_signature;
            }

            let params_now = self.rx_params.lock().ok().and_then(|params| params.clone());
            let selected_now = *self.rx_selected.lock().unwrap();

            if network_changed {
                if let Some(params) = params_now.as_ref() {
                    crate::zcall_log!(
                                "[zcall-rust] local IPv4 changed ({}); stopping P2P and sending ChangeAddress",
                                self.local_ip_sig
                            );
                    stop_p2p_path(
                        &self.rx_socket,
                        selected_now,
                        &self.rx_p2p_path,
                        &self.rx_p2p_enabled,
                        params,
                        true,
                    );
                    send_change_address(
                        &self.rx_socket,
                        selected_now,
                        params,
                        self.change_addr_seq,
                    );
                    self.change_addr_seq = self.change_addr_seq.wrapping_add(1);
                    self.change_address_retries.push_back((
                        recovery_now + Duration::from_secs(1),
                        "local-ip-change retry 1",
                    ));
                    self.change_address_retries.push_back((
                        recovery_now + Duration::from_millis(2500),
                        "local-ip-change retry 2",
                    ));
                    self.last_recovery = recovery_now;
                }
            } else if self.rx_p2p_path.lock().is_ok_and(|path| path.is_some())
                && self.last_p2p_rx.is_some_and(|last| {
                    recovery_now.duration_since(last) > Duration::from_millis(2500)
                })
            {
                if let Some(params) = params_now.as_ref() {
                    crate::zcall_log!(
                        "[zcall-rust] P2P receive timed out; requesting relay fallback"
                    );
                    stop_p2p_path(
                        &self.rx_socket,
                        selected_now,
                        &self.rx_p2p_path,
                        &self.rx_p2p_enabled,
                        params,
                        true,
                    );
                    send_change_address(
                        &self.rx_socket,
                        selected_now,
                        params,
                        self.change_addr_seq,
                    );
                    self.change_addr_seq = self.change_addr_seq.wrapping_add(1);
                    self.relay_audio_streak = 0;
                    self.last_recovery = recovery_now;
                }
            } else if !params_now
                .as_ref()
                .is_some_and(|params| params.kind == MediaKind::Group)
                && recovery_now.duration_since(self.last_audio_rx) > Duration::from_secs(2)
                && recovery_now.duration_since(self.last_audio_rx) < Duration::from_secs(20)
                && recovery_now.duration_since(self.last_recovery) > Duration::from_millis(2500)
            {
                if let Some(params) = params_now.as_ref() {
                    crate::zcall_log!(
                                "[zcall-rust] audio receive stalled; requesting relay fallback and ChangeAddress"
                            );
                    stop_p2p_path(
                        &self.rx_socket,
                        selected_now,
                        &self.rx_p2p_path,
                        &self.rx_p2p_enabled,
                        params,
                        true,
                    );
                    send_change_address(
                        &self.rx_socket,
                        selected_now,
                        params,
                        self.change_addr_seq,
                    );
                    self.change_addr_seq = self.change_addr_seq.wrapping_add(1);
                    self.relay_audio_streak = 0;
                    self.last_recovery = recovery_now;
                }
            }
        }
    }
    fn expire_member_speaking(&mut self) {
        let speaking_now = Instant::now();
        for (uid, tracker) in &mut self.member_speaking {
            if tracker.expire(speaking_now) {
                if let Ok(mut events) = self.rx_group_events.lock() {
                    events.push_back(GroupMediaEvent::MemberSpeaking(*uid, false));
                }
            }
        }
    }
    fn send_change_address_retries(&mut self, recovery_now: Instant) {
        while self
            .change_address_retries
            .front()
            .is_some_and(|(due, _)| recovery_now >= *due)
        {
            if let Some((_, reason)) = self.change_address_retries.pop_front() {
                if let Some(params) = self.rx_params.lock().ok().and_then(|params| params.clone()) {
                    let selected = *self.rx_selected.lock().unwrap();
                    crate::zcall_log!("[zcall-rust] sending {reason}");
                    send_change_address(&self.rx_socket, selected, &params, self.change_addr_seq);
                    self.change_addr_seq = self.change_addr_seq.wrapping_add(1);
                }
            }
        }
    }
    fn log_receive_summary(&mut self) {
        let summary_mode = self.rx_params.lock().ok().and_then(|params| {
            params.as_ref().map(|params| {
                if params.kind == MediaKind::Group {
                    "group"
                } else {
                    "one-to-one"
                }
            })
        });
        if let Some(mode) = summary_mode {
            if self.group_rx_last_summary.elapsed() >= Duration::from_secs(5) {
                media_debug(&format!(
                            "summary mode={mode} rx_video={} decoded_video={} bad_video={} tx_video={} tx_audio={} rx_audio_delivered={} group_rx_audio={} group_decoded_audio={} group_bad_audio={} group_rx_video={} group_decoded_video={} group_bad_video={} other_host={} same_host_alt_port={} last_video_tx_ms={}",
                            self.rx_video_packets,
                            self.rx_video_decoded,
                            self.rx_video_bad,
                            self.rx_tx_video_packets.load(Ordering::Relaxed),
                            self.rx_tx_audio_packets.load(Ordering::Relaxed),
                            self.rx_audio_packets.load(Ordering::Relaxed),
                            self.group_rx_audio_packets,
                            self.group_rx_audio_decoded,
                            self.group_rx_audio_bad,
                            self.group_rx_video_packets,
                            self.group_rx_video_decoded,
                            self.group_rx_video_bad,
                            self.group_rx_other_host,
                            self.group_rx_alt_port,
                            self.rx_last_video_sent_ms.load(Ordering::Relaxed)
                        ));
                self.group_rx_last_summary = Instant::now();
            }
        }
    }
    fn poll_pending_video_nacks(&mut self) {
        if let Ok(mut tracker) = self.rx_nack.lock() {
            let requests = tracker.poll(unix_now_ms());
            if !requests.is_empty() {
                let selected = *self.rx_selected.lock().unwrap();
                if let Some((server, token)) = selected {
                    let local_uid = self
                        .rx_params
                        .lock()
                        .unwrap()
                        .as_ref()
                        .map_or_default(|params| params.uid);
                    for (ssrc, missing) in requests {
                        let report = build_nack_many(local_uid, ssrc, &missing);
                        let packet = wrap_media_packet(msg_types::VIDEO_FEC, token, &report);
                        let _ = self.rx_socket.send_to(&packet, server);
                        if let Some(peer) = self.rx_p2p_path.lock().ok().and_then(|path| *path) {
                            let params = self.rx_params.lock().unwrap().clone();
                            if let Some(params) = params {
                                let direct_packet = p2p::wrap(
                                    params.role == MediaRole::Callee,
                                    params.call_id,
                                    p2p::VIDEO_RTCP,
                                    &report,
                                );
                                let _ = self.rx_socket.send_to(&direct_packet, peer);
                            }
                        }
                    }
                }
            }
        }
    }

    fn process_packet(&mut self, pkt: &[u8], from: SocketAddr) -> bool {
        let params_snapshot = self.rx_params.lock().ok().and_then(|params| params.clone());
        self.record_group_packet_source(pkt, from, params_snapshot.as_ref());
        let Some((msg_type, payload, direct)) =
            self.classify_incoming_packet(pkt, from, params_snapshot.as_ref())
        else {
            return true;
        };
        self.log_first_video_packet(msg_type, payload, direct, params_snapshot.as_ref());
        self.dispatch_media_message(
            msg_type,
            payload,
            pkt,
            from,
            direct,
            params_snapshot.as_ref(),
        )
    }

    fn record_group_packet_source(
        &mut self,
        pkt: &[u8],
        from: SocketAddr,
        params: Option<&MediaSessionParams>,
    ) {
        if !params.is_some_and(|params| params.kind == MediaKind::Group)
            || !matches!(pkt[0], msg_types::AUDIO_RTP_DOWN | msg_types::VIDEO_DOWN)
        {
            return;
        }
        let selected = self
            .rx_selected
            .lock()
            .ok()
            .and_then(|selected| selected.map(|(addr, _)| addr));
        let same_host = selected.is_some_and(|selected| selected.ip() == from.ip());
        let source = if selected == Some(from) {
            "selected"
        } else if same_host {
            self.group_rx_alt_port = self.group_rx_alt_port.saturating_add(1);
            "same-host-alt-port"
        } else {
            self.group_rx_other_host = self.group_rx_other_host.saturating_add(1);
            "other-host"
        };
        let ssrc =
            (pkt.len() >= 13).then(|| u32::from_be_bytes([pkt[9], pkt[10], pkt[11], pkt[12]]));
        if pkt[0] == msg_types::AUDIO_RTP_DOWN {
            self.group_rx_audio_packets = self.group_rx_audio_packets.saturating_add(1);
            if self.group_rx_audio_first < 3 {
                media_debug(&format!(
                    "udp-audio source={source} bytes={} ssrc={}",
                    pkt.len(),
                    ssrc.map_or_else(|| "short".into(), |uid| uid.to_string())
                ));
                self.group_rx_audio_first += 1;
            }
        } else {
            self.group_rx_video_packets = self.group_rx_video_packets.saturating_add(1);
            if self.group_rx_video_first < 3 {
                media_debug(&format!(
                    "udp-video source={source} bytes={} ssrc={}",
                    pkt.len(),
                    ssrc.map_or_else(|| "short".into(), |uid| uid.to_string())
                ));
                self.group_rx_video_first += 1;
            }
        }
    }

    fn classify_incoming_packet<'a>(
        &mut self,
        pkt: &'a [u8],
        from: SocketAddr,
        params: Option<&MediaSessionParams>,
    ) -> Option<(u8, &'a [u8], bool)> {
        if pkt[0] == msg_types::P2P_EXT {
            self.classify_p2p_packet(pkt, from, params)
        } else {
            self.classify_relay_packet(pkt, from, params)
        }
    }

    fn classify_p2p_packet<'a>(
        &mut self,
        pkt: &'a [u8],
        from: SocketAddr,
        params: Option<&MediaSessionParams>,
    ) -> Option<(u8, &'a [u8], bool)> {
        let params = params?;
        let (header, body) = p2p::parse(pkt)?;
        let peer_is_callee = params.role == MediaRole::Caller;
        if header.call_id != params.call_id
            || header.is_callee != peer_is_callee
            || (!params.p2p_candidates.is_empty()
                && !params
                    .p2p_candidates
                    .iter()
                    .any(|candidate| candidate.ip() == from.ip()))
        {
            return None;
        }
        self.last_p2p_rx = Some(Instant::now());
        match header.kind {
            p2p::BIND_PEER => {
                if let Some(answer) = p2p::bind_answer(
                    params.role == MediaRole::Callee,
                    params.call_id,
                    body,
                    &format!("{}|{}", from.ip(), from.port()),
                ) {
                    let _ = self.rx_socket.send_to(&answer, from);
                }
                None
            }
            p2p::SIGNAL => {
                match body.first() {
                    Some(1) => self.rx_p2p_enabled.store(true, Ordering::Release),
                    Some(2) => self.rx_p2p_enabled.store(false, Ordering::Release),
                    _ => {}
                }
                if matches!(body.first(), Some(1 | 2)) {
                    if let Ok(mut path) = self.rx_p2p_path.lock() {
                        *path = None;
                    }
                    self.relay_audio_streak = 0;
                }
                None
            }
            p2p::KEEPALIVE => {
                if matches!(body.first(), Some(1 | 2)) {
                    // Publish the path before replying so TX can route duplicates immediately.
                    if let Ok(mut path) = self.rx_p2p_path.lock() {
                        *path = Some(from);
                    }
                    if body.first() == Some(&1) {
                        if let Some(pong) = p2p::pong(params.role == MediaRole::Callee, pkt) {
                            let _ = self.rx_socket.send_to(&pong, from);
                        }
                    }
                }
                None
            }
            p2p::AUDIO => Some((msg_types::AUDIO_RTP_DOWN, body, true)),
            p2p::VIDEO => Some((msg_types::VIDEO_DOWN, body, true)),
            p2p::AUDIO_RTCP => Some((msg_types::AUDIO_FEC, body, false)),
            p2p::VIDEO_RTCP => Some((msg_types::VIDEO_FEC, body, false)),
            _ => None,
        }
    }

    fn classify_relay_packet<'a>(
        &mut self,
        pkt: &'a [u8],
        from: SocketAddr,
        params: Option<&MediaSessionParams>,
    ) -> Option<(u8, &'a [u8], bool)> {
        let selected_from = self
            .rx_selected
            .lock()
            .ok()
            .and_then(|selected| selected.map(|(addr, _)| addr));
        if !accepts_relay_source(
            selected_from,
            from,
            params.is_some_and(|params| params.kind == MediaKind::Group),
        ) {
            return None;
        }
        let (msg_type, payload) = match pkt[0] {
            msg_types::AUDIO_RTP_DOWN | msg_types::VIDEO_DOWN => (pkt[0], &pkt[1..]),
            msg_types::AUDIO_FEC | msg_types::VIDEO_FEC => {
                if pkt.len() < 9 {
                    return None;
                }
                let token = u32::from_le_bytes([pkt[1], pkt[2], pkt[3], pkt[4]]);
                let expected_token = self
                    .rx_selected
                    .lock()
                    .ok()
                    .and_then(|selected| selected.map(|(_, token)| token));
                if expected_token != Some(token) {
                    return None;
                }
                (pkt[0], &pkt[5..])
            }
            other => (other, pkt),
        };
        Some((msg_type, payload, false))
    }

    fn log_first_video_packet(
        &mut self,
        msg_type: u8,
        payload: &[u8],
        direct: bool,
        params: Option<&MediaSessionParams>,
    ) {
        if msg_type != msg_types::VIDEO_DOWN {
            return;
        }
        self.rx_video_packets = self.rx_video_packets.saturating_add(1);
        if self.rx_video_first < 3 && payload.len() >= 12 {
            let ssrc = u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]);
            let mode = if params.is_some_and(|params| params.kind == MediaKind::Group) {
                "group"
            } else {
                "one-to-one"
            };
            media_debug(&format!(
                "rx-video mode={mode} path={} bytes={} ssrc={ssrc}",
                if direct { "direct" } else { "relay" },
                payload.len()
            ));
            self.rx_video_first += 1;
        }
    }

    fn dispatch_media_message(
        &mut self,
        msg_type: u8,
        payload: &[u8],
        pkt: &[u8],
        from: SocketAddr,
        direct: bool,
        params_snapshot: Option<&MediaSessionParams>,
    ) -> bool {
        match msg_type {
            msg_types::AUDIO_FEC | msg_types::VIDEO_FEC => {
                self.handle_rtcp_feedback(msg_type, payload);
                false
            }
            msg_types::AUDIO_RTP_DOWN => {
                self.handle_audio_rtp_down(payload, from, direct, params_snapshot)
            }
            msg_types::VIDEO_DOWN => self.handle_video_down(payload, from, direct, params_snapshot),
            msg_types::SERVER_REPLY => {
                self.handle_server_reply(pkt, from);
                false
            }
            _ => false,
        }
    }

    fn handle_rtcp_feedback(&mut self, msg_type: u8, payload: &[u8]) {
        let is_video = msg_type == msg_types::VIDEO_FEC;
        let now_ms = unix_now_ms();
        let targets = self
            .rx_rtcp
            .lock()
            .ok()
            .and_then(|mut feedback| {
                feedback
                    .as_mut()
                    .map(|feedback| feedback.on_rtcp(is_video, payload, now_ms))
            })
            .unwrap_or_default();
        if let Ok(mut controller) = self.rx_send_rate.lock() {
            if let Some(controller) = controller.as_mut() {
                controller.on_feedback(&targets.twcc_feedback, &targets.report_blocks, now_ms);
            }
        }
        let local_uid = self
            .rx_params
            .lock()
            .ok()
            .and_then(|params| params.as_ref().map(|params| params.uid));
        if local_uid.is_some_and(|uid| targets.keyframe_targets.contains(&uid)) {
            self.rx_keyframe_request.store(true, Ordering::Relaxed);
        }
        if is_video {
            let params = self.rx_params.lock().ok().and_then(|params| params.clone());
            if let Some(params) = params {
                if let Some((relay, token)) = *self.rx_selected.lock().unwrap() {
                    for nack in targets.nack_requests {
                        if nack.media_ssrc != 0 && nack.media_ssrc != params.uid {
                            continue;
                        }
                        for sequence in nack.lost_sequences {
                            let cached = self.rx_video_history.lock().ok().and_then(|history| {
                                history
                                    .iter()
                                    .rev()
                                    .find(|(seq, _)| *seq == sequence)
                                    .map(|(_, packet)| packet.clone())
                            });
                            if let Some(protected) = cached {
                                let relay_packet =
                                    wrap_media_packet(msg_types::VIDEO_RTP, token, &protected);
                                let _ = self.rx_socket.send_to(&relay_packet, relay);
                                if let Some(peer) =
                                    self.rx_p2p_path.lock().ok().and_then(|path| *path)
                                {
                                    let direct_packet = p2p::wrap(
                                        params.role == MediaRole::Callee,
                                        params.call_id,
                                        p2p::VIDEO,
                                        &protected,
                                    );
                                    let _ = self.rx_socket.send_to(&direct_packet, peer);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn handle_server_reply(&mut self, pkt: &[u8], from: SocketAddr) {
        if let Some(reply) = parse_server_reply(pkt) {
            let selected_from = self
                .rx_selected
                .lock()
                .ok()
                .and_then(|selected| selected.map(|(addr, _)| addr));
            let group_uid = self.rx_params.lock().ok().and_then(|params| {
                params
                    .as_ref()
                    .filter(|p| p.kind == MediaKind::Group)
                    .map(|p| p.uid)
            });
            if selected_from == Some(from) || group_uid.is_some() {
                if let Some(local_uid) = group_uid {
                    if let Some(event) = group_event_from_reply(&reply, local_uid) {
                        if let Ok(mut events) = self.rx_group_events.lock() {
                            events.push_back(event);
                            while events.len() > 128 {
                                events.pop_front();
                            }
                        }
                    }
                }
                if reply.cmd == crate::protocol::zrtp::commands::CHANGE_ADDRESS {
                    crate::zcall_log!("[zcall-rust] relay acknowledged ChangeAddress");
                } else if reply.cmd == crate::protocol::zrtp::commands::REQ_FORWARD
                    && matches!(reply.sub_cmd, 2 | 3)
                {
                    if let Ok(mut path) = self.rx_p2p_path.lock() {
                        *path = None;
                    }
                    self.rx_p2p_enabled
                        .store(reply.sub_cmd == 3, Ordering::Release);
                    self.last_p2p_rx = None;
                    self.relay_audio_streak = 0;
                }
                let mut selected = self.rx_selected.lock().unwrap();
                if let Some((addr, _)) = *selected {
                    if addr == from {
                        *selected = Some((from, reply.server_token.unwrap_or(reply.token)));
                    }
                }
            }
        }
    }

    fn handle_group_audio_rtp(
        &mut self,
        rtp_body: &[u8],
        source: AudioRtpSource,
        params_snapshot: Option<&MediaSessionParams>,
        ssrc: u32,
        seq: u16,
        local_uid: u32,
    ) -> bool {
        if ssrc == local_uid {
            return false;
        }
        let decoded = self.decode_group_audio_packet(rtp_body, ssrc, seq);
        self.deliver_group_audio_packet(
            rtp_body,
            source.from,
            source.direct,
            params_snapshot,
            decoded,
        )
    }

    fn decode_group_audio_packet(
        &mut self,
        rtp_body: &[u8],
        ssrc: u32,
        seq: u16,
    ) -> Option<(u16, u32, u32, Vec<u8>)> {
        let candidates = self
            .rx_group_audio
            .lock()
            .map_or_else(|_| vec![0], |mixer| mixer.candidate_rocs(ssrc, seq));
        let mut decoded = None;
        let mut fallback_key_used = false;
        let mut auth_profile = (0usize, 10usize);
        let profile_to_try = self.group_audio_srtp_profile.or_else(|| {
            (self.group_audio_srtp_profile_discovery_attempts >= GROUP_SRTP_PROFILE_DISCOVERY_LIMIT)
                .then_some((0, 10))
        });
        if self.group_audio_srtp_profile.is_none()
            && self.group_audio_srtp_profile_discovery_attempts < GROUP_SRTP_PROFILE_DISCOVERY_LIMIT
        {
            self.group_audio_srtp_profile_discovery_attempts += 1;
        }
        if let (Ok(keys), Ok(fallback)) = (self.rx_keys.lock(), self.rx_fallback_keys.lock()) {
            let primary = keys.as_ref();
            let fallback = fallback.as_ref();
            if let Some((clear, roc, used_fallback, mki_len, tag_len)) =
                unprotect_group_with_candidates(
                    rtp_body,
                    primary,
                    fallback,
                    &candidates,
                    profile_to_try,
                )
            {
                decoded = Some((clear.seq, clear.ssrc, roc, clear.opus));
                fallback_key_used = used_fallback;
                auth_profile = (mki_len, tag_len);
                self.group_audio_srtp_profile = Some(auth_profile);
            }
            // Preserve legacy plain-RTP mode only when
            // zrtcConfig.srtpKey is absent. Try the
            // authenticated session candidate first.
            if decoded.is_none() && primary.is_none() {
                if let Some((clear_seq, clear_ssrc, opus)) = read_plain_rtp_payload(rtp_body) {
                    decoded = Some((clear_seq, clear_ssrc, candidates[0], opus));
                }
            }
        }
        if let Some((clear_seq, clear_ssrc, _, opus)) = decoded.as_ref() {
            self.group_rx_audio_decoded = self.group_rx_audio_decoded.saturating_add(1);
            if !self.audio_auth.reported {
                media_debug(&format!(
                                                    "group-audio-auth=ok ssrc={clear_ssrc} seq={clear_seq} key_source={} mki_bytes={} tag_bytes={} opus_bytes={}",
                                                    if fallback_key_used { "session" } else { "group" },
                                                    auth_profile.0,
                                                    auth_profile.1,
                                                    opus.len()
                                                ));
                self.audio_auth.reported = true;
            }
        } else {
            self.group_rx_audio_bad = self.group_rx_audio_bad.saturating_add(1);
            if !self.audio_auth.failed_reported {
                if std::env::var("ZCALL_GROUP_AUTH_DUMP").is_ok_and(|value| value == "1") {
                    let packet_hex = hex::encode(rtp_body);
                    media_debug(&format!("group-audio-auth-dump={packet_hex}"));
                }
                let fallback_probe = self
                    .rx_fallback_keys
                    .lock()
                    .ok()
                    .and_then(|keys| {
                        keys.as_ref()
                            .map(|keys| probe_srtp_auth_layout(rtp_body, keys))
                    })
                    .unwrap_or_else(|| "no-keys".into());
                media_debug(&format!(
                                                    "group-audio-auth=failed ssrc={ssrc} seq={seq} srtp_keys={} fallback_probe={} rtp_bytes={}",
                                                    self.rx_keys.lock().is_ok_and(|keys| keys.is_some()),
                                                    fallback_probe,
                                                    rtp_body.len()
                                                ));
                self.audio_auth.failed_reported = true;
            }
        }
        decoded
    }

    fn deliver_group_audio_packet(
        &mut self,
        rtp_body: &[u8],
        from: SocketAddr,
        direct: bool,
        params_snapshot: Option<&MediaSessionParams>,
        decoded: Option<(u16, u32, u32, Vec<u8>)>,
    ) -> bool {
        if let Some((clear_seq, clear_ssrc, roc, opus)) = decoded {
            if self
                .rx_share_peer
                .lock()
                .ok()
                .and_then(|peer| peer.as_ref().map(|peer| peer.uid))
                == Some(clear_ssrc)
                || self.rx_remote_share_uid.load(Ordering::Acquire) == clear_ssrc
            {
                return true;
            }
            let ext_seq = (roc << 16) | u32::from(clear_seq);
            self.last_audio_rx = Instant::now();
            self.rx_last_media_rx_ms
                .store(unix_now_ms(), Ordering::Release);
            let new_source = if direct {
                self.relay_audio_streak = 0;
                if let Ok(mut path) = self.rx_p2p_path.lock() {
                    *path = Some(from);
                }
                true
            } else {
                if self.rx_p2p_path.lock().is_ok_and(|path| path.is_some()) {
                    self.relay_audio_streak = self.relay_audio_streak.saturating_add(1);
                    if self.relay_audio_streak >= 25 {
                        let selected = *self.rx_selected.lock().ok().as_deref().unwrap_or(&None);
                        if let Some(params) = params_snapshot {
                            stop_p2p_path(
                                &self.rx_socket,
                                selected,
                                &self.rx_p2p_path,
                                &self.rx_p2p_enabled,
                                params,
                                true,
                            );
                        }
                        self.relay_audio_streak = 0;
                    }
                }
                false
            };
            let _ = new_source;
            if self.audio_dedup.accept(clear_ssrc, ext_seq) {
                self.rx_audio_second = self.rx_audio_second.saturating_add(1);
                self.rx_audio_packets.fetch_add(1, Ordering::Relaxed);
                if params_snapshot
                    .as_ref()
                    .is_some_and(|params| params.uid != clear_ssrc)
                    && self.member_audio_seen.insert(clear_ssrc)
                {
                    if let Ok(mut events) = self.rx_group_events.lock() {
                        events.push_back(GroupMediaEvent::MemberAudio(clear_ssrc));
                    }
                }
                if let Some(level) = parse_rfc6464_audio_level(rtp_body) {
                    if let Some(on) = self
                        .member_speaking
                        .entry(clear_ssrc)
                        .or_default()
                        .observe(level, Instant::now())
                    {
                        if let Ok(mut events) = self.rx_group_events.lock() {
                            events.push_back(GroupMediaEvent::MemberSpeaking(clear_ssrc, on));
                        }
                    }
                }
                if let Ok(mut mixer) = self.rx_group_audio.lock() {
                    mixer.push_packet(clear_ssrc, clear_seq, roc, opus);
                }
                if let Ok(mut feedback) = self.rx_rtcp.lock() {
                    if let Some(feedback) = feedback.as_mut() {
                        feedback.on_rtp(false, rtp_body, unix_now_ms());
                    }
                }
            }
        }
        false
    }

    fn handle_peer_audio_rtp(
        &mut self,
        rtp_body: &[u8],
        from: SocketAddr,
        direct: bool,
        params_snapshot: Option<&MediaSessionParams>,
    ) -> bool {
        let mut decoded = None;
        if let Ok(keys) = self.rx_keys.lock() {
            if let Some(keys) = keys.as_ref() {
                decoded = unprotect_with_roc_recovery(rtp_body, keys, &mut self.audio_roc)
                    .map(|(clear, roc)| (clear.seq, clear.ssrc, roc, clear.opus));
            } else if let Some((clear_seq, clear_ssrc, opus)) = read_plain_rtp_payload(rtp_body) {
                let roc = self.audio_roc.guess(clear_seq);
                self.audio_roc.update(clear_seq, roc);
                decoded = Some((clear_seq, clear_ssrc, roc, opus));
            }
        }
        if let Some((clear_seq, clear_ssrc, roc, opus)) = decoded {
            if self
                .rx_share_peer
                .lock()
                .ok()
                .and_then(|peer| peer.as_ref().map(|peer| peer.uid))
                == Some(clear_ssrc)
                || self.rx_remote_share_uid.load(Ordering::Acquire) == clear_ssrc
            {
                return true;
            }
            let ext_seq = (roc << 16) | u32::from(clear_seq);
            self.last_audio_rx = Instant::now();
            if direct {
                self.relay_audio_streak = 0;
                if let Ok(mut path) = self.rx_p2p_path.lock() {
                    *path = Some(from);
                }
            } else if self.rx_p2p_path.lock().is_ok_and(|path| path.is_some()) {
                self.relay_audio_streak = self.relay_audio_streak.saturating_add(1);
                if self.relay_audio_streak >= 25 {
                    let selected = *self.rx_selected.lock().unwrap();
                    if let Some(params) = params_snapshot {
                        stop_p2p_path(
                            &self.rx_socket,
                            selected,
                            &self.rx_p2p_path,
                            &self.rx_p2p_enabled,
                            params,
                            true,
                        );
                    }
                    self.relay_audio_streak = 0;
                }
            }

            if self.audio_dedup.accept(clear_ssrc, ext_seq) {
                self.rx_last_media_rx_ms
                    .store(unix_now_ms(), Ordering::Release);
                self.rx_audio_second = self.rx_audio_second.saturating_add(1);
                self.rx_audio_packets.fetch_add(1, Ordering::Relaxed);
                let early_caller = params_snapshot.is_some_and(|params| {
                    params.role == MediaRole::Caller && params.kind != MediaKind::Group
                });
                if early_caller && !self.rx_audio_enabled.load(Ordering::Acquire) {
                    self.early_audio_rx_count = self.early_audio_rx_count.saturating_add(1);
                    if let Some(phase) =
                        early_audio_phase(true, false, false, self.early_audio_rx_count)
                    {
                        if let Ok(mut events) = self.rx_group_events.lock() {
                            events.push_back(GroupMediaEvent::EarlyAudio(phase));
                        }
                    }
                }
                if let Ok(mut feedback) = self.rx_rtcp.lock() {
                    if let Some(feedback) = feedback.as_mut() {
                        feedback.on_rtp(false, rtp_body, unix_now_ms());
                    }
                }
                if let Some(jitter) = self.rx_jitter_buffer.as_ref() {
                    if let Ok(mut jitter) = jitter.lock() {
                        jitter.push(ext_seq, opus);
                    }
                }
            }
        }
        false
    }

    fn decode_video_rtp(
        &mut self,
        rtp_body: &[u8],
        ssrc: u32,
        is_share: bool,
        is_group_video: bool,
    ) -> VideoRtpDecode {
        let mut auth_profile = (0usize, 10usize);
        let profile_to_try = self.group_video_srtp_profile.or_else(|| {
            (self.group_video_srtp_profile_discovery_attempts >= GROUP_SRTP_PROFILE_DISCOVERY_LIMIT)
                .then_some((0, 10))
        });
        if is_group_video
            && self.group_video_srtp_profile.is_none()
            && self.group_video_srtp_profile_discovery_attempts < GROUP_SRTP_PROFILE_DISCOVERY_LIMIT
        {
            self.group_video_srtp_profile_discovery_attempts += 1;
        }
        let (decoded, fallback_key_used) = if is_share {
            let tracker = self.video_rocs.entry(ssrc).or_default();
            let decoded =
                read_plain_rtp_payload(rtp_body).map(|(sequence, media_ssrc, payload)| {
                    let roc = tracker.guess(sequence);
                    tracker.update(sequence, roc);
                    DecodedVideoRtp {
                        sequence,
                        ssrc: media_ssrc,
                        roc,
                        payload,
                    }
                });
            (decoded, false)
        } else {
            let mut fallback_key_used = false;
            let mut decoded = None;
            if let (Ok(keys), Ok(fallback)) = (self.rx_keys.lock(), self.rx_fallback_keys.lock()) {
                let tracker = self.video_rocs.entry(ssrc).or_default();
                let sequence = u16::from_be_bytes([rtp_body[2], rtp_body[3]]);
                let rocs = tracker.candidates(sequence);
                if let Some((clear, roc, used_fallback, mki_len, tag_len)) =
                    unprotect_group_with_candidates(
                        rtp_body,
                        keys.as_ref(),
                        fallback.as_ref(),
                        &rocs,
                        profile_to_try,
                    )
                {
                    tracker.update(sequence, roc);
                    decoded = Some(DecodedVideoRtp {
                        sequence: clear.seq,
                        ssrc: clear.ssrc,
                        roc,
                        payload: clear.opus,
                    });
                    fallback_key_used = used_fallback;
                    auth_profile = (mki_len, tag_len);
                    self.group_video_srtp_profile = Some(auth_profile);
                }
                // Legacy plain RTP is valid only if no primary SRTP key is configured.
                if decoded.is_none() && keys.is_none() {
                    decoded =
                        read_plain_rtp_payload(rtp_body).map(|(sequence, media_ssrc, payload)| {
                            let roc = tracker.guess(sequence);
                            tracker.update(sequence, roc);
                            DecodedVideoRtp {
                                sequence,
                                ssrc: media_ssrc,
                                roc,
                                payload,
                            }
                        });
                }
            }
            (decoded, fallback_key_used)
        };
        VideoRtpDecode {
            packet: decoded,
            fallback_key_used,
            auth_profile,
        }
    }

    fn record_video_auth_result(
        &mut self,
        rtp_body: &[u8],
        ssrc: u32,
        pt: u8,
        marker: bool,
        is_group_video: bool,
        decode: &VideoRtpDecode,
    ) {
        if let Some(packet) = decode.packet.as_ref() {
            self.rx_video_decoded = self.rx_video_decoded.saturating_add(1);
            if is_group_video {
                self.group_rx_video_decoded = self.group_rx_video_decoded.saturating_add(1);
                if !self.video_auth.reported {
                    media_debug(&format!(
                        "group-video-auth=ok ssrc={} seq={} pt={pt} marker={marker} key_source={} mki_bytes={} tag_bytes={} payload_bytes={}",
                        packet.ssrc,
                        packet.sequence,
                        if decode.fallback_key_used { "session" } else { "group" },
                        decode.auth_profile.0,
                        decode.auth_profile.1,
                        packet.payload.len()
                    ));
                    self.video_auth.reported = true;
                }
            }
            return;
        }
        self.rx_video_bad = self.rx_video_bad.saturating_add(1);
        if !is_group_video {
            return;
        }
        self.group_rx_video_bad = self.group_rx_video_bad.saturating_add(1);
        if self.video_auth.failed_reported {
            return;
        }
        // Opt-in, one-packet capture for diagnosing SFU-side SRTP/header mutation.
        if std::env::var("ZCALL_GROUP_AUTH_DUMP").is_ok_and(|value| value == "1") {
            media_debug(&format!("group-video-auth-dump={}", hex::encode(rtp_body)));
        }
        let header_len = parse_rtp_header_len(rtp_body).unwrap_or(0);
        let header_hex = hex::encode(rtp_body.get(..header_len.min(32)).unwrap_or_default());
        let auth_fp = self
            .rx_keys
            .lock()
            .ok()
            .and_then(|keys| keys.as_ref().map(|keys| sha1(&[&keys.auth])))
            .map_or_else(|| "none".into(), |digest| hex::encode(&digest[..6]));
        let auth_probe = self
            .rx_keys
            .lock()
            .ok()
            .and_then(|keys| {
                keys.as_ref()
                    .map(|keys| probe_srtp_auth_layout(rtp_body, keys))
            })
            .unwrap_or_else(|| "no-keys".into());
        let fallback_probe = self
            .rx_fallback_keys
            .lock()
            .ok()
            .and_then(|keys| {
                keys.as_ref()
                    .map(|keys| probe_srtp_auth_layout(rtp_body, keys))
            })
            .unwrap_or_else(|| "no-keys".into());
        let plain_h264_candidate = read_plain_rtp_payload(rtp_body)
            .is_some_and(|(_, _, payload)| looks_like_group_h264_payload(&payload));
        media_debug(&format!(
            "group-video-auth=failed ssrc={ssrc} seq={} pt={pt} marker={marker} plain_h264_candidate={plain_h264_candidate} srtp_keys={} auth_fp={} auth_probe={} fallback_probe={} rtp_bytes={} rtp_header_bytes={header_len} rtp_header={header_hex}",
            u16::from_be_bytes([rtp_body[2], rtp_body[3]]),
            self.rx_keys.lock().is_ok_and(|keys| keys.is_some()),
            auth_fp, auth_probe, fallback_probe, rtp_body.len()
        ));
        self.video_auth.failed_reported = true;
    }

    fn deliver_video_rtp(
        &mut self,
        packet: &DecodedVideoRtp,
        rtp_body: &[u8],
        context: VideoRtpDeliveryContext,
    ) {
        let VideoRtpDeliveryContext {
            source,
            is_group,
            is_share,
            payload_type,
            marker,
        } = context;
        self.rx_last_media_rx_ms
            .store(unix_now_ms(), Ordering::Release);
        if source.direct {
            if let Ok(mut path) = self.rx_p2p_path.lock() {
                *path = Some(source.from);
            }
        }
        let now_ms = unix_now_ms();
        if let Ok(mut feedback) = self.rx_rtcp.lock() {
            if let Some(feedback) = feedback.as_mut() {
                feedback.on_rtp(true, rtp_body, now_ms);
            }
        }
        self.send_video_nack_feedback(packet.ssrc, packet.sequence, now_ms);
        if is_group || payload_type == VIDEO_PT_KEY || payload_type == VIDEO_PT_INTER {
            let frames = self.assemble_video_frames(
                packet,
                rtp_body,
                is_group,
                is_share,
                payload_type,
                marker,
            );
            self.queue_video_frames(frames, packet.ssrc, is_group);
        }
    }

    fn send_video_nack_feedback(&mut self, ssrc: u32, sequence: u16, now_ms: u64) {
        let Ok(mut tracker) = self.rx_nack.lock() else {
            return;
        };
        let requests = tracker.note(ssrc, sequence, now_ms);
        if requests.is_empty() {
            return;
        }
        let Some((server, token)) = *self.rx_selected.lock().unwrap() else {
            return;
        };
        let Some(params) = self.rx_params.lock().unwrap().clone() else {
            return;
        };
        for (media_ssrc, missing) in requests {
            let report = build_nack_many(params.uid, media_ssrc, &missing);
            let packet = wrap_media_packet(msg_types::VIDEO_FEC, token, &report);
            let _ = self.rx_socket.send_to(&packet, server);
            if let Some(peer) = self.rx_p2p_path.lock().ok().and_then(|path| *path) {
                let direct_packet = p2p::wrap(
                    params.role == MediaRole::Callee,
                    params.call_id,
                    p2p::VIDEO_RTCP,
                    &report,
                );
                let _ = self.rx_socket.send_to(&direct_packet, peer);
            }
        }
    }

    fn assemble_video_frames(
        &mut self,
        packet: &DecodedVideoRtp,
        rtp_body: &[u8],
        is_group: bool,
        is_share: bool,
        pt: u8,
        marker: bool,
    ) -> Vec<VideoFrame> {
        let timestamp = u32::from_be_bytes([rtp_body[4], rtp_body[5], rtp_body[6], rtp_body[7]]);
        let is_new_video_source =
            is_group && !is_share && !self.video_assemblers.contains_key(&packet.ssrc);
        if is_new_video_source {
            if let Ok(mut events) = self.rx_group_events.lock() {
                events.push_back(GroupMediaEvent::MemberVideo(packet.ssrc));
            }
        }
        let (frames, request_keyframe) = {
            let assembler = self
                .video_assemblers
                .entry(packet.ssrc)
                .or_insert_with(|| VideoAssembler::with_group(is_group));
            let frames = assembler.push_packet(
                packet.sequence,
                packet.roc,
                timestamp,
                &packet.payload,
                marker,
                pt,
            );
            let request_keyframe = assembler.take_keyframe_request();
            (frames, request_keyframe)
        };
        if is_group && !frames.is_empty() && !self.video_auth.frame_reported {
            let frame = &frames[0];
            media_debug(&format!(
                "group-video-frame=assembled ssrc={} frames={} key={} bytes={}",
                packet.ssrc,
                frames.len(),
                frame.key,
                frame.data.len()
            ));
            self.video_auth.frame_reported = true;
        }
        if request_keyframe {
            self.request_video_keyframe(packet.ssrc);
        }
        frames
    }

    fn request_video_keyframe(&mut self, ssrc: u32) {
        let now = Instant::now();
        let can_request = self
            .last_video_pli
            .get(&ssrc)
            .is_none_or(|last| now.duration_since(*last) >= Duration::from_secs(1));
        if !can_request {
            return;
        }
        self.last_video_pli.insert(ssrc, now);
        let report = self
            .rx_rtcp
            .lock()
            .ok()
            .and_then(|mut feedback| feedback.as_mut().map(|feedback| feedback.pli(ssrc)));
        let Some(report) = report else {
            return;
        };
        let Some((server, token)) = *self.rx_selected.lock().unwrap() else {
            return;
        };
        let packet = wrap_media_packet(msg_types::VIDEO_FEC, token, &report);
        let _ = self.rx_socket.send_to(&packet, server);
        if let Some(peer) = self.rx_p2p_path.lock().ok().and_then(|path| *path) {
            if let Some(params) = self.rx_params.lock().ok().and_then(|params| params.clone()) {
                let direct_packet = p2p::wrap(
                    params.role == MediaRole::Callee,
                    params.call_id,
                    p2p::VIDEO_RTCP,
                    &report,
                );
                let _ = self.rx_socket.send_to(&direct_packet, peer);
            }
        }
    }

    fn queue_video_frames(&mut self, frames: Vec<VideoFrame>, ssrc: u32, is_group: bool) {
        if let Ok(mut queue) = self.video_frames.lock() {
            for mut frame in frames {
                if is_group {
                    frame.src = Some(ssrc);
                }
                if queue.len() >= 8 {
                    queue.pop_front();
                }
                queue.push_back(frame);
            }
        }
    }
}

#[derive(Clone)]
pub struct MediaSessionParams {
    pub call_id: u32,
    pub host_call: u32,
    pub uid: u32,
    pub peer_uid: u32,
    pub sess_id: Vec<u8>,
    pub role: MediaRole,
    pub kind: MediaKind,
    pub send_udp: bool,
    pub video_enabled: bool,
    pub video_config: Value,
    pub p2p_candidates: Vec<SocketAddr>,
}

type VideoHistory = Arc<Mutex<VecDeque<(u16, Vec<u8>)>>>;

#[derive(Default)]
struct VideoTxState {
    seq: u16,
    roc: u32,
    layer_seq: u16,
    started_at: Option<Instant>,
    key_sent: bool,
}

struct VideoTxContext<'a> {
    params: &'a MediaSessionParams,
    server: SocketAddr,
    token: u32,
    keys: Option<&'a SrtpKeys>,
    group_plain: bool,
    plan: Option<&'a GroupVideoPlan>,
    chunks: &'a [Vec<u8>],
    key: bool,
    state: &'a mut VideoTxState,
}

#[derive(Clone, Copy)]
struct VideoHeaderContext<'a> {
    params: &'a MediaSessionParams,
    group_plain: bool,
    plan: Option<&'a GroupVideoPlan>,
    payload_type: u8,
    capture_ms: u64,
}

struct GroupSharePeer {
    socket: Arc<UdpSocket>,
    server: SocketAddr,
    uid: u32,
    token: u32,
    sess_id: Vec<u8>,
    call_id: u32,
    active: Arc<AtomicBool>,
    seq: u16,
    twcc_seq: u16,
    layer_seq: u16,
    control_seq: u32,
    started_at: Option<Instant>,
    timestamp_base: u32,
    last_ping: Instant,
    width: u16,
    height: u16,
    max_shr_screen_br: u32,
    sfu_id: u8,
    sfu_long: bool,
    key_sent: bool,
    keyframe_requested: bool,
    queue: VecDeque<Vec<u8>>,
}

pub struct ZrtcMediaBackend {
    socket: Arc<UdpSocket>,
    running: Arc<AtomicBool>,
    audio_enabled: Arc<AtomicBool>,
    selected_server: Arc<Mutex<Option<(SocketAddr, u32)>>>,
    ring_servers: Arc<Mutex<Vec<(SocketAddr, u32)>>>,
    keys: Arc<Mutex<Option<SrtpKeys>>>,
    // Group SFU receive can use a distinct session master from zrtcConfig.srtpKey.
    // Keep it receive-only and accept it only after SRTP authentication succeeds.
    rx_fallback_keys: Arc<Mutex<Option<SrtpKeys>>>,
    params: Arc<Mutex<Option<MediaSessionParams>>>,
    last_ping: Arc<Mutex<Instant>>,
    ring_ping_running: Arc<AtomicBool>,
    public_addr: Arc<Mutex<Option<String>>>,
    muted: Arc<AtomicBool>,
    speaker_off: Arc<AtomicBool>,
    devices: Arc<Mutex<(Option<String>, Option<String>)>>,
    device_generation: Arc<AtomicU64>,
    video_frames: Arc<Mutex<VecDeque<VideoFrame>>>,
    video_history: VideoHistory,
    video_tx: Mutex<VideoTxState>,
    twcc_seq: Arc<AtomicU16>,
    rtcp: Arc<Mutex<Option<RtcpFeedback>>>,
    nack: Arc<Mutex<NackTracker>>,
    local_keyframe_request: Arc<AtomicBool>,
    last_peer_pli: Mutex<Option<Instant>>,
    p2p_path: Arc<Mutex<Option<SocketAddr>>>,
    p2p_enabled: Arc<AtomicBool>,
    p2p_seq: Arc<AtomicU16>,
    send_rate: Arc<Mutex<Option<SendRateController>>>,
    pending_video_profile: Arc<Mutex<Option<VideoProfile>>>,
    group_video_plan: Arc<Mutex<Option<GroupVideoPlan>>>,
    group_video_wanted: Arc<Mutex<HashMap<u32, u8>>>,
    group_video_render_width: Arc<Mutex<HashMap<u32, u32>>>,
    group_control_seq: Arc<AtomicU32>,
    group_events: Arc<Mutex<VecDeque<GroupMediaEvent>>>,
    share_peer: Arc<Mutex<Option<GroupSharePeer>>>,
    remote_share_uid: Arc<AtomicU32>,
    last_video_sent_ms: Arc<AtomicU64>,
    last_media_rx_ms: Arc<AtomicU64>,
    first_video_ui_drain_logged: AtomicBool,
    mic_volume: Arc<AtomicU8>,
    mic_volume_dirty: Arc<AtomicBool>,
    mic_manual: Arc<AtomicBool>,
    pending_mic_volume: Arc<Mutex<Option<u8>>>,
    mic_level: Arc<AtomicU8>,
    mic_level_dirty: Arc<AtomicBool>,
    tx_audio_packets: Arc<AtomicU64>,
    rx_audio_packets: Arc<AtomicU64>,
    tx_video_packets: Arc<AtomicU64>,
}

fn build_video_rtp_header(
    context: VideoHeaderContext<'_>,
    sequence: u16,
    timestamp: u32,
    twcc: u16,
    marker: bool,
    layer_sequence: &mut u16,
) -> Vec<u8> {
    let VideoHeaderContext {
        params,
        group_plain,
        plan,
        payload_type,
        capture_ms,
    } = context;
    if group_plain {
        if let Some(plan) = plan.filter(|plan| plan.sfu_id.is_some()) {
            *layer_sequence = layer_sequence.wrapping_add(1);
            build_group_video_rtp_header(
                sequence,
                timestamp,
                params.uid,
                twcc,
                marker,
                payload_type,
                plan.sfu_long,
                plan.sfu_id.unwrap_or(0),
                *layer_sequence,
                capture_ms,
                unix_now_ms(),
            )
        } else {
            build_audio_rtp_header(sequence, timestamp, params.uid, twcc, marker, payload_type)
                .to_vec()
        }
    } else {
        build_audio_rtp_header(sequence, timestamp, params.uid, twcc, marker, payload_type).to_vec()
    }
}

impl ZrtcMediaBackend {
    pub fn new() -> Result<Self, String> {
        let socket =
            UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("Failed to bind UDP socket: {e}"))?;
        socket
            .set_read_timeout(Some(Duration::from_millis(20)))
            .map_err(|e| format!("Failed to set socket read timeout: {e}"))?;

        let backend = Self {
            socket: Arc::new(socket),
            running: Arc::new(AtomicBool::new(false)),
            audio_enabled: Arc::new(AtomicBool::new(false)),
            selected_server: Arc::new(Mutex::new(None)),
            ring_servers: Arc::new(Mutex::new(Vec::new())),
            keys: Arc::new(Mutex::new(None)),
            rx_fallback_keys: Arc::new(Mutex::new(None)),
            params: Arc::new(Mutex::new(None)),
            last_ping: Arc::new(Mutex::new(Instant::now())),
            ring_ping_running: Arc::new(AtomicBool::new(false)),
            public_addr: Arc::new(Mutex::new(None)),
            muted: Arc::new(AtomicBool::new(false)),
            speaker_off: Arc::new(AtomicBool::new(false)),
            devices: Arc::new(Mutex::new((None, None))),
            device_generation: Arc::new(AtomicU64::new(0)),
            video_frames: Arc::new(Mutex::new(VecDeque::new())),
            video_history: Arc::new(Mutex::new(VecDeque::new())),
            video_tx: Mutex::new(VideoTxState::default()),
            twcc_seq: Arc::new(AtomicU16::new(0)),
            rtcp: Arc::new(Mutex::new(None)),
            nack: Arc::new(Mutex::new(NackTracker::default())),
            local_keyframe_request: Arc::new(AtomicBool::new(false)),
            last_peer_pli: Mutex::new(None),
            p2p_path: Arc::new(Mutex::new(None)),
            p2p_enabled: Arc::new(AtomicBool::new(false)),
            p2p_seq: Arc::new(AtomicU16::new(0)),
            send_rate: Arc::new(Mutex::new(None)),
            pending_video_profile: Arc::new(Mutex::new(None)),
            group_video_plan: Arc::new(Mutex::new(None)),
            group_video_wanted: Arc::new(Mutex::new(HashMap::new())),
            group_video_render_width: Arc::new(Mutex::new(HashMap::new())),
            group_control_seq: Arc::new(AtomicU32::new(0)),
            group_events: Arc::new(Mutex::new(VecDeque::new())),
            share_peer: Arc::new(Mutex::new(None)),
            remote_share_uid: Arc::new(AtomicU32::new(0)),
            last_video_sent_ms: Arc::new(AtomicU64::new(0)),
            last_media_rx_ms: Arc::new(AtomicU64::new(0)),
            first_video_ui_drain_logged: AtomicBool::new(false),
            mic_volume: Arc::new(AtomicU8::new(100)),
            mic_volume_dirty: Arc::new(AtomicBool::new(true)),
            mic_manual: Arc::new(AtomicBool::new(false)),
            pending_mic_volume: Arc::new(Mutex::new(None)),
            mic_level: Arc::new(AtomicU8::new(127)),
            mic_level_dirty: Arc::new(AtomicBool::new(false)),
            tx_audio_packets: Arc::new(AtomicU64::new(0)),
            rx_audio_packets: Arc::new(AtomicU64::new(0)),
            tx_video_packets: Arc::new(AtomicU64::new(0)),
        };

        Ok(backend)
    }

    pub fn prepare(
        &self,
        params: MediaSessionParams,
        servers: &[SocketAddr],
    ) -> Result<Option<SocketAddr>, String> {
        media_debug_reset();
        media_debug(&format!(
            "session-start mode={} uid={} video_enabled={} send_udp={} relay_candidates={}",
            if params.kind == MediaKind::Group {
                "group"
            } else {
                "one-to-one"
            },
            params.uid,
            params.video_enabled,
            params.send_udp,
            servers.len()
        ));
        self.reset_media_session(&params)?;

        if !params.send_udp {
            let selected = servers.first().copied();
            *self.params.lock().map_err(|e| e.to_string())? = Some(params);
            if let Some(addr) = selected {
                *self.selected_server.lock().map_err(|e| e.to_string())? = Some((addr, 0));
            }
            return Ok(selected);
        }
        if params.kind == MediaKind::Group {
            return self.prepare_group(params, servers);
        }
        self.prepare_one_to_one(params, servers)
    }

    fn reset_media_session(&self, params: &MediaSessionParams) -> Result<(), String> {
        self.audio_enabled.store(false, Ordering::Release);
        self.tx_audio_packets.store(0, Ordering::Relaxed);
        self.rx_audio_packets.store(0, Ordering::Relaxed);
        self.tx_video_packets.store(0, Ordering::Relaxed);
        self.stop_group_share();
        self.remote_share_uid.store(0, Ordering::Release);
        self.ring_ping_running.store(false, Ordering::SeqCst);
        *self.ring_servers.lock().map_err(|e| e.to_string())? = Vec::new();
        *self.selected_server.lock().map_err(|e| e.to_string())? = None;
        *self.public_addr.lock().map_err(|e| e.to_string())? = None;
        *self.keys.lock().map_err(|e| e.to_string())? = None;
        *self.rx_fallback_keys.lock().map_err(|e| e.to_string())? = None;
        self.video_tx
            .lock()
            .map_err(|e| e.to_string())
            .map(|mut state| *state = VideoTxState::default())?;
        self.video_frames.lock().map_err(|e| e.to_string())?.clear();
        self.video_history
            .lock()
            .map_err(|e| e.to_string())?
            .clear();
        *self.rtcp.lock().map_err(|e| e.to_string())? =
            params.send_udp.then(|| RtcpFeedback::new(params.uid));
        *self.send_rate.lock().map_err(|e| e.to_string())? =
            if params.send_udp && params.video_enabled {
                Some(SendRateController::new(&params.video_config, params.uid))
            } else {
                None
            };
        *self
            .pending_video_profile
            .lock()
            .map_err(|e| e.to_string())? = None;
        self.last_video_sent_ms.store(0, Ordering::Relaxed);
        self.last_media_rx_ms.store(0, Ordering::Relaxed);
        self.first_video_ui_drain_logged
            .store(false, Ordering::Relaxed);
        let group_plan = if params.kind == MediaKind::Group {
            parse_group_video_plan(&params.video_config)
        } else {
            None
        };
        *self.group_video_plan.lock().map_err(|e| e.to_string())? = group_plan;
        self.group_video_wanted
            .lock()
            .map_err(|e| e.to_string())?
            .clear();
        self.group_video_render_width
            .lock()
            .map_err(|e| e.to_string())?
            .clear();
        self.group_control_seq.store(0, Ordering::Relaxed);
        self.group_events.lock().map_err(|e| e.to_string())?.clear();
        self.nack.lock().map_err(|e| e.to_string())?.clear();
        self.local_keyframe_request.store(false, Ordering::Relaxed);
        *self.last_peer_pli.lock().map_err(|e| e.to_string())? = None;
        *self.p2p_path.lock().map_err(|e| e.to_string())? = None;
        self.p2p_enabled
            .store(!params.p2p_candidates.is_empty(), Ordering::Release);
        self.p2p_seq.store(0, Ordering::Relaxed);
        Ok(())
    }

    fn prepare_one_to_one(
        &self,
        params: MediaSessionParams,
        servers: &[SocketAddr],
    ) -> Result<Option<SocketAddr>, String> {
        let master = master_from_sess_id(&params.sess_id)
            .map_err(|err| format!("invalid call-v2 session id: {err}"))?;
        let session = session_keys(&master)
            .map_err(|err| format!("failed to derive SRTP session keys: {err}"))?;
        *self.keys.lock().map_err(|e| e.to_string())? = Some(session);
        *self.rx_fallback_keys.lock().map_err(|e| e.to_string())? = None;

        let init_pkt = build_init_zrtp_packet(
            params.role == MediaRole::Caller,
            params.uid,
            params.peer_uid,
            params.call_id,
            &params.sess_id,
            &[],
            0x02,
        );

        let allowed: HashSet<SocketAddr> = servers.iter().copied().collect();
        let mut responded = HashSet::new();
        let mut accepted: Vec<(SocketAddr, u32, Option<String>)> = Vec::new();
        let mut buf = [0u8; 1500];
        let started = Instant::now();
        let mut next_retry = Duration::from_millis(1000);
        let max_wait = Duration::from_millis(3200);
        let mut attempt = 0u8;

        while started.elapsed() < max_wait && responded.len() < allowed.len() {
            if attempt == 0 || (attempt < 3 && started.elapsed() >= next_retry) {
                for &server in servers {
                    if !responded.contains(&server) {
                        let _ = self.socket.send_to(&init_pkt, server);
                    }
                }
                attempt += 1;
                next_retry = Duration::from_millis(1000 * u64::from(attempt));
            }

            match self.socket.recv_from(&mut buf) {
                Ok((len, from)) => {
                    // Do not let an unrelated datagram select or poison the relay session.
                    if !allowed.contains(&from) || responded.contains(&from) {
                        continue;
                    }
                    if let Some(reply) = parse_server_reply(&buf[..len]) {
                        if reply.cmd != crate::protocol::zrtp::commands::INIT_CALL_CALLER
                            && reply.cmd != crate::protocol::zrtp::commands::INIT_CALL_CALLEE
                        {
                            continue;
                        }
                        responded.insert(from);
                        if reply.result == Some(0) {
                            if let Some(token) = reply.server_token {
                                accepted.push((from, token, reply.public_addr));
                            }
                        }
                    }
                }
                Err(err)
                    if err.kind() == std::io::ErrorKind::WouldBlock
                        || err.kind() == std::io::ErrorKind::TimedOut => {}
                Err(err) => return Err(format!("ZRTP receive failed: {err}")),
            }
        }

        // Preserve response arrival order: the first successful relay is the
        // lowest-latency path and is selected for the connected media session.
        let candidates: Vec<(SocketAddr, u32)> = accepted
            .iter()
            .map(|(addr, token, _)| (*addr, *token))
            .collect();
        let fastest = candidates.first().copied();
        if let Some((addr, token)) = fastest {
            let public = accepted.first().and_then(|(_, _, public)| public.clone());
            *self.selected_server.lock().map_err(|e| e.to_string())? = Some((addr, token));
            *self.public_addr.lock().map_err(|e| e.to_string())? = public;
            *self.ring_servers.lock().map_err(|e| e.to_string())? = candidates;
            self.start_ring_ping();
        } else {
            *self.selected_server.lock().map_err(|e| e.to_string())? = None;
            *self.ring_servers.lock().map_err(|e| e.to_string())? = Vec::new();
            *self.public_addr.lock().map_err(|e| e.to_string())? = None;
        }

        *self.params.lock().map_err(|e| e.to_string())? = Some(params);
        Ok(fastest.map(|(addr, _)| addr))
    }

    /// Group calls use the SFU's separate `InitZRTP` layout and derive media
    /// protection from zrtcConfig.srtpKey (or plain RTP when it is empty).
    /// Candidate layouts are advanced only after an explicit server refusal.
    fn prepare_group(
        &self,
        params: MediaSessionParams,
        servers: &[SocketAddr],
    ) -> Result<Option<SocketAddr>, String> {
        let plan = self.log_group_video_plan();
        let group_key = params
            .video_config
            .get("srtpKey")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Self::log_group_init_config(&params, servers, group_key, plan.as_ref());
        if servers.is_empty() {
            *self.params.lock().map_err(|e| e.to_string())? = Some(params);
            return Ok(None);
        }

        self.configure_group_srtp_keys(&params, group_key)?;
        let (layouts, forced) = Self::group_init_layouts()?;
        let expected_cmd = if params.role == MediaRole::Caller {
            crate::protocol::zrtp::commands::INIT_CALL_CALLER
        } else {
            crate::protocol::zrtp::commands::INIT_CALL_CALLEE
        };

        for layout in layouts {
            let attempt = self.send_group_init_layout(&params, servers, layout, expected_cmd)?;
            if let Some((server, token, public)) = attempt.accepted.first().cloned() {
                *self.selected_server.lock().map_err(|e| e.to_string())? = Some((server, token));
                *self.public_addr.lock().map_err(|e| e.to_string())? = public;
                *self.ring_servers.lock().map_err(|e| e.to_string())? = attempt
                    .accepted
                    .iter()
                    .map(|(address, media_token, _)| (*address, *media_token))
                    .collect();
                *self.params.lock().map_err(|e| e.to_string())? = Some(params);
                if forced.is_none() {
                    Self::save_group_init_layout(layout);
                }
                crate::zcall_log!(
                    "[zcall-rust] group InitZRTP accepted with layout {}",
                    layout.name()
                );
                media_debug(&format!(
                    "init-accepted layout={} accepted_relays={}",
                    layout.name(),
                    attempt.accepted.len()
                ));
                return Ok(Some(server));
            }

            if attempt.responded.is_empty() {
                // A silent relay is a network/route failure, not evidence that
                // another speculative payload layout will be accepted.
                *self.params.lock().map_err(|e| e.to_string())? = Some(params);
                return Ok(None);
            }
            crate::zcall_log!(
                "[zcall-rust] group InitZRTP layout {} refused by {}/{} server(s)",
                layout.name(),
                attempt.responded.len(),
                servers.iter().copied().collect::<HashSet<_>>().len()
            );
        }

        *self.selected_server.lock().map_err(|e| e.to_string())? = None;
        *self.ring_servers.lock().map_err(|e| e.to_string())? = Vec::new();
        *self.public_addr.lock().map_err(|e| e.to_string())? = None;
        *self.params.lock().map_err(|e| e.to_string())? = Some(params);
        Ok(None)
    }

    fn log_group_video_plan(&self) -> Option<GroupVideoPlan> {
        let plan = self
            .group_video_plan
            .lock()
            .ok()
            .and_then(|plan| plan.clone());
        media_debug(&format!(
            "group-video-plan sfu_id={:?} sfu_long={} encode={}x{}@{}fps send_layers={:?}",
            plan.as_ref().and_then(|plan| plan.sfu_id),
            plan.as_ref().is_some_and(|plan| plan.sfu_long),
            plan.as_ref().map_or(0, |plan| plan.encode.width),
            plan.as_ref().map_or(0, |plan| plan.encode.height),
            plan.as_ref().map_or(0, |plan| plan.encode.fps),
            plan.as_ref().map_or_default(|plan| {
                plan.send.iter().map(|layer| layer.id).collect::<Vec<_>>()
            })
        ));
        plan
    }

    fn log_group_init_config(
        params: &MediaSessionParams,
        servers: &[SocketAddr],
        group_key: &str,
        plan: Option<&GroupVideoPlan>,
    ) {
        let master_fp = sha1(&[group_key.as_bytes()]);
        // These SFU flags affect the wire SRTP profile. Log scalar metadata only.
        let config_scalar = |name: &str| {
            params.video_config.get(name).map_or_else(
                || "missing".to_string(),
                |value| match value {
                    Value::Null => "null".to_string(),
                    Value::Bool(value) => value.to_string(),
                    Value::Number(value) => value.to_string(),
                    Value::String(value) => format!("string(len={})", value.len()),
                    Value::Array(_) => "array".to_string(),
                    Value::Object(_) => "object".to_string(),
                },
            )
        };
        media_debug(&format!(
            "group-config uid={} video_enabled={} srtp={} key_bytes={} key_fp={} srtp_mode={} srtp_no_seq={} audio_srtp_no_seq={} relay_candidates={} video_layers={}",
            params.uid,
            params.video_enabled,
            !group_key.is_empty(),
            group_key.len(),
            hex::encode(&master_fp[..6]),
            config_scalar("srtpMode"),
            config_scalar("srtpNoSeq"),
            config_scalar("audSrtpNoSeq"),
            servers.len(),
            plan.map_or(0, |plan| plan.layers.len())
        ));
    }

    fn configure_group_srtp_keys(
        &self,
        params: &MediaSessionParams,
        group_key: &str,
    ) -> Result<(), String> {
        let primary = if group_key.is_empty() {
            None
        } else {
            let master = master_from_sess_id(group_key.as_bytes())
                .map_err(|error| format!("invalid group srtpKey: {error}"))?;
            Some(
                session_keys(&master)
                    .map_err(|error| format!("failed to derive group SRTP keys: {error}"))?,
            )
        };
        let primary_present = primary.is_some();
        *self.keys.lock().map_err(|e| e.to_string())? = primary;

        // Some SFU receive paths use callSetting.session as a receive-only
        // candidate. A failed HMAC never falls through as decrypted media.
        let fallback = if params.sess_id.as_slice() == group_key.as_bytes() {
            None
        } else {
            master_from_sess_id(&params.sess_id)
                .ok()
                .and_then(|master| session_keys(&master).ok())
        };
        let session_fp = sha1(&[&params.sess_id]);
        media_debug(&format!(
            "group-key-candidates primary={} fallback_session={} session_bytes={} session_fp={}",
            primary_present,
            fallback.is_some(),
            params.sess_id.len(),
            hex::encode(&session_fp[..6])
        ));
        *self.rx_fallback_keys.lock().map_err(|e| e.to_string())? = fallback;
        Ok(())
    }

    fn group_init_layouts() -> Result<(Vec<GroupInitLayout>, Option<String>), String> {
        let forced = std::env::var("ZCALL_GROUP_INIT_LAYOUT").ok();
        let mut layouts = Vec::new();
        if let Some(value) = forced.as_deref() {
            let layout = GroupInitLayout::parse(value)
                .ok_or_else(|| format!("unknown ZCALL_GROUP_INIT_LAYOUT: {value}"))?;
            layouts.push(layout);
        } else {
            let saved = std::env::var("ZCALL_LOG_DIR")
                .ok()
                .map(|directory| std::path::PathBuf::from(directory).join("group-init-layout"))
                .and_then(|path| std::fs::read_to_string(path).ok());
            if let Some(layout) = saved.as_deref().and_then(GroupInitLayout::parse) {
                layouts.push(layout);
            }
            for layout in GroupInitLayout::ALL {
                if !layouts.contains(&layout) {
                    layouts.push(layout);
                }
            }
        }
        Ok((layouts, forced))
    }

    fn save_group_init_layout(layout: GroupInitLayout) {
        if let Ok(directory) = std::env::var("ZCALL_LOG_DIR") {
            let path = std::path::PathBuf::from(directory);
            let _ = std::fs::create_dir_all(&path);
            let _ = std::fs::write(path.join("group-init-layout"), layout.name());
        }
    }

    fn send_group_init_layout(
        &self,
        params: &MediaSessionParams,
        servers: &[SocketAddr],
        layout: GroupInitLayout,
        expected_cmd: u16,
    ) -> Result<GroupLayoutAttempt, String> {
        let packet = build_group_init_zrtp_packet(
            params.role == MediaRole::Caller,
            params.uid,
            &params.sess_id,
            params.call_id,
            params.host_call,
            params.video_enabled,
            layout,
        );
        let allowed: HashSet<SocketAddr> = servers.iter().copied().collect();
        let mut attempt = GroupLayoutAttempt::default();
        let mut buf = [0u8; 1500];
        let started = Instant::now();
        let max_wait = Duration::from_millis(3200);
        let mut retry = 0u8;
        let mut next_retry = Duration::from_millis(1000);
        while started.elapsed() < max_wait && attempt.responded.len() < allowed.len() {
            if retry == 0 || (retry < 3 && started.elapsed() >= next_retry) {
                for server in servers {
                    if !attempt.responded.contains(server) {
                        let _ = self.socket.send_to(&packet, server);
                    }
                }
                retry += 1;
                next_retry = Duration::from_millis(1000 * u64::from(retry));
            }
            match self.socket.recv_from(&mut buf) {
                Ok((len, from)) => {
                    if !allowed.contains(&from) || attempt.responded.contains(&from) {
                        continue;
                    }
                    let Some(reply) = parse_server_reply(&buf[..len]) else {
                        continue;
                    };
                    if reply.cmd != expected_cmd {
                        continue;
                    }
                    attempt.responded.insert(from);
                    if reply.result == Some(0) {
                        if let Some(token) = reply.server_token {
                            attempt.accepted.push((from, token, reply.public_addr));
                        }
                    }
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::TimedOut => {}
                Err(error) => return Err(format!("group ZRTP receive failed: {error}")),
            }
        }
        Ok(attempt)
    }

    pub fn media_timed_out(&self) -> bool {
        let params = self.params.lock().ok().and_then(|params| params.clone());
        let Some(params) = params else {
            return false;
        };
        if params.kind == MediaKind::Group
            || !params.send_udp
            || !self.running.load(Ordering::Acquire)
        {
            return false;
        }
        media_receive_timed_out(self.last_media_rx_ms.load(Ordering::Acquire), unix_now_ms())
    }

    pub fn start(&self) {
        self.start_with_audio(true);
    }

    /// Start the packet transport without opening hardware audio devices.
    /// Used by transport-only tests and diagnostics; production calls use
    /// `start()`, which enables capture, decode and playback.
    pub fn start_with_audio(&self, audio_enabled: bool) {
        self.ring_ping_running.store(false, Ordering::SeqCst);
        self.finish_ring_selection();
        let send_udp = self
            .params
            .lock()
            .ok()
            .and_then(|p| p.as_ref().map(|p| p.send_udp))
            .unwrap_or(false);
        if !send_udp {
            return;
        }
        if self.running.swap(true, Ordering::SeqCst) {
            if audio_enabled && !self.audio_enabled.swap(true, Ordering::AcqRel) {
                let (microphone, speaker) = self
                    .devices
                    .lock()
                    .map_or_default(|devices| devices.clone());
                probe_audio_hardware(speaker.as_deref(), microphone.as_deref());
            }
            return;
        }
        self.audio_enabled.store(audio_enabled, Ordering::Release);
        self.last_media_rx_ms
            .store(unix_now_ms(), Ordering::Release);

        if audio_enabled {
            let (microphone, speaker) = self
                .devices
                .lock()
                .map_or_default(|devices| devices.clone());
            probe_audio_hardware(speaker.as_deref(), microphone.as_deref());
        }

        let muted = self.muted.clone();
        let speaker_off = self.speaker_off.clone();
        let running = self.running.clone();
        let socket = self.socket.clone();
        let selected_server = self.selected_server.clone();
        let keys = self.keys.clone();
        let params = self.params.clone();
        let last_ping = self.last_ping.clone();
        let tx_group_plan = self.group_video_plan.clone();
        let tx_group_wanted = self.group_video_wanted.clone();
        let tx_group_control_seq = self.group_control_seq.clone();
        let (apm, far_active) = Self::create_audio_processing(&params);
        spawn_p2p_bind_worker(
            self.params.lock().ok().and_then(|params| params.clone()),
            socket.clone(),
            running.clone(),
            self.p2p_path.clone(),
            self.p2p_enabled.clone(),
            self.p2p_seq.clone(),
        );

        let (jitter_buffer, group_audio) =
            self.spawn_audio_playout(&running, &params, speaker_off, apm.as_ref(), &far_active);

        self.spawn_receive_worker(ReceiveWorkerSetup {
            running: running.clone(),
            socket: socket.clone(),
            keys: keys.clone(),
            selected_server: selected_server.clone(),
            params: params.clone(),
            jitter_buffer: jitter_buffer.clone(),
            group_audio: group_audio.clone(),
        });

        // Audio recording & transmit thread
        let resources = AudioTransmitResources {
            tx_running: running,
            tx_audio_packets: self.tx_audio_packets.clone(),
            tx_socket: socket,
            tx_keys: keys,
            tx_params: params,
            tx_selected: selected_server,
            tx_muted: muted,
            tx_twcc: self.twcc_seq.clone(),
            tx_devices: self.devices.clone(),
            tx_device_generation: self.device_generation.clone(),
            tx_rtcp: self.rtcp.clone(),
            tx_p2p_path: self.p2p_path.clone(),
            tx_rate: self.send_rate.clone(),
            tx_last_video: self.last_video_sent_ms.clone(),
            tx_pending_profile: self.pending_video_profile.clone(),
            tx_apm: apm,
            tx_far_active: far_active,
            tx_mic_volume: self.mic_volume.clone(),
            tx_mic_volume_dirty: self.mic_volume_dirty.clone(),
            tx_mic_manual: self.mic_manual.clone(),
            tx_pending_mic_volume: self.pending_mic_volume.clone(),
            tx_mic_level: self.mic_level.clone(),
            tx_mic_level_dirty: self.mic_level_dirty.clone(),
            tx_audio_enabled: self.audio_enabled.clone(),
            tx_media_events: self.group_events.clone(),
            last_ping,
            tx_group_plan,
            tx_group_wanted,
            tx_group_control_seq,
        };
        thread::spawn(move || AudioTransmitWorker::new(resources).run());
    }

    fn create_audio_processing(
        params: &Arc<Mutex<Option<MediaSessionParams>>>,
    ) -> (Option<Arc<Apm>>, Arc<AtomicBool>) {
        let audio_config = params
            .lock()
            .ok()
            .and_then(|params| params.as_ref().map(|params| params.video_config.clone()))
            .unwrap_or(Value::Null);
        let far_active = Arc::new(AtomicBool::new(false));
        let aec_enabled = audio_config
            .get("audioEchoCancellation")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let ns_enabled = audio_config
            .get("audioNoiseSuppression")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let hpf_enabled = audio_config
            .get("audioHighPassFilter")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let apm = match Apm::new(
            SAMPLE_RATE.cast_signed(),
            1,
            aec_enabled,
            ns_enabled,
            hpf_enabled,
        ) {
            Ok(apm) => {
                crate::zcall_log!(
                    "[zcall-rust] Sonora audio processing enabled: AEC={aec_enabled} NS={ns_enabled} HPF={hpf_enabled}"
                );
                Some(Arc::new(apm))
            }
            Err(error) => {
                crate::zcall_log!("[zcall-rust] Sonora audio processing unavailable: {error}");
                None
            }
        };
        (apm, far_active)
    }

    fn spawn_audio_playout(
        &self,
        running: &Arc<AtomicBool>,
        params: &Arc<Mutex<Option<MediaSessionParams>>>,
        speaker_off: Arc<AtomicBool>,
        apm: Option<&Arc<Apm>>,
        far_active: &Arc<AtomicBool>,
    ) -> (
        Option<Arc<Mutex<JitterBuffer>>>,
        Arc<Mutex<GroupAudioMixer>>,
    ) {
        let params_for_playout = params.clone();
        let jitter_buffer = match JitterBuffer::new(SAMPLE_RATE.cast_signed(), 1, 10) {
            Ok(mut buffer) => {
                buffer.set_min_target(3);
                Some(Arc::new(Mutex::new(buffer)))
            }
            Err(error) => {
                crate::zcall_log!("[zcall-rust] audio jitter buffer unavailable: {error}");
                None
            }
        };
        let group_audio = Arc::new(Mutex::new(GroupAudioMixer::new()));
        spawn_playout_worker(
            running.clone(),
            self.audio_enabled.clone(),
            self.devices.clone(),
            self.device_generation.clone(),
            speaker_off,
            params_for_playout,
            jitter_buffer.clone(),
            group_audio.clone(),
            apm.cloned(),
            far_active.clone(),
            self.mic_volume.clone(),
            self.group_events.clone(),
        );

        (jitter_buffer, group_audio)
    }

    fn spawn_receive_worker(&self, setup: ReceiveWorkerSetup) {
        let ReceiveWorkerSetup {
            running,
            socket,
            keys,
            selected_server,
            params,
            jitter_buffer,
            group_audio,
        } = setup;
        // UDP receive thread: authenticate/depacketize and queue audio. Playback
        // is intentionally independent and follows the hardware clock.
        let rx_running = running.clone();
        let rx_audio_packets = self.rx_audio_packets.clone();
        let rx_socket = socket.clone();
        let rx_keys = keys.clone();
        let rx_fallback_keys = self.rx_fallback_keys.clone();
        let rx_selected = selected_server.clone();
        let video_frames = self.video_frames.clone();
        let rx_params = params.clone();
        let rx_rtcp = self.rtcp.clone();
        let rx_nack = self.nack.clone();
        let rx_keyframe_request = self.local_keyframe_request.clone();
        let rx_p2p_path = self.p2p_path.clone();
        let rx_p2p_enabled = self.p2p_enabled.clone();
        let rx_video_history = self.video_history.clone();
        let rx_group_events = self.group_events.clone();
        let rx_share_peer = self.share_peer.clone();
        let rx_remote_share_uid = self.remote_share_uid.clone();
        let rx_send_rate = self.send_rate.clone();
        let rx_last_media_rx_ms = self.last_media_rx_ms.clone();
        let rx_audio_enabled = self.audio_enabled.clone();
        let rx_tx_audio_packets = self.tx_audio_packets.clone();
        let rx_last_video_sent_ms = self.last_video_sent_ms.clone();
        let rx_tx_video_packets = self.tx_video_packets.clone();

        thread::spawn(move || {
            let worker = ReceiveWorker {
                rx_running,
                rx_audio_packets,
                rx_socket,
                rx_keys,
                rx_fallback_keys,
                rx_selected,
                video_frames,
                rx_params,
                rx_rtcp,
                rx_nack,
                rx_keyframe_request,
                rx_p2p_path,
                rx_p2p_enabled,
                rx_video_history,
                rx_group_events,
                rx_share_peer,
                rx_remote_share_uid,
                rx_send_rate,
                rx_last_media_rx_ms,
                rx_audio_enabled,
                rx_tx_audio_packets,
                rx_last_video_sent_ms,
                rx_tx_video_packets,
                rx_jitter_buffer: jitter_buffer,
                rx_group_audio: group_audio,
                video_assemblers: HashMap::new(),
                audio_roc: RocTracker::default(),
                video_rocs: HashMap::new(),
                last_video_pli: HashMap::new(),
                audio_dedup: AudioDeduplicator::new(256),
                member_audio_seen: HashSet::new(),
                member_speaking: HashMap::new(),
                last_audio_rx: Instant::now(),
                last_p2p_rx: None,
                last_recovery: Instant::now().checked_sub(Duration::from_secs(3)).unwrap(),
                local_ip_sig: local_ipv4_signature(),
                next_network_check: Instant::now() + Duration::from_millis(250),
                change_address_retries: VecDeque::new(),
                change_addr_seq: 0,
                relay_audio_streak: 0,
                rx_audio_second: 0,
                early_audio_rx_count: 0,
                group_rx_audio_packets: 0,
                group_rx_video_packets: 0,
                group_rx_audio_decoded: 0,
                group_rx_video_decoded: 0,
                group_rx_audio_bad: 0,
                group_rx_video_bad: 0,
                rx_video_packets: 0,
                rx_video_decoded: 0,
                rx_video_bad: 0,
                rx_video_first: 0,
                group_rx_other_host: 0,
                group_rx_alt_port: 0,
                group_rx_last_summary: Instant::now(),
                group_rx_audio_first: 0,
                group_rx_video_first: 0,
                group_audio_srtp_profile: None,
                group_video_srtp_profile: None,
                group_audio_srtp_profile_discovery_attempts: 0,
                group_video_srtp_profile_discovery_attempts: 0,
                audio_auth: AudioAuthDiagnostics::default(),
                video_auth: VideoAuthDiagnostics::default(),
            };
            worker.run();
        });
    }

    fn finish_ring_selection(&self) {
        let servers = self
            .ring_servers
            .lock()
            .map_or_default(|mut servers| std::mem::take(&mut *servers));
        if servers.is_empty() {
            return;
        }
        let params = self.params.lock().ok().and_then(|params| params.clone());
        let selected = self.selected_server.lock().ok().and_then(|server| *server);
        let Some(params) = params else {
            return;
        };
        for (server, token) in servers {
            let packet = if selected == Some((server, token)) {
                if params.kind == MediaKind::Group {
                    // The group SFU expects its dedicated ZaviPing control
                    // message immediately after InitZRTP. The legacy
                    // _sendPing() never used the one-to-one PING command here.
                    let camera_enabled =
                        !std::env::var("ZCALL_GROUP_CAMERA").is_ok_and(|value| value == "0");
                    let video_on = group_camera_advertised(params.video_enabled, camera_enabled);
                    let seq = next_group_control_sequence(&self.group_control_seq);
                    build_zavi_ping_packet(params.uid, token, seq, &params.sess_id, video_on)
                } else {
                    build_ping_packet(params.uid, token, 0, &params.sess_id)
                }
            } else {
                build_end_call_packet(params.uid, token, 0, params.call_id)
            };
            let _ = self.socket.send_to(&packet, server);
        }
        if params.kind == MediaKind::Group {
            if let Ok(mut last_ping) = self.last_ping.lock() {
                *last_ping = Instant::now();
            }
            media_debug("initial-keepalive type=zavi-ping");
        }
    }

    fn start_ring_ping(&self) {
        self.start_ring_ping_with_interval(ring_ping_interval());
    }

    fn start_ring_ping_with_interval(&self, interval: Duration) {
        let candidates = self
            .ring_servers
            .lock()
            .map_or_default(|servers| servers.clone());
        if candidates.is_empty() {
            return;
        }
        self.ring_ping_running.store(true, Ordering::SeqCst);
        let active = self.ring_ping_running.clone();
        let socket = self.socket.clone();
        let params = self.params.clone();
        thread::spawn(move || {
            let mut seq = 0u32;
            while active.load(Ordering::Acquire) {
                thread::sleep(interval);
                if !active.load(Ordering::Acquire) {
                    break;
                }
                let Some((uid, sess_id)) = params.lock().ok().and_then(|params| {
                    params
                        .as_ref()
                        .map(|params| (params.uid, params.sess_id.clone()))
                }) else {
                    continue;
                };
                for (server, token) in &candidates {
                    let packet = build_ping_packet(uid, *token, seq, &sess_id);
                    let _ = socket.send_to(&packet, server);
                }
                seq = seq.wrapping_add(1);
            }
        });
    }

    pub fn public_address(&self) -> Option<String> {
        self.public_addr.lock().ok().and_then(|addr| addr.clone())
    }

    pub fn note_debug_event(&self, message: &str) {
        media_debug(message);
    }

    /// Send one encoded H.264 Annex-B frame through the active relay. Group
    /// calls use the SFU RTP extension and plain RTP when the negotiated group
    /// session has no SRTP key, matching the existing Zalo group transport.
    fn send_video_chunks(&self, context: VideoTxContext<'_>) -> Result<usize, String> {
        let VideoTxContext {
            params,
            server,
            token,
            keys,
            group_plain,
            plan,
            chunks,
            key,
            state,
        } = context;
        let started = *state.started_at.get_or_insert_with(Instant::now);
        let timestamp = started.elapsed().as_millis().saturating_mul(90) as u32;
        let capture_ms = unix_now_ms();
        let payload_type = if key { VIDEO_PT_KEY } else { VIDEO_PT_INTER };
        let mut sent = 0usize;

        for (index, chunk) in chunks.iter().enumerate() {
            if index > 0 && index % 4 == 0 {
                thread::sleep(Duration::from_millis(1));
            }
            let seq = state.seq;
            let roc = state.roc;
            let twcc = self.twcc_seq.fetch_add(1, Ordering::Relaxed);
            if let Ok(mut controller) = self.send_rate.lock() {
                if let Some(controller) = controller.as_mut() {
                    controller.on_sent(twcc, capture_ms);
                }
            }
            let header = build_video_rtp_header(
                VideoHeaderContext {
                    params,
                    group_plain,
                    plan,
                    payload_type,
                    capture_ms,
                },
                seq,
                timestamp,
                twcc,
                index + 1 == chunks.len(),
                &mut state.layer_seq,
            );
            let packet_payload = if group_plain {
                let mut payload = header;
                payload.extend_from_slice(chunk);
                payload
            } else {
                protect_rtp(
                    &header,
                    chunk,
                    keys.expect("SRTP keys required for encrypted video"),
                    seq,
                    params.uid,
                    roc,
                )
            };
            if let Ok(mut history) = self.video_history.lock() {
                history.push_back((seq, packet_payload.clone()));
                while history.len() > 512 {
                    history.pop_front();
                }
            }
            let packet = wrap_media_packet(msg_types::VIDEO_RTP, token, &packet_payload);
            self.socket
                .send_to(&packet, server)
                .map_err(|err| format!("video RTP send failed: {err}"))?;
            self.last_video_sent_ms.store(capture_ms, Ordering::Relaxed);
            let video_packet_count = self.tx_video_packets.fetch_add(1, Ordering::Relaxed) + 1;
            if video_packet_count == 1 {
                media_debug(&format!(
                    "tx-video-first mode={} key={} rtp_bytes={} packet_type={}",
                    if params.kind == MediaKind::Group {
                        "group"
                    } else {
                        "one-to-one"
                    },
                    key,
                    packet_payload.len(),
                    msg_types::VIDEO_RTP
                ));
            }
            if params.kind != MediaKind::Group {
                if let Some(peer) = self.p2p_path.lock().ok().and_then(|path| *path) {
                    let direct = p2p::wrap(
                        params.role == MediaRole::Callee,
                        params.call_id,
                        p2p::VIDEO,
                        &packet_payload,
                    );
                    let _ = self.socket.send_to(&direct, peer);
                }
            }
            sent += 1;

            state.seq = state.seq.wrapping_add(1);
            if state.seq == 0 {
                state.roc = state.roc.wrapping_add(1);
            }
        }
        Ok(sent)
    }

    pub fn send_video_frame(&self, key: bool, annex_b: &[u8]) -> Result<usize, String> {
        if !self.running.load(Ordering::Relaxed) || annex_b.is_empty() {
            if key && !annex_b.is_empty() {
                media_debug("video keyframe dropped: media backend is not running");
            }
            return Ok(0);
        }
        let params_guard = self.params.lock().map_err(|e| e.to_string())?;
        let params = params_guard.as_ref().ok_or("no active media session")?;
        if !params.video_enabled {
            if key {
                media_debug("video keyframe dropped: media session video_enabled=false");
            }
            return Ok(0);
        }
        let (server, token) = self
            .selected_server
            .lock()
            .map_err(|e| e.to_string())?
            .ok_or("no selected media relay")?;
        let keys_guard = self.keys.lock().map_err(|e| e.to_string())?;
        let keys = keys_guard.as_ref();
        if params.kind == MediaKind::Group && !params.video_enabled {
            return Ok(0);
        }
        let group_plain = params.kind == MediaKind::Group
            && keys.is_none()
            && !std::env::var("ZCALL_GROUP_CAMERA").is_ok_and(|value| value == "0");
        if keys.is_none() && !group_plain {
            if key {
                media_debug("video keyframe dropped: SRTP keys are not ready");
            }
            return Ok(0);
        }
        let plan = if params.kind == MediaKind::Group {
            self.group_video_plan
                .lock()
                .map_err(|e| e.to_string())?
                .clone()
        } else {
            None
        };
        let chunks = packetize_frame(annex_b, 1068);
        let mut state = self.video_tx.lock().map_err(|e| e.to_string())?;
        if !key && !state.key_sent {
            self.local_keyframe_request.store(true, Ordering::Release);
            return Ok(0);
        }
        if key {
            state.key_sent = true;
        }
        self.send_video_chunks(VideoTxContext {
            params,
            server,
            token,
            keys,
            group_plain,
            plan: plan.as_ref(),
            chunks: &chunks,
            key,
            state: &mut state,
        })
    }

    pub fn video_profile(&self) -> Option<VideoProfile> {
        if let Some(plan) = self
            .group_video_plan
            .lock()
            .ok()
            .and_then(|plan| plan.clone())
        {
            return Some(VideoProfile {
                rung: plan.start as u32 + 1,
                width: plan.encode.width,
                height: plan.encode.height,
                fps: plan.encode.fps,
                bitrate: plan.encode.bitrate,
                keyframe_interval_ms: plan.encode.keyframe_interval_ms,
            });
        }
        self.send_rate
            .lock()
            .ok()
            .and_then(|controller| controller.as_ref().map(SendRateController::profile))
    }

    /// Update the local group-camera bit carried in InitZRTP/ZaviPing.
    pub fn set_group_camera(&self, on: bool) -> Result<(), String> {
        let mut params = self.params.lock().map_err(|e| e.to_string())?;
        let Some(params) = params.as_mut() else {
            return Ok(());
        };
        if params.kind == MediaKind::Group {
            params.video_enabled = on;
        }
        Ok(())
    }

    /// Announce the local SFU layer that this process is actually encoding.
    pub fn send_group_video_quality_config(&self) -> Result<bool, String> {
        let params = self
            .params
            .lock()
            .map_err(|e| e.to_string())?
            .clone()
            .ok_or("no active media session")?;
        if params.kind != MediaKind::Group {
            return Ok(false);
        }
        let Some(plan) = self
            .group_video_plan
            .lock()
            .map_err(|e| e.to_string())?
            .clone()
        else {
            return Ok(false);
        };
        if plan.send.is_empty() {
            return Ok(false);
        }
        let Some((server, token)) = *self.selected_server.lock().map_err(|e| e.to_string())? else {
            return Ok(false);
        };
        let seq = next_group_control_sequence(&self.group_control_seq);
        let packet = build_group_video_quality_config(params.uid, token, seq, &plan.send);
        self.socket
            .send_to(&packet, server)
            .map_err(|error| format!("group VidQualConfig send failed: {error}"))?;
        Ok(true)
    }

    /// Request one remote member's SFU video layer.
    pub fn request_group_video(&self, uid: u32) -> Result<bool, String> {
        let params = self
            .params
            .lock()
            .map_err(|e| e.to_string())?
            .clone()
            .ok_or("no active media session")?;
        if params.kind != MediaKind::Group || uid == 0 || uid == params.uid {
            return Ok(false);
        }
        let width = self
            .group_video_render_width
            .lock()
            .map_err(|e| e.to_string())?
            .get(&uid)
            .copied()
            .unwrap_or(0);
        let quality_id = if std::env::var("ZCALL_GROUP_QUALITY").is_ok_and(|value| value == "0") {
            0xff
        } else if std::env::var("ZCALL_SHARE_LOOPBACK").is_ok_and(|value| value == "1")
            && self
                .share_peer
                .lock()
                .ok()
                .and_then(|peer| peer.as_ref().map(|peer| peer.uid))
                == Some(uid)
        {
            0
        } else {
            self.group_video_plan
                .lock()
                .map_err(|e| e.to_string())?
                .as_ref()
                .map_or(0xff, |plan| select_group_video_quality(&plan.layers, width))
        };
        {
            let mut wanted = self.group_video_wanted.lock().map_err(|e| e.to_string())?;
            if wanted.get(&uid) == Some(&quality_id) {
                return Ok(false);
            }
            wanted.insert(uid, quality_id);
        }
        let Some((server, token)) = *self.selected_server.lock().map_err(|e| e.to_string())? else {
            return Ok(false);
        };
        let seq = next_group_control_sequence(&self.group_control_seq);
        let packet = build_group_video_quality_request(
            params.uid,
            token,
            seq,
            &[GroupVideoQualityRequest { uid, quality_id }],
        );
        self.socket
            .send_to(&packet, server)
            .map_err(|error| format!("group VidQualReq send failed: {error}"))?;
        media_debug(&format!(
            "subscribe-video uid={uid} quality_id={quality_id}"
        ));
        Ok(true)
    }

    /// Track the current share SSRC so its plain RTP is not mistaken for SRTP.
    pub fn set_group_share_remote_uid(&self, share_uid: u32) {
        self.remote_share_uid.store(share_uid, Ordering::Release);
    }

    /// Open the dedicated SFU peer used by group screen sharing.
    pub fn start_group_share(&self, share_uid: u32) -> Result<(), String> {
        if share_uid == 0 {
            return Err("invalid group share UID".into());
        }
        let params = self
            .params
            .lock()
            .map_err(|e| e.to_string())?
            .clone()
            .ok_or("no active media session")?;
        if params.kind != MediaKind::Group
            || !params.send_udp
            || !self.running.load(Ordering::Acquire)
        {
            return Err("group screen share requires an active UDP group call".into());
        }
        let (server, _) = self
            .selected_server
            .lock()
            .map_err(|e| e.to_string())?
            .ok_or("no selected group SFU")?;
        self.stop_group_share();

        let socket = Arc::new(
            UdpSocket::bind("0.0.0.0:0")
                .map_err(|error| format!("bind group share UDP socket: {error}"))?,
        );
        socket
            .set_nonblocking(true)
            .map_err(|error| format!("set group share socket nonblocking: {error}"))?;
        let active = Arc::new(AtomicBool::new(true));
        let plan = self
            .group_video_plan
            .lock()
            .map_err(|e| e.to_string())?
            .clone();
        let max_shr_screen_br = params
            .video_config
            .get("maxShrScreenBr")
            .and_then(Value::as_f64)
            .filter(|value| *value > 0.0)
            .unwrap_or(1500.0)
            .trunc() as u32;
        let (sfu_id, sfu_long) = plan
            .and_then(|plan| plan.sfu_id.map(|id| (id, plan.sfu_long)))
            .unwrap_or((12, false));
        let init = build_share_init_zrtp_packet(share_uid, &params.sess_id);
        socket
            .send_to(&init, server)
            .map_err(|error| format!("send group share InitZRTP: {error}"))?;

        let peer = GroupSharePeer {
            socket: socket.clone(),
            server,
            uid: share_uid,
            token: 0,
            sess_id: params.sess_id.clone(),
            call_id: params.call_id,
            active: active.clone(),
            seq: (unix_now_ms() as u16) & 0x7fff,
            twcc_seq: 1,
            layer_seq: 0,
            control_seq: 0,
            started_at: None,
            timestamp_base: (unix_now_ms() as u32)
                .wrapping_mul(90)
                .wrapping_add(share_uid.rotate_left(7)),
            last_ping: Instant::now(),
            width: 0,
            height: 0,
            max_shr_screen_br,
            sfu_id,
            sfu_long,
            key_sent: false,
            keyframe_requested: false,
            queue: VecDeque::new(),
        };
        *self.share_peer.lock().map_err(|e| e.to_string())? = Some(peer);

        let share_state = self.share_peer.clone();
        let share_events = self.group_events.clone();
        let running = self.running.clone();
        thread::Builder::new()
            .name("zcall-group-share".into())
            .spawn(move || {
                group_share_worker(
                    &socket,
                    &share_state,
                    &share_events,
                    &running,
                    &active,
                    &init,
                );
            })
            .map_err(|error| {
                self.stop_group_share();
                format!("spawn group share worker: {error}")
            })?;
        if std::env::var("ZCALL_SHARE_LOOPBACK").is_ok_and(|value| value == "1") {
            let _ = self.request_group_video(share_uid);
        }
        Ok(())
    }

    /// Stop only the share peer; the group camera/audio peer remains active.
    pub fn stop_group_share(&self) {
        let peer = self.share_peer.lock().ok().and_then(|mut peer| peer.take());
        if let Some(mut peer) = peer {
            peer.active.store(false, Ordering::Release);
            peer.queue.clear();
            if peer.token != 0 {
                let end =
                    build_end_call_packet(peer.uid, peer.token, peer.control_seq, peer.call_id);
                let _ = peer.socket.send_to(&end, peer.server);
            }
        }
    }

    /// Queue one encoded screen frame on the share peer, never on the camera SSRC.
    pub fn send_group_share_frame(
        &self,
        key: bool,
        annex_b: &[u8],
        width: u32,
        height: u32,
    ) -> Result<usize, String> {
        if annex_b.is_empty() || width < 2 || height < 2 {
            return Ok(0);
        }
        let mut guard = self.share_peer.lock().map_err(|e| e.to_string())?;
        let Some(peer) = guard.as_mut() else {
            if key {
                media_debug("group video keyframe dropped: share peer is not initialized");
            }
            return Ok(0);
        };
        if peer.token == 0 {
            if key {
                media_debug("group video keyframe dropped: share peer token is not ready");
            }
            return Ok(0);
        }
        let width = width.min(u32::from(u16::MAX)) as u16;
        let height = height.min(u32::from(u16::MAX)) as u16;
        if peer.width != width || peer.height != height {
            peer.width = width;
            peer.height = height;
            let layer = GroupVideoLayer {
                id: 0,
                bitrate_kbps: peer.max_shr_screen_br,
                width,
                height,
            };
            let packet =
                build_group_video_quality_config(peer.uid, peer.token, peer.control_seq, &[layer]);
            peer.control_seq = peer.control_seq.wrapping_add(1);
            let _ = peer.socket.send_to(&packet, peer.server);
        }
        if !key && !peer.key_sent {
            if !peer.keyframe_requested {
                peer.keyframe_requested = true;
                if let Ok(mut events) = self.group_events.lock() {
                    events.push_back(GroupMediaEvent::ShareKeyframeRequested);
                }
            }
            return Ok(0);
        }
        if key {
            peer.key_sent = true;
            peer.keyframe_requested = false;
        }
        let started = *peer.started_at.get_or_insert_with(Instant::now);
        let timestamp = peer
            .timestamp_base
            .wrapping_add(started.elapsed().as_millis().saturating_mul(90) as u32);
        let capture_ms = unix_now_ms();
        let chunks = packetize_frame(annex_b, 1068);
        for (index, chunk) in chunks.iter().enumerate() {
            let twcc = peer.twcc_seq;
            peer.layer_seq = peer.layer_seq.wrapping_add(1);
            let header = build_group_video_rtp_header(
                peer.seq,
                timestamp,
                peer.uid,
                twcc,
                index + 1 == chunks.len(),
                if key { VIDEO_PT_KEY } else { VIDEO_PT_INTER },
                peer.sfu_long,
                peer.sfu_id,
                peer.layer_seq,
                capture_ms,
                unix_now_ms(),
            );
            let mut payload = header;
            payload.extend_from_slice(chunk);
            peer.queue.push_back(wrap_media_packet(
                msg_types::VIDEO_RTP,
                peer.token,
                &payload,
            ));
            peer.seq = peer.seq.wrapping_add(1);
            peer.twcc_seq = peer.twcc_seq.wrapping_add(1);
        }
        while peer.queue.len() > 512 {
            peer.queue.pop_front();
        }
        Ok(chunks.len())
    }

    /// Re-evaluate a member's requested quality after its UI tile is resized.
    pub fn set_group_member_render_width(&self, uid: u32, width: u32) -> Result<bool, String> {
        if uid == 0 {
            return Ok(false);
        }
        self.group_video_render_width
            .lock()
            .map_err(|e| e.to_string())?
            .insert(uid, width);
        let already_receiving = self
            .group_video_wanted
            .lock()
            .map_err(|e| e.to_string())?
            .contains_key(&uid);
        if !already_receiving {
            return Ok(false);
        }
        self.request_group_video(uid)
    }

    pub fn take_video_profile_change(&self) -> Option<VideoProfile> {
        self.pending_video_profile
            .lock()
            .ok()
            .and_then(|mut profile| profile.take())
    }

    pub fn send_rate_totals(&self) -> Option<RateTotals> {
        self.send_rate
            .lock()
            .ok()
            .and_then(|controller| controller.as_ref().map(SendRateController::totals))
    }

    /// Authenticated audio RTP packet totals, matching the legacy call-finish payload.
    pub fn stats(&self) -> (u64, u64) {
        (
            self.rx_audio_packets.load(Ordering::Acquire),
            self.tx_audio_packets.load(Ordering::Acquire),
        )
    }

    pub fn take_local_keyframe_request(&self) -> bool {
        self.local_keyframe_request.swap(false, Ordering::AcqRel)
    }

    pub fn request_peer_keyframe(&self, media_ssrc: Option<u32>) -> Result<bool, String> {
        let now = Instant::now();
        {
            let mut last = self
                .last_peer_pli
                .lock()
                .map_err(|error| error.to_string())?;
            if last.is_some_and(|previous| now.duration_since(previous) < Duration::from_secs(1)) {
                return Ok(false);
            }
            *last = Some(now);
        }
        let mut feedback = self.rtcp.lock().map_err(|error| error.to_string())?;
        let Some(feedback) = feedback.as_mut() else {
            return Ok(false);
        };
        let Some(peer_ssrc) = media_ssrc
            .filter(|ssrc| *ssrc != 0)
            .or_else(|| feedback.video.ssrc())
            .or_else(|| (feedback.peer_ssrc != 0).then_some(feedback.peer_ssrc))
        else {
            return Ok(false);
        };
        let Some((server, token)) = *self
            .selected_server
            .lock()
            .map_err(|error| error.to_string())?
        else {
            return Ok(false);
        };
        let report = feedback.pli(peer_ssrc);
        let packet = wrap_media_packet(msg_types::VIDEO_FEC, token, &report);
        self.socket
            .send_to(&packet, server)
            .map_err(|error| format!("send RTCP PLI failed: {error}"))?;
        if let Some(peer) = self.p2p_path.lock().ok().and_then(|path| *path) {
            if let Some(params) = self.params.lock().ok().and_then(|params| params.clone()) {
                let direct = p2p::wrap(
                    params.role == MediaRole::Callee,
                    params.call_id,
                    p2p::VIDEO_RTCP,
                    &report,
                );
                let _ = self.socket.send_to(&direct, peer);
            }
        }
        Ok(true)
    }

    pub fn drain_group_events(&self) -> Vec<GroupMediaEvent> {
        self.group_events
            .lock()
            .map_or_default(|mut events| events.drain(..).collect())
    }

    pub fn drain_video_frames(&self) -> Vec<VideoFrame> {
        let frames = self
            .video_frames
            .lock()
            .map_or_default(|mut queue| queue.drain(..).collect::<Vec<_>>());
        if !frames.is_empty()
            && !self
                .first_video_ui_drain_logged
                .swap(true, Ordering::AcqRel)
        {
            let frame = &frames[0];
            media_debug(&format!(
                "group-video-frame=sent-to-ui src={:?} key={} bytes={}",
                frame.src,
                frame.key,
                frame.data.len()
            ));
        }
        frames
    }

    pub fn set_p2p_candidates(&self, candidates: Vec<SocketAddr>) -> Result<(), String> {
        let mut params = self.params.lock().map_err(|error| error.to_string())?;
        let Some(params) = params.as_mut() else {
            return Err("cannot set P2P candidates before media preparation".into());
        };
        params.p2p_candidates = candidates;
        self.p2p_enabled
            .store(!params.p2p_candidates.is_empty(), Ordering::Release);
        Ok(())
    }

    pub fn p2p_candidates(&self, include_local: bool) -> Vec<serde_json::Value> {
        let mut candidates = Vec::new();
        if let Some(public) = self.public_address() {
            if let Some((ip, port)) = public.split_once('|') {
                if let Ok(port) = port.parse::<u16>() {
                    candidates.push(serde_json::json!({"ip":ip,"port":port,"type":1}));
                }
            }
        }
        if include_local {
            candidates.extend(self.local_candidates());
        }
        candidates
    }

    pub fn local_candidates(&self) -> Vec<serde_json::Value> {
        let port = self.socket.local_addr().map_or(0, |a| a.port());
        if port == 0 {
            return Vec::new();
        }
        local_ipv4_addresses()
            .into_iter()
            .map(|ip| serde_json::json!({"ip":ip.to_string(),"port":port,"type":0}))
            .collect()
    }

    pub fn set_mute(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    pub fn set_speaker_off(&self, off: bool) {
        self.speaker_off.store(off, Ordering::Relaxed);
    }

    /// User-selected hardware microphone volume; a manual adjustment disables
    /// automatic `ClipGuard` changes for the remainder of the call.
    pub fn set_mic_volume(&self, percent: u8) {
        let percent = percent.min(100);
        self.mic_manual.store(true, Ordering::Relaxed);
        if let Ok(mut pending) = self.pending_mic_volume.lock() {
            *pending = Some(percent);
        }
    }

    pub fn set_mic_auto_gain(&self, enabled: bool) {
        self.mic_manual.store(!enabled, Ordering::Release);
    }

    pub fn mic_auto_gain(&self) -> bool {
        !self.mic_manual.load(Ordering::Acquire)
    }

    pub fn mic_volume(&self) -> u8 {
        self.mic_volume.load(Ordering::Relaxed).min(100)
    }

    pub fn take_mic_volume_update(&self) -> Option<u8> {
        self.mic_volume_dirty
            .swap(false, Ordering::AcqRel)
            .then(|| self.mic_volume())
    }

    pub fn take_mic_level_update(&self) -> Option<u8> {
        self.mic_level_dirty
            .swap(false, Ordering::AcqRel)
            .then(|| self.mic_level.load(Ordering::Relaxed).min(127))
    }

    /// Active `PulseAudio` streams are reopened on their owning worker thread.
    /// The media session, SRTP state and jitter buffer are retained.
    pub fn set_device(&self, kind: &str, name: &str) {
        if let Ok(mut devices) = self.devices.lock() {
            let selected = if name.is_empty() {
                None
            } else {
                Some(name.to_owned())
            };
            let slot = match kind {
                "mic" => &mut devices.0,
                "speaker" => &mut devices.1,
                _ => return,
            };
            if *slot != selected {
                *slot = selected;
                self.device_generation.fetch_add(1, Ordering::AcqRel);
            }
        }
    }

    pub fn selected_devices(&self) -> (Option<String>, Option<String>) {
        self.devices
            .lock()
            .map_or_default(|devices| devices.clone())
    }

    pub fn stop(&self) {
        self.stop_group_share();
        self.ring_ping_running.store(false, Ordering::SeqCst);
        self.audio_enabled.store(false, Ordering::Release);
        self.running.store(false, Ordering::SeqCst);
        if let Ok(mut queue) = self.video_frames.lock() {
            queue.clear();
        }
        if let Ok(mut history) = self.video_history.lock() {
            history.clear();
        }
        if let Ok(mut state) = self.video_tx.lock() {
            *state = VideoTxState::default();
        }
        if let Ok(mut feedback) = self.rtcp.lock() {
            *feedback = None;
        }
        if let Ok(mut nack) = self.nack.lock() {
            nack.clear();
        }
        if let Ok(mut path) = self.p2p_path.lock() {
            *path = None;
        }
        self.p2p_enabled.store(false, Ordering::Release);
        if let Ok(mut controller) = self.send_rate.lock() {
            *controller = None;
        }
        if let Ok(mut pending) = self.pending_video_profile.lock() {
            *pending = None;
        }

        let params = self.params.lock().ok().and_then(|p| p.clone());
        let selected = self.selected_server.lock().ok().and_then(|s| *s);
        let pending = self
            .ring_servers
            .lock()
            .map_or_default(|mut servers| std::mem::take(&mut *servers));
        if let Some(params) = params.as_ref() {
            for (server, token) in pending {
                if selected != Some((server, token)) {
                    let end = build_end_call_packet(params.uid, token, 1, params.call_id);
                    let _ = self.socket.send_to(&end, server);
                }
            }
        }
        if let (Some(params), Some((server, token))) = (params, selected) {
            if params.send_udp && token != 0 {
                let end_packet = build_end_call_packet(params.uid, token, 1, params.call_id);
                let _ = self.socket.send_to(&end_packet, server);
            }
        }
        if let Ok(mut selected) = self.selected_server.lock() {
            *selected = None;
        }
        if let Ok(mut servers) = self.ring_servers.lock() {
            servers.clear();
        }
        if let Ok(mut public) = self.public_addr.lock() {
            *public = None;
        }
    }
}

#[cfg(test)]
#[path = "../tests/media/audio_dedup.rs"]
mod audio_dedup_tests;

#[cfg(test)]
#[path = "../tests/media/roc_recovery.rs"]
mod roc_recovery_tests;

#[cfg(test)]
#[path = "../tests/media/video_transport.rs"]
mod video_transport_tests;
