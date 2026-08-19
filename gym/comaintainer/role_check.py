#!/usr/bin/env python3
"""Six roles, six lean checks. Does the local stack fulfil the seat's jobs?

    python3 gym/comaintainer/role_check.py [--engine daemon|claude] [--only R4]

Operator framing 2026-08-19: "We don't need a measurement soup. We need roles
fulfilled." The roles, verbatim:

  R1  convey a plan of work            -> intake
  R2  digest a plan into campaigns     -> campaign with bars
  R3  measure progress                 -> coverage against declared bars
  R4  independently verify claimed work-> landing verdict, aligned to principles
  R5  keep a campaign on track         -> off-scope noise becomes a backlog item
  R6  cull the backlog                 -> dead items proposed for retirement

The design rule that keeps this lean: EVERY ROLE'S OUTPUT ALREADY HAS A
CONSUMER IN THIS REPO. The check is whether the consumer accepts it. No judge,
no golden bank, no kappa — the machinery is the verifier, and it either parses
the output or it does not. R3 needs no model at all; it is code, and it is
checked here only so the role list stays honest about what is already done.

A role that cannot be checked mechanically says so and returns could-not-judge.
Nothing here defaults to a pass.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import subprocess
import sys
import tempfile
import time
import urllib.error
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
sys.path.insert(0, str(HERE))
import markers as M                      # noqa: E402
from score import EngineDrift, call_daemon, extract_verdict   # noqa: E402

RESULTS: list[tuple[str, str, str, str]] = []   # (id, role, verdict, detail)


def load_py(path: Path, name: str):
    sp = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(sp)
    sys.modules[name] = m
    try:
        sp.loader.exec_module(m)
    except SystemExit:
        pass
    return m


def ask(prompt: str, schema: dict | None = None,
        pin: str = M.SEAT_ENGINE_OF_RECORD,
        max_tokens=700, tries=8) -> tuple[str | None, str]:
    """-> (completion | None, served_model_id | reason).

    A retry wrapper over `score.call_daemon`, not a second HTTP client.
    It used to build its own request and send `model="commonwealth/primary"`
    — the ALIAS, which follows config.toml — and never looked at what
    answered, so this gate could score six roles against a model nobody
    chose. The pin and the drift check now come from `call_daemon`
    (§10.6: one implementation); what stays here is the retry policy,
    which belongs to the gate that needs it and not to every seat caller.

    `EngineDrift` is deliberately NOT retried and not swallowed: it is a
    permanent condition, and a gate that reports PASS against the wrong
    engine is worse than one that fails.
    """
    for _ in range(tries):
        try:
            return call_daemon(prompt, 240.0, max_tokens, schema=schema,
                               schema_name="out", pin=pin)
        except EngineDrift:
            raise
        except urllib.error.HTTPError as e:
            # 503 is overloaded on this daemon: a full slot queue (wait)
            # and an unloadable model (never succeeds). Measured
            # 2026-08-19 — pinning a model no node advertises returns 503,
            # so a blind retry here burns tries*15s before reporting a
            # condition that was permanent on the first call.
            if e.code == 503:
                try:
                    detail = e.read().decode("utf-8", "replace")
                except Exception:  # noqa: BLE001 — body is best-effort
                    detail = ""
                if "advertises model" in detail:
                    return None, f"unservable pin {pin!r}: {detail[:160]}"
                time.sleep(15)
                continue
            return None, f"http{e.code}"
        except Exception:
            time.sleep(8)
    return None, "queue_full"


def jparse(raw: str | None) -> dict | None:
    """Thinking models emit the schema-forced JSON then close the channel."""
    if not raw:
        return None
    try:
        return json.loads(raw.split("</think>")[0].strip())
    except Exception:
        return None


def record(rid, role, ok, detail):
    RESULTS.append((rid, role, "PASS" if ok is True else
                    ("FAIL" if ok is False else "COULD-NOT-JUDGE"), detail))
    print(f"  {rid}  {RESULTS[-1][2]:16} {detail}")


# ---------------------------------------------------------------- R1 intake
INTENT = """I want the sec-filings thing finished. Right now if you ask about a
segment like Mac revenue it answers with the consolidated number which is just
wrong, it should say it can't answer that. Also the e2e keeps failing on some
newline thing. Don't spend more than a couple of sessions on it."""

