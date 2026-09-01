#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""
SABOTAGE (Rust side) — does the suite actually notice when the product breaks?

The sibling of sovereign/crates/sovereign-desktop/tests/e2e/scripts/sabotage.mjs,
deliberately the same noun and the same bank schema. That one adjudicates
Playwright specs; this one adjudicates the 11k-test Rust workspace, and it is
the ADJUDICATOR for requirement conformance: a claim is minted only when the
test it names dies under the mutation it names.

  CAUGHT   — the declared test failed. The requirement is genuinely defended.
  SURVIVED — the product was broken and the suite stayed green. The claim is an
             overclaim and must NOT be counted.
  STALE    — `find` no longer occurs exactly once. The bank is lying about what
             it covers; fix the mutant, never this script.

WHY THIS EXISTS. Hand-adjudicating "does this test prove GR-19?" measured 22%
accurate on 2026-08-31 (note cf566968). This replaces the judgement with a run,
which means a candidate generator only needs RECALL — precision comes free,
because a wrong candidate SURVIVES and is discarded.

BATCHING, AND WHY IT IS SOUND. Mutants are applied in batches and the suite runs
once per batch, because a single run is ~193s and per-mutant runs would cost
days. Attribution stays exact by an invariant, not a heuristic: AT MOST ONE
MUTANT PER CRATE PER BATCH. A mutation in sovereign-mesh cannot be why a
corpus-engine test failed, so a mutant's own test failing is attributable to it
alone.

SAFETY. This writes to tracked files. It refuses to start on a dirty tree unless
--allow-dirty, restores every target in a finally, and verifies the restore.

  scripts/sabotage.py --bank quality/sabotage/<bank>.toml
  scripts/sabotage.py --bank ... --only GR-19 --json out.json
