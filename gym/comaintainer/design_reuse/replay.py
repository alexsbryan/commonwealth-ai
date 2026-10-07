#!/usr/bin/env python3
"""Three-condition design-reuse replay: the design-reuse lane's runner.

Conditions (identical scoring; deliberately different inputs):
  A  unaided      requirement + constraints + output contract
  B  dossier      A + candidate existing surfaces found at BASE
  C  protocol     B + binding design protocol (grounded evidence, explicit
                  new_components, stated limits) enforced by the schema
  D  verifier     C's dossier + per-candidate checkable observations graded
                  by the controller; contradicted answers and inconsistent
                  dispositions are refused (loop.py)

Every call persists the full prompt, raw completion and served model under
`runs/<stamp>/`, so `--rescore <dir>` reproduces every metric with ZERO
model calls (the score.py instrument rule). Model failure is its own
outcome — could-not-judge — never folded into a wrong design.

    python3 replay.py --dry-run
    python3 replay.py --limit 3                      # dev cases, all conditions
    python3 replay.py --case contrast-core-read-port --conditions A,B
    python3 replay.py --rescore runs/<stamp>
"""
from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import sys
import time
import urllib.error
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))

import common as C  # noqa: E402
import contracts as H  # noqa: E402
import frozen as F  # noqa: E402
import historical as T  # noqa: E402
import loop as D  # noqa: E402
from markers import SEAT_ENGINE_OF_RECORD  # noqa: E402
from score import EngineDrift, call_daemon  # noqa: E402  (one HTTP client)

RUNS = HERE / "runs"
CONDITIONS = ("A", "B", "C", "D")


def ask(prompt: str, schema: dict, pin: str, max_tokens: int, tries: int):
    """-> (text, model) on success, (None, reason) otherwise. Retries only
    the transient 503 (a full queue); a permanent refusal and engine drift
    return immediately — a gate against the wrong engine is worse than red."""
    for attempt in range(tries):
        try:
            text, model = call_daemon(prompt, 240.0, max_tokens, schema=schema,
                                      schema_name="proposal", pin=pin)
            return text, model
        except EngineDrift as exc:
            return None, f"engine-drift: {exc}"
        except urllib.error.HTTPError as exc:
            try:
                body = exc.read().decode("utf-8", "replace")
            except Exception:  # noqa: BLE001 — body is best-effort
                body = ""
            if exc.code == 503 and "advertises model" not in body and attempt + 1 < tries:
                time.sleep(10)
                continue
            return None, f"http{exc.code}: {body[:200]}"
        except Exception as exc:  # noqa: BLE001 — transport failure is a verdict
            return None, f"{type(exc).__name__}: {exc}"
    return None, "retries exhausted"


def run_one(case: dict, condition: str, pin: str, max_tokens: int, tries: int) -> dict:
    base_prompt = C.build_prompt(case, condition)
    base_prompt += (f"\nBASE: {case['provenance']['base_sha']}\n"
                    "You may list source paths, read at most 120 lines, or search a literal query. "
                    "All lookups use BASE; no live checkout or future revision is accessible. "
                    f"Budget: {T.MAX_TOOL_REQUESTS} lookups, {T.MAX_GENERATIONS} generations. "
                    "Emit a read/search/list action, or propose with the required proposal object.")
    proposal_arm = {"type": "object", "properties": {
        "action": {"const": "propose"}, "proposal": C.schema_for(case, condition)},
        "required": ["action", "proposal"], "additionalProperties": False}
    transcript, used = [], 0
    row = {"case": case["id"], "condition": condition, "prompt": base_prompt,
           "raw": None, "model": None, "parsed": None, "verdict": "never-ran",
           "reason": None, "metrics": None, "steps": transcript,
           "measurement_kind": "historical_source_selection", "semantic_verdict": "could-not-judge"}
    for _ in range(T.MAX_GENERATIONS):
        arms = [proposal_arm] + (T.schema()["oneOf"] if used < T.MAX_TOOL_REQUESTS else [])
        schema = {"oneOf": arms}
        feedback = [{k: e[k] for k in ("action", "observation", "refused") if k in e} for e in transcript]
        prompt = base_prompt + "\nPrevious actions and host observations:\n" + json.dumps(feedback)
        if len(prompt) > 60000:
            row.update(verdict="could-not-judge", reason="source context budget exceeded; no silent truncation")
            return row
        text, model = ask(prompt, schema, pin, max_tokens, tries)
        if text is None:
            row.update(verdict="could-not-judge", reason=model)
            return row
        row.update(raw=text, model=model)
        event = {"prompt": prompt, "schema": schema, "raw": text, "model": model}
        transcript.append(event)
        try:
            action = json.loads(text)
            event["action"] = action
            errors = H.problems(action, schema)
            if errors:
                raise ValueError("; ".join(errors))
            if action["action"] == "propose":
                row.update(parsed=action["proposal"], verdict="parsed")
                row["metrics"] = C.score_proposal(case, row["parsed"], condition)
                return row
            used += 1
            event["observation"] = T.lookup(case, action)
        except (ValueError, OSError, json.JSONDecodeError) as exc:
            event["refused"] = str(exc)
    row.update(verdict="failed", reason="generation budget exhausted without a valid proposal")
    return row


