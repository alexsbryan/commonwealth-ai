#!/usr/bin/env python3
"""Fabrication-etiology reader over SOVEREIGN_GATE_AUDIT_FORENSICS ledgers.

The mechanical half of the D0 classification in
results/fabrication_etiology_20260814.md, made repeatable so the after-arm
scores against the same key (the order's D3: "the etiology distribution table
is the after-arm's scoring key").

For every FAILED claim in one or more forensics ledgers it reports, against
the recorded evidence window (the byte-faithful drafter view):

  - proper names detected in the claim, and where each name occurs
    (leaf index / summary index, with char offset);
  - names absent from the entire window (the parametric-padding signal,
    class iii);
  - claims whose text is substring-verbatim in a summary chunk but in no
    leaf chunk (the summary-carriage signal, class iv);
  - per-turn sizing: answer_chars, n_claims, n_failed (the R3 signal).

CLASSES (i)/(ii)/(v)/(vi) require reading the surrounding text — this tool
emits the dossier excerpts for that read; it does not pretend to judge them
(a claim can garble content that is present, and only a reader catches the
inversion). Compass #7: this is the instrument; the classification is the
measurement.

Usage:
  python3 fabrication_etiology.py results/gate_audit_forensics_*.jsonl
  python3 fabrication_etiology.py --dossiers /tmp/dossiers LEDGER [LEDGER...]
"""
import argparse
import json
import os
import re
import sys
from collections import Counter

STOP = set(
    """The This That These Those A An In On At By For With From To Of And Or
But It Its He She They We You I Because Therefore However Moreover If When
While Although Though Since Free Will Determinism Compatibilism
Incompatibilism Hard Soft Libertarianism Libertarian Metaphysical Source Web
Summary Consequence Argument Frankfurt Principle Alternate Possibilities God
Stoics Ancient Modern Classical No Forking Paths Mind Physics""".split()
)


def names_in(text):
    out = []
    for m in re.finditer(r"(?:[A-Z]\.\s*)*[A-Z][a-z]+(?:[-'][A-Za-z]+)?", text):
        w = m.group(0)
        if w.split()[-1] not in STOP and len(w) > 2:
            out.append(w)
    return sorted(set(out))


def load(path):
    audits, claims = {}, []
    with open(path) as f:
        for line in f:
            r = json.loads(line)
            if r["kind"] == "audit":
                audits[r["audit_id"]] = r
            elif r["kind"] == "claim" and str(r.get("failed")) == "True":
                claims.append(r)
    return audits, claims


def occurrences(term, chunks):
    hits = []
    for i, ch in enumerate(chunks):
        off = ch.find(term)
        if off >= 0:
            hits.append((i, off))
    return hits


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("ledgers", nargs="+")
    ap.add_argument("--dossiers", help="directory for per-specimen excerpt files")
    ap.add_argument("--context", type=int, default=350)
    args = ap.parse_args()

    class_counts = Counter()
    per_turn = []
    specimens = []

    for path in args.ledgers:
        if not os.path.exists(path):
            print(f"MISSING: {path}", file=sys.stderr)
            continue
        audits, claims = load(path)
        tag = os.path.basename(path)
        for aid, a in audits.items():
            nf = sum(1 for c in claims if c["audit_id"] == aid)
            per_turn.append(
                (tag, a["ts"], a["recheck"], a["answer_chars"], a["n_claims_extracted"], nf)
            )
        for c in claims:
            a = audits.get(c["audit_id"])
            if a is None:
                print(f"UNJOINABLE claim (no audit record): {c['claim'][:60]}", file=sys.stderr)
                continue
            leaves, sums = a["leaf_chunks"], a["summary_chunks"]
            claim_names = names_in(c["claim"])
            name_report = {}
            absent = []
            for n in claim_names:
                surname = n.split()[-1]
                lo = occurrences(surname, leaves)
                so = occurrences(surname, sums)
                name_report[n] = {"leaf": lo, "summary": so}
                if not lo and not so:
                    absent.append(n)
            # summary-carriage signal: claim text verbatim-ish in a summary
            frag = c["claim"].strip().rstrip(".")[:80]
            in_sum_only = bool(frag) and any(frag in s for s in sums) and not any(
                frag in l for l in leaves
            )
            mech_signal = (
                "iii-candidate"
                if claim_names and len(absent) == len(claim_names)
                else "iv-candidate"
                if in_sum_only
                or (
                    claim_names
                    and all(not v["leaf"] and v["summary"] for v in name_report.values())
                )
                else "read-required"
            )
            if claim_names:
                class_counts[mech_signal] += 1
            specimens.append(
                {
                    "ledger": tag,
                    "audit_id": c["audit_id"],
                    "mechanism": c["mechanism"],
                    "vp": c.get("vp"),
                    "claim": c["claim"],
                    "names": claim_names,
                    "names_absent_from_window": absent,
                    "name_occurrences": name_report,
                    "mechanical_signal": mech_signal,
                }
            )
            if args.dossiers:
                os.makedirs(args.dossiers, exist_ok=True)
                idx = len(specimens) - 1
                with open(os.path.join(args.dossiers, f"spec_{idx:02d}.txt"), "w") as f:
                    f.write(json.dumps(specimens[-1], indent=1, default=str))
                    f.write("\n" + "=" * 78 + "\n")
                    for n in claim_names:
                        surname = n.split()[-1]
                        for kind, chunks in (("leaf", leaves), ("summary", sums)):
                            for i, off in occurrences(surname, chunks):
                                s = max(0, off - args.context)
                                e = min(len(chunks[i]), off + args.context)
                                f.write(f"\n--- {kind}[{i}] @{off}\n...{chunks[i][s:e]}...\n")

    print("== per-turn sizing (ledger, ts, recheck, answer_chars, claims, failed)")
    for row in sorted(per_turn, key=lambda r: (r[0], r[1])):
        print("  ", *row)
    named = [s for s in specimens if s["names"]]
    print(f"\n== failed claims: {len(specimens)} total, {len(named)} named-attribution")
    print("== mechanical pre-classification (named only; final class needs the read):")
    for k, v in class_counts.most_common():
        print(f"   {k}: {v}")
    print("\n== specimens")
    for i, s in enumerate(specimens):
        marker = "NAMED" if s["names"] else "     "
        vp = f"{float(s['vp']):.3f}" if s["vp"] not in (None, "None") else " -- "
        print(
            f"  {i:02d} {marker} {s['mechanism']:16s} vp={vp} "
            f"absent={s['names_absent_from_window'] or '-'} "
            f"sig={s['mechanical_signal']} :: {s['claim'][:80]}"
        )


if __name__ == "__main__":
    main()
