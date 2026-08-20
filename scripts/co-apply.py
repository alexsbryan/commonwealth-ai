#!/usr/bin/env python3
"""Replay the console's exported actions by driving the REAL scripts.

    scripts/co-apply.py seat-actions.jsonl            # apply
    scripts/co-apply.py seat-actions.jsonl --dry-run  # print, touch nothing
    scripts/co-apply.py --self-test                   # the lane

A `file://` page cannot write to disk, and standing up a local HTTP
endpoint to let it would be a new daemon — which the plan's restraint
list forbids. So the console EXPORTS a line per decision and this
replays them.

THE ONE RULE THIS FILE EXISTS TO KEEP: the console is a DRIVER, not a
second path. Every action here shells out to the script that already
owns the decision — `co-directive-log.sh --resolve` with the explicit
`--edited` / `--unedited` / `--no-decision` flag the operator chose,
`co-role.py <R>` for an actuation, `co-order.sh close` for a closure.
Nothing writes to a store directly. A directive resolved from the page
must be indistinguishable in `directives.jsonl` from one resolved by
hand, or the audit rail has forked and the edit-rate statistic is
measuring two different things.

ACTION SHAPES (one JSON object per line):

  {"action": "resolve", "id": "<directive id>", "final": "<text>",
   "verdict": "edited" | "unedited" | "no-decision",
   "edit_class": "scope" | "tone" | "content" | "none"}   # optional

  {"action": "actuate", "role": "R1".."R6", "input": "<text or path>",
   "gate": "draft" | "auto"}                              # gate optional

  {"action": "close", "order": "<order id>",
   "state": "landed" | "abandoned"}                       # state optional

FAILURE POLICY. Each line is applied independently and its outcome is
reported. A line that fails does NOT stop the rest — the operator made
several decisions and one bad id should not silently discard the others
— but the exit code is non-zero if ANY line failed, so a caller cannot
mistake a partial apply for a clean one (§18.2).
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DIRECTIVE_LOG = REPO / "scripts" / "co-directive-log.sh"
CO_ROLE = REPO / "scripts" / "co-role.py"
CO_ORDER = REPO / "scripts" / "co-order.sh"

VERDICT_FLAG = {"edited": "--edited", "unedited": "--unedited",
                "no-decision": "--no-decision"}


class ActionError(RuntimeError):
    """A line that cannot be applied, with the reason the operator needs."""


def build_argv(rec: dict) -> list[str]:
    """-> the argv this action becomes. Pure, so --dry-run and --self-test
    can check the COMMAND without running it — the thing most worth
    testing here is that a decision maps to the right flag."""
    action = rec.get("action")
    if action == "resolve":
        did = rec.get("id")
        if not did:
            raise ActionError("resolve without an id")
        if "final" not in rec:
            raise ActionError(f"resolve {did}: no final text")
        verdict = rec.get("verdict")
        flag = VERDICT_FLAG.get(verdict)
        if flag is None:
            # The edit verdict is the whole point of the log. Guessing it
            # would silently fabricate the statistic the operator reads.
            raise ActionError(
                f"resolve {did}: verdict must be one of "
                f"{sorted(VERDICT_FLAG)}, got {verdict!r}")
        argv = ["bash", str(DIRECTIVE_LOG), "--resolve", did,
                "--final", rec["final"], flag]
        if rec.get("edit_class"):
            argv += ["--edit-class", rec["edit_class"]]
        return argv
    if action == "actuate":
        role = (rec.get("role") or "").upper()
        if role not in {f"R{i}" for i in range(1, 7)}:
            raise ActionError(f"actuate: unknown role {rec.get('role')!r}")
        argv = [sys.executable, str(CO_ROLE), role,
                "--input", rec.get("input", "")]
        gate = rec.get("gate")
        if gate == "draft":
            argv.append("--draft")
        elif gate == "auto":
            argv.append("--auto")
        elif gate:
            raise ActionError(f"actuate {role}: unknown gate {gate!r}")
        return argv
    if action == "close":
        oid = rec.get("order")
        if not oid:
            raise ActionError("close without an order id")
        state = rec.get("state", "landed")
        if state not in ("landed", "abandoned"):
            raise ActionError(f"close {oid}: unknown state {state!r}")
        return ["bash", str(CO_ORDER), "close", oid, state]
    raise ActionError(f"unknown action {action!r}")


def label(rec: dict) -> str:
    a = rec.get("action")
    if a == "resolve":
        return f"resolve {rec.get('id')} ({rec.get('verdict')})"
    if a == "actuate":
        return f"actuate {str(rec.get('role', '')).upper()}"
    if a == "close":
        return f"close {rec.get('order')} {rec.get('state', 'landed')}"
    return f"line ({a!r})"


def apply_file(path: Path, dry_run: bool) -> int:
    if not path.exists():
        print(f"co-apply: no such file {path}", file=sys.stderr)
        return 2
    lines = [ln for ln in path.read_text(encoding="utf-8").splitlines()
             if ln.strip()]
    if not lines:
        # An empty export is not an error and not a success. Say which.
        print("co-apply: the export is empty — nothing to apply "
              "(that is a fine state, not a failure)")
        return 0
    failed = 0
    for n, line in enumerate(lines, 1):
        try:
            rec = json.loads(line)
            if not isinstance(rec, dict):
                raise ActionError("line is not a JSON object")
            argv = build_argv(rec)
        except (json.JSONDecodeError, ActionError) as e:
            # Reported with its line number, never skipped: a dropped
            # decision is an operator decision that silently did not
            # happen.
            print(f"  line {n}: FAILED — {e}")
            failed += 1
            continue
        if dry_run:
            print(f"  line {n}: would run — {' '.join(argv[:6])}"
                  f"{' …' if len(argv) > 6 else ''}")
            continue
        r = subprocess.run(argv, capture_output=True, text=True, cwd=REPO,
                           timeout=1800)
        tail = ((r.stdout or "") + (r.stderr or "")).strip().splitlines()
        detail = tail[-1][:160] if tail else ""
        if r.returncode == 0:
            print(f"  line {n}: ok — {label(rec)} — {detail}")
        else:
            print(f"  line {n}: FAILED (exit {r.returncode}) — "
                  f"{label(rec)} — {detail}")
            failed += 1
    print()
    verb = "would apply" if dry_run else "applied"
    print(f"co-apply: {len(lines) - failed}/{len(lines)} {verb}, {failed} failed")
    return 1 if failed else 0


# ---- the lane ----------------------------------------------------------
# Every check watched in BOTH directions. What matters most here is that
# an operator's edit verdict reaches co-directive-log.sh as the flag they
# chose — an apply path that quietly turned `edited` into `unedited`
# would corrupt the one statistic the M0 loop is measured by, and every
# row would still look fine.


def self_test() -> int:
    failures = []

    def check(name: str, ok: bool, detail: str = ""):
        print(f"  {'PASS' if ok else 'FAIL'}  {name}"
              + (f" — {detail}" if detail else ""))
        if not ok:
            failures.append(name)

    print("check 1 — an edit verdict becomes the flag the operator chose")
    for verdict, flag in VERDICT_FLAG.items():
        argv = build_argv({"action": "resolve", "id": "abc12345",
                           "final": "text", "verdict": verdict})
        check(f"{verdict} -> {flag}", flag in argv and "--resolve" in argv)
    check("NEGATIVE: an unknown verdict is refused, not guessed",
          _refuses({"action": "resolve", "id": "a", "final": "t",
                    "verdict": "probably-fine"}))
    check("NEGATIVE: a missing verdict is refused, not defaulted",
          _refuses({"action": "resolve", "id": "a", "final": "t"}))
    check("NEGATIVE: resolve with no final text is refused",
          _refuses({"action": "resolve", "id": "a", "verdict": "edited"}))
    check("edit_class rides along when given",
          "--edit-class" in build_argv(
              {"action": "resolve", "id": "a", "final": "t",
               "verdict": "edited", "edit_class": "scope"}))

    print("check 2 — every action drives the REAL script, never a store")
    argv = build_argv({"action": "resolve", "id": "a", "final": "t",
                       "verdict": "unedited"})
    check("resolve drives co-directive-log.sh",
          argv[1].endswith("co-directive-log.sh"))
    argv = build_argv({"action": "actuate", "role": "r5", "input": "x"})
    check("actuate drives co-role.py (and upper-cases the role)",
          argv[1].endswith("co-role.py") and "R5" in argv)
    argv = build_argv({"action": "close", "order": "ord-1"})
    check("close drives co-order.sh, defaulting to landed",
          argv[1].endswith("co-order.sh") and argv[-1] == "landed")
    check("NEGATIVE: an unknown role is refused",
          _refuses({"action": "actuate", "role": "R9", "input": "x"}))
    check("NEGATIVE: an unknown action is refused",
          _refuses({"action": "retire-everything"}))
    check("NEGATIVE: an unknown close state is refused",
          _refuses({"action": "close", "order": "o", "state": "deleted"}))

    print("check 3 — a bad line is reported and the good ones still apply")
    with tempfile.TemporaryDirectory(prefix="co-apply-selftest-") as tmp:
        p = Path(tmp) / "actions.jsonl"
        p.write_text(
            json.dumps({"action": "resolve", "id": "aaa11111",
                        "final": "t", "verdict": "edited"}) + "\n"
            + "{not json\n"
            + json.dumps({"action": "actuate", "role": "R3",
                          "input": "financial-corpora"}) + "\n",
            encoding="utf-8")
        code = apply_file(p, dry_run=True)
        check("a malformed line makes the exit code non-zero", code == 1)
        empty = Path(tmp) / "empty.jsonl"
        empty.write_text("", encoding="utf-8")
        check("NEGATIVE: an empty export is 0, not a fabricated failure",
              apply_file(empty, dry_run=True) == 0)

    print()
    if failures:
        print(f"self-test FAILED — {len(failures)} check(s): "
              + "; ".join(failures))
        return 1
    print("self-test PASSED — 3 checks, both directions each.")
    return 0


def _refuses(rec: dict) -> bool:
    try:
        build_argv(rec)
    except ActionError:
        return True
    return False


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(
        prog="co-apply.py",
        description="Replay the seat console's exported actions.")
    ap.add_argument("file", nargs="?", help="seat-actions.jsonl")
    ap.add_argument("--dry-run", action="store_true",
                    help="print what would run; touch nothing")
    ap.add_argument("--self-test", action="store_true",
                    help="run the lane and exit")
    a = ap.parse_args(argv)
    if a.self_test:
        return self_test()
    if not a.file:
        ap.error("a file, or --self-test, is required")
    return apply_file(Path(a.file), a.dry_run)


if __name__ == "__main__":
    raise SystemExit(main())
