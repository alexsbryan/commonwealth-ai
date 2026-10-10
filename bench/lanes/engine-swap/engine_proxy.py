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
import math
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


def forced_choice_labels(v):
    """The enum of the schema carrying the x_forced_choice sentinel, or None."""
    if isinstance(v, dict):
        if v.get("x_forced_choice") is True and isinstance(v.get("enum"), list):
            return [str(x) for x in v["enum"]]
        for x in v.values():
            found = forced_choice_labels(x)
            if found:
                return found
    if isinstance(v, list):
        for x in v:
            found = forced_choice_labels(x)
            if found:
                return found
    return None


class EngineProxy(tap.Tap):
    log_every_post = True
    rerank_model = None
    forced_choice_logprobs = False
    top_logprobs = 20

    def answer_here(self, body: bytes, rec: dict, is_chat: bool):
        """With --forced-choice-logprobs: answer an x_forced_choice call the
        way the embedded engine does, as a label->probability map from ONE
        next-token distribution. llama-server is asked for one token with its
        top logprobs, the schema removed and thinking off; the labels' mass is
        renormalised. A label outside the top N gets no mass. If no label is in
        the top N, the answer is an empty content, which the caller reads as
        no answer, the same outcome as without this flag."""
        if not (is_chat and self.forced_choice_logprobs):
            return None
        try:
            req = json.loads(body)
        except ValueError:
            return None
        labels = forced_choice_labels(req.get("response_format")) or forced_choice_labels(req.get("tools"))
        if not labels or req.get("stream"):
            return None
        for k in ("response_format", "grammar", "tools", "tool_choice", "reasoning_budget_tokens"):
            req.pop(k, None)
        req.update(max_tokens=1, temperature=0, logprobs=True, top_logprobs=self.top_logprobs)
        req.setdefault("chat_template_kwargs", {})["enable_thinking"] = False
        up = self.upstream
        conn = tap.http.client.HTTPConnection(up.hostname, up.port, timeout=600)
        try:
            conn.request("POST", "/v1/chat/completions", body=json.dumps(req),
                         headers={"Content-Type": "application/json"})
            resp = conn.getresponse()
            raw = resp.read()
        finally:
            conn.close()
        if resp.status != 200:
            rec.setdefault("ext", {})["x_forced_choice"] = f"translated: upstream {resp.status}"
            return resp.status, raw
        out = json.loads(raw)
        top = (((out.get("choices") or [{}])[0].get("logprobs") or {}).get("content") or [{}])[0].get("top_logprobs") or []
        mass = {lab: 0.0 for lab in labels}
        for t in top:
            tok = t.get("token", "")
            for lab in labels:
                if tok == lab or tok.strip() == lab:
                    mass[lab] += math.exp(t.get("logprob", -1e9))
        total = sum(mass.values())
        content = json.dumps({k: v / total for k, v in mass.items()}) if total > 0 else ""
        rec.setdefault("ext", {})["x_forced_choice"] = (
            f"translated: top-{self.top_logprobs} logprobs, label mass {total:.4f}" if total > 0
            else f"translated: no label in top-{self.top_logprobs}, no answer")
        answer = {"id": out.get("id"), "object": "chat.completion", "model": out.get("model"),
                  "choices": [{"index": 0, "finish_reason": "stop",
                               "message": {"role": "assistant", "content": content}}],
                  "usage": out.get("usage")}
        return 200, json.dumps(answer).encode()

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
    ap.add_argument("--forced-choice-logprobs", action="store_true",
                    help="answer x_forced_choice calls from one token's top logprobs (see answer_here)")
    a = ap.parse_args()
    host, port = a.listen.rsplit(":", 1)
    EngineProxy.upstream = urllib.parse.urlparse(a.upstream)
    EngineProxy.arm = a.arm
    EngineProxy.rerank_model = a.rerank_model
    EngineProxy.forced_choice_logprobs = a.forced_choice_logprobs
    EngineProxy.log = open(a.log, "a", encoding="utf-8")
    srv = ThreadingHTTPServer((host, int(port)), EngineProxy)
    srv.daemon_threads = True
    print(f"engine_proxy: {a.listen} -> {a.upstream} arm={a.arm} log={a.log}", file=sys.stderr, flush=True)
    srv.serve_forever()
    return 0


if __name__ == "__main__":
    sys.exit(main())
