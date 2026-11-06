"""Silent screenshot portal client using GIO/GLib ctypes (no PyGObject or external tools)."""

import ctypes as c
import os
import shutil
import sys
import urllib.parse
import uuid
from pathlib import Path

gio = c.CDLL('libgio-2.0.so.0')
glib = c.CDLL('libglib-2.0.so.0')
ptr = c.c_void_p
string = c.c_char_p
uint = c.c_uint


def bind(lib, name, result, *args):
    fn = getattr(lib, name)
    fn.restype = result
    fn.argtypes = args
    return fn


bus_get = bind(gio, 'g_bus_get_sync', ptr, c.c_int, ptr, ptr)
unique_name = bind(gio, 'g_dbus_connection_get_unique_name', string, ptr)
call = bind(gio, 'g_dbus_connection_call_sync', ptr,
            ptr, string, string, string, string, ptr, ptr, c.c_int, c.c_int, ptr, ptr)
subscribe = bind(gio, 'g_dbus_connection_signal_subscribe', uint,
                 ptr, string, string, string, string, string, c.c_int, ptr, ptr, ptr)
parse = bind(glib, 'g_variant_parse', ptr, ptr, string, ptr, ptr, ptr)
child = bind(glib, 'g_variant_get_child_value', ptr, ptr, c.c_size_t)
get_uint = bind(glib, 'g_variant_get_uint32', uint, ptr)
get_string = bind(glib, 'g_variant_get_string', string, ptr, ptr)
lookup = bind(glib, 'g_variant_lookup_value', ptr, ptr, string, ptr)
unref = bind(glib, 'g_variant_unref', None, ptr)
loop_new = bind(glib, 'g_main_loop_new', ptr, ptr, c.c_int)
loop_run = bind(glib, 'g_main_loop_run', None, ptr)
loop_quit = bind(glib, 'g_main_loop_quit', None, ptr)
timeout_add = bind(glib, 'g_timeout_add', uint, uint, ptr, ptr)


def capture(output_path=None, interactive=False, timeout_ms=10000):
    error = ptr()
    bus = bus_get(2, None, c.byref(error))  # G_BUS_TYPE_SESSION
    if not bus:
        err_msg = 'Unknown error'
        if error:
            class GError(c.Structure):
                _fields_ = [('domain', uint), ('code', c.c_int), ('message', string)]
            err_msg = c.cast(error, c.POINTER(GError)).contents.message.decode('utf-8', errors='ignore')
        raise RuntimeError(f'Cannot connect to session bus: {err_msg}')

    destination = b'org.freedesktop.portal.Desktop'
    token = 'zalo_screenshot_' + uuid.uuid4().hex
    sender = unique_name(bus).decode()[1:].replace('.', '_')
    request = f'/org/freedesktop/portal/desktop/request/{sender}/{token}'.encode()
    loop = loop_new(None, False)
    result_path = None
    response_code = None
    timed_out = False

    callback_type = c.CFUNCTYPE(None, ptr, string, string, string, string, ptr, ptr)
    timer_type = c.CFUNCTYPE(c.c_int, ptr)

    @callback_type
    def on_response(connection, sender_name, object_path, interface, signal, parameters, data):
        nonlocal result_path, response_code
        if object_path != request:
            return
        response = child(parameters, 0)
        response_code = get_uint(response)
        if response_code == 0:
            results = child(parameters, 1)
            value = lookup(results, b'uri', None)
            if value:
                uri = get_string(value, None).decode()
                if uri.startswith('file://'):
                    result_path = urllib.parse.unquote(urllib.parse.urlparse(uri).path)
                else:
                    result_path = uri
                unref(value)
            unref(results)
        unref(response)
        loop_quit(loop)

    @timer_type
    def on_timeout(data):
        nonlocal timed_out
        timed_out = True
        loop_quit(loop)
        return False

    subscribe(bus, destination, b'org.freedesktop.portal.Request', b'Response',
              None, None, 0, on_response, None, None)

    inter_str = 'true' if interactive else 'false'
    parameters = parse(None, (
        "('', {'handle_token': <'%s'>, 'interactive': <%s>})" % (token, inter_str)
    ).encode(), None, None, None)

    call_err = ptr()
    reply = call(bus, destination, b'/org/freedesktop/portal/desktop',
                 b'org.freedesktop.portal.Screenshot', b'Screenshot',
                 parameters, None, 0, 10000, None, c.byref(call_err))
    if not reply:
        err_msg = 'Unknown D-Bus call error'
        if call_err:
            class GError(c.Structure):
                _fields_ = [('domain', uint), ('code', c.c_int), ('message', string)]
            err_msg = c.cast(call_err, c.POINTER(GError)).contents.message.decode('utf-8', errors='ignore')
        raise RuntimeError(f'Screenshot portal request failed: {err_msg}')

    handle = child(reply, 0)
    unref(handle)
    unref(reply)

    timeout_add(timeout_ms, on_timeout, None)
    loop_run(loop)

    if timed_out:
        raise TimeoutError(f'Screenshot portal timed out after {timeout_ms}ms waiting for response')

    if response_code is not None and response_code != 0:
        desc = {1: 'User cancelled', 2: 'Request failed/denied'}.get(response_code, f'Error code {response_code}')
        raise RuntimeError(f'Portal response returned non-zero code {response_code}: {desc}')

    if not result_path or not os.path.isfile(result_path):
        raise FileNotFoundError(f'Portal did not produce a valid image file (got: {result_path})')

    if output_path:
        dest = Path(output_path)
        dest.parent.mkdir(parents=True, exist_ok=True)
        try:
            shutil.copy2(result_path, str(dest))
            # Delete original from user's Pictures directory if it was saved there
            try:
                os.unlink(result_path)
            except OSError:
                pass
            return str(dest)
        except Exception as copy_err:
            sys.stderr.write(f'Warning: failed to move portal output to {output_path}: {copy_err}\n')
            return result_path

    return result_path


def main():
    output_path = None
    interactive = False
    args = sys.argv[1:]
    i = 0
    while i < len(args):
        if args[i] == '--output' and i + 1 < len(args):
            output_path = args[i + 1]
            i += 2
        elif args[i] == '--interactive':
            interactive = True
            i += 1
        else:
            i += 1

    try:
        path = capture(output_path=output_path, interactive=interactive)
        if path:
            print(path)
            sys.exit(0)
        else:
            sys.exit(1)
    except Exception as e:
        sys.stderr.write(f'Portal error: {e}\n')
        sys.exit(1)


if __name__ == '__main__':
    main()
