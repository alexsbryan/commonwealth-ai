#!/usr/bin/env python3
"""Validate a golden bank, and measure what the consult gate even LOOKS at.

Two jobs, both prerequisites to trusting a single score from this bank.

**1. Structural validation.** Every case's held-out truth must be
applicable: offsets in bounds, non-overlapping, UTF-16 round-tripping.
A bank whose ground truth cannot be applied is not ground truth.

**2. Gate admission, measured without a model.** Point the scorer at an
unreachable upstream and the debug block separates the two outcomes
cleanly: `skipped` means the consult gate declined the episode on its
own, `dropped: unavailable` means the gate ADMITTED it and went looking
for inference. So a dead upstream measures, for free and per shape,
which frontier shapes our design would even consider.

That measurement is the point of the whole golden set. The `gen` bank
cannot produce it, because every case in it is a shape the gate already
admits — a missed fire on an unrecognised shape is invisible there by
construction. Here it is a number, per shape.

    ./target/debug/examples/next_edit_score --upstream http://127.0.0.1:1 \\
        --format sweep --model-id gate-probe --port 9799 &
    python3 gym/next-edit/golden/validate_golden.py --cases gym/next-edit/golden/cases.jsonl
"""

from __future__ import annotations

import argparse
import collections
import json
import sys
import urllib.request
from pathlib import Path


def read_cases(path: str) -> list[dict]:
    """Read a bank, gzipped or not. The bank carries a full file body per
    case, so it is ~50 MB raw and ~4 MB compressed — committed gzipped."""
    p = Path(path)
    if not p.exists() and Path(str(p) + ".gz").exists():
        p = Path(str(p) + ".gz")
    if p.suffix == ".gz":
        import gzip
        text = gzip.decompress(p.read_bytes()).decode()
    else:
        text = p.read_text()
    return [json.loads(l) for l in text.splitlines() if l.strip()]


def apply_u16(text: str, edits: list[dict]) -> str | None:
    """Splice UTF-16-offset edits into text; None on overlap or bounds."""
    raw = text.encode("utf-16-le")
    total = len(raw) // 2
    out, pos = bytearray(), 0
    for e in sorted(edits, key=lambda e: (e["start"], e["end"])):
        s, en = e["start"], e["end"]
        if not (isinstance(s, int) and isinstance(en, int) and 0 <= s <= en <= total):
            return None
        if s < pos:
            return None
        out += raw[pos * 2 : s * 2]
        out += e["new_text"].encode("utf-16-le")
        pos = en
    out += raw[pos * 2 :]
    return out.decode("utf-16-le")


