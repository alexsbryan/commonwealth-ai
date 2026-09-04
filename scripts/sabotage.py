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
  SUBJECT-GONE — the target file is not on disk. A DELETION, not a drift: the
             entry must be retired with a reason or repointed. Its own verdict
             because the retirement ledger has to tell "the subject was
             deleted" apart from "the find string moved".

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
import argparse, json, os, re, shutil, signal, subprocess, sys, tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# Isolated from the shared target dir: a concurrent peer's nextest run deletes
# target/nextest/*/junit.xml, and a six-hour batch job cannot have its report
# vanish underneath it. Costs one cold build, then warm.
# The nextest PROFILE is what isolates the report, not CARGO_TARGET_DIR —
# measured 2026-09-01, that env var leaves the store at
# target/nextest/default/ and clobbers the shared report.
PROFILE = "sabotage"


def resolve_features() -> str:
    """The repo's feature contract, asked of the ONE thing that defines it.

    THIS WAS A SECOND DECIDER, AND IT WAS THE WRONG ONE (ARCH §10.6). The
    literal that used to sit here read `corpus-engine/treesitter,
    sovereign-cli/dev-tools`, while scripts/lib/cargo-scope.sh — the helper
    BOTH scripts/sovereign-test.sh and scripts/nextest.sh resolve, precisely so
    the two gates cannot disagree — also carries `sovereign-mesh/mesh-sim` and
    `sovereign-mesh/dst`. So the adjudicator compiled a NARROWER workspace than
    the gate whose coverage it certifies. Every test behind `dst`
    (dst_scenarios, the DstMesh invariant pack) and `mesh-sim`
    (mesh_sim_scoreboard, scheduler_replay_agreement) was simply absent from
    the report, and a mutant naming one resolved COULD-NOT-JUDGE with the
    honest-sounding and entirely wrong reason "not a test in this workspace".

    NO FALLBACK. A narrower feature set does not fail loudly — it hides whole
    test binaries and returns confident could-not-judges, which is exactly the
    silent substitution §18.3 forbids. If the shared helper cannot be reached,
    that is a refusal, not a default.
    """
    r = subprocess.run(
        ["bash", "-c", f"source {ROOT}/scripts/lib/cargo-scope.sh && resolve_features"],
        capture_output=True, text=True,
    )
    if r.returncode != 0 or not r.stdout.strip():
        raise SystemExit(
            "sabotage: cannot resolve the workspace feature set from "
            f"{ROOT}/scripts/lib/cargo-scope.sh (exit {r.returncode}). Refusing to "
            "guess — a narrower feature set silently hides test binaries and "
            "turns real coverage into COULD-NOT-JUDGE.\n" + r.stderr.strip()
        )
    return r.stdout.strip()


FEATURES = resolve_features()


def resolve_build_jobs() -> str:
    """Build concurrency, from the SAME helper both gates throttle by.

    A THIRD PLACE WAS DECIDING THIS, BY NOT DECIDING IT. sovereign-lint.sh and
    sovereign-test.sh both `source lib/cargo-jobs.sh` precisely so they cannot
    disagree; sabotage.py ran uncapped. On 2026-09-01, with a peer agent's
    `cargo check --workspace --all-targets` resident and swap at 44.2G of 45G,
    the OS SIGTERM'd a batch mid-build — and an unthrottled six-hour run on a
    shared machine will keep earning that. `resolve_cargo_jobs` returns 2 here,
    memory-capped at 4GB/job (ARCH §10.6, and §19: the helper already existed).

    Empty string means uncapped, which is what the helper's own 0 means.
    """
    r = subprocess.run(
        ["bash", "-c", f'REPO_ROOT="{ROOT}"; source {ROOT}/scripts/lib/cargo-jobs.sh '
                       '&& resolve_cargo_jobs "" && echo "$CARGO_JOBS"'],
        capture_output=True, text=True,
    )
    n = (r.stdout or "").strip().splitlines()[-1:] or [""]
    return "" if r.returncode != 0 or n[0] in ("", "0") else n[0]


BUILD_JOBS = resolve_build_jobs()


def crate_of(target: str) -> str:
    """The cargo package a path belongs to — the batching key."""
    p = Path(target)
    for i in range(len(p.parts), 0, -1):
        d = ROOT.joinpath(*p.parts[:i])
        if (d / "Cargo.toml").is_file():
            return d.name
    return p.parts[0]


