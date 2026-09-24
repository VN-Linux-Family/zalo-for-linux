#!/usr/bin/env python3
"""
Patch Zalo Auto Dark/Light Theme:
Monitors GNOME / XDG Portal color-scheme and synchronizes Electron nativeTheme,
IPC theme change event to renderers, and updates DOM .dark class and localStorage.
"""

import os
import sys
import subprocess

THEME_MAIN_INJECTION = r'''
// --- Zalo Linux Auto Dark/Light Theme Sync ---
(function(){
  if (process.platform !== "linux") return;
  const { ipcMain: _ipc, BrowserWindow: _bw, nativeTheme: _nt } = require("electron");

  function isLinuxDark() {
    try {
      const { execSync: _es } = require("child_process");
      try {
        const o = _es("gsettings get org.gnome.desktop.interface color-scheme 2>/dev/null", { timeout: 1000 }).toString();
        if (o.includes("prefer-dark")) return true;
        if (o.includes("default") || o.includes("prefer-light")) return false;
      } catch (_) {}
      try {
        const o = _es('dbus-send --session --print-reply=literal --dest=org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop org.freedesktop.portal.Settings.Read string:"org.freedesktop.appearance" string:"color-scheme" 2>/dev/null', { timeout: 1000 }).toString();
        if (o.includes("uint32 1")) return true;
        if (o.includes("uint32 2") || o.includes("uint32 0")) return false;
      } catch (_) {}
      try {
        const o = _es("gsettings get org.gnome.desktop.interface gtk-theme 2>/dev/null", { timeout: 1000 }).toString().toLowerCase();
        if (o.includes("dark")) return true;
      } catch (_) {}
    } catch (_) {}
    return false;
  }

  _ipc.handle("zalo-linux-get-theme", () => isLinuxDark() ? "dark" : "light");

  let _lastDark = null;
  function syncTheme() {
    const d = isLinuxDark();
    if (d !== _lastDark) {
      _lastDark = d;
      _nt.themeSource = d ? "dark" : "light";
      _bw.getAllWindows().forEach((w) => {
        try {
          if (w && !w.isDestroyed() && w.webContents) {
            w.webContents.send("zalo-linux-theme-change", d ? "dark" : "light");
          }
        } catch (_) {}
      });
    }
  }

  syncTheme();
  try {
    const { spawn: _sp } = require("child_process");
    const _w = _sp("gsettings", ["monitor", "org.gnome.desktop.interface", "color-scheme"]);
    _w.stdout.on("data", () => syncTheme());
    _w.on("error", () => {});
  } catch (_) {}
  setInterval(syncTheme, 3000);
})();
'''

THEME_PRELOAD_INJECTION = r'''
// --- Zalo Linux Auto Dark/Light Theme Sync ---
(function() {
  if (process.platform !== "linux") return;
  const { ipcRenderer } = require("electron");

  function applyTheme(isDark) {
    if (isDark) {
      document.documentElement.classList.add("dark");
      if (document.body) document.body.classList.add("dark");
    } else {
      document.documentElement.classList.remove("dark");
      if (document.body) document.body.classList.remove("dark");
    }
    try {
      const confStr = localStorage.getItem("za_theme");
      let conf = confStr ? JSON.parse(confStr) : {};
      if (conf.theme_setting === 2 || !conf.theme) {
        conf.theme = isDark ? "dark" : "light";
        conf.theme_setting = 2;
        localStorage.setItem("za_theme", JSON.stringify(conf));
      }
    } catch (_) {}
  }

  ipcRenderer.on("zalo-linux-theme-change", (e, mode) => {
    applyTheme(mode === "dark");
  });

  try {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    mq.addEventListener("change", (e) => applyTheme(e.matches));
    applyTheme(mq.matches);
  } catch (_) {}

  ipcRenderer.invoke("zalo-linux-get-theme").then((mode) => {
    applyTheme(mode === "dark");
  }).catch(() => {});
})();
'''

def main():
    base_dir = os.path.join(os.path.dirname(__file__), "..", "..", "app")
    if not os.path.exists(base_dir):
        base_dir = "/home/truongit/zalo-build/app"

    main_js = os.path.join(base_dir, "main-dist/main.js")
    if os.path.exists(main_js):
        with open(main_js, "r", encoding="utf-8") as f:
            content = f.read()
        if "zalo-linux-theme-change" not in content:
            anchor = 'Ae=m.createWithMultiWindow(i,o,gn,oe(),t),g(Ae),v(Ae.webContents),et.setMainWindow(Ae)'
            if anchor in content:
                content = content.replace(anchor, anchor + ";\n" + THEME_MAIN_INJECTION + "\n")
            else:
                content += "\n" + THEME_MAIN_INJECTION + "\n"
            with open(main_js, "w", encoding="utf-8") as f:
                f.write(content)
            print("[+] Injected auto theme watcher into main.js")

    preload_js = os.path.join(base_dir, "main-dist/preload-render.js")
    if os.path.exists(preload_js):
        with open(preload_js, "r", encoding="utf-8") as f:
            content = f.read()
        if "zalo-linux-theme-change" not in content:
            content = content.rstrip() + "\n" + THEME_PRELOAD_INJECTION + "\n"
            with open(preload_js, "w", encoding="utf-8") as f:
                f.write(content)
            print("[+] Injected auto theme sync into preload-render.js")

    print("[SUCCESS] Auto Dark/Light theme patch applied!")

if __name__ == "__main__":
    main()
