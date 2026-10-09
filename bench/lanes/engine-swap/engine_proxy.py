#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Between a daemon on `[engine] kind = "remote"` and an upstream llama-server.

The remote engine (`oicp-client`'s `SplitInferenceProvider`) writes
Commonwealth extension fields into every request it sends. A daemon reads
them and llama-server ignores any field it does not know, so without this
proxy each extension would be dropped with no record. This proxy does two
things, and records both on every row:

- It translates the fields that have an upstream counterpart:
  - `lark_grammar` -> `grammar: "%llguidance {}\\n<lark>"` (needs the
    llguidance build);
  - `think_budget` -> `reasoning_budget_tokens`;
  - `assistant_prefix` -> a trailing assistant message, which llama-server
    continues;
  - on `/rerank`, the model id -> `--rerank-model`, because the client sends
    the chat id.
- It counts the fields that have none, under `dropped`: `url_allowlist`,
  `evidence_id_allowlist`, `stable_prefix_len`, `sampling_mode`,
  `cmd_prefix`, `oicp`, and an `x_forced_choice` sentinel inside a JSON
  schema. llama-server still receives the schema and enforces the enum, so
  the label is sampled, not argmaxed over one pass.

Each row is a tap.py row plus `ext: {field: "translated" | "dropped"}`.
`census.py` sums the rows by field and by lane, using the lane windows the
runner records.

    engine_proxy.py --listen 127.0.0.1:18300 --upstream http://127.0.0.1:18301 \\
        --log target/engine-swap/R1.proxy.jsonl --arm R1 \\
        [--rerank-model qwen3-reranker-0.6b-q8_0]
"""
import json
import os
import sys
import urllib.parse
from http.server import ThreadingHTTPServer

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "agent-coding", "arms"))
import tap  # noqa: E402

DROPPED = ("url_allowlist", "evidence_id_allowlist", "stable_prefix_len", "sampling_mode",
           "cmd_prefix", "oicp")


def has_forced_choice(v) -> bool:
    if isinstance(v, dict):
        return v.get("x_forced_choice") is True or any(has_forced_choice(x) for x in v.values())
    if isinstance(v, list):
        return any(has_forced_choice(x) for x in v)
    return False


class EngineProxy(tap.Tap):
    log_every_post = True
    rerank_model = None

    def rewrite(self, body: bytes, rec: dict, is_chat: bool) -> bytes:
        try:
            req = json.loads(body or b"{}")
        except ValueError:
            rec["ext"] = {"body": "unparsable, forwarded as sent"}
            return body
        if not isinstance(req, dict):
            return body
        ext = {}
        if self.path.rstrip("/").endswith("/rerank"):
            ext["rerank_model"] = f"{req.get('model')} -> {self.rerank_model}" if self.rerank_model \
                else f"{req.get('model')} (no --rerank-model: sent as is)"
            if self.rerank_model:
                req["model"] = self.rerank_model
        if is_chat:
            lark = req.pop("lark_grammar", None)
            if lark is not None:
                if "grammar" in req:
                    ext["lark_grammar"] = "dropped: request already carries grammar"
                else:
                    req["grammar"] = "%llguidance {}\n" + lark
                    ext["lark_grammar"] = "translated"
            tb = req.pop("think_budget", None)
            req.pop("thinking", None)  # the DeepSeek spelling of the same budget
            if tb is not None:
                req["reasoning_budget_tokens"] = tb
                ext["think_budget"] = "translated"
            prefix = req.pop("assistant_prefix", None)
            if prefix:
                req.setdefault("messages", []).append({"role": "assistant", "content": prefix})
                ext["assistant_prefix"] = "translated"
            for k in DROPPED:
                if k in req:
                    req.pop(k)
                    ext[k] = "dropped"
            if has_forced_choice(req.get("response_format")) or has_forced_choice(req.get("tools")):
                ext["x_forced_choice"] = "dropped: enum enforced, label sampled"
        if ext:
            rec["ext"] = ext
        return json.dumps(req).encode()


def main() -> int:
    import argparse
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--listen", required=True)
    ap.add_argument("--upstream", required=True, help="the llama-server (router) base URL")
    ap.add_argument("--log", required=True)
    ap.add_argument("--arm", required=True)
    ap.add_argument("--rerank-model", help="router model id that serves /rerank")
    a = ap.parse_args()
    host, port = a.listen.rsplit(":", 1)
    EngineProxy.upstream = urllib.parse.urlparse(a.upstream)
    EngineProxy.arm = a.arm
    EngineProxy.rerank_model = a.rerank_model
    EngineProxy.log = open(a.log, "a", encoding="utf-8")
    srv = ThreadingHTTPServer((host, int(port)), EngineProxy)
    srv.daemon_threads = True
    print(f"engine_proxy: {a.listen} -> {a.upstream} arm={a.arm} log={a.log}", file=sys.stderr, flush=True)
    srv.serve_forever()
    return 0


if __name__ == "__main__":
    sys.exit(main())
