'use strict';

const fs = require('fs');
const path = require('path');

// Feature plugins
const screenshotPlugin = require('./screenshot');
const launcherBadgePlugin = require('./launcher-badge');
const trayBadgePlugin = require('./tray-badge');
const userscriptsPlugin = require('./userscripts');
const languageCorePlugin = require('./language-core');
const advancedSettingsPlugin = require('./advanced-settings');
const devModePlugin = require('./dev-mode');
const waylandTitlebarPlugin = require('./wayland-titlebar');
const offlinePlugin = require('./offline');
const zadarkPlugin = require('./zadark');
const recalledMessagesPlugin = require('./recalled-messages');

let activeAdvancedSettings = null;
let activeScreenshot = null;
let activeDevMode = null;
let currentLanguageLocale = null;
let languageRequestSequence = 0;

/**
 * Safely executes a plugin lifecycle function with isolated error handling.
 *
 * @param {string} name Plugin identifier for logging
 * @param {Function} fn Function to execute
 */
function safeRegister(name, fn) {
 try {
  return fn();
 } catch (error) {
  console.error(`[plugins] Failed to register plugin '${name}':`, error);
  return null;
 }
}

/**
 * Early initialization required before Zalo window creation (e.g. Wayland frame check).
 *
 * @param {object} context
 * @param {import('electron').App} context.app
 * @param {import('electron').IpcMain} context.ipcMain
 */
function initEarly(context = {}) {
 const { app, ipcMain } = context;
 safeRegister('waylandTitlebar:init', () => {
  if (waylandTitlebarPlugin && typeof waylandTitlebarPlugin.init === 'function') {
   waylandTitlebarPlugin.init({ app, ipcMain });
  }
 });
}

/**
 * Registers all UI and IPC feature plugins.
 *
 * @param {object} context
 * @param {import('electron').App} context.app
 * @param {import('electron').IpcMain} context.ipcMain
 * @param {typeof import('electron').BrowserWindow} [context.BrowserWindow]
 * @param {typeof import('electron').dialog} [context.dialog]
 * @param {typeof import('electron').nativeImage} [context.nativeImage]
 * @param {import('electron').Tray} [context.tray]
 * @param {string} [context.appDir]
 * @param {string} [context.iconPath]
 */
