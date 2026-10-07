#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Replay captured agent turns against an arm and judge each tool call.

The turn-level parity instrument. `tap.py --dump-all` saves every chat
request an agent sent; this sends each one again to an arm, the way the
client sent it (streamed when the client streamed), assembles the reply
from its deltas, and gives it one verdict:

  well-formed  every tool call names a tool the request offered, its
               arguments are a JSON object, and the schema's required
               keys are present; or a text reply with no call syntax in it
  malformed    call syntax left in the text (`<tool_call>`, `<function=`),
               an unknown tool, arguments that are not a JSON object, or a
               required key missing
  refused      a non-200 status, or no reply

The verdict reads only the reply and the request's own tool schemas, so it
is the same judge for every arm. Sampling is each server's own; repeats
(`--repeats`) are what turn one reply into a rate.

    replay.py run --corpus target/agent-coding-arms/capture-A0-*/dumps \
        --url http://127.0.0.1:18085 --arm A0 --repeats 3 --out a0.jsonl
    replay.py compare a0.jsonl b.jsonl

Stdlib only, like tap.py.
"""
import argparse
import glob
import http.client
import json
import os
import sys
import time
import urllib.parse

CALL_SYNTAX = ("<tool_call>", "<function=", "</tool_call>")


def send(url: str, body: dict, timeout: float) -> dict:
    """POST one chat request; return the assembled reply and timings."""
    u = urllib.parse.urlparse(url)
    path = (u.path.rstrip("/") or "") + "/v1/chat/completions"
    conn = http.client.HTTPConnection(u.hostname, u.port, timeout=timeout)
    t0 = time.monotonic()
    out = {"status": None, "content": "", "reasoning_chars": 0, "calls": {},
           "finish_reason": None, "usage": None, "first_delta_ms": None}
    try:
        conn.request("POST", path, body=json.dumps(body),
                     headers={"content-type": "application/json"})
        resp = conn.getresponse()
        out["status"] = resp.status
        if resp.status != 200:
            out["error_body"] = resp.read()[:2000].decode("utf-8", "replace")
            return out
        if "text/event-stream" in (resp.getheader("content-type") or ""):
            buf = b""
            while True:
                chunk = resp.read1(65536)
                if not chunk:
                    break
                buf += chunk
                while b"\n" in buf:
                    line, buf = buf.split(b"\n", 1)
                    line = line.strip()
                    if not line.startswith(b"data:") or line[5:].strip() == b"[DONE]":
                        continue
                    try:
                        absorb(out, json.loads(line[5:]), "delta", t0)
                    except ValueError:
                        pass
        else:
            absorb(out, json.loads(resp.read()), "message", t0)
    except (OSError, ValueError) as e:
        out["error"] = str(e)
    finally:
        conn.close()
        out["total_ms"] = round((time.monotonic() - t0) * 1000, 1)
    return out


def absorb(out: dict, obj: dict, key: str, t0: float):
    for ch in obj.get("choices") or []:
        d = ch.get(key) or {}
        c = d.get("content") or ""
        r = d.get("reasoning_content") or d.get("reasoning") or ""
        calls = d.get("tool_calls") or []
        if (c or r or calls) and out["first_delta_ms"] is None:
            out["first_delta_ms"] = round((time.monotonic() - t0) * 1000, 1)
        out["content"] += c
        out["reasoning_chars"] += len(r)
        for i, tc in enumerate(calls):
            slot = out["calls"].setdefault(tc.get("index", i), {"name": "", "arguments": ""})
            fn = tc.get("function") or {}
            slot["name"] += fn.get("name") or ""
            slot["arguments"] += fn.get("arguments") or ""
        if ch.get("finish_reason"):
            out["finish_reason"] = ch["finish_reason"]
    if obj.get("usage"):
        out["usage"] = obj["usage"]


def verdict(reply: dict, tools: list) -> tuple:
    """(verdict, reason) from the reply and the request's own tool schemas."""
    if reply["status"] != 200 or reply.get("error"):
        return "refused", reply.get("error") or reply.get("error_body", "")[:200]
    schemas = {t["function"]["name"]: t["function"].get("parameters") or {}
               for t in tools if t.get("function")}
    leaked = [s for s in CALL_SYNTAX if s in reply["content"]]
    if leaked:
        return "malformed", f"call syntax in content: {leaked}"
    for slot in reply["calls"].values():
        if slot["name"] not in schemas:
            return "malformed", f"unknown tool {slot['name']!r}"
        try:
            args = json.loads(slot["arguments"] or "{}")
        except ValueError:
            return "malformed", f"arguments not JSON: {slot['arguments'][:120]!r}"
        if not isinstance(args, dict):
            return "malformed", "arguments not an object"
        missing = [k for k in schemas[slot["name"]].get("required") or [] if k not in args]
        if missing:
            return "malformed", f"{slot['name']} missing required {missing}"
    if not reply["calls"] and not reply["content"].strip():
        return "malformed", "empty reply"
    return "well-formed", "calls" if reply["calls"] else "text"