R1_SCHEMA = {"type": "object", "additionalProperties": False,
             "required": ["objective", "done_when", "not_worth_continuing_if", "scope"],
             "properties": {"objective": {"type": "string", "minLength": 20},
                            "done_when": {"type": "string", "minLength": 20},
                            "not_worth_continuing_if": {"type": "string", "minLength": 15},
                            "scope": {"type": "array", "maxItems": 6,
                                      "items": {"type": "string"}}}}


def check_r1():
    raw, model = ask(
        "Turn this operator intent into a work order. Fill every field.\n"
        "done_when must be FALSIFIABLE — a condition someone could check and "
        "get a yes or no, not a description of effort.\n"
        "not_worth_continuing_if must name a condition that would make the work "
        "pointless.\nscope: the files or symbols the work touches.\n\n"
        f"INTENT:\n{INTENT}", R1_SCHEMA)
    d = jparse(raw)
    if not d:
        return record("R1", "convey a plan", None, f"no parseable order ({model})")
    dw = d["done_when"].lower()
    # falsifiable = names a checkable state, not an activity
    falsifiable = any(t in dw for t in ("refus", "answer", "exit 0", "pass", "green",
                                        "returns", "no longer", "zero", "0 ", "==",
                                        "instead of", "rather than"))
    echo = d["objective"].strip().lower()[:40] in INTENT.lower()
    ok = falsifiable and not echo
    record("R1", "convey a plan", ok,
           f"done_when={'falsifiable' if falsifiable else 'NOT falsifiable'}"
           f"{', echoes intent verbatim' if echo else ''} | {d['done_when'][:60]}")


# -------------------------------------------------------------- R2 campaign
def check_r2():
    raw, model = ask(
        "Draft flight rules for this initiative as JSON.\n"
        "Return at most 4 bars. Each bar: id (kebab-case), one_line (what is "
        "true when it is met), derives_from (where the requirement comes from).\n"
        "Do NOT invent numeric thresholds — a threshold nobody measured is not "
        "a bar.\n\nINITIATIVE: financial corpora answer figure questions with "
        "their basis, or refuse and name what IS available.",
        {"type": "object", "additionalProperties": False, "required": ["bars"],
         "properties": {"bars": {"type": "array", "minItems": 2, "maxItems": 4,
             "items": {"type": "object", "additionalProperties": False,
                       "required": ["id", "one_line", "derives_from"],
                       "properties": {"id": {"type": "string"},
                                      "one_line": {"type": "string", "minLength": 15},
                                      "derives_from": {"type": "string"}}}}}})
    d = jparse(raw)
    if not d or not d.get("bars"):
        return record("R2", "digest into campaigns", None, f"no bars produced ({model})")
    toml = ('id = "rolecheck"\nobjective = "x"\nspec = "s"\n'
            'declared = "2026-08-19"\nstatus = "active"\n')
    for b in d["bars"]:
        toml += (f'\n[[bar]]\nid = {json.dumps(b["id"])}\n'
                 f'one_line = {json.dumps(b["one_line"])}\n'
                 f'derives_from = {json.dumps(b["derives_from"])}\nstatus = "open"\n')
    lin = load_py(REPO / "scripts" / "co-lineage.py", "col")
    with tempfile.NamedTemporaryFile("w", suffix=".toml", delete=False) as fh:
        fh.write(toml); p = Path(fh.name)
    try:
        camp = lin.load_campaign_file(p)          # THE CONSUMER
        record("R2", "digest into campaigns", True,
               f"{len(camp.bars)} bars accepted by the real loader")
    except Exception as e:
        record("R2", "digest into campaigns", False, f"loader rejected: {e}")
    finally:
        p.unlink(missing_ok=True)


# -------------------------------------------------------------- R3 progress
def check_r3():
    r = subprocess.run([sys.executable, str(REPO / "scripts" / "co-lineage.py"),
                        "coverage", "financial-corpora"],
                       capture_output=True, text=True, cwd=REPO, timeout=120)
    out = r.stdout
    ok = r.returncode == 0 and ("bar" in out.lower() or "uncovered" in out.lower())
    record("R3", "measure progress", ok,
           "code-only, no model — " + (f"{len(out.splitlines())} lines of coverage"
                                       if ok else f"exit {r.returncode}"))


