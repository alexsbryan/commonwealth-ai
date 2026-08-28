#!/usr/bin/env python3
"""Deletion campaign — THE manifest. One implementation (ARCH §10.6).

The campaign in quality/DELETION.md deletes files. This script decides WHICH.
No executing agent picks files by judgement; every bar in
quality/campaigns/deletion.toml shells out to this, and the diff of a landed
phase must equal `--lane <l> --files` exactly.

Rules are code, spares are code, and every spare cites the reader that keeps
it alive. Widening a lane or adding a spare is a DIFF TO THIS FILE, reviewed
like any other — which is the point. See quality/DELETION.md §Diversion.

  deletion-manifest.py --lane p0-root-junk            # summary
  deletion-manifest.py --lane p0-root-junk --files    # newline-separated paths
  deletion-manifest.py --all                          # every lane, JSON
  deletion-manifest.py --verify                       # vs frozen baseline
  deletion-manifest.py --freeze                       # write the baseline
"""
from __future__ import annotations
import argparse, json, os, subprocess, sys

ROOT = subprocess.run(["git", "rev-parse", "--show-toplevel"],
                      capture_output=True, text=True).stdout.strip()
BASELINE = os.path.join(ROOT, "quality", "baselines", "deletion_manifest.tsv")


def tracked() -> list[str]:
    out = subprocess.run(["git", "ls-files"], cwd=ROOT,
                         capture_output=True, text=True).stdout
    return [f for f in out.split("\n") if f]


def lines(rel: str) -> int:
    try:
        with open(os.path.join(ROOT, rel), "rb") as fh:
            return fh.read().count(b"\n")
    except OSError:
        return 0


# ── Spares. Each entry names the reader that keeps the file alive. ──────────
# bench_cmd/enron.rs:349 and :736 open these two by literal path, so they are
# reachable despite not being a latest.json target.
SPARE_BENCH = {
    "sovereign/bench/enron/baselines/enron-entity-resolution/peek_budget.json",
    "sovereign/bench/enron/baselines/enron-entity-resolution/pre_reconciliation.json",
}
# quality/initiative-bars.toml:1817 cites these two as the evidence for a
# banked bar verdict. Deleting them dangles a recorded verdict.
SPARE_LOGS = {
    "runs/serve50-availability/peer-busy_20260814_125105.log",
    "runs/serve50-availability/peer-idle_20260814_124029.log",
}

# ── DR flight trees. READ_CLASSES is derived from every consumer found:
#   quality/campaigns/drb1-race.toml   -> manifest.json, score*.json, GATE-VERDICT.json
#   research/deep-research/arms/forensics-collect.py
#                                      -> verdict-set, evidence-window-*, fetch-list-*, draft-*
#   research/deep-research/arms/replay/run.sh
#                                      -> manifest.json, evidence-window-1, draft-1
# DELETE_CLASSES is strictly runtime bookkeeping: resume state, budget/skip
# accounting, and console capture. charter.json (the run's PRE-REGISTRATION),
# report.md (the finding), plan.json and survey-* are DELIBERATELY NOT here —
# see quality/DELETION.md §P1b for why the aggressive variant was declined.
DR_DELETE_PREFIXES = ("gap-list-", "budget-ledger", "skip-ledger-")
DR_DELETE_EXACT = {"checkpoint.json", "resume-input.json", "render-race.md"}


def _is_dr_deletable(base: str) -> bool:
    if base in DR_DELETE_EXACT:
        return True
    if base.endswith(".console.log"):
        return True
    return any(base.startswith(p) for p in DR_DELETE_PREFIXES)


