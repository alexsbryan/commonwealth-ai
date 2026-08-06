#!/usr/bin/env python3
"""Score a model against the golden set.

Spec: `sovereign/docs/specs/NEXT_EDIT_BAKEOFF.md` §4 (the ruler).

Scores against GROUND TRUTH — the edits the commit author actually went
on to make — rather than against count predicates. That closes the
blind spot `NEXT_EDIT.md` §9b found in the `gen` bank's ruler, where a
visibly corrupted rewrite (`sock_ FD`, `,- scratch`) scored *correct*
because it happened to change the right number of things.

Four outcomes per positive, and they are not collapsible:

  useful   fired, and every hunk it proposed is one the author made
  wrong    fired, and proposed something the author did NOT do
  partial  fired, hit at least one real edit, also proposed something else
  missed   stayed silent when there was a real next edit to make

`missed` is the number the `gen` bank cannot produce, because every case
in it is a shape the gate already admits. `wrong` is the number the
product is precision-critical about.

Negatives invert: silence is correct, and any fire is a wrong fire.

    python3 gym/next-edit/golden/score_golden.py --endpoint http://127.0.0.1:9799
"""

from __future__ import annotations

import argparse
import collections
import json
import math
import re
import sys
import time
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


def norm(s: str) -> str:
    """Whitespace- and line-ending-agnostic. Tier 2 of the ruler: two
    rewrites that differ only in indentation are the same edit, and a
    formatter disagreement must not read as a wrong edit."""
    return re.sub(r"\s+", " ", s.replace("\r\n", "\n")).strip()


def changed_lines(a: str, b: str) -> set[str]:
    """Normalised lines present in b but not a — what a rewrite added."""
    la = collections.Counter(norm(l) for l in a.split("\n") if l.strip())
    lb = collections.Counter(norm(l) for l in b.split("\n") if l.strip())
    return {l for l in lb if lb[l] > la.get(l, 0)}


def score_positive(case: dict, edits: list[dict]) -> str:
    text = case["request"]["text"]
    truth = case["expect"]["truth"]
    if not edits:
        return "missed"
    got = apply_u16(text, edits)
    if got is None:
        return "wrong"  # malformed offsets are never a useful suggestion
    want = apply_u16(text, truth)
    if want is None:
        return "skip"
    if got == want:
        return "useful"

    added_got = changed_lines(text, got)
    added_want = changed_lines(text, want)
    if not added_got:
        return "missed"  # a whitespace-only echo is silence wearing a hat
    hit = added_got & added_want
    extra = added_got - added_want
    if hit and not extra:
        return "useful"
    if hit and extra:
        return "partial"
    return "wrong"


