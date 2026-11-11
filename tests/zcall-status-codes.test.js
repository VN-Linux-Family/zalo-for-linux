'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const root = path.join(__dirname, '..');
const read = (file) => fs.readFileSync(path.join(root, file), 'utf8');
const engine = read('native/engines/zcall/native-engine/engine-core.js');
const transport = read('native/engines/zcall/native-engine/ui-client.js');
const callUi = read('plugins/zcall/ui/call.js');
const incomingUi = read('plugins/zcall/ui/incoming.js');
const dictionary = JSON.parse(read('plugins/zcall/ui/locale.json')).messages;

test('native engine sends symbolic ZCall status codes instead of localized sentences', () => {
  assert.doesNotMatch(engine, /this\.ui\.status\(\s*['"`][^'"`]*[À-ỹ]/u);
  assert.doesNotMatch(engine, /this\.ui\.open\(\{[^}]*\btext\s*:/s);
  assert.doesNotMatch(engine, /this\.ui\.incoming\(\{[^}]*\btext\s*:/s);
  assert.doesNotMatch(engine, /this\.ui\.close\(\s*['"`][^'"`]*[À-ỹ]/u);
  assert.doesNotMatch(engine, /this\.ui\.notice\(\s*['"`][^'"`]*[À-ỹ]/u);
  assert.match(engine, /map\(\(\[, m\]\) => m\.name \|\| null\)/);
  assert.match(engine, /const statusParams = names\.length \? \{ count: names\.length \+ 1, names \} : \{\}/);
  assert.doesNotMatch(engine, /m\.name \|\| 'Thành viên'/);

  for (const code of [
    'calling', 'ringing', 'connecting', 'connected', 'ended', 'endedWithDuration',
    'incomingVoice', 'incomingVideo', 'recipientUnreachable', 'recipientNoAnswer',
    'recipientDeclined', 'callFailed', 'serverUnreachable', 'groupUnsupported',
    'groupCallIgnored', 'groupEnded', 'groupCallFailed', 'groupCallFailedReason',
    'groupJoinRejected', 'groupDeclined', 'groupKicked', 'groupNoAnswer',
    'groupCancelled', 'groupCalling', 'groupRinging', 'groupWaitingOthers',
    'groupParticipants', 'incomingGroupInvite', 'incomingGroupUnsupported',
    'disconnected', 'audioMicSpeakerFailed', 'audioMicFailed', 'audioSpeakerFailed', 'unknownCaller',
  ]) {
    assert.deepEqual(Object.keys(dictionary['zcall.' + code] || {}).sort(), ['en', 'vi'], `missing translation for status code: ${code}`);
  }
});

test('engine-to-renderer state and close messages carry statusCode/statusParams', () => {
  assert.match(transport, /statusCode, statusParams/);
  assert.match(transport, /statusCode\s*:\s*statusCode/);
  assert.match(transport, /statusCode, statusParams, since/);
  assert.match(transport, /type: 'close', statusCode: statusCode \|\| 'ended', statusParams/);
  assert.match(transport, /noticeCode/);
  assert.match(transport, /noticeParams/);
  assert.doesNotMatch(transport, /const s = \{ phase: 'outgoing',[^}]*\btext\s*,/s);
  assert.match(callUi, /tr\(statusCode, s\.statusParams\)/);
  assert.match(callUi, /tr\(s\.noticeCode, s\.noticeParams\)/);
  assert.match(incomingUi, /tr\('unknownCaller'\)/);
  assert.match(incomingUi, /s\.noAnswer \|\| s\.inviter \|\| s\.group/);
  assert.match(incomingUi, /tr\(statusCode, statusParams\)/);
  assert.match(engine, /statusParams: \{ inviter: d\.Dname \|\| '' \}/);
  assert.match(engine, /name \|\| d\.Dname \|\| ''/);
});

test('status templates interpolate dynamic parameters in the active locale', () => {
  const source = read('plugins/zcall/ui/i18n.js');
  const uiDir = path.join(root, 'plugins/zcall/ui');
  const window = { location: { href: new URL('call.html', 'file://' + uiDir + '/').href } };
  vm.runInNewContext(source, {
    window,
    URL,
    require,
    document: { documentElement: { lang: '' } },
  });
  const i18n = window.zcallI18n;
  assert.equal(i18n.t('groupParticipants', { count: 3, names: 'An, Bình' }), '3 người: Bạn, An, Bình');
  assert.equal(i18n.apply('en'), true);
  assert.equal(i18n.t('groupParticipants', { count: 3, names: 'An, Bình' }), '3 people: You, An, Bình');
  assert.equal(i18n.t('incomingGroupInvite', { inviter: 'An' }), 'An invited you to a group call');
  assert.equal(i18n.t('groupParticipants', { count: 4, names: ['An', null, 'Bình'] }), '4 people: You, An, Participant, Bình');
  assert.equal(i18n.t('groupParticipants', { count: 4, names: ['An', '', 'Bình'] }), '4 people: You, An, Participant, Bình');
});
