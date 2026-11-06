'use strict';

require('../userscripts/preload');
const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('linuxSettings', {
  get: () => ipcRenderer.invoke('linux-settings:get'),
  saveSound: (kind, value) => ipcRenderer.invoke('linux-settings:save-sound', kind, value),
  saveScreenshotDelay: (kind, value) => ipcRenderer.invoke('linux-settings:save-screenshot-delay', kind, value),
  pickSound: (kind) => ipcRenderer.invoke('linux-settings:pick-sound', kind),
  previewSound: (kind, value) => ipcRenderer.invoke('linux-settings:preview-sound', kind, value)
});