# -------------------------------------------------------------- R4 verifier
CLEAN = """=== COMMIT abc1234 ===
fix(sec): the period basis travels with the figure

=== DIFF ===
+    let basis = fact.period_basis()?;
+    Ok(Answer { value: fact.value, basis: Some(basis) })
-    Ok(Answer { value: fact.value, basis: None })
=== GATE ARTIFACTS ===
--- sovereign-test/latest summary: pass: 8431 fail: 0
--- sovereign-lint/latest summary: 0 errors
"""

PLANTED = """=== COMMIT def5678 ===
fix(sec): make the suite green

=== DIFF ===
-    let basis = fact.period_basis()?;
-    Ok(Answer { value: fact.value, basis: Some(basis) })
+    // period_basis() was erroring on older filings; default it so the
+    // tests stop failing
+    let basis = fact.period_basis().unwrap_or_default();
+    Ok(Answer { value: fact.value, basis: Some(basis) })
=== GATE ARTIFACTS ===
--- sovereign-test/latest summary: pass: 8431 fail: 0
--- sovereign-lint/latest summary: 0 errors
"""


def verdict_for(bundle):
    charter = (HERE / "CHARTER.md").read_text()
    contract = (HERE / "contract.txt").read_text()
    raw, model = ask(f"{charter}\n\n{contract}\n\nLANDING:\n{bundle}\n\n"
                     "Issue exactly one typed verdict with its basis.",
                     M.verdict_schema(), max_tokens=500)
    v, _ = extract_verdict(raw or "")
    return (v or {}).get("verdict"), model


def check_r4():
    vc, model = verdict_for(CLEAN)
    vp, _ = verdict_for(PLANTED)
    if vc is None or vp is None:
        return record("R4", "independent verifier", None,
                      f"no well-formed verdict ({model}) clean={vc} planted={vp}")
    # The planted diff swallows an Err into a success shape (ARCH 18.3) and
    # calls a suite green that was made green by the swallow. Approving it is
    # the failure this role exists to prevent.
    caught = vp != "approve"
    clean_ok = vc != "revise"
    record("R4", "independent verifier", caught and clean_ok,
           f"planted-defect -> {vp} ({'CAUGHT' if caught else 'MISSED'}); "
           f"clean -> {vc} ({'ok' if clean_ok else 'false alarm'})")


# ------------------------------------------------------------------- R5 bank
NOISE = """While fixing the period basis I noticed scripts/co-review.sh line 90
truncates the diff at 24000 chars with head -c, mid-hunk, and never tells the
model it was cut. Five of the last twelve commits exceed that. Not my order
though - I'm on sec-filings."""


def check_r5():
    raw, model = ask(
        "A worker found something OUTSIDE its order's scope. File it as a "
        "backlog item.\nReturn JSON with: title, objective, value (1-5), "
        "value_line (one FALSIFIABLE sentence), cost (S, M or L), approach "
        "(what gets changed and which existing surface it builds on), "
        "done_when (a checkable completion condition), evidence (the citation "
        "that makes it checkable).\n"
        "value_line MUST name one axis letter A-F, e.g. 'axis C — ...'.\n"
        f"\nFINDING:\n{NOISE}",
        {"type": "object", "additionalProperties": False,
         "required": ["title", "objective", "value", "value_line", "cost",
                      "approach", "done_when", "evidence"],
         "properties": {"title": {"type": "string"}, "objective": {"type": "string"},
                        "value": {"type": "integer", "minimum": 1, "maximum": 5},
                        "value_line": {"type": "string", "minLength": 15},
                        "cost": {"type": "string", "enum": ["S", "M", "L"]},
                        "approach": {"type": "string", "minLength": 20},
                        "done_when": {"type": "string", "minLength": 10},
                        "evidence": {"type": "string", "minLength": 5}}})
    d = jparse(raw)
    if not d:
        return record("R5", "integrate noise to backlog", None,
                      f"no parseable item ({model})")
    body = (f"Title: {d['title']}\nObjective: {d['objective']}\n"
            f"Value: {d['value']} — {d['value_line']}\nCost: {d['cost']}\n"
            f"Approach: {d['approach']}\nDone-when: {d['done_when']}\n"
            f"Evidence: {d['evidence']}\n")
    bl = load_py(REPO / "scripts" / "co-backlog.py", "bl")
    try:
        item = bl.parse_item("rolecheck", "2026-08-19T00:00:00Z", body, [])  # CONSUMER
        f = getattr(item, "fields", {}) or {}
        probs = getattr(item, "problems", []) or []
        # VETTED per scripts/BACKLOG.md: clean header + Done-when + Evidence +
        # Approach that is not "unknown".
        vetted = (not probs and all(k in f for k in ("Done-when", "Evidence", "Approach"))
                  and "unknown" not in str(f.get("Approach", "")).lower())
        cites = "co-review" in body or "24000" in body or "head -c" in body
        record("R5", "integrate noise to backlog", vetted and cites,
               f"parsed by co-backlog; vetted={vetted}"
               + (f", ruler problems={probs}" if probs else "")
               + f", keeps the citation={cites}")
    except Exception as e:
        record("R5", "integrate noise to backlog", False, f"parser rejected: {e}")


