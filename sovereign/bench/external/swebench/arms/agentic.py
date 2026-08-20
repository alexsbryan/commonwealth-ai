#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# ///
# SPDX-License-Identifier: AGPL-3.0-or-later
"""The agent-harness arms: `comaintainer` (order + subagents) and `flat` (control).

Both arms drive the SAME agent binary on the SAME worktree with the SAME
task constraints. The only variable is the framing:

  flat          — the issue text plus the shared constraints. One session,
                  no order, no delegation. This is the control.
  comaintainer  — the issue rendered as a work ORDER in the schema from
                  docs/COMAINTAINER.md §4.3 (objective at altitude,
                  falsifiable done-when, not-worth-continuing-if, scope,
                  budget, seams), with the seat instructed to delegate to
                  subagents and verify their claims before accepting.

`comaintainer − flat` is therefore the seat protocol's value, with the
engine and the tools held fixed. `flat − mini-swe-agent` is what a fuller
agent harness buys over the published minimal scaffold. Neither delta is
readable if the arms differ in more than one thing at a time, which is
why the constraints block below is shared verbatim.

    ./agentic.py --arm flat         --engine claude --model claude-sonnet-5
    ./agentic.py --arm comaintainer --engine claude --model claude-sonnet-5 --limit 10
    ./agentic.py --arm comaintainer --engine pi     --model Qwen3.8-27B-UD-Q6_K_XL

`--model` is required: a prediction that cannot name the engine that
produced it is not evidence, and `claude -p` without it resolves to
whatever the CLI defaults to that day.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lib import ROOT, Instance, extract_patch, load_instances, materialize, write_prediction  # noqa: E402

# Prompts live in prompts/ so the Rust arms (`agent-bench swebench`,
# which drives `native` and `bare-metal`) read the SAME text. One
# decider, one name — a template forked between two languages is a
# silent confound in every delta this bench reports.
PROMPTS = Path(__file__).resolve().parent.parent / "prompts"
CONSTRAINTS = (PROMPTS / "constraints.md").read_text().strip()
FLAT = (PROMPTS / "flat.md").read_text()
ORDER = (PROMPTS / "order.md").read_text()


def build_prompt(arm: str, inst: Instance, workdir: Path, budget_note: str) -> str:
    # The verify command is per-instance (its image and mount path), so
    # the shared constraints text carries a placeholder rather than a
    # second copy of the command in each arm.
    constraints = CONSTRAINTS.replace("{verify_cmd}", inst.verify_cmd(workdir))
    common = dict(
        repo=inst.repo,
        commit=inst.base_commit[:12],
        issue=inst.problem_statement.strip(),
        constraints=constraints,
    )
    if arm == "flat":
        return FLAT.format(**common)
    return ORDER.format(
        instance_id=inst.instance_id,
        workdir=workdir,
        budget_note=budget_note,
        **common,
    )


def engine_argv(engine: str, prompt: str, workdir: Path, model: str | None) -> list[str]:
    """Headless invocation per engine. Both run with cwd=workdir."""
    if engine == "claude":
        argv = ["claude", "-p", prompt, "--permission-mode", "acceptEdits"]
        if model:
            argv += ["--model", model]
        return argv
    if engine == "pi":
        # pi drives the local daemon; the sovereign-hooks extension supplies
        # the same tool surface the seat uses in-repo.
        argv = ["pi", "-p", prompt]
        if model:
            argv += ["--model", model]
        return argv
    raise SystemExit(f"unknown engine {engine!r}")


def run_one(
    inst: Instance, arm: str, engine: str, model: str | None, timeout: int, budget_note: str
) -> dict:
    workdir = materialize(inst, arm)
    prompt = build_prompt(arm, inst, workdir, budget_note)
    argv = engine_argv(engine, prompt, workdir, model)

    t0 = time.monotonic()
    status, stderr_tail = "ok", ""
    try:
        r = subprocess.run(
            argv, cwd=workdir, capture_output=True, text=True, timeout=timeout
        )
        if r.returncode != 0:
            status = f"exit-{r.returncode}"
            stderr_tail = (r.stderr or "")[-1500:]
    except subprocess.TimeoutExpired:
        status = "timeout"
    wall = time.monotonic() - t0

    patch = extract_patch(workdir)
    return {
        "instance_id": inst.instance_id,
        "arm": arm,
        "engine": engine,
        "model": model or "default",
        "status": status,
        "empty_patch": not patch.strip(),
        "patch_bytes": len(patch),
        "wall_seconds": round(wall, 1),
        "stderr_tail": stderr_tail,
        "_patch": patch,
    }


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--arm", choices=["flat", "comaintainer"], required=True)
    p.add_argument("--engine", choices=["claude", "pi"], default="claude")
    # A prediction that cannot name the engine that produced it is not
    # evidence. `claude -p` without --model resolves to whatever the CLI
    # defaults to that day, so an unlabelled run is unreproducible.
    p.add_argument(
        "--model",
        default=None,
        help="engine-specific model handle; REQUIRED unless --allow-unlabelled",
    )
    p.add_argument(
        "--allow-unlabelled",
        action="store_true",
        help="permit a run whose predictions cannot name their model",
    )
    p.add_argument("--limit", type=int, default=0)
    p.add_argument("--only", default=None, help="comma-separated instance_ids")
    p.add_argument("--timeout", type=int, default=1800, help="per-instance wall cap, seconds")
    p.add_argument("--resume", action="store_true", help="skip instances that already have a prediction")
    args = p.parse_args()

    if args.model is None and not args.allow_unlabelled:
        raise SystemExit(
            "refusing to run unlabelled: pass --model <id> so predictions "
            "record which engine produced them, or --allow-unlabelled to override"
        )

    instances = load_instances()
    if args.only:
        keep = {s.strip() for s in args.only.split(",")}
        instances = [i for i in instances if i.instance_id in keep]
    if args.limit:
        instances = instances[: args.limit]

    budget_note = f"{args.timeout // 60} minutes of wall clock. Stop and report at the cap."
    log_path = ROOT / "preds" / f"{args.arm}.runlog.jsonl"
    log_path.parent.mkdir(parents=True, exist_ok=True)

    done = 0
    for i, inst in enumerate(instances, 1):
        pred = ROOT / "preds" / args.arm / f"{inst.instance_id}.json"
        if args.resume and pred.exists():
            print(f"[{i}/{len(instances)}] {inst.instance_id} — already done, skipping")
            continue
        print(f"[{i}/{len(instances)}] {inst.instance_id} ({inst.difficulty}) …", flush=True)
        rec = run_one(inst, args.arm, args.engine, args.model, args.timeout, budget_note)
        write_prediction(args.arm, inst, rec.pop("_patch"), rec["model"])
        with log_path.open("a") as fh:
            fh.write(json.dumps(rec) + "\n")
        flag = " EMPTY" if rec["empty_patch"] else ""
        print(f"    {rec['status']} · {rec['wall_seconds']}s · {rec['patch_bytes']}B{flag}")
        done += 1

    print(f"\n{done} instances run · predictions in {ROOT / 'preds' / args.arm}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
