#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Exercise the installed journal host, on a display, without pairing an owner."""
import argparse
import contextlib
import http.server
import os
import queue
import selectors
import socketserver
import subprocess
import sys
import threading

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("viewer")
parser.add_argument("--user-links", action="store_true",
                    help="Exercise real link activation on a test X11 display (xdotool and PyGObject required)")
parser.add_argument("--private-link-bus", action="store_true", help=argparse.SUPPRESS)
args = parser.parse_args()
if args.user_links and not args.private_link_bus:
    raise SystemExit(subprocess.call(["dbus-run-session", "--", sys.executable, __file__,
                                      *sys.argv[1:], "--private-link-bus"]))
outside_actions = queue.Queue()
test_links = ["https://services.solstone.app/", "https://solstone.app/guide/"]
painted = threading.Event()
frame_loaded = threading.Event()
outside_connection = threading.Event()


def test_portal():
    # The private test bus captures the desktop's OpenURI call before any browser launches.
    from gi.repository import Gio, GLib
    bus = Gio.bus_get_sync(Gio.BusType.SESSION)
    result = bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                           "RequestName", GLib.Variant("(su)", ("org.freedesktop.portal.Desktop", 4)),
                           None, Gio.DBusCallFlags.NONE, 2000, None)
    assert result.unpack() == (1,)
    interface = Gio.DBusNodeInfo.new_for_xml("""<node><interface name="org.freedesktop.portal.OpenURI">
        <method name="OpenURI"><arg type="s" direction="in"/><arg type="s" direction="in"/>
        <arg type="a{sv}" direction="in"/><arg type="o" direction="out"/></method></interface></node>""")
    def opened(_bus, _sender, _path, _interface, _method, parameters, invocation):
        outside_actions.put(parameters.unpack()[1])
        invocation.return_value(GLib.Variant("(o)", ("/org/freedesktop/portal/desktop/request/smoke",)))
    bus.register_object("/org/freedesktop/portal/desktop", interface.interfaces[0], opened, None, None)
    loop = GLib.MainLoop()
    threading.Thread(target=loop.run, daemon=True).start()
    return bus, loop


class OutsideSite(socketserver.BaseRequestHandler):
    def handle(self):
        outside_connection.set()


class Page(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        if self.path.startswith("/_bridge/bootstrap?"):
            self.send_response(303)
            self.send_header("Location", "/page" if self.path.endswith("ok") else ("/missing" if self.path.endswith("missing") else "/unavailable"))
            self.end_headers()
        elif self.path == "/page":
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.end_headers()
            page = """<!doctype html><title>journal fixture</title>
              <a id="outside" href="https://127.0.0.1:PORT/fixture">outside</a>
              <a id="services" href="SERVICES">services</a><a id="help" href="GUIDE">guide</a>
              <iframe src="/embedded-check"></iframe>
              <script>requestAnimationFrame(async () => {
                const host = window.solstoneJournalHost;
                if (!host || host.version !== 2) return;
                const unknown = await host.send({type:'smoke.future', id:'smoke-id', extra:true});
                if (unknown.type !== 'unsupported' || unknown.id !== 'smoke-id') return;
                document.getElementById('outside').click();
                document.getElementById('services').focus();
                setTimeout(() => fetch('/painted?host=' + String(window.solstoneJournalExternalNavigation)), 400);
              });</script>"""
            self.wfile.write(page.replace("PORT", str(outside.server_address[1]))
                             .replace("SERVICES", test_links[0]).replace("GUIDE", test_links[1]).encode())
        elif self.path == "/embedded-check":
            frame_loaded.set()
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.end_headers()
            self.wfile.write(b"<!doctype html><title>frame fixture</title>")
        elif self.path == "/painted?host=1":
            if "SolstoneJournalExternal/1" in self.headers.get("User-Agent", ""):
                painted.set()
            self.send_response(204)
            self.end_headers()
        else:
            self.send_error(503 if self.path == "/unavailable" else 404)


with contextlib.ExitStack() as cleanup:
    if args.user_links:
        bus, event_loop = test_portal()
        cleanup.callback(event_loop.quit)
    outside = cleanup.enter_context(socketserver.TCPServer(("127.0.0.1", 0), OutsideSite))
    threading.Thread(target=outside.serve_forever, daemon=True).start()
    cleanup.callback(outside.shutdown)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Page)
    cleanup.callback(server.server_close)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    cleanup.callback(server.shutdown)
    child = subprocess.Popen([args.viewer], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    cleanup.callback(child.wait)
    cleanup.callback(child.kill)
    base = f"http://127.0.0.1:{server.server_port}"
    with selectors.DefaultSelector() as selector:
        selector.register(child.stdout, selectors.EVENT_READ)
        for target, reply in (("ok", b"+"), ("ok", b"+"), ("missing", b"+"), ("unavailable", b"-")):
            child.stdin.write(f"{base}/_bridge/bootstrap?token={target}\n".encode())
            child.stdin.flush()
            if not selector.select(8) or os.read(child.stdout.fileno(), 1) != reply:
                raise SystemExit("viewer failed to acknowledge " + target)
            if target == "ok" and not painted.wait(5):
                raise SystemExit("journal script or host identification failed")
            if target == "ok" and args.user_links:
                window = subprocess.check_output(["xdotool", "search", "--onlyvisible", "--name", "^solstone$"],
                                                 text=True).splitlines()[-1]
                subprocess.run(["xdotool", "windowfocus", "--sync", window], check=True)
                for index, url in enumerate(test_links):
                    if index:
                        subprocess.run(["xdotool", "key", "Tab"], check=True)
                    subprocess.run(["xdotool", "key", "Return"], check=True)
                    if outside_actions.get(timeout=5) != url:
                        raise SystemExit("outside link did not reach the system handler")
            painted.clear()
    if frame_loaded.is_set():
        raise SystemExit("journal viewer admitted an embedded document")
    if outside_connection.is_set():
        raise SystemExit("an untrusted outside navigation connected to a different origin")
    print("outside connection blocked, first open, same-window reopen, host identification and failed navigation passed")
