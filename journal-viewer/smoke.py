#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Exercise the installed journal host, on a display, without pairing an owner."""
import argparse
import contextlib
import http.server
import os
import selectors
import subprocess
import threading

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("viewer")
args = parser.parse_args()
painted = threading.Event()


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
            self.wfile.write(b"""<!doctype html><title>journal fixture</title><script>
              requestAnimationFrame(() => fetch('/painted?host=' + String(window.solstoneJournalExternalNavigation)));
              </script>""")
        elif self.path == "/painted?host=1":
            if "SolstoneJournalExternal/1" in self.headers.get("User-Agent", ""):
                painted.set()
            self.send_response(204)
            self.end_headers()
        else:
            self.send_error(503 if self.path == "/unavailable" else 404)


with contextlib.ExitStack() as cleanup:
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
            painted.clear()
    print("first open, same-window reopen, host identification and failed navigation passed")
