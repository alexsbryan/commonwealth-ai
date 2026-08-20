#!/usr/bin/env python3
"""Corpus-scale arm driver (order deep-research-t6a phase 1c, pre-registered
in research/deep-research/adversarial/pre-registration.md).

Flies the FROZEN BANK (bank/seeds.md, 12 v0 seeds + bank/v1/seeds.md) through
the shipped CLI in corpus mode:

  thin: --backend auto --search-source corpus --corpora wikipedia
        --search 40 --fetch 60 --max-rounds 6
        (the pre-registered phase-1c flags, verbatim — the estate-at-its-FLOOR
        leg: wikipedia only, zero acquired-estate cache)

  warm: --backend auto --search-source corpus
        --corpora wikipedia,<warm-corpus-id>
        --search 40 --fetch 60 --max-rounds 6
        (the pre-registered warm-estate bracket (seat steer e8bdf4e8): the
        same flags, ONE variable — the acquired-estate cache is added to the
        corpus set. The warm corpus is assembled from the demo13 web runs'
        fetched pages and ingested via `svrn corpus ingest` — the amendment is
        journaled in the phase-1c Execution record BEFORE the warm leg flies,
        per §18.6.)

Thresholds untouched, named: code-set K=3, eps-quota 0.1, evidence window 20
chunks — all hardcoded CLI defaults; retuning is a phase-2 instrument change.

Questions are EXTRACTED from the frozen bank files (the run-arms.sh regex) —
the driver never hardcodes a question, so the flights can never drift from
the mint. Run dirs: <run-root>/<arm>/<id>/dr-<ts>/ (the CLI stamps the dr-*
subdir — the score-arms.py loop shape). Completed flights (manifest terminal
done/done-partial/done-full) are skipped on resume.

Run under systemd-run — NEVER as a bare harness background task (the harness
reaper kills those on this host):

  systemd-run --unit=t6a-corpus-scale-probe.service --collect \
      python3 fly-corpus-scale.py --seeds seed-01,seed-02,seed-03 --arm thin

Writes per flight: <arm>/<id>.console.log + <arm>/<id>.state.json (wall secs,
terminal state). Writes <run-root>/pairs.json (id, question) for the scorer.
"""
import argparse
import json
import re
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).parent
DR_ROOT = HERE.parent.parent          # research/deep-research
BANK = DR_ROOT / "bank"
ARMS = DR_ROOT / "arms"

THIN_CORPORA = "wikipedia"
BASE_FLAGS = ["--backend", "auto", "--search-source", "corpus",
              "--search", "40", "--fetch", "60", "--max-rounds", "6"]


def extract_v0_questions():
    text = (BANK / "seeds.md").read_text(encoding="utf-8")
    qs = re.findall(r'\*\*Question:\*\* "((?:[^"]|\\")*)"', text, re.S)
    qs = [" ".join(q.split()) for q in qs]
    assert len(qs) == 12, f"expected 12 v0 questions, found {len(qs)}"
    return qs


def extract_v1_question():
    text = (BANK / "v1" / "seeds.md").read_text(encoding="utf-8")
    m = re.search(r'## The question\s*\n+"((?:[^"]|\\")*)"', text, re.S)
    assert m, "v1 question not found under ## The question"
    return " ".join(m.group(1).split())


def pairs_for(ids):
    v0 = extract_v0_questions()
    v1q = extract_v1_question()
    pairs = []
    for pid in ids:
        q = v1q if pid == "v1" else v0[int(pid.split("-")[1]) - 1]
        pairs.append({"id": pid, "question": q})
    return pairs


