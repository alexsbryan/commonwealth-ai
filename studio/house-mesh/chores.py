#!/usr/bin/env python3
"""A tiny house app: no accounts, just the identity the mesh verified."""

import html
import json
import os
from http.server import BaseHTTPRequestHandler, HTTPServer


TASKS = ["clean the kitchen", "water the plants", "take out the recycling"]


class Chores(BaseHTTPRequestHandler):
    def do_GET(self):  # noqa: N802 - required by BaseHTTPRequestHandler
        member = self.headers.get("X-Mesh-Member", "someone at this workbench")
        if self.path == "/tasks":
            self.send_json({"asked_by": member, "tasks": TASKS})
            return
        if self.path != "/":
            self.send_error(404)
            return

        name = html.escape(member)
        body = "".join(f"<li>{html.escape(task)}</li>" for task in TASKS)
        page = f"""<!doctype html>
<meta name="viewport" content="width=device-width">
<title>House chores</title>
<h1>House chores</h1>
<p>Hello, <strong>{name}</strong>.</p>
<p>This app knows who asked because the mesh told it. There is no login page.</p>
<ul>{body}</ul>
""".encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(page)))
        self.end_headers()
        self.wfile.write(page)

    def send_json(self, value):
        body = json.dumps(value).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format, *args):
        print(f"{self.address_string()} {format % args}")


port = int(os.environ.get("PORT", "5000"))
print(f"chores listening on localhost:{port}")
HTTPServer(("127.0.0.1", port), Chores).serve_forever()
