#!/usr/bin/env python3
"""screenbridge.py — Wayland screen-share bridge for ZaloCall under Wine.

Requests a screen stream through the XDG ScreenCast portal (this pops the
compositor's permission/source dialog), receives the PipeWire fd, and hands
it to gst-launch which renders the stream fullscreen into the Xvfb display
given as argv[1] (e.g. ":99").

Usage: python3 screenbridge.py :99 [app_id]

Requires: python3-dbus, gst-launch-1.0 with pipewiresrc (plugins-bad) and
ximagesink (plugins-base).
"""

import os
import subprocess
import sys
import time

import dbus
import dbus.mainloop.glib
from dbus.mainloop.glib import DBusGMainLoop

DISPLAY2 = sys.argv[1] if len(sys.argv) > 1 else ":99"
# KDE's portal backend rejects app ids without a matching .desktop file.
APP_ID = sys.argv[2] if len(sys.argv) > 2 else "org.kde.konsole"


def make_iface(bus, name, direct_backend=False):
    portal = bus.get_object(name, "/org/freedesktop/portal/desktop")
    iface_name = (
        "org.freedesktop.impl.portal.ScreenCast"
        if direct_backend
        else "org.freedesktop.portal.ScreenCast"
    )
    return dbus.Interface(portal, iface_name)