def progress_line(row: dict) -> str:
    if row["verdict"] != "parsed":
        return f"[{row['case']}/{row['condition']}] {row['verdict']}: {row['reason']}"
    m = row["metrics"]
    extra = ""
    if row["condition"] == "D":
        extra = (f" refused={row.get('refusals', 0)} wrongobs={row.get('wrong_observations', 0)}"
                 f" attempts={row.get('attempts', 0)}")
    return (f"[{row['case']}/{row['condition']}] parsed disp={m['disposition']} "
            f"home={int(m['home_match'])} path={int(m['path_match'])} "
            f"new={m['new_components']} baits={len(m['bait_hits'])}{extra}")


def select_cases(cases, args) -> list[dict]:
    splits = C.assign_splits(cases)
    if args.case:
        picked = [c for c in cases if c["id"] in set(args.case)]
        if not args.include_holdout and any(splits[c["id"]] == "holdout" for c in picked):
            raise ValueError("holdout selection requires explicit --include-holdout")
    elif args.include_holdout:
        picked = cases
    else:
        picked = [c for c in cases if splits[c["id"]] == "dev"]
    return picked[: args.limit]


def rescore_dir(run_dir: Path) -> dict:
    """Recompute metrics from persisted raw completions; never calls a model."""
    inputs = F.read(run_dir)
    cases = {c["id"]: c for c in inputs["cases"]}
    rows = []
    for line in (run_dir / "calls.jsonl").read_text().splitlines():
        if not line.strip():
            continue
        row = json.loads(line)
        row["metrics"] = None
        row["parsed"] = None
        if row.get("raw") and row["verdict"] == "parsed":
            case = cases[row["case"]]
            try:
                raw = json.loads(row["raw"])
                if row["condition"] == "D":
                    errors = H.problems(raw, D.schema_d(case, inputs["settings"]["d_restate"]))
                    parsed = D.compose(case, raw)
                else:
                    parsed = raw.get("proposal") if isinstance(raw, dict) and raw.get("action") == "propose" else None
                    errors = H.problems(parsed, C.schema_for(case, row["condition"]))
                if errors:
                    raise ValueError("; ".join(errors))
                row["parsed"] = parsed
                row["metrics"] = C.score_proposal(case, parsed, row["condition"])
            except (ValueError, json.JSONDecodeError) as exc:
                row.update(verdict="failed", reason=f"stored raw contract violation: {exc}")
        rows.append(row)
    agg = C.aggregate(rows)
    summary = {"stamp": json.loads((run_dir / "meta.json").read_text())["stamp"],
               "rescore": True, "aggregate": agg,
               "rows": [{k: r.get(k) for k in ("case", "condition", "verdict",
                                               "reason", "metrics", "refusals",
                                               "wrong_observations", "attempts",
                                               "finished")} for r in rows]}
    (run_dir / "rescore.json").write_text(json.dumps(summary, indent=2) + "\n")
    return summary


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--limit", type=int, default=3, help="cases per condition")
    ap.add_argument("--conditions", default="A,B,C")
    ap.add_argument("--case", action="append", default=[])
    ap.add_argument("--include-holdout", action="store_true")
    ap.add_argument("--pin", default=SEAT_ENGINE_OF_RECORD)
    ap.add_argument("--max-tokens", type=int, default=700)
    ap.add_argument("--tries", type=int, default=3)
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--no-restate", action="store_true",
                    help="condition D v2.2: facts are canonical state, not restated")
    ap.add_argument("--rescore", type=Path)
    ap.add_argument("--legacy-label-ablation", action="store_true",
                    help="explicitly permit D's supplied-label consistency experiment")
    args = ap.parse_args()

    if args.rescore:
        try:
            summary = rescore_dir(args.rescore)
        except ValueError as exc:
            print(str(exc), file=sys.stderr)
            return 3
        print(C.render_aggregate(summary["aggregate"]))
        if any(r["verdict"] in ("failed", "malformed") for r in summary["rows"]):
            return 1
        if any(r["verdict"] == "could-not-judge" for r in summary["rows"]):
            return 3
        return 0

    cases = C.load_cases()
    if not cases:
        print("replay: EMPTY BANK — nothing to replay", file=sys.stderr)
        return 4
    conditions = [c for c in args.conditions.split(",") if c]
    bad = [c for c in conditions if c not in CONDITIONS]
    if bad:
        print(f"replay: unknown condition(s) {bad}", file=sys.stderr)
        return 2
    if "D" in conditions and not args.legacy_label_ablation:
        print("D requires --legacy-label-ablation; it does not independently verify design adequacy", file=sys.stderr)
        return 2
    defects = [bad for case in cases for bad in C.verify_case(case)]
    if defects:
        print("\n".join(defects), file=sys.stderr)
        return 1
    try:
        selected = select_cases(cases, args)
    except ValueError as exc:
        print(str(exc), file=sys.stderr)
        return 2
    if not selected:
        print("replay: no cases selected", file=sys.stderr)
        return 4

    if args.dry_run:
        for case in selected:
            for cond in conditions:
                print("=" * 72)
                print(f"[{case['id']} / {cond}]")
                print(C.build_prompt(case, cond))
        return 0

    stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    run_dir = RUNS / stamp
    run_dir.mkdir(parents=True)
    meta = {
        "stamp": stamp,
        "bank_sha256": hashlib.sha256(C.CASES.read_bytes()).hexdigest(),
        "pin": args.pin, "conditions": conditions,
        "cases": [c["id"] for c in selected],
        "splits": {c["id"]: C.assign_splits(cases)[c["id"]] for c in selected},
        "include_holdout": args.include_holdout,
        "d_restate": not args.no_restate,
        "generation": {"temperature": 0, "max_tokens": args.max_tokens,
                       "timeout_s": 240, "tries": args.tries, "retry_sleep_s": 10,
                       "tool_requests": T.MAX_TOOL_REQUESTS, "generations": T.MAX_GENERATIONS},
        "served_engine_identity_verified": False,
    }
    inputs = F.record(run_dir, cases, meta)
    selected = [c for c in inputs["cases"] if c["id"] in {s["id"] for s in selected}]
    (run_dir / "meta.json").write_text(json.dumps(meta, indent=2) + "\n")

    rows = []
    with (run_dir / "calls.jsonl").open("w") as log:
        for case in selected:
            for cond in conditions:
                if cond == "D":
                    row = D.run_case_loop(
                        case, lambda p, s: ask(p, s, args.pin, args.max_tokens, args.tries),
                        restate=not args.no_restate)
                    row["measurement_kind"] = "legacy_supplied_label_ablation"
                    row["semantic_verdict"] = "could-not-judge"
                else:
                    row = run_one(case, cond, args.pin, args.max_tokens, args.tries)
                rows.append(row)
                log.write(json.dumps(row) + "\n")
                log.flush()
                print(progress_line(row))

    summary = {"stamp": stamp, "pin": args.pin, "conditions": conditions,
               "aggregate": C.aggregate(rows),
               "rows": [{k: r.get(k) for k in ("case", "condition", "verdict",
                                               "reason", "metrics", "refusals",
                                               "wrong_observations", "attempts",
                                               "finished")} for r in rows]}
    (run_dir / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print()
    print(C.render_aggregate(summary["aggregate"]))
    print(f"\nrun: {run_dir}")
    if any(r["verdict"] in ("failed", "malformed") for r in rows):
        return 1
    if any(r["verdict"] == "could-not-judge" for r in rows):
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
