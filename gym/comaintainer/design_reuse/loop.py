#!/usr/bin/env python3
"""Condition D — verifier-gated design observations.

The model may not dispose of a candidate until it has restated, for EVERY
candidate, the two facts the controller's canonical state records: is the
surface usable under the task's constraints, and does it already provide
the capability. The recorded facts are rendered in the dossier; the
controller grades the restatement against them, refuses contradicted
submissions (re-read and resubmit), and refuses any disposition whose own
verified observations contradict it. An unverified claim cannot enter
state — the agent-admission split, applied to the design decision itself.

Why the facts are SUPPLIED rather than asked (v2 -> v2.1, 2026-10-05):
v2 asked the model to derive the facts and graded its answers. Across
three smokes the model would not classify a surface it could describe as
"serves" (converge shape), and insisted the prohibited concrete engine
was usable (core-read), identically under field-level refusal feedback —
the historical miss reproduced. That is the concept's own instruction:
do not ask the model to compute what the machine can know. v2.1 keeps
the gate and measures what remains the model's job: restating the record
faithfully and choosing a disposition consistent with it.

The facts are pre-registered per candidate (bank `facts`, referee-needed
like the rest of the oracle; `usable=false` is mechanically spot-checked
against `pub(crate)` excerpts). `composite` cases — where the gold uses
serving parts AND adds missing capability — keep ADD/EXTEND/ADAPT open;
every other case with a serving candidate admits only USE of it.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))

import common as C  # noqa: E402
import contracts as H

MAX_REFUSALS = 4


def facts_map(case: dict) -> dict:
    return {c["id"]: c["facts"] for c in case["dossier"]["candidates"]}


def grade_observations(case: dict, observations) -> tuple[list, set, list]:
    """-> (wrong, missing_ids, duplicate_ids), where `wrong` names the exact
    field ("<candidate>.<field>") so a refusal can teach without leaking the
    answer. A non-list or malformed item reports every candidate as missing —
    the submission is refused."""
    facts = facts_map(case)
    if not isinstance(observations, list):
        return [], set(facts), []
    seen: set[str] = set()
    wrong: list[str] = []
    dupes: list[str] = []
    for obs in observations:
        if not isinstance(obs, dict) or obs.get("candidate") not in facts:
            return [], set(facts), []
        cid = obs["candidate"]
        if cid in seen:
            dupes.append(cid)
            continue
        seen.add(cid)
        fact = facts[cid]
        if obs.get("usable") is not fact["usable"]:
            wrong.append(f"{cid}.usable")
        if obs.get("serves") is not fact["serves"]:
            wrong.append(f"{cid}.serves")
    return wrong, set(facts) - seen, sorted(set(dupes))


def consistency(case: dict, action: dict) -> str | None:
    """None when the disposition is consistent with the verified facts,
    otherwise the refusal reason. Every reason cites the model's own
    observation, never a fact it has not stated.

    When a verified serving surface exists (and the case is not composite),
    the capability is already provided and the ONLY consistent disposition
    is USE of a serving, usable candidate — extending or adding alongside a
    provided capability is the swept-then-invented plan the gate exists to
    stop. Composite cases (gold = reuse serving parts AND add missing
    capability) keep ADD/EXTEND/ADAPT open."""
    facts = facts_map(case)
    composite = bool(case.get("composite"))
    serving = [cid for cid, f in facts.items() if f["serves"]]
    disp, cand = action.get("disposition"), action.get("candidate")
    if disp not in ("USE", "EXTEND", "ADAPT", "ADD"):
        return "unknown disposition"

    def use_check() -> str | None:
        if cand not in facts:
            return "the disposition must name one of the observed candidates"
        if not facts[cand]["usable"]:
            return (f"USE on {cand} is contradicted: it cannot serve the task "
                    "under its stated constraints.")
        if not facts[cand]["serves"]:
            return (f"USE on {cand} is contradicted: your verified observation "
                    "records that it does not already provide the capability.")
        return None

    if serving and not composite:
        if disp == "USE":
            return use_check()
        return ("the capability is already provided by " + ", ".join(serving)
                + "; only USE of a serving surface is consistent with your verified observations.")
    if disp == "USE":
        return use_check()
    if disp == "ADD":
        return None
    if cand not in facts:
        return "the disposition must name one of the observed candidates"
    if not facts[cand]["usable"]:
        return (f"{disp} on {cand} is contradicted: this surface cannot serve "
                "the task under its stated constraints.")
    if disp in ("EXTEND", "ADAPT") and facts[cand]["serves"]:
        return (f"{disp} on {cand} is contradicted: your verified observation records "
                "that it already provides the capability, so building on it is unnecessary.")
    return None


def schema_d(case: dict, restate: bool = True) -> dict:
    ids = [c["id"] for c in case["dossier"]["candidates"]]
    props: dict = {
        "disposition": {"type": "string", "enum": list(C.DISPOSITIONS)},
        "candidate": {"type": "string", "enum": ids + ["none"]},
        "delta": {"type": "string", "minLength": 40},
        "new_components": {"type": "array", "items": {"type": "string"}},
        "limits": {"type": "string", "minLength": 20},
        "evidence": {"type": "array", "minItems": 1,
                     "items": {"type": "string", "enum": ids + ["none-of-these"]}},
    }
    required = ["disposition", "candidate", "delta", "new_components", "limits", "evidence"]
    if restate:
        props["observations"] = {
            "type": "array", "minItems": len(ids), "maxItems": len(ids),
            "items": {
                "type": "object",
                "properties": {
                    "candidate": {"type": "string", "enum": ids},
                    "usable": {"type": "boolean"},
                    "serves": {"type": "boolean"},
                },
                "required": ["candidate", "usable", "serves"],
                "additionalProperties": False,
            },
        }
        required = ["observations"] + required
    return {"type": "object", "properties": props, "required": required,
            "additionalProperties": False}


PROTOCOL = (
    "VERIFIER PROTOCOL (binding):\n"
    "- The controller's canonical record for each candidate is printed beside it "
    "(usable / serves). Restate every candidate's recorded facts exactly once in "
    "your `observations` — a restatement that contradicts the record is refused.\n"
    "    usable — can the task's caller use this surface as it exists: reachable, "
    "permitted by the stated constraints, no lifecycle blocker?\n"
    "    serves — would using this surface (or its output) produce the finding the "
    "task asks for, without building new capability?\n"
    "- Then choose the disposition consistent with the record, name the candidate "
    "it rests on, and give the delta, limits and evidence.")

PROTOCOL_CANONICAL = (
    "VERIFIER PROTOCOL (binding):\n"
    "- The controller's canonical record for each candidate is printed beside it "
    "(usable / serves) — this is the state, not a suggestion.\n"
    "    usable — the task's caller can use this surface as it exists: reachable, "
    "permitted by the stated constraints, no lifecycle blocker.\n"
    "    serves — using this surface (or its output) produces the finding the task "
    "asks for, without building new capability.\n"
    "- Choose the disposition consistent with the record, name the candidate it "
    "rests on, and give the delta, limits and evidence. A disposition that "
    "contradicts a recorded fact is refused.")


def render_dossier_with_facts(case: dict) -> str:
    lines = []
    for cand in case["dossier"]["candidates"]:
        f = cand["facts"]
        lines.append(f"[{cand['id']}] {cand['path']}")
        lines.append(f"    {cand['excerpt']}")
        lines.append("    recorded: usable="
                     f"{'yes' if f['usable'] else 'no'}, serves={'yes' if f['serves'] else 'no'}")
    return "\n".join(lines)


def prompt_d(case: dict, refusals: list[str], restate: bool = True) -> str:
    parts = [C.INTRO, C.LEGEND]
    parts.append("TASK:\n" + case["request"]["requirement"])
    parts.append("CONSTRAINTS:\n" + "\n".join(f"- {c}" for c in case["request"]["constraints"]))
    parts.append("CANDIDATE EXISTING SURFACES (excerpt from the repository at the recorded "
                 "revision; recorded = the controller's canonical facts):\n"
                 + render_dossier_with_facts(case))
    parts.append(PROTOCOL if restate else PROTOCOL_CANONICAL)
    if refusals:
        parts.append("REFUSED SUBMISSIONS (fix and resubmit):\n"
                     + "\n".join(f"- {r}" for r in refusals[-4:]))
    parts.append("Return one JSON object now.")
    return "\n\n".join(parts)


def compose(case: dict, action: dict) -> dict:
    by_id = {c["id"]: c for c in case["dossier"]["candidates"]}
    cid = action.get("candidate")
    cand = by_id.get(cid)
    return {
        "disposition": action.get("disposition"),
        "owner": f"{cand['path']} ({cid})" if cand else (cid or ""),
        "seam": cid or "",
        "delta": action.get("delta", ""),
        "new_components": action.get("new_components") or [],
        "limits": action.get("limits", ""),
        "evidence": action.get("evidence") or [],
    }


def _row(case: dict, prompt: str, raw, model, parsed, verdict, reason,
         refusals: int, wrong: int, attempts: int, finished: bool,
         attempt_log: list, first_wrong: int | None) -> dict:
    metrics = C.score_proposal(case, parsed, "D") if parsed else None
    if metrics is not None:
        metrics.update({"obs_refusals": refusals, "wrong_observations": wrong,
                        "attempts": attempts, "first_attempt_wrong": first_wrong})
    return {"case": case["id"], "condition": "D", "prompt": prompt, "raw": raw,
            "model": model, "parsed": parsed, "verdict": verdict, "reason": reason,
            "metrics": metrics, "refusals": refusals, "wrong_observations": wrong,
            "attempts": attempts, "finished": finished,
            "attempt_log": attempt_log, "first_attempt_wrong": first_wrong}


def run_case_loop(case: dict, ask, max_refusals: int = MAX_REFUSALS,
                  restate: bool = True) -> dict:
    """`ask(prompt, schema) -> (text | None, model_or_reason)`. Returns one
    replay row; every submission and refusal is recorded in `attempt_log`,
    and an unresolved case is a could-not-judge, never a pass.

    `restate=True` (v2.1) requires the model to echo the recorded facts and
    grades the restatement. `restate=False` (v2.2) is the canonical-state
    endpoint: the facts are printed and treated as state; only the
    disposition is checked for consistency against them."""
    refusals: list[str] = []
    attempt_log: list[dict] = []
    wrong_total = 0
    first_wrong: int | None = None
    attempts = 0
    last_prompt, last_raw, model = "", None, None
    while attempts <= max_refusals:
        attempts += 1
        last_prompt = prompt_d(case, refusals, restate=restate)
        text, result = ask(last_prompt, schema_d(case, restate=restate))
        if text is None:
            attempt_log.append({"attempt": attempts, "raw": None, "refused": result})
            return _row(case, last_prompt, None, None, None, "could-not-judge",
                        result, len(refusals), wrong_total, attempts, False,
                        attempt_log, first_wrong)
        last_raw, model = text, result
        record: dict = {"attempt": attempts, "raw": text, "refused": None}
        try:
            action = json.loads(text)
        except json.JSONDecodeError as exc:
            reason = f"malformed JSON ({exc})"
            record["refused"] = reason
            attempt_log.append(record)
            refusals.append(reason)
            continue
        violations = H.problems(action, schema_d(case, restate=restate))
        if violations:
            reason = "host contract refused: " + "; ".join(violations)
            record["refused"] = reason
            attempt_log.append(record)
            refusals.append(reason)
            continue
        if restate:
            wrong, missing, dupes = grade_observations(case, action.get("observations"))
            if first_wrong is None:
                first_wrong = len(wrong) + len(missing) + len(dupes)
            if missing or dupes:
                names = sorted(missing) + [f"{d} (duplicate)" for d in dupes]
                reason = ("every candidate must be observed exactly once; "
                          f"missing or duplicated: {', '.join(names)}")
                record["refused"] = reason
                attempt_log.append(record)
                refusals.append(reason)
                continue
            if wrong:
                wrong_total += len(wrong)
                reason = ("observations contradicted by the frozen excerpts: "
                          + ", ".join(wrong) + " — re-read and resubmit")
                record["refused"] = reason
                attempt_log.append(record)
                refusals.append(reason)
                continue
        err = consistency(case, action)
        if err:
            record["refused"] = err
            attempt_log.append(record)
            refusals.append(err)
            continue
        attempt_log.append(record)
        return _row(case, last_prompt, last_raw, model, compose(case, action),
                    "parsed", None, len(refusals), wrong_total, attempts, True,
                    attempt_log, first_wrong)
    return _row(case, last_prompt, last_raw, model, None, "could-not-judge",
                f"refusal cap ({max_refusals}) reached", len(refusals),
                wrong_total, attempts, False, attempt_log, first_wrong)