def manifest_of(run_dir: Path):
    cands = sorted(run_dir.glob("dr-*/manifest.json"),
                   key=lambda p: p.stat().st_mtime)
    return cands[-1] if cands else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="sovereign")
    ap.add_argument("--arm", choices=["thin", "warm"], required=True)
    ap.add_argument("--corpora", default=None,
                    help="override the corpus set (thin default: wikipedia; "
                         "warm default: wikipedia,<warm-id> from --warm-corpus)")
    ap.add_argument("--warm-corpus", default=None,
                    help="warm-estate corpus id appended to wikipedia for the warm arm")
    ap.add_argument("--seeds", required=True,
                    help="comma list of seed ids (seed-01..seed-12, v1) or 'all'")
    ap.add_argument("--run-root",
                    default=str(HERE / "runs" / "corpus-scale"))
    args = ap.parse_args()

    run_root = Path(args.run_root)
    if args.seeds == "all":
        ids = [f"seed-{i:02d}" for i in range(1, 13)] + ["v1"]
    else:
        ids = [s.strip() for s in args.seeds.split(",")]
    pairs = pairs_for(ids)

    if args.corpora:
        corpora = args.corpora
    elif args.arm == "warm":
        assert args.warm_corpus, "warm arm needs --warm-corpus (or --corpora)"
        corpora = f"wikipedia,{args.warm_corpus}"
    else:
        corpora = THIN_CORPORA
    flags = BASE_FLAGS + ["--corpora", corpora]

    pairs_path = run_root / "pairs.json"
    pairs_path.parent.mkdir(parents=True, exist_ok=True)
    pairs_path.write_text(json.dumps(pairs, indent=2))

    arm_root = run_root / args.arm
    arm_root.mkdir(parents=True, exist_ok=True)
    failures = []

    print(f"[corpus-scale:{args.arm}] corpora={corpora} flags={flags} "
          f"seeds={ids} run-root={run_root}", flush=True)
    for p in pairs:
        pid = p["id"]
        run_dir = arm_root / pid
        log_path = arm_root / f"{pid}.console.log"
        state_path = arm_root / f"{pid}.state.json"
        run_dir.mkdir(parents=True, exist_ok=True)
        mp = manifest_of(run_dir)
        if mp is not None:
            try:
                m = json.load(open(mp, encoding="utf-8"))
                if m.get("terminal_state") in ("done", "done-partial", "done-full"):
                    print(f"[{args.arm}] {pid} already complete "
                          f"({m.get('terminal_state')}, {mp.parent.name}) — skipped",
                          flush=True)
                    state_path.write_text(json.dumps(
                        {"id": pid, "skipped": True, "terminal": m.get("terminal_state"),
                         "run": str(mp.parent)}, indent=2))
                    continue
            except Exception:
                pass
        cmd = [args.bin, "deep-research", p["question"], *flags,
               "--run-dir", str(run_dir)]
        t0 = time.time()
        print(f"[{args.arm}] {pid} start — {cmd[1]} …", flush=True)
        with open(log_path, "w", encoding="utf-8") as logf:
            proc = subprocess.run(cmd, stdout=logf, stderr=subprocess.STDOUT)
        wall = time.time() - t0
        mp = manifest_of(run_dir)
        state = "?"
        if mp is not None:
            try:
                m = json.load(open(mp, encoding="utf-8"))
                state = m.get("terminal_state")
            except Exception:
                pass
        ok = proc.returncode == 0 and state in ("done", "done-partial", "done-full")
        print(f"[{args.arm}] {pid} exit={proc.returncode} terminal={state} "
              f"wall={wall:.0f}s {'OK' if ok else 'FAIL'}", flush=True)
        state_path.write_text(json.dumps(
            {"id": pid, "skipped": False, "exit": proc.returncode,
             "terminal": state, "wall_s": round(wall, 1),
             "run": str(mp.parent) if mp else None}, indent=2))
        if not ok:
            failures.append((args.arm, pid, proc.returncode, state))

    print(f"[corpus-scale:{args.arm}] pairs written: {pairs_path}", flush=True)
    if failures:
        print("FAILURES:", failures, flush=True)
        return 1
    print("ALL FLIGHTS OK", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