def make_batches(items, width, one_per_crate):
    """Group mutants into runs.

    ONE PER FILE, ALWAYS — and this is a CORRECTNESS invariant, not a speed
    knob. Mutations are applied by literal replacement into the file, so two
    mutants sharing a target file in one batch cannot both be present: the
    second write is derived from the pristine text and silently discards the
    first. The mutant that lost the race is then adjudicated against code it
    never mutated, and it SURVIVES — a FALSE SURVIVED, which is the one error
    phase 2 can never repair, because phase 2 re-checks only CAUGHT.

    That defect was real and measured (2026-09-01): on `gr.toml` at `--batch
    25`, 15 of 62 mutations were never applied, and 4 of the 7 known-CAUGHT
    mutants were predicted to come back SURVIVED. It also falsifies the
    original soundness claim for `--wide` ("cross-talk can only manufacture a
    false CAUGHT"). One-per-crate mode was immune only by accident: same file
    implies same crate.

    ONE PER CRATE is the safe shape and the slow one: a failure is then
    attributable to the only mutation that could have caused it, but the number
    of runs is set by the BIGGEST crate, not by the total. On the full registry
    that is hours of cargo rebuilding one crate at a time.

    WIDE packs by count instead, so one build serves many mutants. It is not
    safe on its own — mutant A can break mutant B's declared test, and B is then
    recorded CAUGHT for someone else's change — which is why `--wide` is a
    PHASE, not a mode: everything it reports CAUGHT is re-adjudicated one per
    crate before it counts. Cross-talk can only manufacture a false CAUGHT, so
    re-checking exactly the CAUGHT set is sufficient, and it is a small set.
    """
    batches, pending = [], list(items)
    while pending:
        batch, seen_crate, seen_file, rest = [], set(), set(), []
        for m in pending:
            c = crate_of(m["target"])
            f = m["target"]
            if (one_per_crate and c in seen_crate) or f in seen_file or len(batch) >= width:
                rest.append(m)
            else:
                seen_crate.add(c)
                seen_file.add(f)
                batch.append(m)
        batches.append(batch)
        pending = rest
    return batches