"""
import argparse, json, os, re, shutil, subprocess, sys, tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# Isolated from the shared target dir: a concurrent peer's nextest run deletes
# target/nextest/*/junit.xml, and a six-hour batch job cannot have its report
# vanish underneath it. Costs one cold build, then warm.
# The nextest PROFILE is what isolates the report, not CARGO_TARGET_DIR —
# measured 2026-09-01, that env var leaves the store at
# target/nextest/default/ and clobbers the shared report.
PROFILE = "sabotage"
FEATURES = "corpus-engine/treesitter,sovereign-cli/dev-tools"


def crate_of(target: str) -> str:
    """The cargo package a path belongs to — the batching key."""
    p = Path(target)
    for i in range(len(p.parts), 0, -1):
        d = ROOT.joinpath(*p.parts[:i])
        if (d / "Cargo.toml").is_file():
            return d.name
    return p.parts[0]


def run_suite(env) -> dict:
    """One whole-suite run. Returns {test_key: passed}. Empty dict on no report.

    Cargo's own output is ECHOED on failure, never swallowed. The first version
    captured it and looked at neither the exit code nor stderr, so a red
    baseline surfaced as the uninformative "no junit report" and cost a full
    cold build to diagnose — this file's own §18.1 lesson, in this file.
    """
    r = subprocess.run(
        ["cargo", "nextest", "run", "--workspace", "--features", FEATURES,
         "--no-fail-fast", "--profile", PROFILE],
        cwd=ROOT, env=env, capture_output=True, text=True, stdin=subprocess.DEVNULL,
    )
    junit = ROOT / "target" / "nextest" / PROFILE / "junit.xml"
    if not junit.is_file():
        print(f"sabotage: no junit at {junit} (cargo exit {r.returncode}). Last output:\n"
              + "\n".join((r.stdout + r.stderr).splitlines()[-25:]), file=sys.stderr)
        return {}
    text = junit.read_text()
    out = {}
    for chunk in text.split("<testcase ")[1:]:
        head, _, body = chunk.partition(">")
        name = re.search(r'name="([^"]*)"', head)
        cls = re.search(r'classname="([^"]*)"', head)
        if not (name and cls):
            continue
        case = body.split("</testcase>")[0]
        if "<skipped" in case:
            continue
        out[f"{cls.group(1)}::{name.group(1)}"] = not ("<failure" in case or "<error" in case)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bank", required=True)
    ap.add_argument("--only")
    ap.add_argument("--json")
    ap.add_argument("--allow-dirty", action="store_true")
    ap.add_argument("--batch", type=int, default=20)
    a = ap.parse_args()

    bank = tomllib.load(open(ROOT / a.bank, "rb"))["mutant"]
    if a.only:
        bank = [m for m in bank if m["id"] == a.only or m.get("requirement") == a.only]
    if not bank:
        print("sabotage: selection matched no mutant — a zero-work run is not a pass", file=sys.stderr)
        return 4

    env = dict(os.environ)

    dirty = subprocess.run(["git", "status", "--porcelain"] + [m["target"] for m in bank],
                           cwd=ROOT, capture_output=True, text=True).stdout.strip()
    if dirty and not a.allow_dirty:
        print(f"sabotage: mutation targets are dirty; refusing to write to them.\n{dirty}", file=sys.stderr)
        return 2

    # STALE check first — cheap, and a lying bank invalidates everything after.
    for m in bank:
        src = (ROOT / m["target"]).read_text()
        if src.count(m["find"]) != 1:
            m["verdict"] = "STALE"

    live = [m for m in bank if "verdict" not in m]

    print(f"sabotage: baseline run ({len(live)} live mutant(s), batch {a.batch})", flush=True)
    baseline = run_suite(env)
    if not baseline:
        print("sabotage: no junit report from the baseline run — cannot adjudicate", file=sys.stderr)
        return 2
    red = {k for k, ok in baseline.items() if not ok}
    if red:
        # NOT a refusal, and never a pass. Several tests here spawn real
        # processes on real timers (sovereign-compute's supervisor drains
        # states in 1500ms), so a loaded machine reddens them for reasons that
        # have nothing to do with any mutant. Refusing outright would make a
        # six-hour run impossible to start; counting them would let a flake
        # masquerade as a caught mutant. So they are EXCLUDED and named, and
        # any claim resting on one resolves could-not-judge.
        print(f"sabotage: {len(red)} test(s) red at baseline — excluded from adjudication:\n  "
              + "\n  ".join(sorted(red)[:8]), file=sys.stderr)
    print(f"sabotage: baseline {len(baseline)} tests, {len(red)} excluded", flush=True)

    # Batch: at most one mutant per crate, so a failure is attributable.
    batches, pending = [], list(live)
    while pending:
        batch, seen, rest = [], set(), []
        for m in pending:
            c = crate_of(m["target"])
            if c in seen or len(batch) >= a.batch:
                rest.append(m)
            else:
                seen.add(c)
                batch.append(m)
        batches.append(batch)
        pending = rest

    originals = {}
    try:
        for i, batch in enumerate(batches, 1):
            for m in batch:
                p = ROOT / m["target"]
                originals.setdefault(str(p), p.read_text())
                p.write_text(originals[str(p)].replace(m["find"], m["replace"], 1))
            print(f"sabotage: batch {i}/{len(batches)} — {len(batch)} mutant(s)", flush=True)
            results = run_suite(env)
            for m in batch:
                if not results:
                    m["verdict"] = "COULD-NOT-JUDGE"   # the build broke; not a pass
                    continue
                # CAUGHT iff a test this mutant DECLARES went red.
                if any(t in red for t in m["mustFail"]):
                    m["verdict"] = "COULD-NOT-JUDGE"
                    m["detail"] = "declared test was already red at baseline"
                    continue
                hits = [t for t in m["mustFail"] if results.get(t) is False]
                missing = [t for t in m["mustFail"] if t not in results]
                m["verdict"] = "CAUGHT" if hits else ("COULD-NOT-JUDGE" if missing else "SURVIVED")
                m["killed"] = hits
                if missing:
                    m["detail"] = f"declared test(s) not in the report: {missing}"
            for p, text in originals.items():
                Path(p).write_text(text)
            originals.clear()
    finally:
        for p, text in originals.items():
            Path(p).write_text(text)
        still = subprocess.run(["git", "status", "--porcelain"] + [m["target"] for m in bank],
                               cwd=ROOT, capture_output=True, text=True).stdout.strip()
        if still and not a.allow_dirty:
            print(f"sabotage: RESTORE FAILED — tree still dirty:\n{still}", file=sys.stderr)
            return 2

    for m in bank:
        m.setdefault("verdict", "COULD-NOT-JUDGE")
    counts = {}
    for m in bank:
        counts[m["verdict"]] = counts.get(m["verdict"], 0) + 1
    print("\n" + "  ".join(f"{k} {v}" for k, v in sorted(counts.items())))
    for m in bank:
        exp = m.get("expected", "CAUGHT")
        mark = "ok " if m["verdict"] == exp else "BAD"
        print(f"  {mark} {m['verdict']:16s} {m.get('requirement','-'):8s} {m['id']}")

    if a.json:
        Path(a.json).write_text(json.dumps({"counts": counts, "mutants": bank}, indent=1, default=str))

    return 0 if all(m["verdict"] == m.get("expected", "CAUGHT") for m in bank) else 1


if __name__ == "__main__":
    sys.exit(main())
