'use strict';

const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('screencap', {
  onLanguageUpdate: (callback) => {
    if (typeof callback !== 'function') return;
    ipcRenderer.on('screencap:language', (_event, locale) => callback(locale));
  },
  onInit: (callback) => {
    ipcRenderer.on('screencap:init', (_event, data) => callback(data));
  },
  accept: (dataUrl) => {
    ipcRenderer.send('screencap:action', { action: 'accept', dataUrl });
  },
  copy: (dataUrl) => {
    ipcRenderer.send('screencap:action', { action: 'copy', dataUrl });
  },
  save: (dataUrl) => {
    ipcRenderer.send('screencap:action', { action: 'save', dataUrl });
  },
  cancel: () => {
    ipcRenderer.send('screencap:action', { action: 'cancel' });
  }
});
