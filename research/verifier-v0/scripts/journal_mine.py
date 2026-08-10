#!/usr/bin/env python3
"""Journal mining: turn the daemon's durable conversation store into labeled
grounded-prose verifier cases — the RLVR Stream C substrate.

Source: `~/.sovereign/sovereign.db` `messages` rows where role='assistant'
and metadata.retrieved_chunks is non-empty (real production answers with the
evidence the runtime retrieved for them). The preceding user message in the
same conversation is the question.

LABELS ARE ONE-SIDED, deliberately. The store keeps ~200-char snippet
previews of each retrieved chunk, not the full chunk text the model saw.
Containment against truncated evidence would mislabel grounded claims as
fabrications (the §18.3 silent-substitution failure). One direction stays
sound: a value PRESENT in a snippet is provably present in the full
evidence. So:
  - every asserted value found in the snippets  -> kind=jrnl_grounded
  - any asserted value absent from the snippets -> UNDECIDED, written to the
    candidates sidecar (NOT the bank) for a future full-chunk resolution
    pass. Never emitted as a fabrication.
Consequence: this miner grows the GROUNDED side only. Fabrication volume
stays chaos-manufactured (control_mine.py), where evidence is stored whole.

Bias stated openly (same discipline as control_mine.py): value-anchored
claims only, and grounded-proof is biased toward snippet-visible (head-of-
chunk) content. Claim extraction is the PRODUCTION register via the daemon
(local_only pinned) — reused verbatim from control_mine.extract_claims.

Resumable by id: ids derive from (message_id, claim index) — essence, not a
counter — and a done-log of processed message ids lets a kill-tolerant
supervisor re-run this script until complete.
"""
import argparse
import json
import sqlite3
import sys
import threading
from concurrent.futures import ThreadPoolExecutor, as_completed

from control_mine import norm, asserted_values, extract_claims

MIN_ANSWER_CHARS = 150
MIN_EVIDENCE_CHARS = 300


def load_turns(db_path, limit=None):
    con = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    rows = con.execute(
        "SELECT id, conversation_id, content, metadata, created_at FROM messages "
        "WHERE role='assistant' AND metadata LIKE '%retrieved_chunks%' ORDER BY id"
    ).fetchall()
    turns = []
    for mid, conv, content, md, ts in rows:
        try:
            meta = json.loads(md)
        except Exception:
            continue
        rc = meta.get("retrieved_chunks") or []
        snippets = [c.get("snippet") or "" for c in rc if isinstance(c, dict)]
        titles = [c.get("title") or "" for c in rc if isinstance(c, dict)]
        corpora = sorted({c.get("corpus_id") or "?" for c in rc if isinstance(c, dict)})
        if not snippets or sum(len(s) for s in snippets) < MIN_EVIDENCE_CHARS:
            continue
        if not content or len(content) < MIN_ANSWER_CHARS:
            continue
        q = con.execute(
            "SELECT content FROM messages WHERE conversation_id=? AND role='user' "
            "AND id<? ORDER BY id DESC LIMIT 1", (conv, mid)).fetchone()
        if not q or not q[0]:
            continue
        turns.append({"message_id": mid, "conversation_id": conv, "ts": ts,
                      "question": q[0], "answer": content,
                      "snippets": snippets, "titles": titles, "corpora": corpora})
        if limit and len(turns) >= limit:
            break
    con.close()
    return turns


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", default="/home/alexbryan/.sovereign/sovereign.db")
    ap.add_argument("--out", required=True, help="bank rows (jrnl_grounded), appended")
    ap.add_argument("--candidates", required=True,
                    help="undecided rows sidecar (values absent from snippets), appended")
    ap.add_argument("--done-log", required=True, help="processed message ids, appended")
    ap.add_argument("--daemon-url", default="http://127.0.0.1:9741/v1")
    ap.add_argument("--concurrency", type=int, default=3)
    ap.add_argument("--limit", type=int, help="process at most N turns this run")
    args = ap.parse_args()

    done = set()
    try:
        done = {l.strip() for l in open(args.done_log) if l.strip()}
    except FileNotFoundError:
        pass
    seen = set()  # normalized claim text, across prior output of BOTH files
    for path in (args.out, args.candidates):
        try:
            for l in open(path):
                try:
                    seen.add(norm(json.loads(l)["claim"]).strip())
                except Exception:
                    pass
        except FileNotFoundError:
            pass

    turns = [t for t in load_turns(args.db) if t["message_id"] not in done]
    if args.limit:
        turns = turns[:args.limit]
    print(f"journal_mine: {len(turns)} turns to process ({len(done)} already done)",
          file=sys.stderr)

    lock = threading.Lock()
    out_f = open(args.out, "a")
    cand_f = open(args.candidates, "a")
    done_f = open(args.done_log, "a")
    stats = {"grounded": 0, "undecided": 0, "dropped_no_value": 0,
             "dup": 0, "extract_fail": 0, "no_claim": 0}

    def process(t):
        claims = extract_claims(args.daemon_url, t["question"], t["answer"])
        ev_norm = norm("\n".join(t["titles"] + t["snippets"]))
        rows = []
        for ci, claim in enumerate(claims):
            vals = asserted_values(claim, t["question"])
            missing = [v for v, nv in vals if nv not in ev_norm]
            rows.append((ci, claim, vals, missing))
        return t, rows

    with ThreadPoolExecutor(max_workers=args.concurrency) as ex:
        futs = {ex.submit(process, t): t for t in turns}
        for fut in as_completed(futs):
            t = futs[fut]
            try:
                t, rows = fut.result()
            except Exception as e:
                with lock:
                    stats["extract_fail"] += 1
                    print(f"  extract failed msg {t['message_id']}: {str(e)[:80]}",
                          file=sys.stderr)
                continue  # NOT marked done — retried by the next supervisor pass
            with lock:
                if not rows:
                    stats["no_claim"] += 1
                for ci, claim, vals, missing in rows:
                    key = norm(claim).strip()
                    if key in seen:
                        stats["dup"] += 1
                        continue
                    if not vals:
                        stats["dropped_no_value"] += 1
                        continue
                    seen.add(key)
                    row = {
                        "id": f"jrnl-{t['message_id']}-{ci}",
                        "kind": "jrnl_grounded" if not missing else "jrnl_undecided",
                        "label": "grounded" if not missing else "undecided",
                        "claim": claim,
                        "evidence_chunks": t["snippets"],
                        "provenance": {
                            "source": "journal", "message_id": t["message_id"],
                            "conversation_id": t["conversation_id"], "ts": t["ts"],
                            "corpora": t["corpora"],
                            "asserted_values": [v for v, _ in vals],
                            "missing_values": missing,
                        },
                    }
                    f = out_f if not missing else cand_f
                    f.write(json.dumps(row, ensure_ascii=False) + "\n")
                    f.flush()
                    stats["grounded" if not missing else "undecided"] += 1
                done_f.write(f"{t['message_id']}\n")
                done_f.flush()

    print(json.dumps(stats, indent=1))


if __name__ == "__main__":
    main()
