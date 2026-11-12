#!/usr/bin/env python3
"""Serves a folder of clips over HTTP for the lab's bench (spec 5.2,
criterion 5): range requests (206), and every request must carry
"Authorization: Bearer <token>" or it gets 401. Binds 127.0.0.1 only.

Usage: scripts/serve-clips.py <folder> <token> [port]   (default port 8765)
Then:  gpui-av-lab --bench corpus/tier-a.json --base-url http://127.0.0.1:8765/ \\
           --header "Authorization: Bearer <token>"

Standard library only. Prints one line per request: method, path, range,
and whether the token was there, so a dropped header shows up at once.
"""

import os
import re
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

CHUNK = 256 * 1024
TYPES = {".mp4": "video/mp4", ".m4a": "audio/mp4", ".mov": "video/quicktime"}


def handler(folder, token):
    expected = f"Bearer {token}"

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *args):
            pass

        def do_HEAD(self):
            self.serve(body=False)

        def do_GET(self):
            self.serve(body=True)

        def status_only(self, code):
            self.send_response(code)
            self.send_header("Content-Length", "0")
            self.end_headers()

        def serve(self, body):
            authorized = self.headers.get("Authorization") == expected
            ranged = self.headers.get("Range")
            print(f"{self.command} {self.path} range={ranged} token={authorized}", flush=True)
            if not authorized:
                return self.status_only(401)
            # Only plain file names: nothing outside the folder.
            name = self.path.split("?", 1)[0].lstrip("/")
            if not name or "/" in name or name.startswith("."):
                return self.status_only(404)
            path = os.path.join(folder, name)
            if not os.path.isfile(path):
                return self.status_only(404)
            size = os.path.getsize(path)
            start, end = 0, size - 1
            match = re.fullmatch(r"bytes=(\d*)-(\d*)", (ranged or "").strip())
            if ranged and match and size > 0:
                first, last = match.groups()
                if first:
                    start = int(first)
                    end = min(int(last), size - 1) if last else size - 1
                elif last:
                    start = max(size - int(last), 0)
                if start > end:
                    self.send_response(416)
                    self.send_header("Content-Range", f"bytes */{size}")
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                self.send_response(206)
                self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
            else:
                self.send_response(200)
            length = end - start + 1 if size > 0 else 0
            self.send_header("Accept-Ranges", "bytes")
            self.send_header("Content-Type", TYPES.get(os.path.splitext(name)[1].lower(), "application/octet-stream"))
            self.send_header("Content-Length", str(length))
            self.end_headers()
            if not body:
                return
            try:
                with open(path, "rb") as file:
                    file.seek(start)
                    left = length
                    while left > 0:
                        chunk = file.read(min(left, CHUNK))
                        if not chunk:
                            break
                        self.wfile.write(chunk)
                        left -= len(chunk)
            except (BrokenPipeError, ConnectionResetError):
                pass

    return Handler


def main():
    if len(sys.argv) not in (3, 4):
        sys.exit(__doc__)
    folder, token = sys.argv[1], sys.argv[2]
    port = int(sys.argv[3]) if len(sys.argv) == 4 else 8765
    server = ThreadingHTTPServer(("127.0.0.1", port), handler(os.path.abspath(folder), token))
    print(f"serving {folder} on http://127.0.0.1:{port}/", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
