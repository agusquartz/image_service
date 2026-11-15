#!/usr/bin/env python3
"""Loopback-only static server and same-origin proxy for minio-playground.html."""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import HTTPError, URLError
from urllib.parse import parse_qs, urlsplit
from urllib.request import Request, urlopen
import json
import mimetypes
import os
from pathlib import Path
import re
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parent.parent
HTML = ROOT / "minio-playground.html"
SERVICE_URL = os.environ["PLAYGROUND_SERVICE_URL"].rstrip("/")
MINIO_PORT = int(os.environ["PLAYGROUND_MINIO_PORT"])
API_KEY = os.environ["PLAYGROUND_API_KEY"]
CONFIG = {
    "api_key": API_KEY,
    "platform": os.environ["PLAYGROUND_PLATFORM"],
    "storage": "MinIO (S3-compatible, tmpfs)",
    "image": os.environ["PLAYGROUND_IMAGE"],
    "minio": os.environ["PLAYGROUND_MINIO_IMAGE"],
    "read_auth": os.environ["IMAGE_READ_AUTH"],
    "write_auth": os.environ["IMAGE_WRITE_AUTH"],
    "metrics_enabled": os.environ["IMAGE_METRICS_ENABLED"].lower() == "true",
    "metrics_auth": os.environ["IMAGE_METRICS_AUTH"].lower() == "true",
    "max_input_bytes": int(os.environ["IMAGE_MAX_INPUT_BYTES"]),
    "max_output_bytes": int(os.environ["IMAGE_MAX_OUTPUT_BYTES"]),
    "width": int(os.environ["IMAGE_OUTPUT_WIDTH"]),
    "height": int(os.environ["IMAGE_OUTPUT_HEIGHT"]),
    "max_slots": int(os.environ["IMAGE_MAX_SLOTS"]),
    "profiles": os.environ["IMAGE_PROCESSING_PROFILES"],
    "namespace_profiles": os.environ["IMAGE_NAMESPACE_PROFILES"],
    "rate_read": int(os.environ["IMAGE_RATE_LIMIT_READ_REQUESTS"]),
    "rate_write": int(os.environ["IMAGE_RATE_LIMIT_WRITE_REQUESTS"]),
    "rate_window": int(os.environ["IMAGE_RATE_LIMIT_WINDOW_SECONDS"]),
}
SERVICE_CONTAINER = os.environ["PLAYGROUND_SERVICE_CONTAINER"]
MINIO_ACCESS_KEY = os.environ["PLAYGROUND_MINIO_ACCESS_KEY"]
MINIO_SECRET_KEY = os.environ["PLAYGROUND_MINIO_SECRET_KEY"]
MINIO_BUCKET = os.environ["PLAYGROUND_MINIO_BUCKET"]


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        print(f"[playground] {self.command} {urlsplit(self.path).path}", flush=True)

    def do_GET(self):
        path = urlsplit(self.path).path
        if path in ("/", "/minio-playground.html"):
            self.send_file()
        elif path == "/__config":
            self.send_bytes(200, json.dumps(CONFIG).encode(), "application/json; charset=utf-8")
        elif path == "/__logs":
            self.service_logs()
        elif path == "/__presigned":
            self.proxy_presigned()
        elif path.startswith("/api/") or path in ("/health", "/metrics"):
            self.proxy_service("GET")
        else:
            self.send_bytes(404, b"Not found", "text/plain; charset=utf-8")

    def do_HEAD(self):
        if self.path.startswith("/api/"):
            self.proxy_service("HEAD")
        else:
            self.send_bytes(404, b"", "text/plain; charset=utf-8")

    def do_POST(self):
        if urlsplit(self.path).path == "/__seed-slot":
            self.seed_slot()
        else:
            self.proxy_service("POST")

    def do_PUT(self):
        self.proxy_service("PUT")

    def do_DELETE(self):
        self.proxy_service("DELETE")

    def send_bytes(self, status, body, content_type, extra_headers=None):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        for key, value in (extra_headers or {}).items():
            self.send_header(key, value)
        self.end_headers()
        if self.command != "HEAD" and body:
            self.wfile.write(body)

    def send_file(self):
        body = HTML.read_bytes()
        self.send_bytes(200, body, "text/html; charset=utf-8")

    def service_logs(self):
        try:
            result = subprocess.run(
                ["docker", "logs", "--tail", "200", SERVICE_CONTAINER],
                capture_output=True, check=True, timeout=10,
            )
            body = result.stdout + result.stderr
            self.send_bytes(200, body, "application/x-ndjson; charset=utf-8")
        except (OSError, subprocess.SubprocessError) as error:
            self.send_bytes(502, str(error).encode(), "text/plain; charset=utf-8")

    def seed_slot(self):
        query = parse_qs(urlsplit(self.path).query)
        namespace = query.get("namespace", [""])[0]
        resource_id = query.get("resource_id", [""])[0]
        slot_text = query.get("slot", [""])[0]
        if not re.fullmatch(r"[a-z0-9_-]{1,32}", namespace) or not re.fullmatch(r"[A-Za-z0-9_-]{1,100}", resource_id):
            self.send_bytes(400, b"Invalid namespace or resource_id", "text/plain; charset=utf-8")
            return
        try:
            slot = int(slot_text)
        except ValueError:
            slot = 0
        if not 1 <= slot <= 255 or self.headers.get_content_type() != "image/webp":
            self.send_bytes(400, b"Expected slot 1..255 and image/webp body", "text/plain; charset=utf-8")
            return
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        if not body or len(body) > int(CONFIG["max_output_bytes"]):
            self.send_bytes(400, b"WebP body is empty or exceeds the configured output limit", "text/plain; charset=utf-8")
            return
        object_key = f"{namespace}/{resource_id}/{slot:02d}.webp"
        url = f"http://127.0.0.1:{MINIO_PORT}/{MINIO_BUCKET}/{object_key}"
        try:
            with tempfile.NamedTemporaryFile() as image_file:
                image_file.write(body)
                image_file.flush()
                subprocess.run(
                    ["curl", "--fail", "--silent", "--show-error", "--aws-sigv4", "aws:amz:us-east-1:s3",
                     "--user", f"{MINIO_ACCESS_KEY}:{MINIO_SECRET_KEY}", "--upload-file", image_file.name, url],
                    capture_output=True, check=True, timeout=20,
                )
            payload = json.dumps({"stored_key": object_key, "size_bytes": len(body)}).encode()
            self.send_bytes(201, payload, "application/json; charset=utf-8")
        except (OSError, subprocess.SubprocessError) as error:
            details = getattr(error, "stderr", b"")
            self.send_bytes(502, details or str(error).encode(), "text/plain; charset=utf-8")

    def proxy_service(self, method):
        target = SERVICE_URL + self.path
        body = self.rfile.read(int(self.headers.get("Content-Length", "0"))) if method in ("POST", "PUT") else None
        headers = {
            key: value for key, value in self.headers.items()
            if key.lower() not in {"host", "connection", "content-length", "accept-encoding"}
        }
        request = Request(target, data=body, headers=headers, method=method)
        try:
            upstream = urlopen(request, timeout=45)
            status, response_headers, payload = upstream.status, upstream.headers, upstream.read()
        except HTTPError as error:
            status, response_headers, payload = error.code, error.headers, error.read()
        except (URLError, TimeoutError) as error:
            self.send_bytes(502, str(error).encode(), "text/plain; charset=utf-8")
            return
        self.send_response(status)
        for key, value in response_headers.items():
            if key.lower() not in {"connection", "transfer-encoding", "content-encoding", "server", "date"}:
                self.send_header(key, value)
        self.send_header("Connection", "close")
        self.end_headers()
        if method != "HEAD" and payload:
            self.wfile.write(payload)
        self.close_connection = True

    def proxy_presigned(self):
        signed_url = parse_qs(urlsplit(self.path).query).get("url", [""])[0]
        parsed = urlsplit(signed_url)
        if parsed.scheme != "http" or parsed.hostname != "minio" or parsed.port != 9000:
            self.send_bytes(400, b"Unexpected presigned MinIO URL", "text/plain; charset=utf-8")
            return
        local_target = f"http://127.0.0.1:{MINIO_PORT}{parsed.path}"
        if parsed.query:
            local_target += "?" + parsed.query
        request = Request(local_target, headers={"Host": "minio:9000"}, method="GET")
        try:
            upstream = urlopen(request, timeout=45)
            status, response_headers, payload = upstream.status, upstream.headers, upstream.read()
        except HTTPError as error:
            status, response_headers, payload = error.code, error.headers, error.read()
        except (URLError, TimeoutError) as error:
            self.send_bytes(502, str(error).encode(), "text/plain; charset=utf-8")
            return
        self.send_response(status)
        for key, value in response_headers.items():
            if key.lower() not in {"connection", "transfer-encoding", "content-encoding", "server", "date"}:
                self.send_header(key, value)
        self.send_header("Connection", "close")
        self.end_headers()
        if payload:
            self.wfile.write(payload)
        self.close_connection = True


def main():
    server = ThreadingHTTPServer(("127.0.0.1", int(os.environ.get("PLAYGROUND_PORT", "4173"))), Handler)
    print(f"Playground listening on http://127.0.0.1:{server.server_port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
