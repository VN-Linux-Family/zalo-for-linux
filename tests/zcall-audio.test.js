'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const root = path.join(__dirname, '..');
const manifest = fs.readFileSync(path.join(root, 'native', 'engines', 'zcall', 'Cargo.toml'), 'utf8');
const packageJson = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8'));
const core = fs.readFileSync(path.join(root, 'native', 'engines', 'zcall', 'src', 'lib.rs'), 'utf8');
const codec = fs.readFileSync(path.join(root, 'native', 'engines', 'zcall', 'src', 'audio', 'codec.rs'), 'utf8');
const media = fs.readFileSync(path.join(root, 'native', 'engines', 'zcall', 'src', 'media', 'mod.rs'), 'utf8');
const jitter = fs.readFileSync(path.join(root, 'native', 'engines', 'zcall', 'src', 'audio', 'jitter.rs'), 'utf8');
const clipGuard = fs.readFileSync(path.join(root, 'native', 'engines', 'zcall', 'src', 'audio', 'clip_guard.rs'), 'utf8');
const backend = fs.readFileSync(path.join(root, 'native', 'engines', 'zcall', 'src', 'audio', 'backend.rs'), 'utf8');
const pulseVolume = fs.readFileSync(path.join(root, 'native', 'engines', 'zcall', 'src', 'audio', 'c_pulse_vol.c'), 'utf8');

test('audio capture/playback and Opus are integrated into the standalone zcall Rust process', () => {
  assert.match(manifest, /name\s*=\s*"zcall"/);
  assert.match(core, /pub mod audio/);
  assert.match(codec, /#\[link\(name\s*=\s*"opus"\)\]/);
  assert.match(codec, /opus_voice_frame_encodes_and_decodes_at_call_sample_rate/);
  assert.match(media, /Apm::new\(/, 'PR #220 WebRTC APM must be part of the active media pipeline');
  assert.match(media, /apm\.process_render\(/);
  assert.match(media, /apm\.process_capture\(/);
  assert.ok(media.indexOf('clip_guard.feed(') < media.indexOf('apm.process_capture('),
    'PR #220 ClipGuard must inspect raw PCM before APM processing');
  assert.match(media, /clip_guard\.set_device\(/, 'ClipGuard must follow the active microphone');
  assert.match(media, /ClipGuard::new\(/);
  assert.match(media, /MicLeveler::new\(/);
  assert.match(media, /JitterBuffer::new\(/);
  assert.match(jitter, /decode\(next_payload, out_pcm, true\)/);
  assert.match(jitter, /consecutive_plc/);
  assert.match(clipGuard, /apply_soft_limiter/);
  assert.match(clipGuard, /ZCALL_MIC_AGC/);
  assert.match(backend, /zcall_pulse_get_source_volume/);
  assert.match(backend, /zcall_pulse_set_source_volume/);
  assert.match(backend, /Command::new\(\"pactl\"\)/, 'pactl remains a fallback when libpulse volume control is unavailable');
  assert.match(pulseVolume, /pa_context_get_source_info_by_name/);
  assert.match(pulseVolume, /pa_context_set_source_volume_by_index/);
  assert.doesNotMatch(fs.readFileSync(path.join(root, 'scripts', 'build-native.js'), 'utf8'), /name:\s*'zcall-audio'/);
  assert.ok(packageJson.build.extraFiles.some((entry) => entry.from === 'app' && entry.to === 'app'),
    'the standalone app/native/zcall binary must be copied into the AppDir');
  assert.ok(!packageJson.build.extraFiles.some((entry) => String(entry.from).includes('native/engines/zcall')),
    'the source-only reference tree must not be packaged as a runtime engine');
});

test('built zcall binary exposes its version without loading Electron or Python', (t) => {
  const binary = path.join(root, 'target', 'release', 'zcall');
  if (!fs.existsSync(binary)) {
    t.skip('Rust zcall release binary has not been built');
    return;
  }
  const result = spawnSync(binary, ['--version'], { encoding: 'utf8', timeout: 3000 });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /^zcall \d+\.\d+\.\d+/);
});
