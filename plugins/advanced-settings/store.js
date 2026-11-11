'use strict';

const fs = require('fs');
const path = require('path');
const { pathToFileURL } = require('url');

const SOUND_KINDS = ['ringtone', 'ringback'];
const AUDIO_EXTENSIONS = ['mp3', 'wav', 'ogg', 'm4a', 'aac', 'flac', 'opus', 'webm'];

const ERROR_CODES = Object.freeze({
  INVALID_SOUND_KIND: 'ADVANCED_SETTINGS_INVALID_SOUND_KIND',
  INVALID_DELAY: 'ADVANCED_SETTINGS_INVALID_DELAY',
  NEGATIVE_DELAY: 'ADVANCED_SETTINGS_NEGATIVE_DELAY',
  MAX_DELAY_EXCEEDED: 'ADVANCED_SETTINGS_MAX_DELAY_EXCEEDED',
  INVALID_PATH: 'ADVANCED_SETTINGS_INVALID_PATH',
  ABSOLUTE_PATH_REQUIRED: 'ADVANCED_SETTINGS_ABSOLUTE_PATH_REQUIRED',
  UNSUPPORTED_AUDIO_FORMAT: 'ADVANCED_SETTINGS_UNSUPPORTED_AUDIO_FORMAT',
  AUDIO_FILE_UNREADABLE: 'ADVANCED_SETTINGS_AUDIO_FILE_UNREADABLE',
  INVALID_VALUE: 'ADVANCED_SETTINGS_INVALID_VALUE',
  INVALID_SCREENSHOT_KIND: 'ADVANCED_SETTINGS_INVALID_SCREENSHOT_KIND',
  INVALID_DEV_MODE_VALUE: 'ADVANCED_SETTINGS_INVALID_DEV_MODE_VALUE'
});

function createError(code) {
  const error = new Error(code);
  error.code = code;
  return error;
}

const DEFAULT_SETTINGS = Object.freeze({
  ringtone: '',
  ringback: '',
  screenshotDelayWithZalo: 100,
  screenshotDelayWithoutZalo: 500,
  keepRecalled: false
});

function defaultSoundUrl(kind) {
  if (!SOUND_KINDS.includes(kind)) throw createError(ERROR_CODES.INVALID_SOUND_KIND);
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
    throw createError(ERROR_CODES.INVALID_DELAY);
  }
  if (parsed < 0) {
    throw createError(ERROR_CODES.NEGATIVE_DELAY);
  }
  if (parsed > 10000) {
    throw createError(ERROR_CODES.MAX_DELAY_EXCEEDED);
  }
  return Math.round(parsed);
}

function createStore(userDataDir) {
  const filename = path.join(userDataDir, 'zalo-linux-settings.json');

  function readRaw() {
    try {
      const saved = JSON.parse(fs.readFileSync(filename, 'utf8'));
      return saved && typeof saved === 'object' && !Array.isArray(saved) ? saved : {};
    } catch (error) {
      if (error.code !== 'ENOENT') console.error('[advanced-settings] Cannot read settings:', error.message);
      return {};
    }
  }

  function read() {
    const settings = { ...DEFAULT_SETTINGS };
    try {
      const saved = readRaw();
      for (const kind of SOUND_KINDS) {
        if (typeof saved[kind] === 'string') settings[kind] = saved[kind];
      }
      if (typeof saved.screenshotDelayWithZalo === 'number' && Number.isFinite(saved.screenshotDelayWithZalo) && saved.screenshotDelayWithZalo >= 0) {
        settings.screenshotDelayWithZalo = Math.round(saved.screenshotDelayWithZalo);
      }
      if (typeof saved.screenshotDelayWithoutZalo === 'number' && Number.isFinite(saved.screenshotDelayWithoutZalo) && saved.screenshotDelayWithoutZalo >= 0) {
        settings.screenshotDelayWithoutZalo = Math.round(saved.screenshotDelayWithoutZalo);
      }
      if (saved.keepRecalled === true) settings.keepRecalled = true;
    } catch (error) {
      console.error('[advanced-settings] Cannot read settings:', error.message);
    }
    return settings;
  }

  function validateKind(kind) {
    if (!SOUND_KINDS.includes(kind)) throw createError(ERROR_CODES.INVALID_SOUND_KIND);
  }

  function validatePath(value) {
    if (typeof value !== 'string') throw createError(ERROR_CODES.INVALID_PATH);
    if (!value) return '';
    if (!path.isAbsolute(value)) throw createError(ERROR_CODES.ABSOLUTE_PATH_REQUIRED);
    if (!AUDIO_EXTENSIONS.includes(path.extname(value).slice(1).toLowerCase())) {
      throw createError(ERROR_CODES.UNSUPPORTED_AUDIO_FORMAT);
    }
    try {
      if (!fs.statSync(value).isFile()) throw new Error('not a file');
      fs.accessSync(value, fs.constants.R_OK);
    } catch (_) {
      throw createError(ERROR_CODES.AUDIO_FILE_UNREADABLE);
    }
    return path.normalize(value);
  }

  function write(settings) {
    fs.mkdirSync(userDataDir, { recursive: true });
    const merged = { ...readRaw(), ...settings };
    const temporary = filename + '.tmp';
    fs.writeFileSync(temporary, JSON.stringify(merged, null, 2), { mode: 0o600 });
    fs.renameSync(temporary, filename);
    return merged;
  }

  function save(kind, value) {
    validateKind(kind);
    const validated = validatePath(value);
    return write({ ...read(), [kind]: validated });
  }

  function setKeepRecalled(value) {
    if (typeof value !== 'boolean') throw createError(ERROR_CODES.INVALID_VALUE);
    return write({ ...read(), keepRecalled: value });
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
      throw createError(ERROR_CODES.INVALID_SCREENSHOT_KIND);
    }

    const validated = validateScreenshotDelay(value, fallback);
    return write({ ...read(), [key]: validated });
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

  return { read, save, saveScreenshotDelay, setKeepRecalled, soundUrls, validateKind, validatePath };
}

module.exports = { createStore, AUDIO_EXTENSIONS, defaultSoundUrl, DEFAULT_SETTINGS, validateScreenshotDelay, ERROR_CODES, createError };