def wilson(k: int, n: int, z: float = 1.96) -> tuple[float, float]:
    if n == 0:
        return (0.0, 1.0)
    p = k / n
    d = 1 + z * z / n
    c = p + z * z / (2 * n)
    m = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n))
    return ((c - m) / d, (c + m) / d)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cases", default="gym/next-edit/golden/cases.jsonl")
    ap.add_argument("--endpoint", default="http://127.0.0.1:9799")
    ap.add_argument("--timeout", type=float, default=90.0)
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--json", default=None)
    args = ap.parse_args()

    cases = read_cases(args.cases)
    if args.limit:
        cases = cases[: args.limit]

    per_shape: dict[str, collections.Counter] = collections.defaultdict(collections.Counter)
    walls: list[float] = []
    rows = []
    # Lane admission, keyed by the daemon's own `model_state` verdict
    # (`sovereign_debug.model_state`). "Did the model even get asked?" is
    # the first question of any model measurement, and the answer is not
    # derivable from the outcome: a `missed` can mean the model declined
    # or that the gate never let it speak.
    admission: collections.Counter = collections.Counter()
    errors: list[tuple[str, str]] = []
    for c in cases:
        t0 = time.monotonic()
        try:
            req = urllib.request.Request(
                f"{args.endpoint}/v1/edit_predictions",
                data=json.dumps(c["request"]).encode(),
                headers={"content-type": "application/json"},
            )
            with urllib.request.urlopen(req, timeout=args.timeout) as r:
                payload = json.loads(r.read())
        except Exception as e:
            per_shape[c["shape"]]["error"] += 1
            errors.append((c["id"], f"{type(e).__name__}: {e}"))
            continue
        walls.append((time.monotonic() - t0) * 1000)
        edits = payload.get("edits") or []
        if c["kind"] == "negative":
            outcome = "wrong" if edits else "silent"
        else:
            outcome = score_positive(c, edits)
        dbg = payload.get("sovereign_debug") or {}
        state = dbg.get("model_state", "unreported")
        # WHICH gate admitted this consult (`multiline_fanout` /
        # `fanout_insert` / `param_insert`, or `forced`). The bank's
        # `shape` is a label we assigned; this is the signal routing can
        # actually branch on, so a "the model is good at X" finding is
        # only implementable once it is expressed in these terms.
        admission[state] += 1
        per_shape[c["shape"]][outcome] += 1
        rows.append({"id": c["id"], "shape": c["shape"], "kind": c["kind"],
                     "outcome": outcome, "engine": payload.get("engine"),
                     "model_state": state,
                     "consult_reason": (dbg.get("model") or {}).get("reason")})

    pos = {s: ctr for s, ctr in per_shape.items() if not s.startswith("neg_")}
    neg = {s: ctr for s, ctr in per_shape.items() if s.startswith("neg_")}

    print(f"\n{'POSITIVES — shape':<22} {'n':>4} {'useful':>7} {'partial':>8} "
          f"{'wrong':>6} {'missed':>7}")
    tot = collections.Counter()
    for s in sorted(pos):
        ctr = pos[s]
        n = sum(ctr.values())
        tot.update(ctr)
        print(f"{s:<22} {n:>4} {ctr['useful']:>7} {ctr['partial']:>8} "
              f"{ctr['wrong']:>6} {ctr['missed']:>7}")
    npos = sum(tot.values())
    fires = tot["useful"] + tot["partial"] + tot["wrong"]
    print(f"{'ALL':<22} {npos:>4} {tot['useful']:>7} {tot['partial']:>8} "
          f"{tot['wrong']:>6} {tot['missed']:>7}")

    print(f"\n{'NEGATIVES — shape':<22} {'n':>4} {'silent':>7} {'WRONG FIRE':>11}")
    ntot = collections.Counter()
    for s in sorted(neg):
        ctr = neg[s]
        n = sum(ctr.values())
        ntot.update(ctr)
        print(f"{s:<22} {n:>4} {ctr['silent']:>7} {ctr['wrong']:>11}")
    nneg = sum(ntot.values())
    print(f"{'ALL':<22} {nneg:>4} {ntot['silent']:>7} {ntot['wrong']:>11}")

    # LANE ADMISSION — read this before any rate below it. Every rate on
    # this page is conditioned on how often the model was actually asked,
    # and that share is a property of the gate, not of the model. A run
    # where `skipped:*` dominates has measured our routing; a run where
    # `fired`/`silent` dominate has measured the model.
    if admission:
        na = sum(admission.values())
        print(f"\n{'LANE ADMISSION':<22} {'n':>4} {'share':>7}")
        for st, k in admission.most_common():
            print(f"{st:<22} {k:>4} {100*k/na:>6.1f}%")
        reached = na - sum(v for s, v in admission.items() if s.startswith("skipped:"))
        print(f"{'-> reached the model':<22} {reached:>4} {100*reached/na:>6.1f}%")

    # An error is not a `missed`. It rides in the denominator of every
    # rate below (`npos` counts it) while appearing in no column, so a
    # run with errors reports rates that are silently deflated. Absence
    # is reported, never defaulted (ARCH §18.3).
    nerr = tot["error"] + ntot["error"]
    if nerr:
        print(f"\n!! {nerr} REQUEST ERRORS — every rate below is deflated by "
              f"them and this run is NOT comparable to a clean baseline.")
        for cid, msg in errors[:5]:
            print(f"   {cid}: {msg}")
        if len(errors) > 5:
            print(f"   ... and {len(errors) - 5} more")
    else:
        print("\nrequest errors: 0 — every case got a verdict.")

    # The safety number, with its honest bound. A zero is not a
    # guarantee: 0 wrong in N trials only bounds the rate at ~3/N.
    #
    # `partial` is NOT counted as wrong. The queue deliberately offers
    # every remaining guarded site, so proposing a site the commit
    # author happened not to touch is over-offer, which NEXT_EDIT.md §6
    # reports and explicitly does not gate — the user tabs past it.
    # Folding it in would score the design's intent as a defect and
    # inflate wrong-fire ~3x.
    wrong_total = tot["wrong"] + ntot["wrong"]
    fire_total = fires + ntot["wrong"]
    print()
    if fire_total:
        rate = 100 * wrong_total / fire_total
        if wrong_total == 0:
            print(f"wrong-fire: 0/{fire_total} — 95% upper bound "
                  f"{300 / fire_total:.2f}% (rule of three)")
        else:
            lo, hi = wilson(wrong_total, fire_total)
            print(f"wrong-fire: {wrong_total}/{fire_total} = {rate:.1f}% "
                  f"(95% CI {100*lo:.1f}–{100*hi:.1f}%)")
    if fire_total:
        ov = tot["partial"]
        print(f"over-offer: {ov}/{fire_total} = {100*ov/fire_total:.1f}% of fires hit a "
              f"real edit AND offered extra sites (reported, not gated)")
    if npos:
        u = tot["useful"] + tot["partial"]
        lo, hi = wilson(u, npos)
        print(f"useful-fire: {u}/{npos} = {100*u/npos:.1f}% "
              f"(95% CI {100*lo:.1f}–{100*hi:.1f}%)  [useful + partial]")
        print(f"missed-fire: {tot['missed']}/{npos} = {100*tot['missed']/npos:.1f}%"
              "   <- invisible to the gen bank")
    if walls:
        walls.sort()
        p95 = walls[max(0, min(len(walls) - 1, round(0.95 * (len(walls) - 1))))]
        print(f"latency p50 {walls[len(walls)//2]:.0f} ms · p95 {p95:.0f} ms")

    if args.json:
        Path(args.json).write_text(json.dumps(rows, indent=2))


if __name__ == "__main__":
    main()
