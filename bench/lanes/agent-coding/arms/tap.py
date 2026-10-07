#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Recording pass-through in front of whichever arm is serving.

One instrument for every arm: the agent talks to the tap, the tap forwards
byte-for-byte to the arm's server and writes one JSON line per request. The
servers' own logs disagree in shape (llama-server prints `timings`, the daemon
does not), so nothing arm-side is trusted for a cross-arm number; only what
the tap timed on the wire.

Per request it records what the CLIENT sent (sampling keys, max_tokens,
tool count, message count, prompt chars) because a sampling default that
differs between servers is a confound the A/B must see, and what came back
(status, time to first byte, time to first delta, total, finish_reason,
usage, tool-call count, content and reasoning chars, llama-server `timings`
when present). A refused request (4xx/5xx) is recorded with the server's
error body, and with `--dump-dir` the request that drew it is saved whole
as `<arm>-<seq>.request.json`: a shape one server rejects and another
accepts is the finding, and it has to be reproducible from the file.
`--dump-all` saves every chat request the same way, because a 200 can
carry a malformed tool call just as well.

    tap.py --listen 127.0.0.1:18180 --upstream http://127.0.0.1:18080 \
           --log target/agent-coding-arms/A.tap.jsonl --arm A

Stdlib only. Serial clients are the design point (one agent per arm); the
threading server is there so opencode's concurrent title request is
forwarded as the client sent it, not serialised by the tap.
"""
import argparse
import http.client
import json
import sys
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HOP = {"connection", "keep-alive", "transfer-encoding", "te", "trailer",
       "upgrade", "proxy-connection", "content-length", "host"}
SAMPLING = ("temperature", "top_p", "top_k", "min_p", "presence_penalty",
            "frequency_penalty", "repeat_penalty", "seed", "max_tokens",
            "max_completion_tokens", "tool_choice", "parallel_tool_calls",
            "reasoning_effort", "stream_options", "chat_template_kwargs")

LOCK = threading.Lock()
SEQ = [0]


def request_shape(body: bytes) -> dict:
    try:
        req = json.loads(body or b"{}")
    except ValueError:
        return {"unparsed_request_bytes": len(body)}
    msgs = req.get("messages") or []
    chars = sum(len(json.dumps(m.get("content"))) for m in msgs if isinstance(m, dict))
    return {
        "model": req.get("model"),
        "stream": bool(req.get("stream")),
        "n_messages": len(msgs),
        "roles": [m.get("role") for m in msgs if isinstance(m, dict)][-6:],
        "n_tools": len(req.get("tools") or []),
        "prompt_chars": chars,
        "sent": {k: req[k] for k in SAMPLING if k in req},
    }


class Tally:
    """Accumulates one response, streamed or not, into the logged record."""

    def __init__(self, t0: float):
        self.t0 = t0
        self.first_byte_ms = None
        self.first_delta_ms = None
        self.content_chars = 0
        self.reasoning_chars = 0
        self.tool_call_ids = set()
        self.finish_reason = None
        self.usage = None
        self.timings = None
        self.buf = b""

    def ms(self) -> float:
        return round((time.monotonic() - self.t0) * 1000, 1)

    def choice(self, ch: dict, delta_key: str):
        d = ch.get(delta_key) or {}
        c, r = d.get("content") or "", d.get("reasoning_content") or d.get("reasoning") or ""
        calls = d.get("tool_calls") or []
        if (c or r or calls) and self.first_delta_ms is None:
            self.first_delta_ms = self.ms()
        self.content_chars += len(c)
        self.reasoning_chars += len(r)
        for i, tc in enumerate(calls):
            self.tool_call_ids.add(tc.get("id") or tc.get("index", i))
        if ch.get("finish_reason"):
            self.finish_reason = ch["finish_reason"]

    def obj(self, o: dict, delta_key: str):
        for ch in o.get("choices") or []:
            self.choice(ch, delta_key)
        if o.get("usage"):
            self.usage = o["usage"]
        if o.get("timings"):
            self.timings = o["timings"]

    def feed_sse(self, chunk: bytes):
        self.buf += chunk
        while b"\n" in self.buf:
            line, self.buf = self.buf.split(b"\n", 1)
            line = line.strip()
            if not line.startswith(b"data:"):
                continue
            data = line[5:].strip()
            if data == b"[DONE]":
                continue
            try:
                self.obj(json.loads(data), "delta")
            except ValueError:
                pass


class Tap(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    upstream = None
    arm = ""
    log = None
    dump_dir = None
    dump_all = False

    def log_message(self, *_):
        pass

    def do_GET(self):
        self.forward()

    def do_POST(self):
        self.forward()

    def forward(self):
        t0 = time.monotonic()
        with LOCK:
            SEQ[0] += 1
            seq = SEQ[0]
        n = int(self.headers.get("content-length") or 0)
        body = self.rfile.read(n) if n else b""
        up = self.upstream
        conn = http.client.HTTPConnection(up.hostname, up.port, timeout=3600)
        headers = {k: v for k, v in self.headers.items() if k.lower() not in HOP}
        path = (up.path.rstrip("/") + self.path) if up.path not in ("", "/") else self.path
        rec = {"seq": seq, "arm": self.arm, "ts": time.time(), "method": self.command, "path": self.path}
        is_chat = self.command == "POST" and self.path.rstrip("/").endswith("/chat/completions")
        if is_chat:
            rec.update(request_shape(body))
        tally = Tally(t0)
        try:
            conn.request(self.command, path, body=body, headers=headers)
            resp = conn.getresponse()
        except OSError as e:
            rec.update(status=None, error=f"upstream: {e}", total_ms=tally.ms())
            self.write_log(rec, is_chat)
            self.send_error(502, f"tap: upstream unreachable: {e}")
            return
        self.send_response(resp.status)
        for k, v in resp.getheaders():
            if k.lower() not in HOP:
                self.send_header(k, v)
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()
        sse = "text/event-stream" in (resp.getheader("content-type") or "")
        whole = b""
        try:
            while True:
                chunk = resp.read1(65536)
                if not chunk:
                    break
                if tally.first_byte_ms is None:
                    tally.first_byte_ms = tally.ms()
                if sse:
                    tally.feed_sse(chunk)
                else:
                    whole += chunk
                self.wfile.write(b"%x\r\n%s\r\n" % (len(chunk), chunk))
                self.wfile.flush()
            self.wfile.write(b"0\r\n\r\n")
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError) as e:
            rec["client_gone"] = str(e)
        finally:
            conn.close()
        if not sse and whole:
            try:
                tally.obj(json.loads(whole), "message")
            except ValueError:
                pass
        rec.update(status=resp.status, stream_seen=sse, total_ms=tally.ms(),
                   first_byte_ms=tally.first_byte_ms, first_delta_ms=tally.first_delta_ms)
        if is_chat:
            rec.update(finish_reason=tally.finish_reason, usage=tally.usage, timings=tally.timings,
                       content_chars=tally.content_chars, reasoning_chars=tally.reasoning_chars,
                       n_tool_calls=len(tally.tool_call_ids))
        refused = resp.status >= 400
        if refused:
            rec["error_body"] = whole[:4000].decode("utf-8", "replace")
        if self.dump_dir and (refused or (self.dump_all and is_chat)):
            dump = f"{self.dump_dir}/{self.arm}-{seq}.request.json"
            with open(dump, "wb") as f:
                f.write(body)
            rec["request_dump"] = dump
        self.write_log(rec, is_chat)

    def write_log(self, rec: dict, is_chat: bool):
        if not is_chat and rec.get("status") and rec["status"] < 400:
            return  # /v1/models polls and health checks are not turns
        line = json.dumps(rec, separators=(",", ":"))
        with LOCK:
            self.log.write(line + "\n")
            self.log.flush()


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--listen", required=True, help="host:port to accept agents on")
    ap.add_argument("--upstream", required=True, help="the arm's base URL, e.g. http://127.0.0.1:18080")
    ap.add_argument("--log", required=True, help="JSONL path, appended")
    ap.add_argument("--arm", required=True, help="label written on every record")
    ap.add_argument("--dump-dir", help="save the body of every refused request here")
    ap.add_argument("--dump-all", action="store_true",
                    help="with --dump-dir, save every chat request, not only refused ones: "
                         "a 200 can carry a malformed call, and replaying it needs the body")
    a = ap.parse_args()
    if a.dump_all and not a.dump_dir:
        ap.error("--dump-all needs --dump-dir")
    host, port = a.listen.rsplit(":", 1)
    Tap.upstream = urllib.parse.urlparse(a.upstream)
    Tap.arm = a.arm
    Tap.log = open(a.log, "a", encoding="utf-8")
    if a.dump_dir:
        import os
        os.makedirs(a.dump_dir, exist_ok=True)
        Tap.dump_dir = a.dump_dir
        Tap.dump_all = a.dump_all
    srv = ThreadingHTTPServer((host, int(port)), Tap)
    srv.daemon_threads = True
    print(f"tap: {a.listen} -> {a.upstream} arm={a.arm} log={a.log}", file=sys.stderr, flush=True)
    srv.serve_forever()
    return 0


if __name__ == "__main__":
    sys.exit(main())
