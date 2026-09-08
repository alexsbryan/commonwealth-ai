#!/usr/bin/env python3
"""truth-recall.py — ONE scorer for "did the catalogued facts reach the atoms?"

Why this exists (ei-3c, 2026-09-07). `corpus-mcp/acceptance.sh` used to decide
its EI3 bar by diffing the `got` column of two runs of
`scripts/setup-numismatics-corpus.sh --atlas`. Three of that table's five bars
ARE recall (`catalogue_ref`, `mint`, `ruler`: expected names found, capped at
the expected count). Two are RAW YIELD — `coin family` counts every atom in the
coin family and `attribution` counts every `claim_kind == "attribution"` atom,
both of which the corpus over-produces on purpose (the prose discusses coins
outside the catalogue). Comparing yields across two atlases and printing the
result under the name "recall" made the bare-endpoint run FAIL for producing 40
attribution atoms where the daemon-built control produced 49 — while an atlas
that emitted 200 junk attribution claims and none of the seven labelled ones
would have PASSED. That is two names for one number (ARCH §10.6) and a check
with no failing input anyone could name (§18.1).

So: expected facts hit over expected facts, one class per fact kind, the same
code over both atlases. Over-production is not scored — it is a diagnostic the
caller may still print.

The four classes, and the match rule for each (the identity conventions come
from `setup-numismatics-corpus.sh`, which is still the control's own gate):

  coin identity  `truth.entities.coin[].catalogue_ref` appears as some atom's
                 `data.attributes.catalogue_ref`. The declared external identity
                 key — the bar the whole identity arc exists to move.
  mint           `truth.entities.mint[].name` is contained in the
                 `canonical_name` of some `entity_type == "mint"` atom.
  ruler          `truth.entities.ruler[].name` is contained in the
                 `canonical_name` of the entity a `state_type == "ruler"` atom
                 points at (`ruler` declares `role_of = "person"`: a part played
                 is not an essence, ARCH §7.5).
  attribution    each row of `truth.attributions` is a scholar proposing a date
                 for a coin. Matched when SOME `claim_kind == "attribution"`
                 atom's content or anchor names that scholar (`by`). Keyed on
                 (id, by) so the seven rows stay seven facts — five distinct
                 scholars appear across them, and collapsing to the surname
                 would silently score 5 where the manifest labels 7.

Measured 2026-09-07 on this host: the daemon-built `wessex-hoard` control scores
21/21; the two bare-endpoint atlases `corpus ingest` produced on 2026-09-05 score
14/21 and 16/21, and the whole of the gap is the attribution class — a bare
endpoint types "Halstead 2014" as a *sceatta* carrying `catalogue_ref =
"Halstead 2014"` and never attaches the scholar to an attribution claim. The old
yield comparison reported that same run as "attribution 40 vs 49", which named
the wrong thing.

Usage:
    truth-recall.py --truth T.json --control A.json --candidate B.json [--gate]
    truth-recall.py --truth T.json --control A.json            # score one
    truth-recall.py --truth T.json --control A.json --self-test # watch it fail

`--gate` exits 1 when recall(candidate) < recall(control) — the acceptance
leg's PASS. Without it the process exits 0 and the verdict rides in `value`,
which is what a campaign instrument needs (a non-zero exit is
`could-not-judge`, not a miss: `co-lineage.py::measure_bar`). Exit 3 means the
inputs could not be read at all — could-not-judge, by that same contract.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


def _load(path: str, what: str) -> dict:
    try:
        return json.loads(Path(path).read_text())
    except FileNotFoundError:
        print(f"truth-recall: no {what} at {path}", file=sys.stderr)
        raise SystemExit(3)
    except (OSError, json.JSONDecodeError) as exc:
        print(f"truth-recall: {what} at {path} is unreadable: {exc}", file=sys.stderr)
        raise SystemExit(3)


def score(truth: dict, atoms_path: str) -> dict:
    """-> {"recall", "matched", "expected", "classes": {name: [m, e, [misses]]}}."""
    atoms = _load(atoms_path, "atoms.json")
    data = [a.get("data") or {} for a in atoms.get("atoms") or []]
    if not data:
        print(f"truth-recall: {atoms_path} carries no atoms", file=sys.stderr)
        raise SystemExit(3)

    refs = {
        d["attributes"].get("catalogue_ref")
        for d in data
        if isinstance(d.get("attributes"), dict)
    }
    mints = [d.get("canonical_name") or "" for d in data if d.get("entity_type") == "mint"]
    ruler_ids = {d.get("entity_id") for d in data if d.get("state_type") == "ruler"}
    rulers = [d.get("canonical_name") or "" for d in data if d.get("id") in ruler_ids]
    attributions = [
        f"{d.get('content') or ''} {d.get('anchor') or ''}"
        for d in data
        if d.get("claim_kind") == "attribution"
    ]

    classes: dict[str, list] = {}

    def cls(name, expected, label, hit):
        misses = [label(e) for e in expected if not hit(e)]
        classes[name] = [len(expected) - len(misses), len(expected), misses]

    cls("coin identity", truth["entities"]["coin"], lambda c: c["catalogue_ref"],
        lambda c: c["catalogue_ref"] in refs)
    cls("mint", truth["entities"]["mint"], lambda m: m["name"],
        lambda m: any(m["name"] in n for n in mints))
    cls("ruler", truth["entities"]["ruler"], lambda r: r["name"],
        lambda r: any(r["name"] in n for n in rulers))
    cls("attribution", truth["attributions"], lambda a: f"{a['id']} ({a['by']})",
        lambda a: any(a["by"] in t for t in attributions))

    matched = sum(c[0] for c in classes.values())
    expected = sum(c[1] for c in classes.values())
    if expected == 0:
        print(f"truth-recall: {truth.get('corpus_id')} labels no facts", file=sys.stderr)
        raise SystemExit(3)
    return {
        "recall": round(matched / expected, 4),
        "matched": matched,
        "expected": expected,
        "classes": classes,
        "atoms": atoms_path,
        # Reported, never scored: the over-production the old comparison
        # mistook for recall. Kept so a reader can still see it.
        "attribution_atoms": len(attributions),
    }


def render(label: str, s: dict) -> None:
    print(f"  {label}: truth.json recall {s['matched']}/{s['expected']} "
          f"= {s['recall']:.4f}  ({s['attribution_atoms']} attribution atoms produced)",
          file=sys.stderr)
    for name, (m, e, misses) in s["classes"].items():
        line = f"    {name:<15} {m:>2} / {e:<2}"
        if misses:
            line += "   MISSED: " + ", ".join(misses)
        print(line, file=sys.stderr)


def self_test(truth: dict, control_atoms: str) -> int:
    """The failing inputs this gate is named for (ARCH §18.1).

    Derives three candidates from the control itself, so the fixtures cannot
    drift away from the artefact they are meant to perturb:

      identical    same atoms  -> PASS. A gate that fails everything is not a
                                 gate; this is the arm that proves it can pass.
      one-missing  one declared `catalogue_ref` mangled -> BELOW CONTROL.
                   Exactly the one-fact miss the order asks to watch.
      junk-yield   every attribution claim replaced by 60 fabricated ones that
                   carry a declared grade and name no scholar -> BELOW CONTROL.
                   This is the input the OLD comparison could not fail: 60 >= 49
                   attribution atoms and every row of the yield table `ok`, with
                   0 of the 7 labelled attributions reached. Measured 2026-09-07.
    """
    import copy
    import tempfile

    base = _load(control_atoms, "atoms.json")
    grade = (truth["attributions"][0] or {}).get("grade")
    ok = True
    with tempfile.TemporaryDirectory() as tmp:
        def written(name, doc):
            p = str(Path(tmp) / f"{name}.json")
            Path(p).write_text(json.dumps(doc))
            return p

        identical = written("identical", base)

        one_missing = copy.deepcopy(base)
        mangled = 0
        for a in one_missing["atoms"]:
            at = a.get("data", {}).get("attributes")
            if isinstance(at, dict) and at.get("catalogue_ref") == truth["entities"]["coin"][0]["catalogue_ref"]:
                at["catalogue_ref"] = at["catalogue_ref"] + " (mangled)"
                mangled += 1
        if mangled == 0:
            print("truth-recall: self-test cannot perturb this control — it "
                  "carries none of the declared catalogue_refs", file=sys.stderr)
            return 3
        one_missing = written("one-missing", one_missing)

        junk = copy.deepcopy(base)
        junk["atoms"] = [a for a in junk["atoms"]
                         if a.get("data", {}).get("claim_kind") != "attribution"]
        junk["atoms"] += [{"atom_type": "Claim", "data": {
            "id": f"junk-{i}", "claim_kind": "attribution", "grade": grade,
            "content": "a claim about nothing in particular", "anchor": "nothing"}}
            for i in range(60)]
        junk = written("junk-yield", junk)

        control = score(truth, control_atoms)
        for name, path, want_pass in (("identical", identical, True),
                                      ("one-missing", one_missing, False),
                                      ("junk-yield", junk, False)):
            got = score(truth, path)
            passed = got["recall"] >= control["recall"]
            mark = "ok" if passed == want_pass else "SELF-TEST FAILED"
            if passed != want_pass:
                ok = False
            print(f"  self-test {name:<12} recall {got['matched']}/{got['expected']}"
                  f"  -> {'PASS' if passed else 'BELOW CONTROL'}  [{mark}]",
                  file=sys.stderr)
    return 0 if ok else 1


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--truth", required=True, help="path to truth.json")
    ap.add_argument("--control", required=True, help="atoms.json of the control atlas")
    ap.add_argument("--candidate", help="atoms.json of the atlas under test")
    ap.add_argument("--gate", action="store_true",
                    help="exit 1 when recall(candidate) < recall(control)")
    ap.add_argument("--self-test", action="store_true",
                    help="derive three candidates from the control and assert "
                         "the gate passes one and fails two (ARCH §18.1)")
    args = ap.parse_args()

    truth = _load(args.truth, "truth.json")
    if args.self_test:
        return self_test(truth, args.control)
    control = score(truth, args.control)
    print(f"truth-recall: {truth.get('corpus_id')} — "
          f"{control['expected']} labelled facts in 4 classes", file=sys.stderr)
    render("control  ", control)

    if not args.candidate:
        print(json.dumps({"value": control["recall"], "artifact": args.control,
                          "control": control["recall"]}))
        return 0

    candidate = score(truth, args.candidate)
    render("candidate", candidate)

    # The bar, in the bar's own words: recall >= the daemon-built corpus.
    # A control that scored 0 cannot be a denominator; that is an instrument
    # failure, not a candidate miss (ARCH §18.4).
    if control["recall"] == 0:
        print("truth-recall: the control scored 0 — nothing to compare against",
              file=sys.stderr)
        return 3
    ratio = round(candidate["recall"] / control["recall"], 4)
    verdict = "PASS" if candidate["recall"] >= control["recall"] else "BELOW CONTROL"
    print(f"  ratio candidate/control = {ratio:.4f} -> {verdict}", file=sys.stderr)
    for name, (m, e, _misses) in candidate["classes"].items():
        cm = control["classes"][name][0]
        if m < cm:
            print(f"    below control on {name}: {m} vs {cm}", file=sys.stderr)

    print(json.dumps({"value": ratio, "artifact": args.candidate,
                      "control": control["recall"], "candidate": candidate["recall"],
                      "verdict": verdict}))
    if args.gate and candidate["recall"] < control["recall"]:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
