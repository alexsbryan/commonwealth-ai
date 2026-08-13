#!/usr/bin/env python3
"""Spike v2: TRUE ground truth from transcript tool results.

GT = the note ids the seat actually received from its manual notes calls
(parsed from toolUseResult blocks — what the engine returned THEN, no skew).
Then: can verbatim-turn-as-query or short-query-as-query retrieve the same
topical notes? Rail/id lookups are scored separately (not recall-servable).
"""
import json, re, sys, urllib.request
from datetime import datetime

PORT = 9741
PROJ = "/home/alexbryan/.claude/projects/-home-alexbryan-dev-commonwealth-ai"
SESSIONS = {
    "c3a7b73f": "c3a7b73f-a7e4-4fbb-8a4d-4b7c6b47d73a",
    "40b5cc0b": "40b5cc0b-31d6-415b-922c-c39c2f6e8faa",
    "5c8a3275": "5c8a3275-5922-4d66-ac60-78eb5e1d2183",
}
HEX8 = re.compile(r"^[0-9a-f]{8}$")
ANCHORS = ("comaintainer-seat", "order-seat", "directive-log", "backlog")

def read_notes(args, timeout=15):
    payload = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                          "params": {"name": "read_notes", "arguments": args}}).encode()
    req = urllib.request.Request(f"http://localhost:{PORT}/mcp", data=payload,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(json.load(r)["result"]["content"][0]["text"])

def parse_ts(t):
    return datetime.fromisoformat(t.replace("Z", "+00:00"))

def classify(args):
    """rail = identifier/anchor lookup; topical = everything else."""
    if args.get("related_to"):
        return "rail"
    q = (args.get("query") or "").strip()
    if HEX8.match(q) or q.startswith("PROBE-") or any(a in q for a in ANCHORS):
        return "rail"
    return "topical"

def extract(path):
    turns, tool_uses, results = [], {}, {}
    with open(path) as f:
        for line in f:
            try: rec = json.loads(line)
            except Exception: continue
            t = rec.get("type")
            msg = rec.get("message") or {}
            content = msg.get("content")
            if t == "user":
                if isinstance(content, str):
                    text = content
                elif isinstance(content, list):
                    for b in content:
                        if isinstance(b, dict) and b.get("type") == "tool_result":
                            results[b.get("tool_use_id", "")] = b.get("content")
                    texts = [b.get("text") for b in content
                             if isinstance(b, dict) and b.get("type") == "text"]
                    text = "\n".join(texts)
                else:
                    text = ""
                if text and not any(m in text for m in
                        ("<command-name>", "<task-notification>", "<agent-message")) \
                   and not text.startswith("Base directory for this skill"):
                    turns.append({"ts": rec.get("timestamp"), "text": text})
            elif t == "assistant" and isinstance(content, list):
                for b in content:
                    if isinstance(b, dict) and b.get("type") == "tool_use" and \
                       b.get("name") in ("mcp__sovereign__notes", "mcp__sovereign__read_notes"):
                        tool_uses[b["id"]] = {"ts": rec.get("timestamp"),
                                              "args": b.get("input") or {}}
    return turns, tool_uses, results

def ids_from_result(res):
    ids = []
    for blob in res if isinstance(res, list) else [res]:
        if isinstance(blob, dict) and blob.get("type") == "text":
            try:
                inner = json.loads(blob["text"])
                for n in inner.get("notes", []):
                    if isinstance(n, dict) and n.get("id"):
                        ids.append({"id": n["id"], "created": n.get("created_at", "")})
            except Exception:
                pass
    return ids

def turn_for(ts, turns):
    prior = [t for t in turns if parse_ts(t["ts"]) <= parse_ts(ts)]
    return prior[-1]["ts"] if prior else None

for tag, sid in SESSIONS.items():
    turns, tool_uses, results = extract(f"{PROJ}/{sid}.jsonl")
    print(f"\n######## {tag}  turns={len(turns)} manual_calls={len(tool_uses)} "
          f"results={len(results)}")
    gt = {}          # id -> {created, cls}
    for uid, call in tool_uses.items():
        tts = call["ts"]
        cls = classify(call["args"])
        for n in ids_from_result(results.get(uid, [])):
            if n["id"] not in gt:
                gt[n["id"]] = {"created": n["created"], "cls": cls, "turn": turn_for(tts, turns) or "?"}
    n_rail = sum(1 for v in gt.values() if v["cls"] == "rail")
    n_top = len(gt) - n_rail
    man_tokens = sum(len(v["created"]) for v in gt.values())  # placeholder below
    print(f"GT total={len(gt)}  rail={n_rail}  topical={n_top}")
    if not gt:
        continue
    verb_hits, short_hits = set(), set()
    gt_by_turn = {}
    for i, (nid, v) in enumerate(gt.items()):
        gt_by_turn.setdefault(v["turn"], []).append((nid, v))
    print(f"--- replay per turn (topical GT only) ---")
    for t in turns:
        gt_t = {nid: v for nid, v in gt_by_turn.get(t["ts"], []) if v["cls"] == "topical"}
        if not gt_t:
            continue
        alive = {nid for nid, v in gt_t.items()
                 if v["created"] and parse_ts(v["created"]) <= parse_ts(t["ts"])}
        if not alive:
            continue
        for label, q in (("verb", t["text"][:500]), ("short", " ".join(t["text"].split()[:12]))):
            r = read_notes({"query": q, "limit": 10, "include_operational": True})
            got = r.get("notes", [])[:10]
            hit = [n["id"] for n in got if n["id"] in alive]
            ranks = {n["id"]: i + 1 for i, n in enumerate(got) if n["id"] in alive}
            (verb_hits if label == "verb" else short_hits).update(hit)
            print(f"  T {t['ts'][11:19]} alive={len(alive):2d} {label:5s} hits={len(hit)} "
                  f"ranks={[ranks[h] for h in hit]}")
        if tag == "c3a7b73f" and t["ts"].startswith("2026-08-13T00:01"):
            r = read_notes({"query": t["text"][:500], "limit": 10, "include_operational": True})
            print("    failure-mode top-3 returned:")
            for n in r.get("notes", [])[:3]:
                print(f"      {n['id'][:8]} [{n.get('kind')}] "
                      f"{(n.get('content') or '')[:70].splitlines()[0]}")
    gt_top_ids = {i for i, v in gt.items() if v["cls"] == "topical"}
    print(f"UNION recall (topical {len(gt_top_ids)}): "
          f"verb={len(verb_hits & gt_top_ids)}  short={len(short_hits & gt_top_ids)}")
    # hook baseline vs GT
    seat = read_notes({"kinds": ["invariant", "decision"], "scope": ["global"],
                       "limit": 20, "include_operational": True})
    hook_ids = {n["id"] for n in seat.get("notes", [])}
    print(f"hook newest-20 ∩ GT: {len({i for i in gt if i in hook_ids})}/{len(gt)}")

# hook payload cost, live
non = read_notes({"kinds": ["invariant", "decision"], "scope": ["global"], "limit": 20})
print(f"\nhook .sh firehose (live, per prompt, non-seat): {len(non.get('notes',[]))} notes, "
      f"{sum(len(n['content'])//4 for n in non.get('notes',[]))} tok, no dedupe")
