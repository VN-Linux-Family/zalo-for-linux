'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const root = path.join(__dirname, '..');
const read = (file) => fs.readFileSync(path.join(root, file), 'utf8');
const engine = read('native/engines/zcall/src/engine/mod.rs');
const uiTransport = read('native/engines/zcall/src/ui/tcp.rs');
const callUi = read('plugins/zcall/ui/call.js');
const incomingUi = read('plugins/zcall/ui/incoming.js');
const dictionary = JSON.parse(read('plugins/zcall/ui/locale.json')).messages;

test('Rust engine emits symbolic status codes and structured parameters', () => {
  assert.doesNotMatch(engine, /self\.ui_message\(json!\(\{[^}]*\btext\s*:/s);
  assert.doesNotMatch(engine, /self\.ui_message\(json!\(\{[^}]*\bmessage\s*:\s*['"][^'"]*[À-ỹ]/s);
  assert.match(engine, /"statusCode":status/);
  assert.match(engine, /"statusParams":\{"count":names\.len\(\)\+1,"names":names\}/);
  assert.match(engine, /"statusParams":\{"duration":duration\}/);
  assert.match(engine, /"statusCode":"incomingGroupInvite"/);
  assert.match(engine, /"inviter":host_name/);
});

test('Rust UI transport forwards structured messages and renderer translates status codes', () => {
  assert.match(uiTransport, /serde_json::to_writer/);
  assert.match(callUi, /tr\(statusCode, s\.statusParams\)/);
  assert.match(callUi, /s\.noticeCode \? tr\(s\.noticeCode, s\.noticeParams\)/);
  assert.match(callUi, /s\.statusCode \|\|/);
  assert.match(incomingUi, /tr\(statusCode, statusParams\)/);
  assert.match(incomingUi, /tr\('unknownCaller'\)/);
  assert.match(incomingUi, /statusParams\.inviter/);
  assert.doesNotMatch(callUi, /statusCode\s*\|\|\s*['"][^'"]*[À-ỹ]/);
});

test('all public status codes have English/Vietnamese translations', () => {
  const codes = [
    'calling', 'ringing', 'connecting', 'connected', 'ended', 'endedWithDuration',
    'incomingVoice', 'incomingVideo', 'recipientUnreachable', 'recipientNoAnswer',
    'recipientDeclined', 'callFailed', 'serverUnreachable', 'groupUnsupported',
    'groupCallIgnored', 'groupEnded', 'groupCallFailed', 'groupCallFailedReason',
    'groupJoinRejected', 'groupDeclined', 'groupKicked', 'groupNoAnswer',
    'groupCancelled', 'groupCalling', 'groupRinging', 'groupWaitingOthers',
    'groupParticipants', 'incomingGroupInvite', 'incomingGroupUnsupported',
    'disconnected', 'audioMicSpeakerFailed', 'audioMicFailed', 'audioSpeakerFailed',
    'unknownCaller', 'reconnecting',
  ];
  for (const code of codes) {
    assert.deepEqual(
      Object.keys(dictionary['zcall.' + code] || {}).sort(),
      ['en', 'vi'],
      'missing translation for status code: ' + code,
    );
  }
});

test('status templates interpolate group names and inviter in the active locale', () => {
  const source = read('plugins/zcall/ui/i18n.js');
  const uiDir = path.join(root, 'plugins/zcall/ui');
  const window = {
    location: { href: new URL('call.html', 'file://' + uiDir + '/').href },
  };
  vm.runInNewContext(source, {
    window,
    URL,
    require,
    document: { documentElement: { lang: '' } },
  });
  const i18n = window.zcallI18n;
  assert.equal(
    i18n.t('groupParticipants', { count: 3, names: 'An, Bình' }),
    '3 người: Bạn, An, Bình',
  );
  assert.equal(i18n.apply('en'), true);
  assert.equal(
    i18n.t('groupParticipants', { count: 3, names: 'An, Bình' }),
    '3 people: You, An, Bình',
  );
  assert.equal(
    i18n.t('incomingGroupInvite', { inviter: 'An' }),
    'An invited you to a group call',
  );
  assert.equal(
    i18n.t('groupParticipants', { count: 4, names: ['An', null, 'Bình'] }),
    '4 people: You, An, Participant, Bình',
  );
});
