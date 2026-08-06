#!/usr/bin/env python3
"""Precision audit — is each episode a GENUINE next-edit opportunity?

The one soft claim under every number the golden set produces: the
detectors in `shapes.py` are regex recall filters, so some episodes may
be coincidence rather than a predictable next edit, which would make the
missed-fire rate an over-estimate. This module replaces that prose
caveat with a measurement.

**Where the false positives actually are.** Not in the shape labels —
every truth edit satisfies its detector's predicate by construction,
because that predicate is what grouped it. The risk is that the
*predicate is loose*: `import_addition` follows on "an edit mentioning
the imported symbol", and if that symbol is a common token like `get` or
`self`, unrelated edits join the group.

So the discriminating question is not "does the truth match the shape"
but **"is the truth predictable from the exemplars"** — which is the
definition of a next-edit opportunity, and is mechanically checkable.

Three tiers, strongest first:

  A  literal      applying the exemplars' own transformation to the
                  truth's old text REPRODUCES the truth exactly. This is
                  a next edit by definition — no judgement involved.
  B  shared-intent the truth introduces tokens the exemplars also
                  introduced (beyond the anchor itself). Predictable in
                  kind but not literally — the model lane's whole
                  purpose (`param_insert` varies per site).
  C  anchor-only  the only thing linking truth to exemplars is the
                  anchor token. This is where coincidence lives.

Tier C is reported and, with `--drop-c`, removed. It is NOT assumed
wrong — a human pass is still the arbiter, and `--audit-sample` writes a
readable slice so that pass is cheap. What this buys is that the headline
numbers can be quoted over A+B, where "predictable" is mechanical.

    python3 gym/next-edit/golden/precision.py --audit-sample 60
    python3 gym/next-edit/golden/precision.py --drop-c --out cases.strict.jsonl.gz
"""

from __future__ import annotations

import argparse
import collections
import gzip
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from harvest import expand_rule  # noqa: E402

# Tokens too common to evidence shared intent. Not a language keyword
# list — a short or ubiquitous identifier is weak evidence in ANY
# language, and a per-language list would be one more thing to rot.
# Deliberately SMALL. An earlier, larger list scored genuine episodes as
# coincidence: `json_list(&state)` -> `json_list(&state, None)` is a
# textbook next edit, and `None` had been stopped out of the evidence.
# The shape predicate already establishes relatedness; this list only
# removes tokens that carry no intent at all.
STOP = {"self", "this", "the", "and", "not", "for", "if", "else"}
MIN_TOKEN = 3


def toks(s: str) -> set[str]:
    return {t for t in re.findall(r"[A-Za-z_][A-Za-z0-9_]*", s)
            if len(t) >= MIN_TOKEN and t.lower() not in STOP}


def u16_slice(text: str, start: int, end: int) -> str:
    raw = text.encode("utf-16-le")
    return raw[start * 2 : end * 2].decode("utf-16-le", errors="replace")


def norm(s: str) -> str:
    return re.sub(r"\s+", " ", s).strip()


