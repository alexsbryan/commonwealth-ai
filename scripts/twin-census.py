#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""twin-census.py — watch every family census fail.

sv-surface's third construction: *per family, a planted second implementation
turns the family census red*. Every census in `quality/twin-plants.toml`
already claims in its own header that it was "sabotage-verified at landing".
That claim is a reviewer's word. This runner turns it into a run.

ARCH §18.1 — a gate you have not WATCHED fail is not a gate. §18.2 — four
verdicts, not two, each with its own exit code.

Per family:

  1. run the census on the CLEAN tree. A census that is red before planting
     judges nothing, so that is could-not-judge, never proof.
  2. apply the plant — a real second implementation, in real production
     source, that must still compile.
  3. run the census again and require it to FAIL *naming* the expected
     assertion. "It failed" is not enough: a census reddened by some other
     assertion has not been shown to detect THIS twin.
  4. restore the file byte-for-byte and verify the restore.

VERDICTS
  proved-red       green clean, red planted, and the red named its rule.
  NOT-A-GATE       green clean, still green planted. The census does not
                   detect the twin it is named for. A real finding.
  could-not-judge  the run made no claim — a dirty plant file, a stale
                   `find`, a plant that did not compile, a census already
                   red, or a red that named something else.
  never-ran        the gate script reported zero tests (its exit 4): the
                   filter matched nothing, so nothing was measured.

EXIT
  0  every selected family proved-red
  1  at least one NOT-A-GATE
  4  no NOT-A-GATE, but at least one could-not-judge / never-ran
  2  usage / registry error

The plant is a MUTATION OF PRODUCTION SOURCE applied and reverted inside one
invocation. `git checkout --` is the restore, which is why a dirty plant file
is refused rather than planted into: the restore would eat a peer's
uncommitted work. Every cargo-touching command goes through
`scripts/with-cargo-lock.sh` (AGENTS.md, "Compilation and test feedback").

THE SURFACE THAT WAS CHECKED FIRST (ARCH §19)
---------------------------------------------
`scripts/sabotage.py` + `quality/sabotage/*.toml` is the same mechanism and
came first: a TOML bank of find/replace mutants, applied to tracked source,
run, restored in a `finally`, verified — with a RICHER verdict machine than
this one (baseline-red detection, STALE, SUBJECT-GONE, batched attribution at
one mutant per crate, and a phase-2 re-adjudication of every CAUGHT to kill
false positives from cross-talk).

It is not forked here, and this file's schema is deliberately ITS schema —
`target` / `find` / `replace` / `mustFail` / `breaks` — so a plant lifts
between the two adjudicators without translation. Three things it does not do,
and each is load-bearing for the twin-census bar:

  1. It runs `cargo nextest run --workspace` per batch, at one mutant per
     crate. Six of the nine families are sovereign-desktop, so adjudicating
     them there is six full-workspace runs. This runner drives the scoped gate
     (`sovereign-test.sh --package X --filter <test>`), which the brief
     mandates and which is the difference between minutes and hours.
  2. Its CAUGHT means "a declared test went red". It never checks WHAT the red
     said. A census file here holds three or four assertions; "it failed" is
     weak evidence that THIS twin was the one detected — §18.1's "assert on
     something the subject cannot author", turned on the gate itself. That is
     `expect_red`, and it is the only genuinely new field.
  3. It does not route through `scripts/with-cargo-lock.sh`, which AGENTS.md
     now requires while a peer worker builds in the same tree.

