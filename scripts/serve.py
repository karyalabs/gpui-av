#!/usr/bin/env python3
"""Serves media for gpui-av-lab, on 127.0.0.1 only. Standard library only.

    scripts/serve.py [--port 8090] [--token lab] [--app DIR] FOLDER

  /clips/<file>     a file in FOLDER, with range requests
  /clips/index.json the names of the media files in FOLDER
  /private/<file>   the same files, for requests that carry
                    "Authorization: Bearer <token>"; 401 without
  /                 the lab's web build, with --app (scripts/web passes it)

Every answer carries the headers that make a page cross-origin isolated,
which the lab's web build needs.
"""

import argparse
import functools
import http.server
import json
import mimetypes
import os
import re
import sys
import urllib.parse

MEDIA = {".mp4", ".m4v", ".mov", ".m4a", ".webm", ".mkv", ".mp3", ".ogg", ".wav"}
TYPES = {
    ".wasm": "application/wasm",
    ".js": "text/javascript",
    ".mp4": "video/mp4",
    ".m4v": "video/mp4",
    ".m4a": "audio/mp4",
    ".mov": "video/quicktime",
    ".webm": "video/webm",
    ".mkv": "video/x-matroska",
}
RANGE = re.compile(r"bytes=(\d*)-(\d*)$")


def parse_range(header, size):
    """The first and last byte a Range header asks for, or None when the
    header is absent or cannot be satisfied."""
    match = RANGE.match(header or "")
    if not match or size == 0:
        return None
    first, last = match.groups()
    if first == "" and last == "":
        return None
    if first == "":
        # The last N bytes.
        start, end = max(size - int(last), 0), size - 1
    else:
        start = int(first)
        end = min(int(last), size - 1) if last else size - 1
    return (start, end) if start <= end < size else None


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def __init__(self, *args, folder, app, token, **kwargs):
        self.folder, self.app, self.token = folder, app, token
        super().__init__(*args, **kwargs)

    def log_message(self, format, *args):
        if os.environ.get("GPUI_AV_SERVE_LOG"):
            super().log_message(format, *args)

    def end_headers(self):
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        self.send_header("Cross-Origin-Resource-Policy", "same-origin")
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def answer(self, status, body=b"", content_type="text/plain; charset=utf-8", extra=()):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        for name, value in extra:
            self.send_header(name, value)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def resolve(self, base, name):
        """The file `name` inside `base`, or None when it is not one."""
        path = os.path.realpath(os.path.join(base, name))
        inside = path.startswith(os.path.realpath(base) + os.sep)
        return path if inside and os.path.isfile(path) else None

    def do_HEAD(self):
        self.do_GET()

    def do_GET(self):
        path = urllib.parse.unquote(urllib.parse.urlsplit(self.path).path)
        if path.startswith("/private/"):
            if self.headers.get("Authorization") != f"Bearer {self.token}":
                return self.answer(401, b"unauthorized\n", extra=[("WWW-Authenticate", "Bearer")])
            return self.send_file(self.resolve(self.folder, path[len("/private/"):]))
        if path == "/clips/index.json":
            names = sorted(
                name
                for name in os.listdir(self.folder)
                if os.path.splitext(name)[1].lower() in MEDIA
                and os.path.isfile(os.path.join(self.folder, name))
            )
            return self.answer(200, json.dumps(names).encode(), "application/json")
        if path.startswith("/clips/"):
            return self.send_file(self.resolve(self.folder, path[len("/clips/"):]))
        if self.app:
            name = "index.html" if path == "/" else path.lstrip("/")
            return self.send_file(self.resolve(self.app, name))
        self.answer(404, b"not found\n")

    def send_file(self, path):
        if path is None:
            return self.answer(404, b"not found\n")
        size = os.path.getsize(path)
        extension = os.path.splitext(path)[1].lower()
        content_type = (
            TYPES.get(extension) or mimetypes.guess_type(path)[0] or "application/octet-stream"
        )
        wanted = self.headers.get("Range")
        span = parse_range(wanted, size)
        if wanted and span is None:
            return self.answer(416, extra=[("Content-Range", f"bytes */{size}")])
        start, end = span or (0, size - 1)
        self.send_response(206 if span else 200)
        self.send_header("Content-Type", content_type)
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Length", str(end - start + 1 if size else 0))
        if span:
            self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        self.end_headers()
        if self.command == "HEAD" or size == 0:
            return
        try:
            with open(path, "rb") as file:
                file.seek(start)
                left = end - start + 1
                while left > 0:
                    chunk = file.read(min(left, 256 * 1024))
                    if not chunk:
                        break
                    self.wfile.write(chunk)
                    left -= len(chunk)
        except (BrokenPipeError, ConnectionResetError):
            # A player drops a request as soon as it has what it needs.
            self.close_connection = True


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("folder", help="the folder of media to serve")
    parser.add_argument("--port", type=int, default=8090)
    parser.add_argument("--token", default="lab", help="the bearer token /private/ asks for")
    parser.add_argument("--app", help="the lab's web build, served at /")
    options = parser.parse_args()
    if not os.path.isdir(options.folder):
        sys.exit(f"{options.folder} is not a folder")
    handler = functools.partial(
        Handler, folder=options.folder, app=options.app, token=options.token
    )
    server = http.server.ThreadingHTTPServer(("127.0.0.1", options.port), handler)
    print(
        f"serving {options.folder} on http://127.0.0.1:{options.port}/ (Ctrl-C stops)",
        flush=True,
    )
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