function register(context = {}) {
 const {
  app,
  ipcMain,
  BrowserWindow,
  dialog,
  nativeImage,
  tray,
  appDir,
  iconPath
 } = context;

 // 1. Core runtime state shared with the main process and settings UI.
 const devMode = safeRegister('devMode', () => {
  if (devModePlugin && typeof devModePlugin.register === 'function') {
   return devModePlugin.register({
    app,
    BrowserWindow,
    onTrayMenuChanged: context.onTrayMenuChanged
   });
  }
  return null;
 });

 activeDevMode = devMode;
 if (currentLanguageLocale && activeDevMode && typeof activeDevMode.setLocale === 'function') {
  activeDevMode.setLocale(currentLanguageLocale);
 }

 // 2. Standalone IPC/UI plugins
 safeRegister('trayBadge', () => {
  if (trayBadgePlugin && typeof trayBadgePlugin.register === 'function') {
   trayBadgePlugin.register({ app, tray, nativeImage, appDir, iconPath });
  }
 });

 safeRegister('launcherBadge', () => {
  if (launcherBadgePlugin && typeof launcherBadgePlugin.register === 'function') {
   launcherBadgePlugin.register({ app, ipcMain });
  }
 });

 safeRegister('screenshot', () => {
  if (screenshotPlugin && typeof screenshotPlugin.register === 'function') {
   activeScreenshot = screenshotPlugin.register({ app, ipcMain });
   if (currentLanguageLocale && activeScreenshot && typeof activeScreenshot.setLocale === 'function') activeScreenshot.setLocale(currentLanguageLocale);
  }
 });

 safeRegister('waylandTitlebar', () => {
  if (waylandTitlebarPlugin && typeof waylandTitlebarPlugin.register === 'function') {
   waylandTitlebarPlugin.register({ app, ipcMain, BrowserWindow });
  }
 });

 safeRegister('offline', () => {
  if (offlinePlugin && typeof offlinePlugin.register === 'function') {
   offlinePlugin.register({ app, ipcMain, appDir });
  }
 });

 // 3. Language Core must be injected before userscripts that may consume its API.
 safeRegister('languageCore', () => {
  if (languageCorePlugin && typeof languageCorePlugin.register === 'function') {
   languageCorePlugin.register({ app, getInitialLocale: () => currentLanguageLocale });
  }
 });

 // 4. Settings & Userscripts (chained dependency)
 const advancedSettings = safeRegister('advancedSettings', () => {
  if (advancedSettingsPlugin && typeof advancedSettingsPlugin.register === 'function') {
   return advancedSettingsPlugin.register({
    app,
    ipcMain,
    BrowserWindow,
    dialog,
    screenshotPlugin,
    devMode
   });
  }
  return null;
 });
 activeAdvancedSettings = advancedSettings;
 if (currentLanguageLocale && activeAdvancedSettings && typeof activeAdvancedSettings.setLocale === 'function') activeAdvancedSettings.setLocale(currentLanguageLocale);

 safeRegister('userscripts', () => {
  if (userscriptsPlugin && typeof userscriptsPlugin.register === 'function') {
   userscriptsPlugin.register({ app, ipcMain, advancedSettings, dialog });
  }
 });

 safeRegister('recalledMessages', () => {
  if (advancedSettings && recalledMessagesPlugin && typeof recalledMessagesPlugin.register === 'function') {
   recalledMessagesPlugin.register({ app, ipcMain, BrowserWindow, advancedSettings });
  }
 });

 // 5. ZaDark Theme Synchronization Plugin (if active in build)
 safeRegister('zadark', () => {
  if (zadarkPlugin && typeof zadarkPlugin.register === 'function') {
   zadarkPlugin.register({ app, ipcMain, appDir });
  }
 });
 return devMode;
}

/**
 * Main-process fan-out for plugin-owned windows that cannot access the
 * Zalo renderer's ZaloLanguage global. Locale is an in-memory snapshot only.
 */
function updateLanguage(locale) {
 if (locale !== 'vi' && locale !== 'en') return false;
 const previousLocale = currentLanguageLocale;
 currentLanguageLocale = locale;
 const request = Object.freeze({
  type: 'LANGUAGE_UPDATE_REQUEST',
  requestId: 'main-language-' + Date.now() + '-' + (++languageRequestSequence),
  locale,
  previousLocale,
  source: 'zalo',
  timestamp: Date.now()
 });
 for (const [name, target] of [['advanced-settings', activeAdvancedSettings], ['screenshot', activeScreenshot], ['dev-mode', activeDevMode]]) {
  try {
   if (target && typeof target.onLanguageUpdateRequest === 'function') target.onLanguageUpdateRequest(request);
  } catch (error) {
   console.error('[plugins] Language update failed for ' + name + ':', error);
  }
 }
 return true;
}

/**
 * Forwards the main window instance to plugins that require it.
 *
 * @param {import('electron').BrowserWindow} mainWindow
 */
function onMainWindowCreated(mainWindow) {
 try {
  if (screenshotPlugin && typeof screenshotPlugin.setMainWindow === 'function') {
   screenshotPlugin.setMainWindow(mainWindow);
  }
 } catch (error) {
  console.error('[plugins] Error in onMainWindowCreated:', error);
 }
}

module.exports = {
 initEarly,
 register,
 updateLanguage,
 onMainWindowCreated,
 _plugins: {
  screenshotPlugin,
  launcherBadgePlugin,
  trayBadgePlugin,
  userscriptsPlugin,
  languageCorePlugin,
  advancedSettingsPlugin,
  devModePlugin,
  waylandTitlebarPlugin,
  offlinePlugin,
  recalledMessagesPlugin
 }
};