If (1) and (3) are ever fixed in sabotage.py and `expect_red` lands in its
bank schema, this runner should be deleted and these families moved to
`quality/sabotage/`. One adjudicator is better than two (§10.6).
"""

from __future__ import annotations

import argparse
import atexit
import json
import os
import re
import signal
import subprocess
import sys
import time
import tomllib
from dataclasses import dataclass, field, asdict
from datetime import datetime, timezone
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
REGISTRY = REPO_ROOT / "quality" / "twin-plants.toml"

# The tree the plants are applied to and the gate is run in. Defaults to this
# checkout; `--worktree` points it at a detached worktree so a PEER'S
# uncommitted work in this checkout cannot decide a verdict. Measured
# 2026-09-10: it did — a half-landed `ServingCore.features` in sovereign-mesh
# left sovereign-desktop not compiling, and six families abstained on someone
# else's edit. (`scripts/evidence-verdict.py` uses a worktree for the same
# reason; ARCH §19, the prior art was checked.)
WORK_ROOT = REPO_ROOT

PROVED_RED = "proved-red"
NOT_A_GATE = "NOT-A-GATE"
CANNOT_JUDGE = "could-not-judge"
NEVER_RAN = "never-ran"

# The gate script's own contract: a zero-test run is never green.
GATE_EXIT_ZERO_TESTS = 4
GATE_EXIT_UNATTRIBUTABLE = 5

COMPILE_ERROR = re.compile(
    r"^(error\[E\d+\]|error: could not compile|error: expected|error: cannot find)",
    re.MULTILINE,
)


# ── verdict rows ───────────────────────────────────────────────────────────


@dataclass
class Row:
    family: str
    crate: str
    census: str
    test: str
    breaks: str
    target: str
    verdict: str = NEVER_RAN
    reason: str = "not reached"
    green_secs: float = 0.0
    red_secs: float = 0.0
    green_exit: int | None = None
    red_exit: int | None = None
    logs: list[str] = field(default_factory=list)

    @property
    def total_secs(self) -> float:
        return self.green_secs + self.red_secs


# ── the plant, and its guaranteed removal ──────────────────────────────────


class Plant:
    """One applied mutation. Restores on drop, on signal, and at exit."""

    _active: "Plant | None" = None

    def __init__(self, path: Path, find: str, replace: str):
        self.path = path
        self.find = find
        self.replace = replace
        self.applied = False

    def apply(self) -> None:
        text = self.path.read_text()
        assert text.count(self.find) == 1, "uniqueness is checked before apply"
        self.path.write_text(text.replace(self.find, self.replace, 1))
        self.applied = True
        Plant._active = self

    def restore(self) -> None:
        if not self.applied:
            return
        rel = self.path.relative_to(WORK_ROOT)
        subprocess.run(
            ["git", "checkout", "--", str(rel)], cwd=WORK_ROOT, check=False
        )
        self.applied = False
        if Plant._active is self:
            Plant._active = None
        dirty = git_porcelain([rel])
        if dirty:
            # Loud, not silent: every verdict after a failed restore is
            # measured against a contaminated tree.
            print(
                f"\n!! RESTORE FAILED for {rel} — tree is dirty:\n{dirty}\n"
                "!! Fix by hand before trusting anything below.",
                file=sys.stderr,
            )
            raise SystemExit(2)


def _emergency_restore(*_args) -> None:
    if Plant._active is not None:
        print("\ntwin-census: interrupted — restoring the active plant", file=sys.stderr)
        try:
            Plant._active.restore()
        except SystemExit:
            pass
    raise SystemExit(130)


atexit.register(lambda: Plant._active.restore() if Plant._active else None)
signal.signal(signal.SIGINT, _emergency_restore)
signal.signal(signal.SIGTERM, _emergency_restore)


# ── shelling out ───────────────────────────────────────────────────────────


def git_porcelain(paths: list[Path | str]) -> str:
    out = subprocess.run(
        ["git", "status", "--porcelain", "--"] + [str(p) for p in paths],
        cwd=WORK_ROOT,
        capture_output=True,
        text=True,
    )
    return out.stdout.strip()


def run_census(crate: str, test: str, log_path: Path, timeout: int) -> tuple[int, str, float]:
    """One gate-script invocation, through the cargo lock. Never bare cargo."""
    argv = [
        str(WORK_ROOT / "scripts" / "with-cargo-lock.sh"),
        str(WORK_ROOT / "scripts" / "sovereign-test.sh"),
        "--human",
        "--package",
        crate,
        "--filter",
        test,
    ]
    started = time.monotonic()
    try:
        proc = subprocess.run(
            argv,
            cwd=WORK_ROOT,
            capture_output=True,
            text=True,
            timeout=timeout,
            stdin=subprocess.DEVNULL,
        )
        out = proc.stdout + "\n" + proc.stderr
        code = proc.returncode
    except subprocess.TimeoutExpired as e:
        out = (e.stdout or b"").decode(errors="replace") if isinstance(e.stdout, bytes) else (e.stdout or "")
        out += f"\n!! twin-census: timed out after {timeout}s"
        code = 124
    secs = time.monotonic() - started
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_path.write_text(f"$ {' '.join(argv)}\nexit={code}\n\n{out}")
    return code, out, secs


# ── the run ────────────────────────────────────────────────────────────────


def load_registry() -> tuple[dict, list[dict]]:
    if not REGISTRY.exists():
        print(f"twin-census: no registry at {REGISTRY}", file=sys.stderr)
        raise SystemExit(2)
    with REGISTRY.open("rb") as fh:
        doc = tomllib.load(fh)
    fams = doc.get("family", [])
    if not fams:
        print("twin-census: registry declares no families", file=sys.stderr)
        raise SystemExit(2)
    # `mustFail` is the sabotage bank's field and is a LIST there, because that
    # adjudicator runs the whole workspace and a mutant may name several tests.
    # This runner's unit of work is one `--filter`, so one entry, and the
    # refusal is loud rather than a silent "first one wins".
    for f in fams:
        must = f.get("mustFail", [])
        if len(must) != 1:
            print(
                f"twin-census: family `{f.get('id')}` declares {len(must)} mustFail "
                "entries; this runner filters ONE test per family. Split it.",
                file=sys.stderr,
            )
            raise SystemExit(2)
        f["test"] = must[0]
    return doc.get("meta", {}), fams


def preflight(fam: dict, row: Row) -> Plant | None:
    """Guard cleanliness + plant staleness. Both are could-not-judge, and
    both are decided BEFORE any cargo cost is paid."""
    plant_path = WORK_ROOT / fam["target"]
    guards = [fam["target"], fam["census"]] + list(fam.get("guard_paths", []))
    missing = [g for g in guards if not (WORK_ROOT / g).exists()]
    if missing:
        row.verdict = CANNOT_JUDGE
        row.reason = f"registry path does not exist: {', '.join(missing)}"
        return None

    dirty = git_porcelain(guards)
    if dirty:
        row.verdict = CANNOT_JUDGE
        row.reason = (
            "refusing to plant: uncommitted changes under this family's guard "
            f"paths ({dirty.splitlines()[0].strip()}"
            + (f" +{len(dirty.splitlines()) - 1} more" if len(dirty.splitlines()) > 1 else "")
            + "). The restore is `git checkout --`, which would discard them."
        )
        return None

    text = plant_path.read_text()
    hits = text.count(fam["find"])
    if hits != 1:
        row.verdict = CANNOT_JUDGE
        row.reason = (
            f"STALE plant: `find` occurs {hits}x in {fam['target']} (need "
            "exactly 1). A plant that stops applying keeps printing proved-red."
        )
        return None

    # The OTHER half of the anti-rot check, and the one that fails silently:
    # if someone rewords a census assertion, `expect_red` stops matching and
    # every family here turns could-not-judge with a reason that reads like a
    # census problem. Caught here it is one line, for free, before any build.
    # `\`+newline in a Rust string literal eats the newline AND the following
    # indentation, so the census source is flattened the way rustc sees it.
    census_flat = re.sub(r"\\\n\s*", "", (WORK_ROOT / fam["census"]).read_text())
    if fam["expect_red"] not in census_flat:
        row.verdict = CANNOT_JUDGE
        row.reason = (
            f"STALE expectation: `expect_red` ({fam['expect_red']!r}) is not "
            f"in {fam['census']} any more. The assertion was reworded; the "
            "registry has to follow, or a real red reads as could-not-judge."
        )
        return None

    return Plant(plant_path, fam["find"], fam["replace"])


def judge_red(fam: dict, row: Row, code: int, out: str) -> None:
    expect = fam["expect_red"]
    if code == 0:
        row.verdict = NOT_A_GATE
        row.reason = (
            f"the census stayed GREEN with the twin planted. It does not "
            f"detect: {fam['breaks']}"
        )
        return
    if expect in out:
        row.verdict = PROVED_RED
        row.reason = f"red, naming its rule: …{expect}…"
        return
    if COMPILE_ERROR.search(out):
        row.verdict = CANNOT_JUDGE
        row.reason = (
            "the plant did not COMPILE, so the run went red for the wrong "
            "reason — this says nothing about the census. Fix the `replace`."
        )
        return
    if code == GATE_EXIT_ZERO_TESTS:
        row.verdict = NEVER_RAN
        row.reason = "gate exit 4: the planted run resolved zero tests."
        return
    row.verdict = CANNOT_JUDGE
    row.reason = (
        f"red (exit {code}) but the failure never named `{expect}` — the "
        "census may have been reddened by a different assertion."
    )


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--family", action="append", default=[], help="run only these family ids (repeatable)")
    ap.add_argument("--list", action="store_true", help="print the registry and exit")
    ap.add_argument(
        "--dry-run",
        action="store_true",
        help="preflight + apply/restore round-trip only; runs no cargo",
    )
    ap.add_argument("--timeout", type=int, default=3600, help="seconds per census run (default 3600)")
    ap.add_argument(
        "--worktree",
        nargs="?",
        const="HEAD",
        default=None,
        metavar="REF",
        help="run the censuses in a detached worktree at REF (default HEAD), so a "
        "peer's uncommitted work in this checkout cannot decide a verdict",
    )
    args = ap.parse_args()

    meta, fams = load_registry()

    if args.family:
        wanted = set(args.family)
        unknown = wanted - {f["id"] for f in fams}
        if unknown:
            print(f"twin-census: unknown family id(s): {', '.join(sorted(unknown))}", file=sys.stderr)
            return 2
        fams = [f for f in fams if f["id"] in wanted]

    if args.list:
        for f in fams:
            print(f"{f['id']:<22} {f['crate']:<20} {f['test']}")
            print(f"{'':<22} breaks: {f['breaks']}")
            print(f"{'':<22} target: {f['target']}")
        return 0

    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    out_dir = REPO_ROOT / "target" / "twin-census" / stamp
    if not args.dry_run:
        # --dry-run runs nothing and writes nothing; littering a stamp dir per
        # invocation makes the artifact tree lie about how often this ran.
        out_dir.mkdir(parents=True, exist_ok=True)

    global WORK_ROOT
    wt_dir: Path | None = None
    if args.worktree:
        wt_dir = REPO_ROOT / "target" / "twin-census-wt" / stamp
        wt_dir.parent.mkdir(parents=True, exist_ok=True)
        r = subprocess.run(
            ["git", "worktree", "add", "--detach", str(wt_dir), args.worktree],
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
        )
        if r.returncode != 0:
            print(f"twin-census: could not create worktree at {args.worktree}:\n{r.stderr}", file=sys.stderr)
            return 2
        WORK_ROOT = wt_dir
        head = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"], cwd=wt_dir, capture_output=True, text=True
        ).stdout.strip()
        print(f"twin-census: judging in a detached worktree at {args.worktree} ({head})")
        print(f"             {wt_dir.relative_to(REPO_ROOT)}\n")
        atexit.register(
            lambda: subprocess.run(
                ["git", "worktree", "remove", "--force", str(wt_dir)],
                cwd=REPO_ROOT,
                capture_output=True,
            )
        )

    rows: dict[str, Row] = {}
    plants: dict[str, Plant] = {}
    for f in fams:
        rows[f["id"]] = Row(
            family=f["id"],
            crate=f["crate"],
            census=f["census"],
            test=f["test"],
            breaks=f["breaks"],
            target=f["target"],
        )

    started = time.monotonic()

    # ── preflight ──────────────────────────────────────────────────────────
    print(f"twin-census: {len(fams)} families, registry {REGISTRY.relative_to(REPO_ROOT)}\n")
    for f in fams:
        row = rows[f["id"]]
        plant = preflight(f, row)
        if plant is None:
            print(f"  {f['id']:<22} PREFLIGHT  {row.verdict}: {row.reason}")
        else:
            plants[f["id"]] = plant

    if args.dry_run:
        # Prove the plant APPLIES and the restore is byte-exact, with no
        # cargo cost. This is the check that keeps the registry from rotting.
        for fid, plant in plants.items():
            before = plant.path.read_bytes()
            plant.apply()
            after = plant.path.read_bytes()
            plant.restore()
            back = plant.path.read_bytes()
            ok = after != before and back == before
            rows[fid].verdict = "dry-run-ok" if ok else CANNOT_JUDGE
            rows[fid].reason = (
                "plant applies and restores byte-for-byte"
                if ok
                else "plant did not change the file, or the restore was not byte-exact"
            )
            print(f"  {fid:<22} DRY-RUN    {rows[fid].verdict}")
        print()
        print_table(list(rows.values()))
        if any(r.verdict == CANNOT_JUDGE and r.family in plants for r in rows.values()):
            return 2  # a plant that will not apply or will not restore
        return 0 if all(r.verdict == "dry-run-ok" for r in rows.values()) else 4

    # ── phase 1: the census on the CLEAN tree ──────────────────────────────
    # All greens first, so one build per crate serves every family in it.
    print("\n── phase 1: the census on the clean tree ──")
    for f in fams:
        fid = f["id"]
        if fid not in plants:
            continue
        row = rows[fid]
        log = out_dir / f"{fid}.green.log"
        code, out, secs = run_census(f["crate"], f["test"], log, args.timeout)
        row.green_exit, row.green_secs = code, secs
        row.logs.append(str(log.relative_to(REPO_ROOT)))
        # A BUILD FAILURE IS A BUILD FAILURE, whatever exit code carries it.
        # This ordering is not cosmetic: watched 2026-09-10, a peer's
        # half-landed `ServingCore.features` left sovereign-desktop not
        # compiling, cargo exited 101, nextest wrote no junit, and the gate
        # reported its exit 5 — so the first cut of this branch published
        # "unattributable results (a concurrent nextest run)" for six
        # families. That is §18.3's silent substitution wearing an honest
        # face: a confident wrong reason sends the reader to the wrong fix.
        compile_broke = COMPILE_ERROR.search(out)
        if compile_broke:
            row.verdict = CANNOT_JUDGE
            row.reason = (
                f"the crate does not BUILD on this tree (exit {code}): "
                f"{compile_broke.group(0)[:80]}. The census never ran, so it "
                "made no claim — run with --worktree to judge committed code."
            )
        elif code == GATE_EXIT_ZERO_TESTS:
            row.verdict = NEVER_RAN
            row.reason = (
                "gate exit 4: zero tests resolved for "
                f"--package {f['crate']} --filter {f['test']}. Nothing was measured."
            )
        elif code == GATE_EXIT_UNATTRIBUTABLE:
            row.verdict = CANNOT_JUDGE
            row.reason = (
                "gate exit 5: results could not be attributed to this run "
                "(a concurrent nextest run overwrote the shared report)."
            )
        elif code != 0:
            row.verdict = CANNOT_JUDGE
            row.reason = (
                f"the census is ALREADY RED on the clean tree (exit {code}); "
                "a red-before-planting census proves nothing."
            )
        print(f"  {fid:<22} GREEN      exit={code} {secs:6.1f}s"
              + ("" if code == 0 else f"  -> {row.verdict}"))
        if code != 0:
            plants.pop(fid, None)

    # ── phase 2: plant, watch it go red, restore ───────────────────────────
    print("\n── phase 2: plant / watch red / restore ──")
    for f in fams:
        fid = f["id"]
        if fid not in plants:
            continue
        row, plant = rows[fid], plants[fid]
        plant.apply()
        try:
            log = out_dir / f"{fid}.red.log"
            code, out, secs = run_census(f["crate"], f["test"], log, args.timeout)
            row.red_exit, row.red_secs = code, secs
            row.logs.append(str(log.relative_to(REPO_ROOT)))
            judge_red(f, row, code, out)
        finally:
            plant.restore()
        print(f"  {fid:<22} PLANTED    exit={code} {secs:6.1f}s  -> {row.verdict}")

    elapsed = time.monotonic() - started
    print()
    print_table(list(rows.values()))
    print(f"\ntotal runtime: {elapsed / 60:.1f} min   logs: {out_dir.relative_to(REPO_ROOT)}")

    summary = {
        "stamp": stamp,
        "meta": meta,
        "commit": subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=REPO_ROOT, capture_output=True, text=True
        ).stdout.strip(),
        "total_secs": round(elapsed, 1),
        "counts": {
            v: sum(1 for r in rows.values() if r.verdict == v)
            for v in (PROVED_RED, NOT_A_GATE, CANNOT_JUDGE, NEVER_RAN)
        },
        "families": [asdict(r) for r in rows.values()],
    }
    (out_dir / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")

    if any(r.verdict == NOT_A_GATE for r in rows.values()):
        return 1
    if any(r.verdict in (CANNOT_JUDGE, NEVER_RAN) for r in rows.values()):
        return 4
    return 0


def print_table(rows: list[Row]) -> None:
    w_fam = max(6, *(len(r.family) for r in rows))
    w_crate = max(5, *(len(r.crate) for r in rows))
    w_test = min(46, max(4, *(len(r.test) for r in rows)))
    head = f"{'family':<{w_fam}}  {'crate':<{w_crate}}  {'census test':<{w_test}}  {'verdict':<15}  {'green':>7}  {'red':>7}"
    print(head)
    print("-" * len(head))
    for r in sorted(rows, key=lambda x: (x.verdict != NOT_A_GATE, x.family)):
        test = r.test if len(r.test) <= w_test else r.test[: w_test - 1] + "…"
        print(
            f"{r.family:<{w_fam}}  {r.crate:<{w_crate}}  {test:<{w_test}}  "
            f"{r.verdict:<15}  {r.green_secs:6.1f}s  {r.red_secs:6.1f}s"
        )
        if r.verdict != PROVED_RED:
            print(f"{'':<{w_fam}}  ↳ {r.reason}")
    counts: dict[str, int] = {}
    for r in rows:
        counts[r.verdict] = counts.get(r.verdict, 0) + 1
    print("-" * len(head))
    print("  ".join(f"{v}: {n}" for v, n in sorted(counts.items())))


if __name__ == "__main__":
    sys.stdout.reconfigure(line_buffering=True)
    os.chdir(REPO_ROOT)
    sys.exit(main())
