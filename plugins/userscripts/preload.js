'use strict';

const { contextBridge, ipcRenderer } = require('electron');
const localeMessages = require('./locale.json').messages;

contextBridge.exposeInMainWorld('userscripts', {
  localeMessages,
  translate: (key, locale) => { const entry = localeMessages['userscripts.' + key]; return entry ? entry[(locale === 'en' ? 'en' : 'vi')] : key; },
  list: () => ipcRenderer.invoke('userscripts:list'),
  save: (script) => ipcRenderer.invoke('userscripts:save', script),
  remove: (id) => ipcRenderer.invoke('userscripts:remove', id),
  toggle: (id, enabled) => ipcRenderer.invoke('userscripts:toggle', id, enabled),
  importFile: () => ipcRenderer.invoke('userscripts:import-file')
});
