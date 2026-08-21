#!/usr/bin/env python3
"""nc-thesis — how much of the product claim is a TYPE rather than a gate?

The bar `nc-thesis` declares FIVE illegal constructions and says each must ship
a compile-fail test, red-first (ARCH §18.1). Until today it carried no
`instrument` and no `today`, so it rendered UNMEASURED while the three
type-counting bars were fully instrumented — the same asymmetry `nc-extends`
was minted to correct. A bar nobody can score is a bar nobody can fail: rungs
10 and 11 could have landed with no way to tell whether the thesis moved.

WHAT THIS SCORES, AND WHAT IT DELIBERATELY DOES NOT
---------------------------------------------------
It scores COVERAGE of the declared list: for each of the five constructions,
is there a fixture wired into a `compile_fail(...)` harness, with a recorded
`.stderr`?

It does NOT re-run the compiler. `co-lineage.py` caps an instrument at 10
seconds and a trybuild run needs a full crate build, so an instrument that
tried would only ever report could-not-judge. The "do these fixtures still
fail to compile?" question is already answered, and answered better, by the
definition-of-done sweep: `evidence_reds.rs` is an ordinary test target, so
`./scripts/sovereign-test.sh --human` compiles and runs it with everything
else. The division is deliberate — the SWEEP proves the reds are still red,
this INSTRUMENT proves the declared list is covered. Neither substitutes for
the other, and a green here with a red sweep is not a thesis.

THE POSITIVE CONTROL GATES THE WHOLE READING (ARCH §18.4)
----------------------------------------------------------
`harness_positive_control.rs` names no first-party type and cannot compile
under any feature resolution, so a working harness must always report it
failing. If it is missing or unwired, this script REFUSES to emit a value and
exits 4 — could-not-judge, not zero. That distinction is the whole point of
§18.2's four verdicts: a suite that is not evaluating anything must not be
reported as a suite that found nothing wrong. The comment block in
`evidence_reds.rs` records the hour that earned this rule, when all five
fixtures reported "expected to fail, but SUCCEEDED" because the dependency
crate itself did not build.
"""
import json
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# The five illegal constructions, transcribed from the bar's own `proof` field
# in quality/campaigns/noun-convergence.toml. This is NOT a new standard — it
# is the declared one, made countable. `fixture` is the basename under a
# `tests/ui/` directory; None means no rung has proven it yet.
#
# Constructions 2 and 3 landed with nc-4-evidence; 1, 4 and 5 with
# nc-11-answer (`kernel-types/tests/answer_reds.rs`), which is the rung the bar
# names in `closes_at`.
#
# FILLING IN A FIXTURE NAME HERE MOVES NOTHING ON ITS OWN, and that is the
# point (ARCH §18.6 — a judge change reported only in the direction it was
# meant to fix). A row scores only when `wired_fixtures()` finds the basename
# inside a real `compile_fail(...)` call AND `has_recorded_stderr()` finds a
# recorded `.stderr` beside it. Both are read off the tree, so a name typed
# here without the test behind it scores zero exactly as before.
DECLARED = [
    ("an Answer with no Judgement", "answer_without_a_judgement"),
    ("an Evidence with no Origin", "evidence_without_an_origin"),
    ("an Evidence with no Custody", "evidence_without_a_custody"),
    ("a Citation not pointing into a sealed EvidenceSet", "citation_without_a_seal"),
    ("a non-shareable Evidence in a peer-bound reply",
     "non_shareable_evidence_in_a_peer_reply"),
]

POSITIVE_CONTROL = "harness_positive_control"


def wired_fixtures():
    """Basenames passed to `compile_fail(...)` in any tracked test harness.

    Wired matters more than present: a fixture file sitting in `tests/ui/`
    that no harness calls is dead weight that would otherwise score as proof.
    """
    out = subprocess.run(
        ["git", "grep", "-hI", "-e", r"compile_fail(", "--", "*.rs"],
        cwd=REPO, capture_output=True, text=True).stdout
    return {Path(m).stem for m in re.findall(r'compile_fail\("([^"]+)"\)', out)}


def has_recorded_stderr(name):
    """A fixture with no recorded `.stderr` has never been watched failing."""
    return any(REPO.glob(f"**/tests/ui/{name}.stderr"))


def main():
    wired = wired_fixtures()

    # Instrument before result: no live positive control, no number at all.
    if POSITIVE_CONTROL not in wired or not has_recorded_stderr(POSITIVE_CONTROL):
        print(f"could-not-judge: positive control `{POSITIVE_CONTROL}` is not "
              f"wired into any compile_fail harness with a recorded .stderr. "
              f"The suite cannot be shown to be evaluating anything, so its "
              f"result is not a result (ARCH §18.2, §18.4).", file=sys.stderr)
        return 4

    rows = []
    for claim, fixture in DECLARED:
        proven = bool(fixture) and fixture in wired and has_recorded_stderr(fixture)
        rows.append({"claim": claim, "fixture": fixture, "proven": proven})

    proven = sum(r["proven"] for r in rows)
    value = round(proven / len(DECLARED), 4)

    if "--json" in sys.argv:
        print(json.dumps({"value": value, "proven": proven,
                          "declared": len(DECLARED), "claims": rows}, indent=2))
        return 0

    print("\n  nc-thesis — is the product claim a TYPE?\n")
    print(f"  {'illegal construction':<52}  {'proven':<7}  fixture")
    print(f"  {'-' * 96}")
    for r in rows:
        mark = "TYPE" if r["proven"] else "open"
        print(f"  {r['claim']:<52}  {mark:<7}  {r['fixture'] or '(no rung has proven this)'}")
    print(f"  {'-' * 96}")
    print(f"\n  {proven}/{len(DECLARED)} declared constructions are un-constructible "
          f"by type  ->  {value}")
    print(f"  positive control `{POSITIVE_CONTROL}` wired and recorded — the reading counts.")
    if proven < len(DECLARED):
        print("\n  This instrument scores COVERAGE of the declared list; that the")
        print("  wired reds still fail is proven by the definition-of-done sweep,")
        print("  which builds and runs them as ordinary test targets.")
    else:
        print("\n  Every declared construction has a wired fixture with a recorded")
        print("  .stderr. COVERAGE only: that the reds still FAIL is proven by the")
        print("  definition-of-done sweep, which builds and runs them as ordinary")
        print("  test targets. A green here over a red sweep is not a thesis.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
