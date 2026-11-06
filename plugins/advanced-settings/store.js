'use strict';

const fs = require('fs');
const path = require('path');
const { pathToFileURL } = require('url');

const SOUND_KINDS = ['ringtone', 'ringback'];
const AUDIO_EXTENSIONS = ['mp3', 'wav', 'ogg', 'm4a', 'aac', 'flac', 'opus', 'webm'];

const DEFAULT_SETTINGS = Object.freeze({
  ringtone: '',
  ringback: '',
  screenshotDelayWithZalo: 100,
  screenshotDelayWithoutZalo: 500
});

function defaultSoundUrl(kind) {
  if (!SOUND_KINDS.includes(kind)) throw new Error('Loại âm thanh không hợp lệ.');
  const devAssets = path.join(__dirname, '..', '..', 'app', 'zcall-assets');
  const assets = fs.existsSync(devAssets) ? devAssets :
    path.join(path.dirname(process.execPath), 'app', 'zcall-assets');
  return pathToFileURL(path.join(assets, 'sound', `zalo_${kind}.mp3`)).href;
}

function validateScreenshotDelay(value, fallbackDefault = 0) {
  if (value === undefined || value === null || value === '') {
    return fallbackDefault;
  }
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || isNaN(parsed)) {
    throw new Error('Thời gian chờ phải là một số hợp lệ.');
  }
  if (parsed < 0) {
    throw new Error('Thời gian chờ không được nhỏ hơn 0 mili-giây.');
  }
  if (parsed > 10000) {
    throw new Error('Thời gian chờ tối đa là 10000 mili-giây (10 giây).');
  }
  return Math.round(parsed);
}

function createStore(userDataDir) {
  const filename = path.join(userDataDir, 'zalo-linux-settings.json');

  function read() {
    const settings = { ...DEFAULT_SETTINGS };
    try {
      const saved = JSON.parse(fs.readFileSync(filename, 'utf8'));
      for (const kind of SOUND_KINDS) {
        if (typeof saved[kind] === 'string') settings[kind] = saved[kind];
      }
      if (typeof saved.screenshotDelayWithZalo === 'number' && Number.isFinite(saved.screenshotDelayWithZalo) && saved.screenshotDelayWithZalo >= 0) {
        settings.screenshotDelayWithZalo = Math.round(saved.screenshotDelayWithZalo);
      }
      if (typeof saved.screenshotDelayWithoutZalo === 'number' && Number.isFinite(saved.screenshotDelayWithoutZalo) && saved.screenshotDelayWithoutZalo >= 0) {
        settings.screenshotDelayWithoutZalo = Math.round(saved.screenshotDelayWithoutZalo);
      }
    } catch (error) {
      if (error.code !== 'ENOENT') console.error('[advanced-settings] Cannot read settings:', error.message);
    }
    return settings;
  }

  function validateKind(kind) {
    if (!SOUND_KINDS.includes(kind)) throw new Error('Loại âm thanh không hợp lệ.');
  }

  function validatePath(value) {
    if (typeof value !== 'string') throw new Error('Đường dẫn không hợp lệ.');
    if (!value) return '';
    if (!path.isAbsolute(value)) throw new Error('Hãy nhập đường dẫn tuyệt đối đến tệp âm thanh.');
    if (!AUDIO_EXTENSIONS.includes(path.extname(value).slice(1).toLowerCase())) {
      throw new Error('Định dạng âm thanh không được hỗ trợ.');
    }
    try {
      if (!fs.statSync(value).isFile()) throw new Error('not a file');
      fs.accessSync(value, fs.constants.R_OK);
    } catch (_) {
      throw new Error('Không thể đọc tệp âm thanh. Hãy kiểm tra lại đường dẫn và quyền truy cập.');
    }
    return path.normalize(value);
  }

  function save(kind, value) {
    validateKind(kind);
    const validated = validatePath(value);
    const settings = { ...read(), [kind]: validated };
    fs.mkdirSync(userDataDir, { recursive: true });
    const temporary = filename + '.tmp';
    fs.writeFileSync(temporary, JSON.stringify(settings, null, 2), { mode: 0o600 });
    fs.renameSync(temporary, filename);
    return settings;
  }

  function saveScreenshotDelay(kind, value) {
    let key;
    let fallback;
    if (kind === 'withZalo' || kind === 'screenshotDelayWithZalo') {
      key = 'screenshotDelayWithZalo';
      fallback = DEFAULT_SETTINGS.screenshotDelayWithZalo;
    } else if (kind === 'withoutZalo' || kind === 'screenshotDelayWithoutZalo') {
      key = 'screenshotDelayWithoutZalo';
      fallback = DEFAULT_SETTINGS.screenshotDelayWithoutZalo;
    } else {
      throw new Error('Loại chụp màn hình không hợp lệ.');
    }

    const validated = validateScreenshotDelay(value, fallback);
    const settings = { ...read(), [key]: validated };
    fs.mkdirSync(userDataDir, { recursive: true });
    const temporary = filename + '.tmp';
    fs.writeFileSync(temporary, JSON.stringify(settings, null, 2), { mode: 0o600 });
    fs.renameSync(temporary, filename);
    return settings;
  }

  function soundUrls() {
    const settings = read();
    const urls = {};
    for (const kind of SOUND_KINDS) {
      try {
        const value = validatePath(settings[kind]);
        if (value) urls[kind] = pathToFileURL(value).href;
      } catch (_) { /* Missing or unreadable files use Zalo's default sound. */ }
    }
    return urls;
  }

  return { read, save, saveScreenshotDelay, soundUrls, validateKind, validatePath };
}

module.exports = { createStore, AUDIO_EXTENSIONS, defaultSoundUrl, DEFAULT_SETTINGS, validateScreenshotDelay };
