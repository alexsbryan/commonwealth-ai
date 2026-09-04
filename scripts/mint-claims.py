#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""
Turn adjudicated CAUGHT verdicts into `covers:` tags — the last mile.

A CAUGHT verdict is the EVIDENCE; a `covers:` tag is the CLAIM. Nothing counts
until the tag exists, because `svrn conformance` reads the generated manifests
and those are scanned from tags in the source. This script is the only step
between the two, and it refuses everything it cannot do exactly.

  scripts/mint-claims.py --verdicts test-artifacts/gr.json           # dry run
  scripts/mint-claims.py --verdicts test-artifacts/gr.json --write

WHAT IT WILL NOT DO. Only CAUGHT is minted: SURVIVED means the test does not
assert the clause, and COULD-NOT-JUDGE means nothing was proven at all — a tag
written from either is the overclaim this whole campaign exists to stop
(6 of 13 hand-written claims were overclaims, note cf566968). A requirement
already claimed by the SAME test is left alone; one claimed by a DIFFERENT test
is reported, not overwritten, because a second independent proof is a decision
for a person to make.

AFTER THIS RUNS, TWO THINGS ARE STILL REQUIRED and neither is optional:
  1. UPDATE_CONFORMANCE_TAGS=1 cargo test -p xtask --test conformance_tags
     regenerates quality/conformance/*.toml from the tags.
  2. A FULL suite run. `svrn conformance` refuses a claim whose source file
     changed after the report was written — and adding the tag IS that change,
     so a freshly tagged claim reads could-not-judge until the tests run again.
     A FILTERED run is worse than none: the report it writes contains only the
     tests it ran, so every other claim in the repo becomes never-ran.
"""
import argparse, json, re, sys, tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TEST_ATTR = re.compile(r"^(\s*)#\[(?:tokio::)?test\b")
CLAIMS_DIR = ROOT / "quality/conformance"


def inventory():
    """fn name -> LIST of (path, line of the `#[test]` attribute).

    A LIST, not a single entry, and that is the whole point. This used to
    `setdefault` on the bare function name, so for a name defined twice the
    winner was whichever path `rglob` reached first — and the `covers:` tag
    then landed on a test that was never mutated, claiming a requirement is
    proven by evidence belonging to a different test. That is exactly the
    overclaim this campaign exists to stop, arriving through the same join-key
    door that produced 62 could-not-judges on 2026-09-01. 99 function names
    are duplicated across this workspace; `expired_grant_is_not_live` is one
    (guest_grant.rs and ingest_grant.rs). Callers disambiguate against the
    adjudicated report key, or refuse."""
    fn_decl = re.compile(r"^\s*(?:pub\s+)?(?:async\s+)?fn\s+([a-z_][a-z_0-9]*)\s*\(")
    out = {}
    for path in ROOT.rglob("*.rs"):
        rel = path.relative_to(ROOT).as_posix()
        if rel.startswith(("target", ".cargo", "research/verifier-v0")):
            continue
        try:
            lines = path.read_text(errors="ignore").splitlines()
        except OSError:
            continue
        attr_line = None
        for i, line in enumerate(lines, 1):
            if TEST_ATTR.match(line):
                attr_line = i
                continue
            m = fn_decl.match(line)
            if m and attr_line:
                out.setdefault(m.group(1), []).append((rel, attr_line))
                attr_line = None
            elif line.strip() and not line.lstrip().startswith("#["):
                if not m:
                    attr_line = attr_line
                else:
                    attr_line = None
    return out


def already_claimed():
    """requirement -> set of tests already claiming it."""
    out = {}
    for f in sorted(CLAIMS_DIR.glob("*.toml")):
        for c in tomllib.load(open(f, "rb")).get("claim", []):
            out.setdefault(c["requirement"], set()).add(c["test"])
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--verdicts", required=True, help="sabotage.py --json output")
    ap.add_argument("--write", action="store_true", help="apply; default is a dry run")
    a = ap.parse_args()

    data = json.loads((ROOT / a.verdicts).read_text())
    mutants = data["mutants"] if isinstance(data, dict) else data
    caught = [m for m in mutants if m.get("verdict") == "CAUGHT"]
    tests = inventory()
    claims = already_claimed()

    minted, skipped, refused = [], [], []
    edits = {}                        # path -> list of (attr_line, req)
    for m in caught:
        req, key = m["requirement"], m["mustFail"][0]
        fn = key.rsplit("::", 1)[-1]
        if key in claims.get(req, set()):
            skipped.append((req, fn, "this exact test already claims it"))
            continue
        if req in claims:
            refused.append((req, fn, f"already claimed by {sorted(claims[req])[0]}"))
            continue
        cands = tests.get(fn, [])
        if not cands:
            refused.append((req, fn, "no #[test] with this name found in the tree"))
            continue
        if len(cands) > 1:
            # The adjudicated key carries the module path (`<binary id>::
            # guest_grant::tests::fn`); a file's stem is its module. Keep the
            # candidates the key actually names, and refuse unless exactly one
            # survives — never pick among them.
            segs = set(key.split("::"))
            narrowed = [c for c in cands if Path(c[0]).stem in segs]
            if len(narrowed) != 1:
                refused.append((req, fn, f"{len(cands)} tests share this name "
                                f"({', '.join(c[0] for c in cands[:3])}) and the "
                                "verdict key does not single one out"))
                continue
            cands = narrowed
        path, attr_line = cands[0]
        edits.setdefault(path, []).append((attr_line, req, fn))
        minted.append((req, fn, f"{path}:{attr_line}"))

    for path, items in edits.items():
        p = ROOT / path
        lines = p.read_text().splitlines(keepends=True)
        # Descending, so an earlier insert cannot move a later line number.
        for attr_line, req, _fn in sorted(items, reverse=True):
            indent = re.match(r"^(\s*)", lines[attr_line - 1]).group(1)
            if any(f"covers: {req}" in l for l in lines[max(0, attr_line - 6):attr_line]):
                continue
            lines.insert(attr_line - 1, f"{indent}/// covers: {req}\n")
        if a.write:
            p.write_text("".join(lines))

    for req, fn, where in minted:
        print(f"  mint    {req:<8} {fn[:52]:<52} {where}")
    for req, fn, why in skipped:
        print(f"  skip    {req:<8} {fn[:52]:<52} {why}")
    for req, fn, why in refused:
        print(f"  REFUSE  {req:<8} {fn[:52]:<52} {why}")
    print(f"\n{len(caught)} CAUGHT -> {len(minted)} minted, {len(skipped)} already "
          f"tagged, {len(refused)} refused"
          + ("" if a.write else "   (DRY RUN — pass --write to apply)"))
    if a.write and minted:
        print("\nNow, and both are required:\n"
              "  UPDATE_CONFORMANCE_TAGS=1 cargo test -p xtask --test conformance_tags\n"
              "  ./scripts/sovereign-test.sh --human        # FULL run; a filtered one\n"
              "                                             # makes every other claim never-ran")
    return 0


if __name__ == "__main__":
    sys.exit(main())