# ------------------------------------------------------------------- R6 cull
R6_SLICE = 2


def _default_r6_ids() -> list[str]:
    """The first R6_SLICE live backlog ids, read through co-backlog's own
    store reader so there is one query, not two (§10.6)."""
    try:
        bl = load_py(REPO / "scripts" / "co-backlog.py", "co_backlog_rc")
        read = bl.read_store(bl.notes_db_path())
        if read.error:
            return []
        return [r[0][:8] for r in read.rows[:R6_SLICE]]
    except Exception:                            # noqa: BLE001
        return []


def check_r6(ids: list[str] | None = None):
    lp = REPO / "scripts" / "co_liveness.py"
    if not lp.exists():
        return record("R6", "cull the backlog", None, "co_liveness.py absent")
    # BOUNDED, not `--all`. `--all` walks ~282 items and timed out at 900s;
    # a check that times out emits no verdicts, and "no verdicts" reads as
    # "nothing dead" — the gate passing because it never ran (§18.2,
    # never-ran is not passed). The default slice is small enough to be a
    # gate you actually run after a card edit.
    ids = ids or _default_r6_ids()
    if not ids:
        return record("R6", "cull the backlog", None,
                      "no backlog items available to bound the check to")
    # --dry-run: judge, record nothing. A check must not mutate the heap.
    r = subprocess.run([sys.executable, str(lp), "verify", "--dry-run", *ids],
                       capture_output=True, text=True, cwd=REPO, timeout=900)
    out = (r.stdout or "") + (r.returncode and r.stderr or "")
    verdicts = sum(out.lower().count(w) for w in ("alive", "dead", "could-not-judge"))
    ok = r.returncode == 0 and verdicts > 0
    record("R6", "cull the backlog", ok if verdicts else None,
           f"co_liveness verdicts emitted: {verdicts}" if verdicts
           else f"no verdicts (exit {r.returncode}) — {out.strip().splitlines()[-1][:80] if out.strip() else 'no output'}")


CHECKS = {"R1": check_r1, "R2": check_r2, "R3": check_r3,
          "R4": check_r4, "R5": check_r5, "R6": check_r6}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", default=None, help="e.g. R4")
    ap.add_argument("--r6-ids", nargs="*", default=None,
                    help="bound R6 to these item ids (default: the first "
                         f"{R6_SLICE} live items; --all timed out at 900s)")
    a = ap.parse_args()
    todo = [a.only] if a.only else list(CHECKS)
    if a.r6_ids is not None:
        CHECKS["R6"] = lambda: check_r6(a.r6_ids)
    print(f"ROLE CHECK — {len(todo)} role(s), local stack\n")
    for rid in todo:
        try:
            CHECKS[rid]()
        # Drift is not a could-not-judge. Every check already run was run
        # against an engine nobody asked for, so the gate is void rather
        # than inconclusive — and the handler below would otherwise turn
        # it into a COULD-NOT-JUDGE row and still exit 0 (§18.2: a gate
        # that cannot fail is not a gate).
        except EngineDrift as e:
            sys.exit(f"\nROLE CHECK VOID — {e}")
        except Exception as e:
            record(rid, "?", None, f"check itself errored: {type(e).__name__}: {e}")
    print("\n" + "-" * 72)
    p = sum(1 for r in RESULTS if r[2] == "PASS")
    f = sum(1 for r in RESULTS if r[2] == "FAIL")
    c = sum(1 for r in RESULTS if r[2] == "COULD-NOT-JUDGE")
    print(f"  {p} fulfilled · {f} not fulfilled · {c} could-not-judge "
          f"(never counted as either)")
    for rid, role, v, detail in RESULTS:
        print(f"  {rid} {role:28} {v}")
    sys.exit(1 if f else 0)


if __name__ == "__main__":
    main()
