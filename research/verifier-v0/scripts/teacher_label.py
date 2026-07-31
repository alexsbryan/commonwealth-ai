#!/usr/bin/env python3
"""Stream B teacher labeling (M2): constructed corruption cases -> ORPO pairs.

Discipline (VERIFIER_V0.md §3 Stream B): the label is fixed BY CONSTRUCTION
before any teacher writes a word. chosen = the teacher model's full
reasoning+verdict response, kept ONLY when its binary verdict matches the
constructed label -- a teacher that gets a constructed case wrong contributes
a DISCARDED pair, never a relabel. rejected = the weak model's response,
unfiltered (the HalluGuard-76k recipe: rejected teaches the contrast, not the
label). The prompt is the exact HalluGuard training interface, imported from
eval_grounding.py so there is ONE copy of the register in this repo.

Input:  stream_b.jsonl from `svrn bench verifier export` (StreamBCase lines --
        claim, evidence_chunks, constructed label, kind, span offsets).
Output: --out JSONL rows {prompt, chosen, rejected, meta}; meta carries case
        id / kind / constructed label / spans / corpus provenance (spec §10:
        spans ride along from day one). Discards are logged to
        <out>.discards.jsonl with the mismatching verdict, so teacher
        disagreement is inspectable -- it is the hard-negative signal.

Resumable: appends keyed by case id; reruns skip ids already in --out or the
discard log. A manifest.json lands beside --out (run-manifest rule).

Usage:
  teacher_label.py --cases stream_b.jsonl --out data/stream_b/orpo.jsonl \
      --base-url http://127.0.0.1:9741/v1 \
      --teacher-model primary --rejected-model fast --concurrency 2
"""

import argparse
import collections
import json
import os
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from eval_grounding import build_prompt, chat, parse_verdict  # noqa: E402


def load_done(path):
    done = set()
    if os.path.exists(path):
        with open(path) as f:
            for line in f:
                try:
                    done.add(json.loads(line)["meta"]["id"])
                except (json.JSONDecodeError, KeyError):
                    pass
    return done


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cases", required=True, help="stream_b.jsonl from `svrn bench verifier export`")
    ap.add_argument("--out", required=True, help="ORPO pairs JSONL (appended; resumable)")
    ap.add_argument("--base-url", default="http://127.0.0.1:9741/v1")
    ap.add_argument("--teacher-model", default="primary", help="chosen-side model (35B slow tier)")
    ap.add_argument("--rejected-model", default="fast", help="rejected-side model (0.8B for real runs)")
    ap.add_argument("--teacher-max-tokens", type=int, default=1200)
    ap.add_argument("--rejected-max-tokens", type=int, default=600)
    ap.add_argument("--timeout", type=int, default=600)
    ap.add_argument("--concurrency", type=int, default=2)
    ap.add_argument("--limit", type=int, default=0, help="0 = all cases")
    args = ap.parse_args()

    cases = []
    with open(args.cases) as f:
        for line in f:
            line = line.strip()
            if line:
                cases.append(json.loads(line))
    if args.limit:
        cases = cases[: args.limit]

    discards_path = args.out + ".discards.jsonl"
    done = load_done(args.out) | load_done(discards_path)
    todo = [c for c in cases if c["id"] not in done]
    print(f"{len(cases)} cases, {len(done)} already done, {len(todo)} to label", flush=True)
    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)

    lock = threading.Lock()
    stats = collections.Counter()

    def label_one(case):
        doc = "\n\n".join(case["evidence_chunks"])
        prompt = build_prompt(doc, case["claim"])
        constructed = 1 if case["label"] == "grounded" else 0
        meta = {
            "id": case["id"],
            "kind": case["kind"],
            "label": case["label"],
            "spans": case.get("spans", []),
            "corpus_id": case.get("corpus_id", ""),
            "source_item_id": case.get("source_item_id", ""),
        }
        try:
            chosen_text, _ = chat(
                args.base_url, args.teacher_model, prompt, args.teacher_max_tokens, args.timeout
            )
        except Exception as e:  # noqa: BLE001 -- network/backend errors are per-case, not fatal
            with lock:
                stats["teacher_error"] += 1
                print(f"[{case['id']}] teacher error: {e}", file=sys.stderr, flush=True)
            return
        verdict, cls = parse_verdict(chosen_text)
        if verdict != constructed:
            # Verdict mismatch (or parse failure -> None): discard, never relabel.
            with lock:
                stats[f"discard_{case['kind']}"] += 1
                stats["discarded"] += 1
                with open(discards_path, "a") as f:
                    f.write(
                        json.dumps(
                            {
                                "meta": meta,
                                "teacher_class": cls,
                                "teacher_verdict": verdict,
                                "constructed": constructed,
                            }
                        )
                        + "\n"
                    )
            return
        try:
            rejected_text, _ = chat(
                args.base_url, args.rejected_model, prompt, args.rejected_max_tokens, args.timeout
            )
        except Exception as e:  # noqa: BLE001
            with lock:
                stats["rejected_error"] += 1
                print(f"[{case['id']}] rejected-side error: {e}", file=sys.stderr, flush=True)
            return
        row = {"prompt": prompt, "chosen": chosen_text, "rejected": rejected_text, "meta": meta}
        with lock:
            stats[f"kept_{case['kind']}"] += 1
            stats["kept"] += 1
            with open(args.out, "a") as f:
                f.write(json.dumps(row, ensure_ascii=False) + "\n")
            n = stats["kept"] + stats["discarded"]
            if n % 10 == 0:
                print(
                    f"{n}/{len(todo)} labeled -- kept {stats['kept']}, discarded {stats['discarded']}",
                    flush=True,
                )

    t0 = time.time()
    with ThreadPoolExecutor(max_workers=max(1, args.concurrency)) as pool:
        list(pool.map(label_one, todo))

    manifest = {
        "cases_file": os.path.abspath(args.cases),
        "teacher_model": args.teacher_model,
        "rejected_model": args.rejected_model,
        "base_url": args.base_url,
        "stats": dict(stats),
        "elapsed_secs": round(time.time() - t0, 1),
    }
    manifest_path = os.path.join(os.path.dirname(os.path.abspath(args.out)), "manifest.json")
    with open(manifest_path, "w") as f:
        json.dump(manifest, f, indent=2)
    print(json.dumps(manifest["stats"]), flush=True)
    print(f"manifest: {manifest_path}", flush=True)
    # Nonzero when nothing was kept -- a run that keeps zero pairs verified nothing.
    return 0 if stats["kept"] > 0 else 1


if __name__ == "__main__":
    sys.exit(main())