def classify(case: dict) -> tuple[str, str]:
    """-> (tier, evidence)."""
    hist = case["request"]["history"]
    text = case["request"]["text"]
    truth = case["expect"].get("truth") or []
    if not truth:
        return ("n/a", "negative")

    ex_before = [u["before"] for u in hist]
    ex_after = [u["after"] for u in hist]

    # Tier A must test the EXPANDED rule, not the minimal diff. The
    # minimal diff between `radii` and `radius` is `i` -> `us`, and
    # `line.replace("i", "us")` mangles every other `i` on the line —
    # scoring a textbook literal rename as coincidence. `expand_rule`
    # absorbs the surrounding identifier run, which is what the rule
    # lane itself matches on.
    for u in hist:
        r = expand_rule(u)
        if not r:
            continue
        find, repl = r["find"], r["replace"]
        if not find:
            continue
        for site in truth:
            old = u16_slice(text, site["start"], site["end"])
            if find in old and norm(old.replace(find, repl)) == norm(site["new_text"]):
                return ("A", f"expanded rule {find!r}->{repl!r} reproduces truth")

    # Insertion-shaped exemplars (before empty): the truth must add the
    # same text somewhere.
    if all(not b for b in ex_before) and ex_after and ex_after[0].strip():
        add = norm(ex_after[0])
        for site in truth:
            old = u16_slice(text, site["start"], site["end"])
            if add and add in norm(site["new_text"]) and add not in norm(old):
                return ("A", f"insertion {add[:40]!r} reproduced at truth")

    # ---- tier B: shared introduced tokens ----
    # The anchor token is NOT subtracted. An earlier version removed it
    # as "not independent evidence", which scored the strongest cases as
    # the weakest: when a developer adds a field named
    # `file_permission_api_name` and then adds it again at another site,
    # that identifier IS the intent, and it is also the anchor.
    ex_new = toks(" ".join(ex_after))
    ex_old = toks(" ".join(ex_before))
    introduced, removed_ex = ex_new - ex_old, ex_old - ex_new
    for site in truth:
        old = u16_slice(text, site["start"], site["end"])
        shared = (toks(site["new_text"]) - toks(old)) & introduced
        if shared:
            return ("B", f"shares introduced tokens {sorted(shared)[:3]}")
        shared_rm = (toks(old) - toks(site["new_text"])) & removed_ex
        if shared_rm:
            return ("B", f"shares removed tokens {sorted(shared_rm)[:3]}")

    return ("C", "shares no introduced or removed token with the exemplars")


def render(case: dict, tier: str, why: str) -> str:
    out = [f"=== {case['id']}  [{case['shape']} · {case['language']} · tier {tier}] ===",
           f"    {why}",
           f"    {case['provenance']['repo']}@{case['provenance']['commit'][:8]} "
           f"{case['provenance']['path']}"]
    out.append("  EDIT HISTORY (what the developer just did):")
    for i, u in enumerate(case["request"]["history"], 1):
        out.append(f"    {i}. {u['left'][-40:]!r}")
        out.append(f"       - {u['before'][:90]!r}")
        out.append(f"       + {u['after'][:90]!r}")
    text = case["request"]["text"]
    out.append("  HELD-OUT TRUTH (what they actually did next):")
    for s in (case["expect"].get("truth") or [])[:4]:
        old = u16_slice(text, s["start"], s["end"])
        out.append(f"    - {old[:110]!r}")
        out.append(f"    + {s['new_text'][:110]!r}")
    return "\n".join(out)


