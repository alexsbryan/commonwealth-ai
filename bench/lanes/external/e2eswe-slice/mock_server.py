#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Stub chat-completions server for the no-GPU dry run of the e2eswe-slice battery.

Serves a scripted sequence of turns from a scenario JSON so the whole loop
(mini-swe-agent -> BwrapEnvironment -> verify -> score) can be proven end to
end without a model or the GPU. Also the instrument for the `probe` check: it
dumps every received request body, so whether litellm forwards the
`chat_template_kwargs` extension is observed on the wire, not assumed.

    mock_server.py --port 18199 --scenario scenario.json --dump dumps.jsonl

Scenario: a JSON list; each entry is
  {"bash": "<command>"}    -> returned as a native `bash` tool call
  {"content": "<text>"}    -> returned as a plain assistant message
Nothing is validated beyond the first request; the scenario order is the
conversation order and repeats the last entry if the agent keeps going.
"""
import argparse
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def build_handler(scenario, dump_path):
    lock = threading.Lock()
    state = {"n": 0}

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def _send(self, code, payload):
            body = json.dumps(payload).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            if self.path.rstrip("/").endswith("/models"):
                self._send(200, {"object": "list", "data": [
                    {"id": "mock-model", "object": "model", "owned_by": "mock"}]})
            else:
                self._send(404, {"error": "not found"})

        def do_POST(self):
            length = int(self.headers.get("Content-Length", 0))
            raw = self.rfile.read(length)
            with lock:
                with open(dump_path, "a") as f:
                    f.write(json.dumps({"path": self.path,
                                        "body": json.loads(raw or b"{}")}) + "\n")
                step = scenario[min(state["n"], len(scenario) - 1)]
                state["n"] += 1
            if "bash" in step:
                message = {
                    "role": "assistant", "content": None,
                    "tool_calls": [{"id": f"call_{state['n']}", "type": "function",
                                    "function": {"name": "bash",
                                                 "arguments": json.dumps({"command": step["bash"]})}}],
                }
                finish = "tool_calls"
            else:
                message = {"role": "assistant", "content": step.get("content", "")}
                finish = "stop"
            self._send(200, {
                "id": "mock", "object": "chat.completion", "created": 0,
                "model": "mock-model",
                "choices": [{"index": 0, "message": message, "finish_reason": finish}],
                "usage": {"prompt_tokens": 10, "completion_tokens": 10,
                          "total_tokens": 20},
            })

    return Handler


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--scenario", required=True)
    ap.add_argument("--dump", required=True)
    args = ap.parse_args()
    scenario = json.load(open(args.scenario))
    server = ThreadingHTTPServer(("127.0.0.1", args.port), build_handler(scenario, args.dump))
    print(f"mock_server: serving {len(scenario)} scripted turns on :{args.port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
