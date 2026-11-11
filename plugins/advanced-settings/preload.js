'use strict';

require('../userscripts/preload');
const { contextBridge, ipcRenderer } = require('electron');
const localeMessages = require('./locale.json').messages;

contextBridge.exposeInMainWorld('linuxSettings', {
  localeMessages,
  translate: (key, locale) => { const entry = localeMessages['advanced-settings.' + key]; return entry ? entry[(locale === 'en' ? 'en' : 'vi')] : key; },
  get: () => ipcRenderer.invoke('linux-settings:get'),
  onLanguageUpdate: (callback) => {
    if (typeof callback !== 'function') return;
    ipcRenderer.on('linux-settings:language-update', (_event, locale) => callback(locale));
  },
  saveSound: (kind, value) => ipcRenderer.invoke('linux-settings:save-sound', kind, value),
  saveScreenshotDelay: (kind, value) => ipcRenderer.invoke('linux-settings:save-screenshot-delay', kind, value),
  pickSound: (kind) => ipcRenderer.invoke('linux-settings:pick-sound', kind),
  previewSound: (kind, value) => ipcRenderer.invoke('linux-settings:preview-sound', kind, value),
  setDevMode: (enabled) => ipcRenderer.invoke('linux-settings:set-dev-mode', enabled),
  setKeepRecalled: (enabled) => ipcRenderer.invoke('linux-settings:set-keep-recalled', enabled)
});