def run_suite(env, only=None) -> dict:
    """One suite run. Returns {test_key: passed}. Empty dict on no report.

    `only` is a list of nextest TEST NAMES (the junit `name` attribute, i.e.
    module path + fn, without the binary id). When given, only those tests RUN
    — the workspace is still built, because a mutation must be compiled by
    every crate that depends on it, but 11,333 tests that no mutant in this
    batch declares are not executed. That is the difference between ~26 full
    suite runs and a batch loop that finishes in the same afternoon.

    Cargo's own output is ECHOED on failure, never swallowed. The first version
    captured it and looked at neither the exit code nor stderr, so a red
    baseline surfaced as the uninformative "no junit report" and cost a full
    cold build to diagnose — this file's own §18.1 lesson, in this file.
    """
    argv = ["cargo", "nextest", "run", "--workspace", "--features", FEATURES,
            "--no-fail-fast", "--profile", PROFILE]
    if BUILD_JOBS:
        argv += ["--build-jobs", BUILD_JOBS]
    if only:
        # `test(=name)` is an EXACT match; `+` is union. Built from names taken
        # out of the baseline report itself, so the filter cannot name a test
        # that does not exist.
        argv += ["-E", " + ".join(f"test(={n})" for n in sorted(set(only)))]
    # A REPORT THAT DID NOT MOVE IS NOT THIS RUN'S REPORT. When a mutation
    # fails to build, cargo writes no junit and the previous batch's file is
    # still sitting there — so reading it unconditionally returns the LAST
    # batch's results for THIS batch's mutants. With filtered runs that mostly
    # surfaces as "declared test not in the report" (could-not-judge, honest),
    # but where two batches declare the SAME test it would hand back a verdict
    # belonging to a different mutant. Staleness is checked, not assumed
    # (ARCH §18.4): the file must exist AND its mtime must advance.
    junit = ROOT / "target" / "nextest" / PROFILE / "junit.xml"
    before = junit.stat().st_mtime if junit.is_file() else 0.0
    r = subprocess.run(
        argv,
        cwd=ROOT, env=env, capture_output=True, text=True, stdin=subprocess.DEVNULL,
    )
    # A BUILD THAT WAS KILLED IS NOT A BUILD THAT FAILED. cargo returns a
    # NEGATIVE code when it dies on a signal (-15 SIGTERM, -9 SIGKILL) — which
    # on this host means the machine ran out of memory, not that the mutation
    # is bad. Recording that as "this mutation does not compile" attributes a
    # machine condition to the mutant and then BISECTS to find a broken
    # `replace` that does not exist, burning builds to reach a wrong verdict.
    # Observed 2026-09-01 with a peer agent's `cargo check --workspace
    # --all-targets` resident and swap at 44.2G of 45G.
    killed = r.returncode is not None and r.returncode < 0
    if killed:
        print(f"sabotage: cargo was KILLED by signal {-r.returncode} — the "
              "machine is out of memory or the run was terminated. This says "
              "NOTHING about the mutants in this batch.", file=sys.stderr)
        return None
    if not junit.is_file():
        print(f"sabotage: no junit at {junit} (cargo exit {r.returncode}). Last output:\n"
              + "\n".join((r.stdout + r.stderr).splitlines()[-25:]), file=sys.stderr)
        return {}
    if junit.stat().st_mtime <= before:
        print(f"sabotage: junit at {junit} did NOT advance (cargo exit "
              f"{r.returncode}) — this run produced no report and the file on "
              f"disk belongs to an earlier batch. Refusing it. Last output:\n"
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


def test_name_of(key: str) -> str:
    """The nextest TEST NAME inside a `<binary id>::<test name>` report key.

    The binary id itself can contain `::` (`sovereign-cli::bin/sovereign-cli`),
    so this is resolved against the baseline's own classname set rather than by
    splitting on the first separator.
    """
    return _NAME_OF.get(key, key)


_NAME_OF: dict = {}


def index_report(text: str) -> None:
    """Record key -> test-name for every case in a report."""
    for chunk in text.split("<testcase ")[1:]:
        head, _, _ = chunk.partition(">")
        name = re.search(r'name="([^"]*)"', head)
        cls = re.search(r'classname="([^"]*)"', head)
        if name and cls:
            _NAME_OF[f"{cls.group(1)}::{name.group(1)}"] = name.group(1)


def resolve_declared(bank, baseline):
    """Rewrite each mutant's `mustFail` into keys the report actually uses.

    THE JOIN KEY IS THE SILENT FAILURE, AND IT BIT THIS SIDE TOO. The bank may
    name a test by its bare function name (what the generator emits) while the
    report keys it `<binary id>::<module path>::<fn>` (what the hand-written
    seed bank used). Measured 2026-09-01: all 62 generated candidates ran the
    full 26-batch loop and every one returned COULD-NOT-JUDGE — 90 minutes of
    compute for zero information. It was honest (the four-verdict rule is why
    it was not a false SURVIVED, which would have discarded 62 good candidates
    and "proved" the coverage was absent) but it was avoidable.

    Resolution is by exact key first, then by unique function-name suffix.
    AMBIGUITY IS NOT RESOLVED BY GUESSING: 99 function names are duplicated
    across this workspace, and picking one would attribute a kill to whichever
    crate sorted first. Ambiguous and absent both become COULD-NOT-JUDGE with
    the reason named.
    """
    by_fn: dict = {}
    for k in baseline:
        by_fn.setdefault(k.rsplit("::", 1)[-1], []).append(k)

    stats = {"exact": 0, "resolved": 0, "ambiguous": 0, "absent": 0}
    for m in bank:
        if "verdict" in m:
            continue
        out, bad = [], None
        for t in m["mustFail"]:
            if t in baseline:
                stats["exact"] += 1
                out.append(t)
                continue
            hits = by_fn.get(t.rsplit("::", 1)[-1], [])
            if len(hits) == 1:
                stats["resolved"] += 1
                out.append(hits[0])
            elif len(hits) > 1:
                # A QUALIFIED NAME DISAMBIGUATES — WITHOUT GUESSING. The index
                # is keyed on the bare function name, so `expired_grant_is_not
                # _live` (defined in both guest_grant.rs and ingest_grant.rs)
                # is ambiguous and correctly refused. But a bank that says
                # `guest_grant::tests::expired_grant_is_not_live` has ALREADY
                # said which one, and refusing that is throwing away evidence
                # the bank supplied. Match it as a `::`-delimited SUFFIX of the
                # report key, and accept only if exactly one survives — the
                # rule stays "never pick among candidates", it just uses the
                # whole name it was given instead of the last segment.
                narrowed = [k for k in hits if k.endswith("::" + t)] if "::" in t else []
                if len(narrowed) == 1:
                    stats["resolved"] += 1
                    out.append(narrowed[0])
                    continue
                stats["ambiguous"] += 1
                bad = (f"`{t}` names {len(hits)} tests across the workspace: "
                       f"{hits[:3]}" + ("" if "::" in t else
                       " — qualify it (`module::tests::fn`) to name one"))
                break
            else:
                stats["absent"] += 1
                bad = f"`{t}` is not a test in this workspace"
                break
        if bad:
            m["verdict"] = "COULD-NOT-JUDGE"
            m["detail"] = bad
        else:
            m["mustFail"] = out
    return stats



def apply_mutant(p, m) -> None:
    """Write one mutant's `replace` into its target file.

    ONTO THE CURRENT TEXT, never onto the pristine text. Deriving each write
    from the original is what let a batch-mate's mutation vanish (see
    `make_batches`), and a mutation that vanished is adjudicated as a FALSE
    SURVIVED — the verdict phase 2 cannot repair.

    AND THE WRITE IS VERIFIED, not assumed. `find` was proven to occur exactly
    once at the STALE check, against the pristine file; if it does not occur
    exactly once HERE, something in this batch has moved the site, and the
    honest move is to stop rather than adjudicate a mutant that is not in the
    build (ARCH §18.3 — absence is reported, never defaulted).
    """
    cur = p.read_text()
    n = cur.count(m["find"])
    if n != 1:
        raise SystemExit(
            f"sabotage: mutant {m['id']} cannot be applied — its `find` occurs "
            f"{n} time(s) in {m['target']} at apply time, though it occurred "
            "exactly once at the STALE check. A batch-mate has overwritten its "
            "site; adjudicating it now would report a FALSE SURVIVED."
        )
    p.write_text(cur.replace(m["find"], m["replace"], 1))


def mark_stale(m, src: str) -> bool:
    """STALE iff this mutant's `find` does not occur EXACTLY ONCE in `src`.

    Checked before any build, because a bank that no longer describes the code
    invalidates everything downstream of it. Zero occurrences means the site
    moved or was deleted, so the mutation would never apply; more than one
    means the mutation is ambiguous and whichever site `replace(..., 1)` hits
    is an accident. Neither is a verdict about the suite, and calling either
    one SURVIVED would be a bug report filed against the wrong thing.

    Returns whether the mutant was marked. Separated from `main` so the
    self-test can drive it without a repo checkout.
    """
    n = src.count(m["find"])
    if n != 1:
        m["verdict"] = "STALE"
        m["detail"] = (
            f"`find` occurs {n}x in {m['target']} (needs exactly 1) — the bank "
            "is describing code that moved or forked. Fix the mutant."
        )
        return True
    return False


def mark_subject_gone(m, exists: bool) -> bool:
    """SUBJECT-GONE iff this mutant's target file is not on disk.

    Separate from STALE on purpose. STALE says the bank is lying about code
    that still exists — fix the mutant. SUBJECT-GONE says the code was
    deliberately deleted, and the honest response is to retire the entry with a
    reason (a retired entry is evidence; a deleted one is nothing) or repoint
    it at whatever inherited the behaviour. Collapsing the two would make a
    deletion read as a drift and lose the reason.

    Returns whether the mutant was marked, so the caller can skip the read that
    would otherwise raise.
    """
    if not exists:
        m["verdict"] = "SUBJECT-GONE"
        m["detail"] = (
            f"target {m['target']} is not on disk — the subject was deleted. "
            "Retire this entry with a reason, or repoint it at whatever "
            "inherited the behaviour."
        )
        return True
    return False


def adjudicate(m, results, red) -> str:
    """The verdict for ONE mutant. Sets `verdict` (and `detail`/`killed`) on it.

    FOUR VERDICTS, NOT TWO (ARCH §18.1). Each could-not-judge arm below is a
    distinct way of knowing nothing, and every one of them would read as a
    pass if it were collapsed into SURVIVED:

      CAUGHT           a test this mutant DECLARES went red. The requirement
                       is genuinely defended.
      SURVIVED         every declared test RAN and stayed green. The product
                       was broken and the suite did not notice — a bug report
                       about the suite, not about this script.
      COULD-NOT-JUDGE  the build broke (the suite never ran); or a declared
                       test was ALREADY red at baseline, so its redness proves
                       nothing about this mutation; or a declared test is
                       absent from the report, so nobody looked.

    `results` is `{test: passed}` for the batch, or falsy when the build
    failed. `red` is the set of tests already failing at baseline.

    Extracted from `run_phase` so it can be exercised without cargo: the
    self-test above it covers batching and name resolution, and until this
    was a function the verdict computation itself — the part that decides
    whether a mutation counts as proof — had no falsifier at all.
    """
    if not results:
        m["verdict"] = "COULD-NOT-JUDGE"   # the build broke; not a pass
        m.setdefault("detail", "this mutation does not compile — the "
                               "suite never ran, so nothing is proven")
        return m["verdict"]
    if any(t in red for t in m["mustFail"]):
        m["verdict"] = "COULD-NOT-JUDGE"
        m["detail"] = "declared test was already red at baseline"
        return m["verdict"]
    hits = [t for t in m["mustFail"] if results.get(t) is False]
    missing = [t for t in m["mustFail"] if t not in results]
    m["verdict"] = "CAUGHT" if hits else ("COULD-NOT-JUDGE" if missing else "SURVIVED")
    m["killed"] = hits
    if missing:
        m["detail"] = f"declared test(s) not in the report: {missing}"
    return m["verdict"]


def run_phase(items, env, red, width, one_per_crate, label):
    """One adjudication pass. Sets `verdict` on every item it judges.

    Runs twice: WIDE first (cheap, one build for many mutants), then
    ONE-PER-CRATE over just the CAUGHT set (safe, and small). A verdict is
    only ever reported from a pass that could attribute it.
    """
    batches = make_batches(items, width, one_per_crate=one_per_crate)
    print(f'sabotage: {label} — {len(items)} mutant(s) in {len(batches)} batch(es), {"one per crate" if one_per_crate else "packed"}', flush=True)
    originals = {}
    try:
        # Index-based, because a batch that fails to BUILD re-queues its halves
        # and the list grows while it is walked.
        i = 0
        while i < len(batches):
            batch = batches[i]
            i += 1
            for m in batch:
                p = ROOT / m["target"]
                originals.setdefault(str(p), p.read_text())
                apply_mutant(p, m)
            declared = [test_name_of(t) for m in batch for t in m["mustFail"]]
            print(f"sabotage: batch {i}/{len(batches)} — {len(batch)} mutant(s), "
                  f"{len(set(declared))} declared test(s)", flush=True)
            results = run_suite(env, only=declared)
            if results is None:
                # Retry ONCE — memory pressure is usually transient. A second
                # kill is reported as could-not-judge with the true reason, and
                # is never bisected: there is no broken mutant to isolate.
                print("sabotage: retrying batch after a signal kill", flush=True)
                results = run_suite(env, only=declared)
            if results is None:
                for m in batch:
                    m["verdict"] = "COULD-NOT-JUDGE"
                    m["detail"] = ("the build was KILLED by a signal (machine out "
                                   "of memory), twice — this mutant was never "
                                   "adjudicated and nothing about it is proven")
                for path, text in originals.items():
                    Path(path).write_text(text)
                originals.clear()
                continue
            # ONE NON-COMPILING MUTANT COSTS ITS WHOLE BATCH ITS VERDICTS, and
            # the model produces them: measured 2026-09-01, mutations that
            # referenced a removed function or a type not in scope (E0425,
            # E0422). cargo builds the WORKSPACE, so a single bad `replace`
            # takes down the build for all 16 of its batch-mates, who are then
            # could-not-judge for a reason that has nothing to do with them.
            # Bisect until the broken one is alone and named; everyone else
            # gets the verdict they earned. Only ever runs on failure, so the
            # healthy path pays nothing.
            if not results and len(batch) > 1:
                # BISECT, don't fan out. Re-queueing 25 mutants as singletons
                # costs 25 builds to find one bad `replace`; halving costs
                # log2(25) ~ 5. The halves are re-queued, so a batch with two
                # broken mutants splits again on its own.
                half = len(batch) // 2
                print(f"sabotage: batch {i} produced no report — bisecting its "
                      f"{len(batch)} mutant(s) to isolate the one that does not "
                      f"build", flush=True)
                batches.extend([batch[:half], batch[half:]])
                for path, text in originals.items():
                    Path(path).write_text(text)
                originals.clear()
                continue
            for m in batch:
                adjudicate(m, results, red)
            for p, text in originals.items():
                Path(p).write_text(text)
            originals.clear()
    finally:
        for path, text in originals.items():
            Path(path).write_text(text)

def self_test() -> int:
    """Falsifiers for the batching/application invariants. No cargo, no network.

    Each check names the wrong behaviour it forbids, and each would FAIL
    against the code as it stood on 2026-09-01 (ARCH §18.1: a check with no
    failing input you can name is not a check).

        scripts/sabotage.py --self-test
    """
    import tempfile
    fails = []

    def check(name, cond, detail=""):
        print(f"  {'ok  ' if cond else 'FAIL'}  {name}" + (f"\n        {detail}" if not cond and detail else ""))
        if not cond:
            fails.append(name)

    # 1. Two mutants on ONE file must never share a batch. This is the whole
    #    defect: they cannot both be present in the file at once.
    same_file = [
        {"id": "a", "target": "corpus-engine/src/lib.rs", "find": "A", "replace": "a"},
        {"id": "b", "target": "corpus-engine/src/lib.rs", "find": "B", "replace": "b"},
    ]
    batches = make_batches(same_file, width=25, one_per_crate=False)
    check("wide batching separates two mutants that share a target file",
          len(batches) == 2 and len(batches[0]) == 1,
          f"got {len(batches)} batch(es): {[[m['id'] for m in b] for b in batches]}")

    # 2. Different files MAY share a batch — otherwise --wide buys nothing.
    diff_file = [
        {"id": "a", "target": "corpus-engine/src/lib.rs", "find": "A", "replace": "a"},
        {"id": "b", "target": "corpus-engine/src/other.rs", "find": "B", "replace": "b"},
    ]
    check("wide batching still packs mutants in different files together",
          len(make_batches(diff_file, width=25, one_per_crate=False)) == 1)

    # 2b. ONE-PER-CRATE (what phase 2 runs, and what makes --wide sound) must
    #     still separate two mutants in DIFFERENT files of the SAME crate.
    same_crate = [
        {"id": "a", "target": "corpus-engine/src/lib.rs", "find": "A", "replace": "a"},
        {"id": "b", "target": "corpus-engine/src/other.rs", "find": "B", "replace": "b"},
    ]
    check("one-per-crate separates two files in the same crate (phase 2's guarantee)",
          len(make_batches(same_crate, width=25, one_per_crate=True)) == 2)

    with tempfile.TemporaryDirectory() as d:
        f = Path(d) / "t.rs"
        pristine = "fn one() { A }\nfn two() { B }\n"

        # 3. Sequential application ACCUMULATES.
        f.write_text(pristine)
        apply_mutant(f, {"id": "a", "target": str(f), "find": "A", "replace": "a"})
        apply_mutant(f, {"id": "b", "target": str(f), "find": "B", "replace": "b"})
        got = f.read_text()
        check("two mutations on one file both survive sequential application",
              "a" in got and "b" in got and "A" not in got and "B" not in got,
              f"got {got!r}")

        # 4. THE FALSIFIER FOR THE ORIGINAL BUG. Deriving each write from the
        #    PRISTINE text — what the code did until 2026-09-01 — silently
        #    discards the earlier mutation. Asserted here so the reason this
        #    file reads the current text can never be "cleaned up" back.
        f.write_text(pristine)
        for m in ({"find": "A", "replace": "a"}, {"find": "B", "replace": "b"}):
            f.write_text(pristine.replace(m["find"], m["replace"], 1))
        lost = f.read_text()
        check("pristine-derived application DOES lose the first mutation "
              "(the bug this file is shaped to prevent)",
              "A" in lost and "b" in lost,
              f"expected mutant A to have been discarded; got {lost!r}")

        # 5. A mutation that cannot land is a refusal, never a silent skip.
        f.write_text(pristine)
        try:
            apply_mutant(f, {"id": "gone", "target": str(f),
                             "find": "NOT_PRESENT", "replace": "x"})
            check("apply_mutant refuses a `find` that is not present", False,
                  "it returned instead of raising")
        except SystemExit as e:
            check("apply_mutant refuses a `find` that is not present",
                  "FALSE SURVIVED" in str(e), str(e)[:120])

    # 6. A bare name defined in two files is REFUSED; the same name qualified
    #    by its module resolves. Both halves matter: the refusal is what keeps
    #    a kill from being attributed to whichever crate sorted first.
    base = {
        "ck::commonwealth-knowledge::guest_grant::tests::expired_grant_is_not_live": True,
        "ck::commonwealth-knowledge::ingest_grant::tests::expired_grant_is_not_live": True,
        "ck::commonwealth-knowledge::other::tests::unrelated": True,
    }
    bare = [{"id": "bare", "mustFail": ["expired_grant_is_not_live"]}]
    st = resolve_declared(bare, base)
    check("a bare name defined in two files is refused, not guessed",
          st["ambiguous"] == 1 and bare[0].get("verdict") == "COULD-NOT-JUDGE",
          f"stats={st} verdict={bare[0].get('verdict')}")

    qual = [{"id": "qual", "mustFail": ["guest_grant::tests::expired_grant_is_not_live"]}]
    st = resolve_declared(qual, base)
    check("the same name qualified by its module resolves to exactly one test",
          st["resolved"] == 1 and qual[0]["mustFail"] ==
          ["ck::commonwealth-knowledge::guest_grant::tests::expired_grant_is_not_live"],
          f"stats={st} mustFail={qual[0].get('mustFail')}")

    # LEDGER GA-02 (note 33066b57, 2026-09-01). A JOIN THAT MATCHES NOTHING
    # MUST FAIL LOUDLY NAMING THE KEY. An empty result set is shaped exactly
    # like a clean run, and that is what cost 90 minutes of compute for zero
    # information: 62 candidates ran the full 26-batch loop and every one came
    # back COULD-NOT-JUDGE because the bank keyed tests one way and the report
    # keyed them another.
    #
    # The guard above it (`ambiguous`) was covered; this one was NOT. Probed
    # 2026-09-03 by replacing the refusal with `continue` — an empty `mustFail`
    # and no verdict — and the WHOLE self-test stayed green, 0 failures. The
    # instrument written to catch silent empties had a silent empty in it.
    absent = [{"id": "absent", "mustFail": ["no_such_test_anywhere_in_this_workspace"]}]
    st = resolve_declared(absent, base)
    check("a declared test absent from the report is refused BY NAME, not emptied",
          st["absent"] == 1
          and absent[0].get("verdict") == "COULD-NOT-JUDGE"
          and "no_such_test_anywhere_in_this_workspace" in absent[0].get("detail", ""),
          f"stats={st} verdict={absent[0].get('verdict')} detail={absent[0].get('detail')!r}")
    check("...and its mustFail is never silently emptied into a clean-looking run",
          absent[0]["mustFail"] == ["no_such_test_anywhere_in_this_workspace"],
          f"mustFail={absent[0].get('mustFail')}")

    # CONTROL (ARCH §18.4): the check above must not be answering
    # COULD-NOT-JUDGE to everything. A key that IS in the report resolves.
    present = [{"id": "present", "mustFail": ["ck::commonwealth-knowledge::other::tests::unrelated"]}]
    st = resolve_declared(present, base)
    check("a key present in the report still joins exactly (control)",
          st["exact"] == 1 and present[0].get("verdict") is None,
          f"stats={st} verdict={present[0].get('verdict')}")

    # 8. THE VERDICT ITSELF (EV-25). Checks 1-7 cover batching, application
    #    and name resolution — everything AROUND the judgment. What decides
    #    whether a mutation counts as proof is `adjudicate`, and nothing
    #    exercised it. Every arm below is a way of knowing nothing that would
    #    read as a pass, or as proof, if it were collapsed.
    def mutant(must_fail):
        return {"id": "m", "target": "x/src/lib.rs", "find": "A", "replace": "a",
                "mustFail": list(must_fail)}

    # 8a. A declared test that goes red is the ONLY thing that means CAUGHT.
    m = mutant(["t_one"])
    check("a declared test going red is CAUGHT",
          adjudicate(m, {"t_one": False, "t_two": True}, set()) == "CAUGHT"
          and m["killed"] == ["t_one"],
          f"verdict={m.get('verdict')} killed={m.get('killed')}")

    # 8b. A declared test that RAN and stayed green is SURVIVED — the product
    #     was broken and the suite did not notice. This is the verdict that is
    #     a bug report, and the one it is most tempting to soften.
    m = mutant(["t_one"])
    check("a declared test that ran and stayed green is SURVIVED",
          adjudicate(m, {"t_one": True, "t_two": True}, set()) == "SURVIVED",
          f"verdict={m.get('verdict')}")

    # 8c. A declared test ALREADY RED AT BASELINE is COULD-NOT-JUDGE, never
    #     CAUGHT. Its redness has nothing to do with the mutation — several
    #     tests here spawn real processes on real timers, so a loaded machine
    #     reddens them on its own. Counting one would let a flake masquerade
    #     as a defended requirement, which is a FALSE CAUGHT: the verdict that
    #     manufactures coverage out of noise.
    m = mutant(["t_flaky"])
    check("a declared test already red at baseline is COULD-NOT-JUDGE, not CAUGHT",
          adjudicate(m, {"t_flaky": False}, {"t_flaky"}) == "COULD-NOT-JUDGE"
          and "already red at baseline" in m.get("detail", ""),
          f"verdict={m.get('verdict')} detail={m.get('detail')!r}")

    # 8d. A declared test ABSENT from the report is COULD-NOT-JUDGE, not
    #     SURVIVED. Nobody ran it, so nobody looked — reporting SURVIVED would
    #     file a bug against a suite that was never asked the question.
    m = mutant(["t_missing"])
    check("a declared test missing from the report is COULD-NOT-JUDGE, not SURVIVED",
          adjudicate(m, {"t_other": True}, set()) == "COULD-NOT-JUDGE"
          and "not in the report" in m.get("detail", ""),
          f"verdict={m.get('verdict')} detail={m.get('detail')!r}")

    # 8e. A build that produced no report at all is COULD-NOT-JUDGE. The
    #     suite never ran; an empty report is not a green one.
    m = mutant(["t_one"])
    check("a batch with no report at all is COULD-NOT-JUDGE, not SURVIVED",
          adjudicate(m, {}, set()) == "COULD-NOT-JUDGE"
          and "does not compile" in m.get("detail", ""),
          f"verdict={m.get('verdict')} detail={m.get('detail')!r}")

    # 8f. One red among several declared tests is enough — a mutant declares
    #     the tests that SHOULD catch it, and any one of them doing so is the
    #     requirement being defended.
    m = mutant(["t_one", "t_two"])
    check("one red among several declared tests is CAUGHT, and names which",
          adjudicate(m, {"t_one": True, "t_two": False}, set()) == "CAUGHT"
          and m["killed"] == ["t_two"],
          f"verdict={m.get('verdict')} killed={m.get('killed')}")

    # 8g. STALE is decided BEFORE any build, from the source alone. Zero
    #     occurrences means the site moved; more than one means whichever site
    #     `replace(..., 1)` hits is an accident. Either way the bank is lying
    #     about the code, and neither is a verdict about the suite.
    src = "fn one() { A }\nfn two() { A }\n"
    m = mutant([]); m["find"] = "A"
    check("a `find` occurring twice is STALE before any build",
          mark_stale(m, src) and m["verdict"] == "STALE",
          f"verdict={m.get('verdict')}")

    m = mutant([]); m["find"] = "GONE"
    check("a `find` that no longer occurs is STALE, not a silent skip",
          mark_stale(m, src) and m["verdict"] == "STALE",
          f"verdict={m.get('verdict')}")

    m = mutant([]); m["find"] = "fn one"
    check("a `find` occurring exactly once is NOT stale (the control)",
          not mark_stale(m, src) and "verdict" not in m,
          f"verdict={m.get('verdict')}")

    # 8h. A deleted target is SUBJECT-GONE, and is decided before the read —
    #     the bug this replaces was a FileNotFoundError that killed the whole
    #     bank, so the control below asserts the read is SKIPPED, not merely
    #     that the verdict differs.
    m = mutant([])
    check("a target that is not on disk is SUBJECT-GONE, not a traceback",
          mark_subject_gone(m, False) and m["verdict"] == "SUBJECT-GONE"
          and "not on disk" in m["detail"],
          f"verdict={m.get('verdict')} detail={m.get('detail')}")

    m = mutant([])
    check("a target that exists is NOT subject-gone (the control)",
          not mark_subject_gone(m, True) and "verdict" not in m,
          f"verdict={m.get('verdict')}")

    # 7. A killed run must UNWIND, or `run_phase`'s finally never restores the
    #    mutated tree. Python's default SIGTERM does not unwind.
    import os as _os
    _restore_on_signal()
    unwound = []
    try:
        try:
            _os.kill(_os.getpid(), signal.SIGTERM)
        finally:
            unwound.append(True)
        check("SIGTERM unwinds so mutated files are restored", False,
              "no SystemExit was raised")
    except SystemExit:
        check("SIGTERM unwinds so mutated files are restored", unwound == [True])

    print(f"\nself-test: {len(fails)} failure(s)")
    return 1 if fails else 0


def _restore_on_signal() -> None:
    """Make SIGTERM/SIGINT unwind, so `run_phase`'s `finally` restores the tree.

    Python's default SIGTERM handler terminates without unwinding, so a killed
    run leaves every mutant of the batch in flight WRITTEN INTO TRACKED FILES.
    Measured 2026-09-01: killing a run to free the machine left 8 source files
    mutated in the worktree, and the next run then refused to start (correctly)
    on a dirty tree. Long runs get killed — that is normal operation, not
    misuse — so the restore has to survive it.
    """
    def die(signum, _frame):
        raise SystemExit(f"sabotage: caught signal {signum} — restoring mutated files")
    for sig in (signal.SIGTERM, signal.SIGINT):
        signal.signal(sig, die)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bank")
    ap.add_argument("--self-test", action="store_true",
                    help="run the batching/application falsifiers and exit")
    ap.add_argument("--only")
    ap.add_argument("--json")
    ap.add_argument("--allow-dirty", action="store_true")
    ap.add_argument("--batch", type=int, default=20)
    ap.add_argument("--wide", action="store_true",
                    help="phase 1: pack batches by count instead of one per "
                         "crate. Everything it reports CAUGHT is re-adjudicated "
                         "one-per-crate before it counts.")
    a = ap.parse_args()
    _restore_on_signal()

    if a.self_test:
        return self_test()
    if not a.bank:
        ap.error("--bank is required (or pass --self-test)")

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
    # Existence is checked BEFORE the read, and is its own verdict. Until
    # 2026-09-03 this line was a bare `read_text()`, so one deleted target
    # raised FileNotFoundError and took down adjudication of the entire bank
    # with a traceback and an exit code indistinguishable from a real failure.
    # Deletion is a routine, frequent event here — a whole refactor phase is
    # deletions — and "the subject is gone" is a different fact from "the find
    # string moved" (ARCH §18.3: absence is reported, never defaulted).
    for m in bank:
        path = ROOT / m["target"]
        if not mark_subject_gone(m, path.is_file()):
            mark_stale(m, path.read_text())

    live = [m for m in bank if "verdict" not in m]

    print(f"sabotage: baseline run ({len(live)} live mutant(s), batch {a.batch})", flush=True)
    baseline = run_suite(env)
    if baseline is None:
        print("sabotage: the BASELINE run was killed by a signal — refusing to "
              "adjudicate against an unknown baseline", file=sys.stderr)
        return 2
    index_report((ROOT / "target" / "nextest" / PROFILE / "junit.xml").read_text()
                 if (ROOT / "target" / "nextest" / PROFILE / "junit.xml").is_file() else "")
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

    # Resolve declared test names against the key space the report actually
    # uses, BEFORE spending a batch run on a name that can never match.
    stats = resolve_declared(bank, baseline)
    print("sabotage: declared tests — exact {exact}, resolved {resolved}, "
          "ambiguous {ambiguous}, absent {absent}".format(**stats), flush=True)
    live = [m for m in bank if "verdict" not in m]
    if not live:
        print("sabotage: no mutant has a resolvable declared test — nothing to "
              "adjudicate", file=sys.stderr)

    run_phase(live, env, red, a.batch, one_per_crate=not a.wide,
              label='phase 1 (wide)' if a.wide else 'adjudication')

    # PHASE 2. `--wide` packs unrelated mutants into one build, so a CAUGHT
    # there may belong to a batch-mate. Cross-talk can only manufacture a
    # FALSE CAUGHT — a mutation does not make a failing test pass — so
    # re-adjudicating exactly the CAUGHT set, one per crate, is enough to
    # make every surviving CAUGHT attributable. It is also cheap: CAUGHT was
    # 15% of candidates on the GR family.
    if a.wide:
        provisional = [m for m in live if m.get('verdict') == 'CAUGHT']
        for m in provisional:
            m.pop('verdict', None)
            m.pop('killed', None)
            m.pop('detail', None)
        if provisional:
            run_phase(provisional, env, red, a.batch, one_per_crate=True,
                      label='phase 2 (confirming CAUGHT, one per crate)')
        for m in provisional:
            if m.get('verdict') != 'CAUGHT':
                m['detail'] = ('phase 1 reported CAUGHT; one-per-crate '
                               're-run did not confirm it')

    still = subprocess.run(['git', 'status', '--porcelain'] + [m['target'] for m in bank],
                           cwd=ROOT, capture_output=True, text=True).stdout.strip()
    if still and not a.allow_dirty:
        print(f'sabotage: RESTORE FAILED — tree still dirty:\n{still}', file=sys.stderr)
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
        # The verdict alone does not say WHY for the two arms that are about
        # the bank rather than the suite. A bare `BAD SUBJECT-GONE` sends the
        # reader diffing TOML against the tree by hand; the detail names the
        # path and the two ways out (ARCH §9.1 — a branch with no visible
        # decision).
        if m["verdict"] in ("SUBJECT-GONE", "STALE") and m.get("detail"):
            print(f"       {m['detail']}")

    if a.json:
        Path(a.json).write_text(json.dumps({"counts": counts, "mutants": bank}, indent=1, default=str))

    return 0 if all(m["verdict"] == m.get("expected", "CAUGHT") for m in bank) else 1


if __name__ == "__main__":
    sys.exit(main())
