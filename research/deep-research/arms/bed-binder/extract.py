#!/usr/bin/env python3
"""Extract a BINDER bed from a logged flight's own artifacts.

WHY THIS EXISTS, and why it is not the bed next door. `arms/bed/` is the
OUTER loop: whole flights, ~22 min per rep, scoring a deliverable. The
location loop inside `audit::assess_claim` cannot be tuned at that
granularity — one flight is 105 minutes and answers one question. This bed
is the INNER loop: the (claim, window) pairs that actually reach the binder,
replayed through the production `assess_claim` with no acquisition, no
writer and no judge in the way.

WHICH CLAIMS. Only the ones that REACHED the loop. A claim is identified as
loop-reaching by its own recorded gap-list row carrying a `corroboration`
record: that record is written at the corroboration floor, which sits after
the binder, so nothing else can produce one. On the pin-validate flight of
2026-08-25 that is 6 of 58 round-2 claims (10.3%) — and the timing histogram
independently put 11% of claims in the slow tail, which is how we know the
selector is picking the right population.

WHICH CHUNKS. Every chunk the run had accumulated by that round, merged in
round order, at FULL length. The audit bounds the window itself
(`bounded_audit_texts`), but `claim_figures_present` and `locate_spans` read
the untruncated chunk, so truncating here would measure a different loop.

The bed it writes is LOCAL, never committed — `arms/.gitignore` keeps flight
trees out of the repo and this is derived from one. This script is the
committed half: a finding that rests on a bed cites the run id below and
this file regenerates it.

    ./extract.py <run-dir> [-o bed.json]
"""
import argparse, glob, json, os, sys


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("run_dir")
    ap.add_argument("-o", "--out", default=os.path.join(os.path.dirname(__file__), "bed.json"))
    a = ap.parse_args()

    windows = sorted(glob.glob(os.path.join(a.run_dir, "evidence-window-*.json")))
    gaps = sorted(glob.glob(os.path.join(a.run_dir, "gap-list-*.json")))
    if not windows or not gaps:
        print(f"extract: {a.run_dir} carries no evidence-window-*/gap-list-* pair", file=sys.stderr)
        return 2

    # RECONSTRUCTION, and it is named rather than assumed (ARCH §18.3).
    # `evidence-window-<round>.json` is that round's DELTA, numbered from ev-1
    # locally — NOT the accumulated window the audit saw. Merging the files by
    # id is provably wrong: on the pin-validate flight, window-1's `ev-1` and
    # window-2's `ev-1` are different pages (49,903 vs 17,431 chars). The
    # audited window is the accumulation with CONTINUOUS numbering, which the
    # claims themselves witness — round-2 claims cite ev-8 through ev-50 while
    # window-2 alone holds only ev-1..ev-5. So chunks are concatenated in round
    # order and renumbered by POSITION, which reproduces window-1's own ids
    # exactly (asserted below) and continues past them.
    chunks = []
    for f in windows:
        for c in json.load(open(f)).get("chunks") or []:
            chunks.append({
                "id": f"ev-{len(chunks) + 1}",
                "source_url": c.get("source_url", ""),
                # The audit's own custody rule, not a re-derivation: a chunk is
                # custody-known iff provenance_class == "known" (see
                # DeepResearchLoop::audit_chunks).
                "custody_known": c.get("provenance_class") == "known",
                "content": c.get("content", ""),
            })

    # The reconstruction must reproduce the FIRST window's ids byte-for-byte,
    # or the offset is wrong and every citation handle after it is misresolved.
    first = json.load(open(windows[0])).get("chunks") or []
    if [c["id"] for c in first] != [c["id"] for c in chunks[: len(first)]]:
        print("extract: REFUSING — renumbering does not reproduce the first window's ids; "
              "the run's id scheme is not positional and this bed would misresolve every "
              "[Source: ev-N] handle", file=sys.stderr)
        return 3

    claims = []
    for f in gaps:
        g = json.load(open(f))
        for c in g.get("claims") or []:
            corr = c.get("corroboration")
            if not corr:
                continue  # never reached the binder
            claims.append({
                "text": c["text"],
                "round": g.get("round"),
                "recorded_verdict": c.get("verdict"),
                "recorded_origins": len(corr.get("origins") or []),
                "recorded_support_chunks": corr.get("support_chunks"),
            })

    bed = {
        "_why": __doc__.split("\n\n")[1],
        "source_run": a.run_dir,
        "chunks": chunks,
        "claims": claims,
    }
    with open(a.out, "w") as fh:
        json.dump(bed, fh)
    mb = os.path.getsize(a.out) / 1e6
    print(f"extract: {len(claims)} loop-reaching claim(s), {len(chunks)} chunk(s), "
          f"{sum(len(c['content']) for c in chunks):,} chars -> {a.out} ({mb:.1f} MB)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