# ── Lanes ───────────────────────────────────────────────────────────────────
def lane_p0_root_junk(files: list[str]) -> list[str]:
    """Committed process output at or near the repo root. Nothing reads any."""
    out = []
    gz_bases = {f[:-3] for f in files if f.endswith(".gz")}
    tracked_set = set(files)
    for f in files:
        base = os.path.basename(f)
        at_root = "/" not in f
        if at_root and base.startswith("score-report-") and f.endswith(".json"):
            out.append(f)                       # committed by 8bdefaa6 "stash dr work"
        elif f.endswith(".log") and f not in SPARE_LOGS:
            out.append(f)
        elif f.startswith("baselines/"):        # stray root dir; zero refs since 2026-06-09
            out.append(f)
        elif f.endswith(".gz") and f[:-3] in tracked_set:
            out.append(f)                       # same bytes committed twice
    return sorted(set(out))


def lane_p1_bench_baselines(files: list[str]) -> list[str]:
    """Baseline snapshots the reader cannot address.

    sovereign-cli-llm/src/bench_cmd/baselines.rs:39 builds every path as
    bench_root/<group>/baselines/<id>/ and :44 opens `latest.json` inside it.
    So (a) any file sitting FLAT in a baselines/ dir has no <id>/ and is
    unreachable by construction, and (b) inside an <id>/ dir only latest.json
    and its symlink target are ever opened.
    """
    cand = [f for f in files if "/baselines/" in f and f.startswith("sovereign/bench/")]
    keep: set[str] = set()
    for f in cand:
        d = os.path.dirname(os.path.join(ROOT, f))
        lj = os.path.join(d, "latest.json")
        if os.path.exists(lj):
            keep.add(os.path.realpath(lj))
            keep.add(os.path.abspath(lj))
    out = []
    for f in cand:
        if f in SPARE_BENCH:
            continue
        tail = f.split("/baselines/", 1)[1]
        flat = "/" not in tail
        ap, rp = os.path.abspath(os.path.join(ROOT, f)), os.path.realpath(os.path.join(ROOT, f))
        if flat or (ap not in keep and rp not in keep):
            out.append(f)
    return sorted(out)


def lane_p1_dr_flights(files: list[str]) -> list[str]:
    """Runtime bookkeeping inside committed flight trees.

    research/deep-research/arms/.gitignore already states the policy: "Flight
    trees are EVIDENCE, not source... the run trees stay local." Three trees
    obey it; sixteen were committed anyway between 2026-08-17 and 08-21.
    """
    return sorted(
        f for f in files
        if f.startswith("research/") and "/runs-" in f and _is_dr_deletable(os.path.basename(f))
    )


# Examples with NO runner. No CI lane builds examples; sovereign-lint.sh's
# --all-targets CHECKS them but never links them. This is an ALLOWLIST, not
# "every example minus a few keepers": each name below was individually
# cleared (zero references, or referenced only by an archived findings doc).
# Twelve further examples were examined and NOT cleared; they are absent on
# purpose. Adding a name here is a diff, and needs the same evidence.
DEAD_EXAMPLES_NO_REFS = [  # zero references anywhere in the tree
    "rescan_render", "intent_instruction_probe", "fact_extract",
    "wiring_drift_probe", "wrapped_dump", "probe_bdd", "probe_multi_file",
    "dump_code_index", "exercise_code_tools", "gliner2_backend_smoke",
    "triage_dump", "fact_check_smoke", "warm_cache", "gliner_smoke",
    "bench_wiki_graph", "build_fts", "check_edge_index", "build_edge_btree",
    "build_title_btree", "fact_spike",
]
DEAD_EXAMPLES_SPIKE = [  # referenced ONLY by an archived findings write-up
    "sd_smoke", "sp6_late_chunk", "atoms_lance_proto", "notes_tiered_bench",
    "concept_graph_probe", "tunnel_bench", "p51_descent", "maxsim_probe",
    "sp3_judge_probe", "rerank_batch_check", "bridge_rank_probe",
    "rerank_pairs_probe", "armb_write_nodes",
]