def read_cases(path: str) -> list[dict]:
    p = Path(path)
    if not p.exists() and Path(str(p) + ".gz").exists():
        p = Path(str(p) + ".gz")
    text = gzip.decompress(p.read_bytes()).decode() if p.suffix == ".gz" else p.read_text()
    return [json.loads(l) for l in text.splitlines() if l.strip()]


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cases", default="gym/next-edit/golden/cases.jsonl.gz")
    ap.add_argument("--drop-c", action="store_true")
    ap.add_argument("--out", default=None)
    ap.add_argument("--audit-sample", type=int, default=0,
                    help="write N readable episodes for a human pass")
    ap.add_argument("--audit-out", default="gym/next-edit/golden/AUDIT_SAMPLE.txt")
    args = ap.parse_args()

    cases = read_cases(args.cases)
    tiers: dict[str, collections.Counter] = collections.defaultdict(collections.Counter)
    tagged = []
    for c in cases:
        tier, why = classify(c)
        c["_tier"], c["_why"] = tier, why
        tiers[c["shape"]][tier] += 1
        tagged.append(c)

    pos = [c for c in tagged if c["kind"] == "positive"]
    print(f"{len(cases)} cases · {len(pos)} positives\n")
    print(f"{'shape':<22} {'n':>4} {'A literal':>10} {'B intent':>9} {'C anchor':>9} "
          f"{'precision':>10}")
    tot = collections.Counter()
    for shape in sorted(s for s in tiers if not s.startswith("neg_")):
        ctr = tiers[shape]
        n = ctr["A"] + ctr["B"] + ctr["C"]
        tot.update(ctr)
        prec = 100 * (ctr["A"] + ctr["B"]) / n if n else 0
        print(f"{shape:<22} {n:>4} {ctr['A']:>10} {ctr['B']:>9} {ctr['C']:>9} "
              f"{prec:>9.0f}%")
    n = tot["A"] + tot["B"] + tot["C"]
    prec = 100 * (tot["A"] + tot["B"]) / n if n else 0
    print(f"{'ALL POSITIVES':<22} {n:>4} {tot['A']:>10} {tot['B']:>9} {tot['C']:>9} "
          f"{prec:>9.0f}%")
    print(f"\nMechanically predictable (A+B): {tot['A'] + tot['B']}/{n} = {prec:.0f}%")
    print(f"Anchor-only (C), where coincidence lives: {tot['C']}/{n} = "
          f"{100 * tot['C'] / n if n else 0:.0f}%")

    if args.audit_sample:
        # Stratified and DETERMINISTIC (every k-th within a shape), so a
        # second reviewer audits the same slice.
        by_shape: dict[str, list] = collections.defaultdict(list)
        for c in pos:
            by_shape[c["shape"]].append(c)
        per = max(1, args.audit_sample // max(1, len(by_shape)))
        picked = []
        for shape in sorted(by_shape):
            rows = by_shape[shape]
            step = max(1, len(rows) // per)
            picked += rows[::step][:per]
        body = "\n\n".join(render(c, c["_tier"], c["_why"]) for c in picked)
        header = (
            "NEXT-EDIT GOLDEN SET — HUMAN AUDIT SAMPLE\n"
            f"{len(picked)} episodes, stratified by shape, deterministic "
            "(every k-th) so a second reviewer sees the same slice.\n\n"
            "FOR EACH: would a competent developer, having just made the edits\n"
            "under EDIT HISTORY, plausibly go on to make the edit under\n"
            "HELD-OUT TRUTH? If yes, a silent system MISSED a real opportunity.\n"
            "If no, the episode is coincidence and should be dropped.\n"
            "Mark each  Y / N / ?  and record the tally.\n"
            + "=" * 70 + "\n\n")
        Path(args.audit_out).write_text(header + body + "\n")
        print(f"\naudit sample -> {args.audit_out} ({len(picked)} episodes)")

    if args.out:
        keep = [c for c in tagged if not (args.drop_c and c["_tier"] == "C")]
        for c in keep:
            c.pop("_tier", None)
            c.pop("_why", None)
        blob = "".join(json.dumps(c) + "\n" for c in keep).encode()
        Path(args.out).write_bytes(
            gzip.compress(blob) if args.out.endswith(".gz") else blob)
        print(f"wrote {len(keep)} cases -> {args.out}")


if __name__ == "__main__":
    main()


# ---- engine-independent floor: was the information even available? ----
#
# Tier A ("the exemplars' rule reproduces the truth") is partly circular:
# it is close to "our rule lane could have got this", so it measures
# predictability in the shape of our own engine — the same
# mirror-of-the-gate flaw the gen bank has, one level up.
#
# This test references no engine. It asks whether the tokens the truth
# INTRODUCES were available anywhere in the input the model is given —
# the edit history plus the visible document. A truth that introduces a
# token appearing nowhere came from a ticket or a design decision in the
# developer's head; NO system could have produced it, and counting it as
# a missed fire indicts the system for not reading minds.
#
# `endogenous` is therefore an upper bound on what any next-edit system
# could ever achieve on this bank, independent of how ours is built.

def information_available(case: dict) -> tuple[str, str]:
    truth = case["expect"].get("truth") or []
    if not truth:
        return ("n/a", "negative")
    text = case["request"]["text"]
    context = toks(text) | toks(
        " ".join(u["before"] + " " + u["after"] + " " + u["left"] + " " + u["right"]
                 for u in case["request"]["history"]))
    novel: set[str] = set()
    for site in truth:
        old = u16_slice(text, site["start"], site["end"])
        novel |= (toks(site["new_text"]) - toks(old)) - context
    if not novel:
        return ("endogenous", "every introduced token was already on screen")
    return ("exogenous", f"introduces {sorted(novel)[:4]} found nowhere in the input")