def post(endpoint: str, request: dict, timeout: float) -> dict:
    req = urllib.request.Request(
        f"{endpoint}/v1/edit_predictions",
        data=json.dumps(request).encode(),
        headers={"content-type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read())


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cases", default="gym/next-edit/golden/cases.jsonl")
    ap.add_argument("--endpoint", default=None,
                    help="scorer with a DEAD upstream; omit to skip the gate probe")
    ap.add_argument("--timeout", type=float, default=30.0)
    ap.add_argument("--prune-out", default=None,
                    help="write a bank with ambiguous negatives removed")
    args = ap.parse_args()

    cases = read_cases(args.cases)
    print(f"{len(cases)} cases\n")

    # ---- 1. structural ----
    bad = []
    for c in cases:
        text = c["request"]["text"]
        truth = c["expect"].get("truth")
        if c["kind"] == "negative":
            # A negative's correct answer is silence, so it carries no
            # truth by design; only its history and cursor are checked.
            if c["expect"].get("fire") is not False or not c["expect"].get("why"):
                bad.append((c["id"], "negative without a stated reason for silence"))
            if len(c["request"]["history"]) < 2:
                bad.append((c["id"], "history < 2 units"))
            continue
        if not truth:
            bad.append((c["id"], "no truth"))
            continue
        got = apply_u16(text, truth)
        if got is None:
            bad.append((c["id"], "truth not applicable (overlap/bounds)"))
            continue
        if got == text:
            bad.append((c["id"], "truth is a no-op"))
        cur = c["request"]["cursor"]
        if not (0 <= cur <= len(text.encode("utf-16-le")) // 2):
            bad.append((c["id"], "cursor out of bounds"))
        if len(c["request"]["history"]) < 2:
            bad.append((c["id"], "history < 2 units (gate can never induce)"))
    print(f"structural: {len(cases) - len(bad)}/{len(cases)} valid")
    for cid, why in bad[:10]:
        print(f"   {cid}: {why}")
    if len(bad) > 10:
        print(f"   … and {len(bad) - 10} more")

    if not args.endpoint:
        print("\n(no --endpoint: gate probe skipped)")
        sys.exit(1 if bad else 0)

    # ---- 2. gate admission, no model ----
    by_shape: dict[str, collections.Counter] = collections.defaultdict(collections.Counter)
    rule_fired = collections.Counter()
    kept = []
    for c in cases:
        try:
            payload = post(args.endpoint, c["request"], args.timeout)
        except Exception as e:
            by_shape[c["shape"]]["transport-error"] += 1
            continue
        m = (payload.get("sovereign_debug") or {}).get("model") or {}
        # A negative whose correct answer is silence must be one the
        # DETERMINISTIC lane is already silent on. If the rule lane
        # fires, "it fired" is ambiguous between a mislabel and a rule
        # bug, and an ambiguous negative scores nothing. `literal_trap`
        # is exempt BY DESIGN: a text-only engine firing into comments
        # and string literals IS the finding that shape exists to catch.
        ambiguous = (
            c["kind"] == "negative"
            and c["shape"] != "neg_literal_trap"
            and bool(payload.get("edits"))
        )
        if not ambiguous:
            kept.append(c)
        if payload.get("edits"):
            # The rule lane answered without consulting anything.
            rule_fired[c["shape"]] += 1
            by_shape[c["shape"]]["rule-lane fired"] += 1
        elif m.get("dropped") == "unavailable":
            by_shape[c["shape"]]["gate ADMITS"] += 1
        elif m.get("skipped"):
            by_shape[c["shape"]][f"gate declines ({m['skipped']})"] += 1
        elif m.get("dropped"):
            by_shape[c["shape"]][f"admits, region drop ({m['dropped']})"] += 1
        else:
            by_shape[c["shape"]]["unclassified"] += 1

    print("\nGATE ADMISSION BY SHAPE — what our design would even consider")
    print(f"{'shape':<20} {'n':>4}  {'reached model':>13}  outcomes")
    total_seen = total_reached = 0
    for shape in sorted(by_shape):
        ctr = by_shape[shape]
        n = sum(ctr.values())
        reached = sum(v for k, v in ctr.items() if k.startswith("gate ADMITS")
                      or k.startswith("admits,"))
        total_seen += n
        total_reached += reached
        detail = ", ".join(f"{k}:{v}" for k, v in ctr.most_common())
        print(f"{shape:<20} {n:>4}  {reached:>6}/{n:<6}  {detail}")
    pct = 100 * total_reached / total_seen if total_seen else 0
    print(f"\n{total_reached}/{total_seen} ({pct:.0f}%) of golden episodes reach the model lane.")
    print("The remainder are MISSED FIRES the `gen` bank cannot see: every case in")
    print("that bank is a shape the gate already admits, so its silence is invisible.")

    if args.prune_out:
        dropped = len(cases) - len(kept)
        import gzip as _gz
        _blob = "".join(json.dumps(c) + "\n" for c in kept).encode()
        Path(args.prune_out).write_bytes(
            _gz.compress(_blob) if args.prune_out.endswith(".gz") else _blob)
        print(f"\npruned {dropped} ambiguous negative(s) -> {args.prune_out} "
              f"({len(kept)} cases)")


if __name__ == "__main__":
    main()