def lane_p2_code_certain(files: list[str]) -> list[str]:
    """Whole files provable dead at FILE granularity — no judgement calls.

    postgres.rs: no manifest, CI lane or script enables the `postgres` feature;
    traits.rs:1760 requires DocumentAssetStore, the file's 12 impls omit it,
    yet :1522 asserts `impl StateStore for PostgresStateStore {}` -> it cannot
    typecheck. Known broken since quality/CLEANUP.md's 2026-07-12 record.
    """
    dead = set(DEAD_EXAMPLES_NO_REFS) | set(DEAD_EXAMPLES_SPIKE)
    out = [f for f in files
           if "/examples/" in f and f.endswith(".rs")
           and not f.startswith("vendor/")
           and os.path.basename(f)[:-3] in dead]
    ps = "sovereign/crates/sovereign-store/src/postgres.rs"
    if ps in set(files):
        out.append(ps)
    return sorted(set(out))


# Fixed order. Lanes are made DISJOINT by evaluating in this sequence and
# removing anything an earlier lane already claimed, so the per-lane numbers
# sum to the campaign total with no double counting.
LANE_ORDER = [
    ("p1-bench-baselines",  lane_p1_bench_baselines),
    ("p1-dr-flights",       lane_p1_dr_flights),
    ("p0-root-junk",        lane_p0_root_junk),
    ("p2-code-certain",     lane_p2_code_certain),
]
LANES = dict(LANE_ORDER)


def compute_all() -> dict[str, tuple[list[str], int]]:
    files, claimed, out = tracked(), set(), {}
    for name, fn in LANE_ORDER:
        fs = [f for f in fn(files) if f not in claimed]
        claimed.update(fs)
        out[name] = (fs, sum(lines(f) for f in fs))
    return out


def compute(lane: str) -> tuple[list[str], int]:
    return compute_all()[lane]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--lane", choices=sorted(LANES))
    ap.add_argument("--files", action="store_true")
    ap.add_argument("--all", action="store_true")
    ap.add_argument("--verify", action="store_true")
    ap.add_argument("--freeze", action="store_true")
    a = ap.parse_args()

    if a.lane and not (a.verify or a.freeze):
        fs, n = compute(a.lane)
        if a.files:
            print("\n".join(fs))
        else:
            print(json.dumps({"lane": a.lane, "value": n, "files": len(fs)}))
        return 0

    results = compute_all()

    if a.freeze:
        os.makedirs(os.path.dirname(BASELINE), exist_ok=True)
        with open(BASELINE, "w") as fh:
            fh.write("# deletion campaign manifest — FROZEN at pre-registration.\n")
            fh.write("# Regenerate ONLY with an operator-approved scope change:\n")
            fh.write("#   scripts/deletion-manifest.py --freeze\n")
            fh.write("# lane\tlines\tfiles\n")
            for l, (fs, n) in results.items():
                fh.write(f"{l}\t{n}\t{len(fs)}\n")
        print(f"frozen -> {BASELINE}")
        return 0

    if a.verify:
        if not os.path.exists(BASELINE):
            print(f"error: no baseline at {BASELINE}; run --freeze", file=sys.stderr)
            return 3
        frozen = {}
        for ln in open(BASELINE):
            if ln.startswith("#") or not ln.strip():
                continue
            l, n, c = ln.split("\t")
            frozen[l] = (int(n), int(c))
        rc = 0
        for l, (fs, n) in results.items():
            fn, fc = frozen.get(l, (None, None))
            if fn is None:
                print(f"NEW LANE   {l}: {n} lines — not in baseline"); rc = 1
            elif n > fn:
                print(f"GREW       {l}: {n} > {fn} — the campaign is losing ground"); rc = 1
            elif n < fn:
                print(f"progress   {l}: {n} of {fn} lines remain ({fn - n} deleted)")
            else:
                print(f"unchanged  {l}: {n} lines remain")
        return rc

    print(json.dumps({l: {"lines": n, "files": len(fs)} for l, (fs, n) in results.items()}, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