def run(a) -> int:
    paths = sorted(p for pat in a.corpus for p in glob.glob(os.path.join(pat, "*.request.json")))
    if not paths:
        print(f"replay: no *.request.json under {a.corpus}", file=sys.stderr)
        return 2
    with open(a.out, "a", encoding="utf-8") as log:
        for path in paths:
            body = json.load(open(path))
            for rep in range(a.repeats):
                reply = send(a.url, body, a.timeout)
                v, why = verdict(reply, body.get("tools") or [])
                rec = {"arm": a.arm, "request": path, "repeat": rep, "verdict": v, "reason": why,
                       "n_messages": len(body.get("messages") or []), "status": reply["status"],
                       "finish_reason": reply["finish_reason"], "n_calls": len(reply["calls"]),
                       "calls": [s["name"] for s in reply["calls"].values()],
                       "content_chars": len(reply["content"]), "reasoning_chars": reply["reasoning_chars"],
                       "usage": reply["usage"], "first_delta_ms": reply["first_delta_ms"],
                       "total_ms": reply["total_ms"], "content_tail": reply["content"][-300:]}
                log.write(json.dumps(rec) + "\n")
                log.flush()
                print(f"{a.arm} {os.path.basename(path)} #{rep} {v} {why} "
                      f"calls={rec['calls']} {rec['total_ms']}ms", flush=True)
    return 0


def compare(a) -> int:
    rows = {}
    for path in a.logs:
        for line in open(path):
            r = json.loads(line)
            rows.setdefault(r["arm"], {}).setdefault(r["request"], []).append(r["verdict"])
    arms = sorted(rows)
    print("arm\trequests\treplies\twell-formed\tmalformed\trefused")
    for arm in arms:
        vs = [v for reqs in rows[arm].values() for v in reqs]
        print(f"{arm}\t{len(rows[arm])}\t{len(vs)}\t" + "\t".join(
            str(vs.count(k)) for k in ("well-formed", "malformed", "refused")))
    shared = set.intersection(*(set(rows[arm]) for arm in arms)) if arms else set()
    print(f"\nper request ({len(shared)} replayed on every arm): well-formed / replies")
    for req in sorted(shared):
        cells = [f"{rows[arm][req].count('well-formed')}/{len(rows[arm][req])}" for arm in arms]
        print(os.path.basename(os.path.dirname(os.path.dirname(req))) + "/" + os.path.basename(req),
              *cells, sep="\t")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run", help="replay a corpus against one arm")
    r.add_argument("--corpus", nargs="+", required=True, help="dump dirs (globs allowed)")
    r.add_argument("--url", required=True, help="the arm's base URL, without /v1")
    r.add_argument("--arm", required=True, help="label written on every record")
    r.add_argument("--repeats", type=int, default=3)
    r.add_argument("--timeout", type=float, default=1800)
    r.add_argument("--out", required=True, help="JSONL path, appended")
    c = sub.add_parser("compare", help="tabulate verdicts across replay logs")
    c.add_argument("logs", nargs="+")
    a = ap.parse_args()
    return run(a) if a.cmd == "run" else compare(a)


if __name__ == "__main__":
    sys.exit(main())
