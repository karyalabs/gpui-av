#!/usr/bin/env python3
"""Tests for scripts/serve.py: python3 scripts/test-serve.py"""

import functools
import http.client
import http.server
import json
import os
import sys
import tempfile
import threading
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import serve  # noqa: E402


class ParseRange(unittest.TestCase):
    def test_a_range_names_its_first_and_last_byte(self):
        self.assertEqual(serve.parse_range("bytes=0-99", 1000), (0, 99))
        self.assertEqual(serve.parse_range("bytes=900-", 1000), (900, 999))
        self.assertEqual(serve.parse_range("bytes=-100", 1000), (900, 999))
        self.assertEqual(serve.parse_range("bytes=0-5000", 1000), (0, 999))

    def test_a_range_outside_the_file_is_refused(self):
        self.assertIsNone(serve.parse_range("bytes=1000-", 1000))
        self.assertIsNone(serve.parse_range("bytes=5-2", 1000))
        self.assertIsNone(serve.parse_range("bytes=-", 1000))
        self.assertIsNone(serve.parse_range("lines=1-2", 1000))
        self.assertIsNone(serve.parse_range(None, 1000))


class Server(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.folder = tempfile.TemporaryDirectory()
        cls.app = tempfile.TemporaryDirectory()
        with open(os.path.join(cls.folder.name, "clip.mp4"), "wb") as file:
            file.write(bytes(range(256)) * 4)
        with open(os.path.join(cls.folder.name, "notes.txt"), "w") as file:
            file.write("not media")
        with open(os.path.join(cls.app.name, "index.html"), "w") as file:
            file.write("<p>lab</p>")
        handler = functools.partial(
            serve.Handler, folder=cls.folder.name, app=cls.app.name, token="secret"
        )
        cls.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()
        cls.folder.cleanup()
        cls.app.cleanup()

    def get(self, path, headers=None):
        connection = http.client.HTTPConnection("127.0.0.1", self.server.server_port)
        connection.request("GET", path, headers=headers or {})
        response = connection.getresponse()
        body = response.read()
        connection.close()
        return response, body

    def test_a_clip_is_served_whole_with_the_isolation_headers(self):
        response, body = self.get("/clips/clip.mp4")
        self.assertEqual(response.status, 200)
        self.assertEqual(len(body), 1024)
        self.assertEqual(response.getheader("Content-Type"), "video/mp4")
        self.assertEqual(response.getheader("Accept-Ranges"), "bytes")
        self.assertEqual(response.getheader("Cross-Origin-Opener-Policy"), "same-origin")
        self.assertEqual(response.getheader("Cross-Origin-Embedder-Policy"), "require-corp")

    def test_a_range_request_gets_that_part(self):
        response, body = self.get("/clips/clip.mp4", {"Range": "bytes=10-19"})
        self.assertEqual(response.status, 206)
        self.assertEqual(body, bytes(range(10, 20)))
        self.assertEqual(response.getheader("Content-Range"), "bytes 10-19/1024")

    def test_a_range_past_the_end_is_refused(self):
        response, _ = self.get("/clips/clip.mp4", {"Range": "bytes=5000-"})
        self.assertEqual(response.status, 416)
        self.assertEqual(response.getheader("Content-Range"), "bytes */1024")

    def test_private_needs_the_token(self):
        response, _ = self.get("/private/clip.mp4")
        self.assertEqual(response.status, 401)
        response, _ = self.get("/private/clip.mp4", {"Authorization": "Bearer wrong"})
        self.assertEqual(response.status, 401)
        response, body = self.get(
            "/private/clip.mp4", {"Authorization": "Bearer secret", "Range": "bytes=0-0"}
        )
        self.assertEqual((response.status, body), (206, b"\x00"))

    def test_the_index_lists_media_only(self):
        response, body = self.get("/clips/index.json")
        self.assertEqual(response.status, 200)
        self.assertEqual(json.loads(body), ["clip.mp4"])

    def test_a_missing_file_and_a_path_outside_the_folder_are_not_found(self):
        self.assertEqual(self.get("/clips/missing.mp4")[0].status, 404)
        self.assertEqual(self.get("/clips/../serve.py")[0].status, 404)
        self.assertEqual(self.get("/clips/%2e%2e/%2e%2e/etc/passwd")[0].status, 404)

    def test_the_app_is_served_at_the_root(self):
        response, body = self.get("/")
        self.assertEqual((response.status, body), (200, b"<p>lab</p>"))


if __name__ == "__main__":
    unittest.main()