def run():
    DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    from gi.repository import GLib

    iface = make_iface(bus, "org.freedesktop.portal.Desktop")
    direct_backend = False
    token = "zalo_screenbridge_%d" % os.getpid()

    state = {"phase": "session", "session": None, "node_id": None, "denied": False}

    def on_response(response, results, message=None):
        try:
            if response != 0:
                # Some backends only support monitor sources — retry the
                # selection without the window type before giving up.
                if state["phase"] == "sources" and not state.get("retried"):
                    state["retried"] = True
                    print("source selection rejected (%s), retrying monitors-only" % response, file=sys.stderr)
                    try:
                        iface.SelectSources(state["session"],
                                            {"types": dbus.UInt32(1), "multiple": False})
                        return
                    except Exception as e:
                        print("monitors-only retry failed: %s" % e, file=sys.stderr)
                print("portal response %s, denying" % response, file=sys.stderr)
                state["denied"] = True
                state["loop"].quit()
                return
            if state["phase"] == "session":
                for k, v in results.items():
                    if str(k) == "session_handle":
                        state["session"] = dbus.ObjectPath(str(v))
                        state["phase"] = "sources"
                        print("session handle: %s" % state["session"], file=sys.stderr)
                        break
            elif state["phase"] == "sources":
                # user finished choosing sources (or the choice was implicit)
                print("sources selected", file=sys.stderr)
                state["phase"] = "start"
            elif state["phase"] == "start":
                for k, v in results.items():
                    if str(k) == "streams":
                        streams = v
                        if not streams:
                            print("no streams in result", file=sys.stderr)
                            state["denied"] = True
                            state["loop"].quit()
                            return
                        node_id, second = streams[0]
                        # Portal >= 1.19: connect to the PipeWire node
                        # directly (the fd is no longer passed around).
                        state["node_id"] = int(node_id)
                        print("got pipewire node %d" % state["node_id"], file=sys.stderr)
                        state["loop"].quit()
                        return
        except Exception as e:  # noqa
            print("response error: %s" % e, file=sys.stderr)
            state["loop"].quit()

    # Response signals are emitted from the request object paths, so listen
    # at any path (impl backends use org.freedesktop.impl.portal.Request).
    bus.add_signal_receiver(
        on_response,
        signal_name="Response",
        dbus_interface="org.freedesktop.portal.Request",
        message_keyword="message",
    )
    bus.add_signal_receiver(
        on_response,
        signal_name="Response",
        dbus_interface="org.freedesktop.impl.portal.Request",
        message_keyword="message",
    )

    loop = GLib.MainLoop()
    state["loop"] = loop

    def wait_phase(target, seconds):
        ctx = GLib.MainContext.default()
        waited = 0
        while not state["denied"] and state["phase"] != target and waited < seconds * 50:
            ctx.iteration(False)
            time.sleep(0.02)
            waited += 1
        return state["phase"] == target

    def create_session():
        # session_handle_token is REQUIRED by portal >= 1.20 — missing it
        # triggers an assertion crash in xdp-session.c. persist_mode 0 makes
        # the compositor ask for the source EVERY time (KDE otherwise
        # silently re-uses the previous selection).
        iface.CreateSession(
            {
                "handle_token": token,
                "session_handle_token": token + "_session",
                "persist_mode": dbus.UInt32(0),
            }
        )

    try:
        create_session()
        if not wait_phase("sources", 15):
            print("timed out waiting for session", file=sys.stderr)
            sys.exit(1)
    except Exception as e:
        print("portal attempt failed: %s" % e, file=sys.stderr)
        sys.exit(1)

    try:
        # 1 = monitor, 2 = window — offer both in the compositor's dialog.
        iface.SelectSources(state["session"], {"types": dbus.UInt32(3), "multiple": False})
        # The compositor dialog stays open until the user picks a source.
        if not wait_phase("start", 120):
            print("timed out or denied waiting for source selection", file=sys.stderr)
            sys.exit(1)

        iface.Start(state["session"], "", {"handle_token": token + "_start"})
        loop.run()
    except Exception as e:
        print("start failed: %s" % e, file=sys.stderr)
        sys.exit(1)

    if state["denied"] or state.get("node_id") is None:
        print("portal denied or no streams", file=sys.stderr)
        sys.exit(1)

    cmd = [
        os.environ.get("ZCALL_GST_HOST_LAUNCH") or "gst-launch-1.0",
        "pipewiresrc", "path=%d" % state["node_id"],
        "!", "videoconvert",
        "!", "ximagesink", "display=%s" % DISPLAY2, "sync=false",
    ]
    env = None
    if "ZCALL_GST_HOST_LAUNCH" in os.environ:
        # Host gst stack (set by the app when the host has pipewiresrc +
        # ximagesink): strip the bundle's loader/plugin overrides so the
        # host binary resolves its OWN libs and plugins — the bundled
        # jammy gstpipewiresrc cannot take dma-buf/modifier buffers from
        # modern daemons and would fail with "error alloc buffers".
        env = dict(os.environ)
        for k in ("LD_LIBRARY_PATH", "GST_PLUGIN_PATH", "GST_PLUGIN_SYSTEM_PATH",
                  "GST_PLUGIN_SCANNER", "GST_REGISTRY", "GI_TYPELIB_PATH"):
            env.pop(k, None)
    elif "ZCALL_GST_BUNDLE_LIBDIR" in os.environ:
        # Bundle gst stack: python itself runs on the HOST libs (the app
        # strips the overrides from python's env so the jammy glib never
        # loads into the host interpreter) — re-compose the gst child's
        # env from the ZCALL_GST_BUNDLE_* variables for the pipeline only.
        env = dict(os.environ)
        lib = os.environ["ZCALL_GST_BUNDLE_LIBDIR"]
        host_ld = os.environ.get("LD_LIBRARY_PATH")
        env["LD_LIBRARY_PATH"] = lib + (":" + host_ld if host_ld else "")
        env["GST_PLUGIN_PATH"] = os.environ["ZCALL_GST_BUNDLE_PLUGIN_PATH"]
        env["GST_PLUGIN_SYSTEM_PATH"] = os.environ["ZCALL_GST_BUNDLE_PLUGIN_SYSTEM_PATH"]
        env["GST_PLUGIN_SCANNER"] = os.environ["ZCALL_GST_BUNDLE_PLUGIN_SCANNER"]
        if "ZCALL_GST_BUNDLE_REGISTRY" in os.environ:
            env["GST_REGISTRY"] = os.environ["ZCALL_GST_BUNDLE_REGISTRY"]
        for k in ("ZCALL_GST_BUNDLE_LIBDIR", "ZCALL_GST_BUNDLE_PLUGIN_PATH",
                  "ZCALL_GST_BUNDLE_PLUGIN_SYSTEM_PATH", "ZCALL_GST_BUNDLE_PLUGIN_SCANNER",
                  "ZCALL_GST_BUNDLE_REGISTRY"):
            env.pop(k, None)
    os.environ["DISPLAY"] = DISPLAY2
    proc = subprocess.Popen(cmd, env=env)
    print("gst pid %d" % proc.pid, file=sys.stderr)
    proc.wait()
    # The app kills gst when the share ends (capture heartbeat goes
    # stale) — this is where python wakes up. Close the portal session so
    # the compositor's recording indicator disappears; a session left open
    # keeps the "screen casting" badge alive forever.
    try:
        iface.Close(state["session"])
        ctx = GLib.MainContext.default()
        for _ in range(50):
            if ctx.pending():
                ctx.iteration(False)
            time.sleep(0.02)
        print("portal session closed", file=sys.stderr)
    except Exception as e:
        print("close session failed: %s" % e, file=sys.stderr)


if __name__ == "__main__":
    run()
