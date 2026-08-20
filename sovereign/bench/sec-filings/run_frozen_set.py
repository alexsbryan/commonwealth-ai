#!/usr/bin/env python3
"""Drive the frozen fabrication set through the PRODUCT surface, and RECORD
what the answer path actually did on each turn.

REPO HOME (2026-08-17, order f2-instrument-arming): this runner used to live
in a session scratchpad. An instrument the financial-corpora F2 measurement
depends on must be findable by the next session, so it now sits beside its
bank in `sovereign/bench/sec-filings/`.

WHAT IT DOES
------------
One `sovereign chat ask <question>` per prereg item. The answer text is
extracted from stdout and written to <answers>/<id>.txt for
`scripts/check-sec-answer-path.py` to judge. The runner never reads the
sidecar, never calls the tool directly, and never sees expected values — it
only relays what a user would see.

WHAT IS NEW: THE PER-TURN RECORD
--------------------------------
F2 could not distinguish its two fabrication mechanisms — (a) the bare-numeral
audit armed and prose figures traced through a hole in its rules, versus (b)
`sec_facts` never ran at all — because nothing recorded WHICH happened. The
record fixes that.

No production code was changed to get it, and none needed to be. Every fact
below is already emitted by the answer path as a `tracing` event; they were
invisible only because `sovereign-cli-llm` installs a subscriber for `chat`
ONLY when `RUST_LOG` is set (`sovereign-cli-llm/src/main.rs:106-109`), and no
prior F2 run set it. This runner sets it and captures stderr, which the old
runner discarded.

Signal -> source, all on the child's stderr:
  sec_facts_fired      `tracing::debug!(target: "sec_facts", ...)`
                       — sovereign-tools/src/sec_facts.rs:263,342,545
  router gate          "router: tool-relevance gate decision" with
                       tool/tool_sim/exemplar_top_sim/floor/margin/passes
                       — sovereign-core/src/router.rs:2196-2205
  complex_task entered "runtime: complex_task - generating plan"
                       — sovereign-core/.../handlers/complex_task.rs:24
  audit_bare_armed     `bare_scope=` on the numeric_audit WARN line
  violations[]         `violations=[...]` on the same WARN line
                       — complex_task.rs:397-403
  gate action          `[gate]` lines (SOVEREIGN_AGENTIC_KQ_DEBUG=1) plus the
                       rendered answer's §6.2(4) BLOCK marker

ABSENCE IS REPORTED, NEVER DEFAULTED (ARCH §18.3). Three of these facts have
no negative event — the code path that does not arm the audit emits nothing at
all. Those fields are therefore THREE-valued: True, False, or the string
"not-observed". A turn that never entered ComplexTask cannot have armed the
audit, so that one case resolves to False on structural grounds and the record
names the reason in `basis`. Nothing else is inferred.

COMPARABILITY. The `chat ask` invocation is byte-identical to the one the three
completed F2 runs used (text mode, same argv, same `extract`), so answers stay
comparable with `answers/`, `answers-run2/`, `answers-run3/`. `--format json`
would have handed over the metadata blob directly, but it also suppresses the
live echo and returns think-stripped text, which would change what the judge
reads and break that comparability. The record is gathered from stderr instead.
"""
import argparse
import json
import os
import re
import subprocess
import sys
import time
import tomllib
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

RULE = "─" * 10          # the CLI's horizontal rule
NOISE = ("[router]", "[planner]", "[web]", "[tool]", "[mesh]", "[corpus]")

# The subscriber only exists when RUST_LOG is set, and when it IS set the
# default filter is ignored entirely (EnvFilter::try_from_default_env wins),
# so every target we need must be named here.
#   sec_facts / router.tool_gate  -> explicit `target:` strings; a crate-level
#                                    directive does NOT match them.
#   sovereign_core                -> module-path target for numeric_audit,
#                                    complex_task and the [gate] mirror.
# `sovereign_cli_llm=info` is not decorative: it makes the CLI itself emit at
# least one event per invocation, so the recorder can tell "the subscriber was
# alive and the tool did not fire" apart from "the subscriber never ran". Without
# a liveness line, an empty stderr is unreadable and every field must degrade to
# not-observed (see the TRACE_LINE guard in parse_record).
DEFAULT_RUST_LOG = ("warn,sovereign_cli_llm=info,sovereign_core=info,"
                    "sec_facts=debug,router.tool_gate=debug")

# --- record extraction -------------------------------------------------------

KV = re.compile(r"(\w+)=((?:\"[^\"]*\")|(?:\[[^\]]*\])|(?:[^\s]+))")

# tracing-subscriber writes ANSI colour even into a pipe, so a raw line looks
# like `\x1b[2m2026-…Z\x1b[0m \x1b[32m INFO\x1b[0m …` and the field separators
# come out as `concept\x1b[0m\x1b[2m=\x1b[0m"revenue"`. Every parse below runs
# on the STRIPPED text: without this, neither the timestamp anchor nor any
# `key=value` match survives. Observed live 2026-08-17 — the first control pair
# degraded every field to not-observed because of it.
ANSI = re.compile(r"\x1b\[[0-9;]*m")

# Any tracing-subscriber Full-format line: RFC3339 timestamp then a level.
# Presence proves a subscriber was installed and writing; absence means the
# stderr channel carried nothing we may reason from.
TRACE_LINE = re.compile(r"^\d{4}-\d\d-\d\dT[\d:.]+Z?\s+(TRACE|DEBUG|INFO|WARN|ERROR)\b",
                        re.MULTILINE)

# THE tool-execution marker. `executor` logs this immediately before invoking a
# tool, so it is unambiguous proof the tool RAN on this turn.
TOOL_EXEC = re.compile(r'Executing tool step\s+tool_id="([^"]+)"')

# The router's own verdict for the turn, with the coarse label naming WHICH
# mechanism sent it there (e.g. AUTHORITY_CLAIM vs TOOL_RELEVANCE).
ROUTED = re.compile(r'stream routed\s+intent=(\w+)(?:\s+coarse=Some\("([^"]+)"\))?')

# Exact shape, verified against the three completed runs on disk:
#   **How this was computed** (deterministic — `sec_facts`):
# Emitted by complex_task.rs:466-468.
DERIVATION = re.compile(r"How this was computed\*{0,2}\s*\(deterministic[^`]*`([^`]+)`")


def _derivation_tool(answer: str):
    m = DERIVATION.search(answer)
    return m.group(1) if m else None


def kvs(line: str) -> dict:
    """Every key=value pair on a tracing line, order-independent.

    tracing-subscriber's Full format puts the message and the fields on one
    line and does not promise their order, so the parse must not depend on it.
    """
    out = {}
    for k, v in KV.findall(line):
        out[k] = v.strip('"')
    return out


def parse_record(stderr: str, answer: str) -> dict:
    """Reduce a turn's stderr + answer to the arming record.

    Every field either carries evidence or says "not-observed". A field is
    never defaulted to False just because its event is missing.
    """
    stderr = ANSI.sub("", stderr)
    lines = stderr.splitlines()
    ev = {}

    # DID THE SUBSCRIBER EVEN RUN? This guard exists because the recorder was
    # observed, on 2026-08-17, resolving every field with total confidence when
    # handed empty stderr: with no `complex_task` line, the structural rule
    # "ComplexTask never entered, so no tool step could have run" fires and
    # reports a hard `sec_facts_fired=False` for the whole set. That is a
    # confidently WRONG answer produced from a missing instrument rather than
    # from a real turn — the exact shape ARCH §18.3 forbids.
    #
    # Two ways it happens for real: RUST_LOG unset (no subscriber is installed
    # at all — sovereign-cli-llm/src/main.rs:106-109), or an EnvFilter parse
    # failure on the dotted `router.tool_gate` target, which makes
    # `try_from_default_env()` fall back silently.
    #
    # So: no tracing output at all => nothing derived from stderr is knowable,
    # and the record says so instead of guessing.
    tracing_observed = bool(TRACE_LINE.search(stderr))
    if not tracing_observed:
        return {
            "sec_facts_fired": "not-observed",
            "audit_bare_armed": "not-observed",
            "violations": [],
            "gate_action": "not-observed",
            "router_gate": None,
            "routed_intent": None,
            "routed_coarse": None,
            "tool_steps": [],
            "complex_task_entered": "not-observed",
            "audit_event": "not-observed",
            "tracing_observed": False,
            "provenance_guard_block": "**Provenance guard**" in answer,
            "derivation_tool": _derivation_tool(answer),
            "basis": "NO TRACING OUTPUT — subscriber absent or filter unparsed; "
                     "nothing derived from stderr is knowable for this turn",
            "sec_facts_basis": "no tracing output to read",
            "evidence": {"stderr_bytes": len(stderr)},
        }

    # --- did sec_facts execute? ------------------------------------------
    # TWO independent positives, because neither alone is sufficient:
    #
    #  1. A `sec_facts:`-prefixed tracing message. Matching the bare substring
    #     "sec_facts" is WRONG and dangerously so — the router's gate line
    #     carries `tool=sec_facts` even when it REFUSED the tool, so a refused
    #     gate would read as "the tool fired" and turn mechanism (b) into a
    #     false (a). That is the exact confusion this record exists to resolve.
    #     The message prefix "sec_facts: " is the tool's own; the gate's message
    #     is "router: tool-relevance gate decision".
    #  2. The derivation appendix in the ANSWER. Necessary because sec_facts
    #     only logs on its coverage and REFUSAL paths (sec_facts.rs:263,342,545)
    #     — the successful fact-return path emits no event at all. The appendix
    #     is rendered only when a step output carried `figure_tool`
    #     (complex_task.rs:459-470), so it is positive proof the tool ran and
    #     produced figures.
    #
    # AND A THIRD TRAP, found by the live controls on 2026-08-17: `sec_facts:`
    # as a substring is ALSO wrong. The tool emits
    #   sec_facts: loaded declared-authoritative typed store …
    #   sec_facts: discovery complete declared=1
    # at STARTUP, on every single turn, whether or not the tool ever runs. Both
    # appeared in the negative control ("What is the capital of France?"), which
    # executed no tool at all. Counting them would have reported the tool as
    # having fired on a turn it never touched — the same false-positive
    # direction as the router-gate bug, and the same wrong answer downstream.
    #
    # So execution is read from the EXECUTOR, not from the tool's own chatter,
    # and only these count:
    #   `Executing tool step tool_id="sec_facts"`  (executor, unambiguous)
    #   `sec_facts: matched fact …`                (emitted only while serving)
    tool_steps = TOOL_EXEC.findall(stderr)
    if tool_steps:
        ev["tool_steps"] = tool_steps
    sec_exec_lines = [ln for ln in lines if "sec_facts: matched fact" in ln]
    if sec_exec_lines:
        ev["sec_facts_exec"] = sec_exec_lines[:3]
    # Kept only as evidence — never as proof of firing.
    discovery = [ln for ln in lines
                 if "sec_facts:" in ln and "matched fact" not in ln]
    if discovery:
        ev["sec_facts_discovery_ignored"] = discovery[:2]
    sec_executed = ("sec_facts" in tool_steps) or bool(sec_exec_lines)

    derivation_tool = _derivation_tool(answer)
    if derivation_tool:
        ev["derivation"] = DERIVATION.search(answer).group(0)

    # --- did the router's tool-relevance gate pass? ----------------------
    gate_line = next(
        (ln for ln in lines if "tool-relevance gate decision" in ln), None)
    if gate_line:
        g = kvs(gate_line)
        router_gate = {
            "tool": g.get("tool"),
            "tool_sim": g.get("tool_sim"),
            "exemplar_top_sim": g.get("exemplar_top_sim"),
            "floor": g.get("floor"),
            "margin": g.get("margin"),
            "passes": g.get("passes") == "true",
        }
        ev["router_gate"] = gate_line.strip()
    else:
        # No candidate tool was even scored (the gate only logs when it had a
        # `best`). That is not the same as "scored and failed".
        router_gate = None

    # --- which route did the turn actually take, and by which mechanism? --
    # The live controls showed the assumed mechanism is not the operative one:
    # `figure-revenue` reached ComplexTask via coarse=AUTHORITY_CLAIM
    # (`tool 'sec_facts' claims authority for corpus …`), NOT via the embed
    # tool-relevance gate, whose `router.tool_gate` event never fired at all.
    # Recording the coarse label keeps the two mechanisms distinguishable, so a
    # miss can be attributed to the right one.
    rm = ROUTED.search(stderr)
    routed_intent = rm.group(1) if rm else None
    routed_coarse = rm.group(2) if rm and rm.group(2) else None
    if rm:
        ev["routed"] = rm.group(0)
    authority_claim = next(
        (ln for ln in lines if "claims authority for corpus" in ln), None)
    if authority_claim:
        ev["authority_claim"] = authority_claim.strip()[:300]

    # --- did the turn reach the agentic path at all? ---------------------
    complex_task_entered = any(
        "complex_task" in ln and "generating plan" in ln for ln in lines)
    ct_line = next(
        (ln for ln in lines if "complex_task" in ln and "generating plan" in ln), None)
    if ct_line:
        ev["complex_task"] = ct_line.strip()

    # Resolve sec_facts arming now that the structural fact is known. Tools
    # execute as steps INSIDE the agentic path, so a turn that never entered
    # ComplexTask cannot have run one — that is the only case allowed to read
    # a hard False without a positive event. Everything else is three-valued.
    if sec_executed or derivation_tool == "sec_facts":
        sec_facts_fired = True
        sec_basis = ("executor ran tool_id=sec_facts" if "sec_facts" in tool_steps
                     else "sec_facts emitted `matched fact` while serving"
                     if sec_exec_lines
                     else "derivation appendix names sec_facts")
    elif tool_steps:
        sec_facts_fired = False
        sec_basis = f"executor ran tool step(s) {tool_steps}, none of them sec_facts"
    elif not complex_task_entered:
        sec_facts_fired = False
        sec_basis = "ComplexTask never entered — no tool step could have run"
    elif derivation_tool:
        sec_facts_fired = False
        sec_basis = f"ComplexTask ran but the figure tool was {derivation_tool!r}"
    else:
        sec_facts_fired = "not-observed"
        sec_basis = ("ComplexTask entered, no sec_facts event and no derivation "
                     "appendix — the success path logs nothing, so this is unresolved")

    # --- did the bare-numeral audit arm, and what did it flag? -----------
    # WARN line names `bare_scope` and `violations` explicitly.
    # INFO line means the audit RAN and found nothing, but does not name scope.
    # Neither line means the `!audit_bare && cited_figures.is_empty()` branch,
    # which emits no event (complex_task.rs:389-391).
    warn_line = next((ln for ln in lines if "numeric_audit: answer has" in ln), None)
    info_line = next(
        (ln for ln in lines if "numeric_audit: every answer figure traces" in ln), None)

    violations = []
    if warn_line:
        a = kvs(warn_line)
        audit_bare_armed = a.get("bare_scope") == "true"
        # `violations` is the Debug rendering of a Vec<String>, e.g.
        # `["$33,708 million", "12%"]`. Splitting on "," is WRONG — the
        # figures themselves contain thousands separators, so `$33,708`
        # would split into `$33` and `708 million`. Pull the quoted spans.
        raw = a.get("violations", "[]")
        violations = re.findall(r'"([^"]*)"', raw)
        audit_event = "warn"
        ev["numeric_audit"] = warn_line.strip()
        basis = "WARN line names bare_scope and violations"
    elif info_line:
        # The audit ran with an empty violation set. It does not print scope,
        # so arming is genuinely unresolved from this line alone.
        audit_bare_armed = "not-observed"
        audit_event = "info"
        ev["numeric_audit"] = info_line.strip()
        basis = "INFO line: audit ran, zero violations, scope not named on this line"
    elif not complex_task_entered:
        # Structural, not inferred: the audit lives inside the ComplexTask
        # handler. A turn that never entered it cannot have armed the audit.
        audit_bare_armed = False
        audit_event = "none"
        basis = "ComplexTask never entered — the audit code path did not run"
    else:
        audit_bare_armed = "not-observed"
        audit_event = "none"
        basis = "ComplexTask entered but no numeric_audit event — silent branch"

    # --- the gate's action ------------------------------------------------
    gate_lines = [ln for ln in lines if "[gate]" in ln]
    if gate_lines:
        ev["gate"] = gate_lines[-3:]
    block_fired = "**Provenance guard**" in answer
    if block_fired:
        gate_action = "blocked_6_2_4_provenance_guard"
    elif gate_lines:
        gate_action = "observed-see-evidence"
    else:
        gate_action = "not-observed"

    return {
        "sec_facts_fired": sec_facts_fired,
        "audit_bare_armed": audit_bare_armed,
        "violations": violations,
        "gate_action": gate_action,
        "router_gate": router_gate,
        "routed_intent": routed_intent,
        "routed_coarse": routed_coarse,
        "tool_steps": tool_steps,
        "complex_task_entered": complex_task_entered,
        "audit_event": audit_event,
        "tracing_observed": True,
        "provenance_guard_block": block_fired,
        "derivation_tool": derivation_tool,
        "basis": basis,
        "sec_facts_basis": sec_basis,
        "evidence": ev,
    }


# --- answer extraction (UNCHANGED from the scratchpad runner) ----------------


def extract(stdout: str) -> str:
    """Answer = between the rule that closes the echoed question and the
    rule that opens the footer. Falls back to the whole tail if the CLI
    layout ever changes — a silent empty answer would judge as an
    evasion and misreport the competence half."""
    lines = stdout.splitlines()
    rules = [i for i, ln in enumerate(lines) if RULE in ln]
    if len(rules) >= 3:
        body = lines[rules[1] + 1:rules[2]]
    elif len(rules) >= 2:
        body = lines[rules[1] + 1:]
    else:
        body = lines
    kept = [ln for ln in body if not ln.lstrip().startswith(NOISE)]
    return "\n".join(kept).strip()


# --- instrument self-test ----------------------------------------------------

# Fixtures reproduce tracing-subscriber's Full format with `.with_target(false)`:
# timestamp, level, then the `message` field first and the remaining fields as
# `key=value`. The parse must not depend on field order, so the fixtures
# deliberately vary it.
# Any event at all — proves a subscriber was installed and writing, which is
# what turns "no gate line" into evidence instead of silence.
_ALIVE = ('2026-08-17T14:23:00.500000Z  INFO chat: resolved daemon at 127.0.0.1:9741\n')
# VERBATIM from the 2026-08-17 controls, ANSI escapes intact. These two are
# STARTUP chatter — they appear on every turn, including the negative control
# that executed no tool. Counting them as "the tool fired" was a real bug.
_DISCOVERY = (
    '\x1b[2m2026-08-17T14:39:17.263956Z\x1b[0m \x1b[34mDEBUG\x1b[0m sec_facts: '
    'loaded declared-authoritative typed store \x1b[3mcorpus_id\x1b[0m\x1b[2m=\x1b[0m'
    'sec-cik0000320193 \x1b[3mentity\x1b[0m\x1b[2m=\x1b[0mApple Inc.\n'
    '\x1b[2m2026-08-17T14:39:17.424483Z\x1b[0m \x1b[34mDEBUG\x1b[0m sec_facts: '
    'discovery complete \x1b[3mdeclared\x1b[0m\x1b[2m=\x1b[0m1\n')
# EXECUTION, verbatim from the positive control.
_SEC = (
    '\x1b[2m2026-08-17T14:41:43.784398Z\x1b[0m \x1b[32m INFO\x1b[0m Executing tool '
    'step \x1b[3mtool_id\x1b[0m\x1b[2m=\x1b[0m"sec_facts" \x1b[3mparams\x1b[0m'
    '\x1b[2m=\x1b[0m{"concept":"revenue","period":"FY2025"}\n'
    '\x1b[2m2026-08-17T14:41:43.788273Z\x1b[0m \x1b[34mDEBUG\x1b[0m sec_facts: '
    'matched fact \x1b[3mconcept\x1b[0m\x1b[2m=\x1b[0m"revenue"\n')
# The route actually taken by the positive control — authority claim, NOT the
# embed tool-relevance gate the order assumed.
_ROUTED_CT = (
    '\x1b[2m2026-08-17T14:41:00.963248Z\x1b[0m \x1b[32m INFO\x1b[0m runtime: stream '
    'routed \x1b[3mintent\x1b[0m\x1b[2m=\x1b[0mComplexTask \x1b[3mcoarse\x1b[0m'
    '\x1b[2m=\x1b[0mSome("AUTHORITY_CLAIM")\n')
_ROUTED_KQ = (
    '\x1b[2m2026-08-17T14:39:19.456707Z\x1b[0m \x1b[32m INFO\x1b[0m runtime: stream '
    'routed \x1b[3mintent\x1b[0m\x1b[2m=\x1b[0mKnowledgeQuery \x1b[3mcoarse\x1b[0m'
    '\x1b[2m=\x1b[0mSome("LOOKUP")\n')
_CT = ('2026-08-17T14:23:00.900000Z  INFO runtime: complex_task — generating plan\n')
_GATE_PASS = ('2026-08-17T14:23:00.800000Z DEBUG router: tool-relevance gate decision '
              'tool=sec_facts tool_sim=0.913 exemplar_top_sim=0.881 floor=0.5 '
              'margin=0.05 passes=true\n')
_GATE_FAIL = ('2026-08-17T14:23:00.800000Z DEBUG router: tool-relevance gate decision '
              'tool=sec_facts tool_sim=0.884 exemplar_top_sim=0.902 floor=0.5 '
              'margin=0.05 passes=false\n')
# The figures carry thousands separators — the reason violations cannot be
# parsed by splitting on ",".
_AUDIT_WARN = ('2026-08-17T14:23:09.000000Z  WARN numeric_audit: answer has figure(s) '
               'not traceable to a tool computation or cited datum '
               'violations=["$33,708 million", "$29,984 million", "12%"] bare_scope=true\n')
_AUDIT_INFO = ('2026-08-17T14:23:09.000000Z  INFO numeric_audit: every answer figure '
               'traces to a tool computation or cited datum\n')

SELF_TESTS = [
    # name, stderr, answer, expected subset
    ("tool-fired-clean", _GATE_PASS + _CT + _SEC + _AUDIT_INFO, "Revenue was $416,161 million.",
     {"sec_facts_fired": True, "complex_task_entered": True,
      "audit_bare_armed": "not-observed", "violations": []}),
    ("tool-fired-violations", _GATE_PASS + _CT + _SEC + _AUDIT_WARN,
     "**Provenance guard** — the generated answer was withheld because 3 figure(s)",
     {"sec_facts_fired": True, "audit_bare_armed": True,
      "violations": ["$33,708 million", "$29,984 million", "12%"],
      "gate_action": "blocked_6_2_4_provenance_guard"}),
    # THE REGRESSION GUARD. The gate line carries `tool=sec_facts` while
    # REFUSING it. A substring match on "sec_facts" reads this as fired and
    # silently converts mechanism (b) into a false (a). Caught by this fixture
    # on 2026-08-17 before the instrument was ever trusted with a real turn.
    ("tool-not-fired-gate-refused", _GATE_FAIL, "Mac net sales rose 12%.",
     {"sec_facts_fired": False, "complex_task_entered": False,
      "audit_bare_armed": False, "violations": []}),
    # sec_facts logs NOTHING on its successful path — the derivation appendix
    # is the only positive evidence there.
    ("tool-fired-success-silent", _GATE_PASS + _CT + _AUDIT_INFO,
     "Revenue was $416,161 million.\n\n**How this was computed** "
     "(deterministic — `sec_facts`):\n- Revenues FY2025 = 416,161",
     {"sec_facts_fired": True, "derivation_tool": "sec_facts"}),
    # A different figure tool ran. Not sec_facts, and not unknown either.
    ("other-figure-tool", _CT + _AUDIT_INFO,
     "**How this was computed** (deterministic — `parcel_analytics`):\n- x",
     {"sec_facts_fired": False, "derivation_tool": "parcel_analytics"}),
    # ComplexTask ran, nothing named a tool. Genuinely unresolved — must not
    # be reported as either fired or not-fired.
    ("tool-unresolved", _CT, "A prose answer with no figures.",
     {"sec_facts_fired": "not-observed", "complex_task_entered": True}),
    # No tool was even scored — distinct from "scored and failed". The benign
    # line matters: it proves the SUBSCRIBER was alive, which is what makes the
    # absence of a gate line evidence rather than silence.
    ("tool-never-scored", _ALIVE, "The capital of France is Paris.",
     {"sec_facts_fired": False, "router_gate": None,
      "complex_task_entered": False, "audit_bare_armed": False}),
    # The silent third branch: ComplexTask ran, audit emitted nothing. Arming
    # is genuinely unknown and must NOT be reported as false.
    ("silent-no-arm-branch", _CT, "Some answer with no figures.",
     {"complex_task_entered": True, "audit_bare_armed": "not-observed",
      "audit_event": "none"}),
    # THE THIRD REGRESSION GUARD, from the live controls. Discovery chatter on
    # a turn that executed NO tool must read false. This is verbatim negative-
    # control stderr; a `sec_facts:` substring match reads it as fired.
    ("discovery-only-no-execution", _DISCOVERY + _ROUTED_KQ,
     "The capital of France is Paris.",
     {"sec_facts_fired": False, "complex_task_entered": False,
      "routed_intent": "KnowledgeQuery", "routed_coarse": "LOOKUP",
      "tool_steps": [], "tracing_observed": True}),
    # THE FOURTH. Real execution, ANSI intact, via the authority-claim route.
    ("real-execution-authority-claim", _DISCOVERY + _ROUTED_CT + _SEC + _AUDIT_INFO,
     "Apple's total revenue was $416,161 million.",
     {"sec_facts_fired": True, "tool_steps": ["sec_facts"],
      "routed_intent": "ComplexTask", "routed_coarse": "AUTHORITY_CLAIM",
      "tracing_observed": True}),
    # THE SECOND REGRESSION GUARD. Empty stderr must NOT resolve to a confident
    # false. Before this guard the recorder reported sec_facts_fired=False for
    # every turn of a run whose RUST_LOG was broken — a wrong answer with no
    # hedge, produced by a missing instrument rather than by a real turn.
    ("no-tracing-at-all", "", "Revenue was $416,161 million.",
     {"sec_facts_fired": "not-observed", "complex_task_entered": "not-observed",
      "audit_bare_armed": "not-observed", "tracing_observed": False}),
    # ...but the answer-derived fields still work without tracing, so a
    # retrospective pass over the old answer files stays useful.
    ("no-tracing-but-derivation", "",
     "**How this was computed** (deterministic — `sec_facts`):\n- x",
     {"sec_facts_fired": "not-observed", "derivation_tool": "sec_facts",
      "tracing_observed": False}),
]


def self_test() -> int:
    """Prove the recorder can be WRONG in both directions before it is trusted.

    Covers the parse only — the live end-to-end controls (a turn that really
    fires the tool and one that really does not) are leg 2 of the run script.
    """
    bad = 0
    for name, stderr, answer, want in SELF_TESTS:
        got = parse_record(stderr, answer)
        diffs = [f"{k}: want={want[k]!r} got={got.get(k)!r}"
                 for k in want if got.get(k) != want[k]]
        status = "ok" if not diffs else "MISMATCH"
        print(f"self-test {name:<28} {status}")
        for d in diffs:
            print(f"    {d}")
            bad += 1
    if bad:
        print(f"\nself-test: the recorder did NOT behave ({bad} mismatch) — "
              "fix the recorder before measuring")
        return 4
    print("\nself-test: recorder watched reading fired=True on 2 controls and "
          "fired=False on 2, and refusing to guess arming on the silent branch")
    return 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--prereg")
    ap.add_argument("--answers")
    ap.add_argument("--self-test", action="store_true",
                    help="prove the recorder can read both fired and not-fired")
    ap.add_argument("--cli", default="./target/debug/sovereign-cli")
    ap.add_argument("--daemon", default="http://localhost:9741",
                    help="daemon base URL probed for readiness before turn 1")
    ap.add_argument("--daemon-wait", type=int, default=60, metavar="SECS",
                    help="seconds to wait for /status 200 before refusing to measure")
    ap.add_argument("--only", help="run just this item id")
    ap.add_argument("--question", help="ad-hoc question; bypasses the prereg (instrument controls)")
    ap.add_argument("--id", default="adhoc", help="record id for --question")
    ap.add_argument("--records", help="per-turn record jsonl (default <answers>/records.jsonl)")
    ap.add_argument("--run-label", default="", help="stamped into every record")
    args = ap.parse_args()

    if args.self_test:
        sys.exit(self_test())
    if not args.answers:
        ap.error("--answers <dir> required (or --self-test)")
    if not args.prereg and not args.question:
        ap.error("--prereg <file> required (or --question for an ad-hoc control)")

    out = Path(args.answers)
    out.mkdir(parents=True, exist_ok=True)
    records = Path(args.records) if args.records else out / "records.jsonl"

    if args.question:
        items = [{"id": args.id, "question": args.question}]
    else:
        with open(args.prereg, "rb") as f:
            items = tomllib.load(f)["item"]

    env = dict(os.environ)
    env.setdefault("RUST_LOG", DEFAULT_RUST_LOG)
    env.setdefault("SOVEREIGN_AGENTIC_KQ_DEBUG", "1")

    # GATE ON A SERVING DAEMON, NOT A RUNNING ONE.
    #
    # 2026-08-17: this runner was started minutes after a daemon restart.
    # The process was up; it was not yet serving. Seven turns exited rc=1
    # with `daemon unreachable` two seconds apart, wrote 1-byte answers,
    # and the judge scored three of them as competence FAILURES. A bench
    # must refuse to measure an outage, not convert it into a regression
    # (ARCH §18.3) — and a pid is not readiness, which is why this probes
    # /status for a 200 rather than looking for a process. The other
    # worker hit the same shape earlier the same day (`http=000` with
    # daemon pids present), so this is a known transient, not a one-off.
    if not args.self_test:
        probe = f"{args.daemon.rstrip('/')}/status"
        ready, detail = False, ""
        for attempt in range(args.daemon_wait):
            try:
                with urllib.request.urlopen(probe, timeout=3) as r:
                    if r.status == 200:
                        ready = True
                        break
                    detail = f"HTTP {r.status}"
            except Exception as e:  # noqa: BLE001 — any failure is not-ready
                detail = f"{type(e).__name__}: {e}"
            if attempt == 0:
                print(f"daemon not serving yet at {probe} ({detail}) — waiting up to "
                      f"{args.daemon_wait}s", flush=True)
            time.sleep(1)
        if not ready:
            print(f"REFUSING TO MEASURE: {probe} never returned 200 within "
                  f"{args.daemon_wait}s (last: {detail}). Nothing was run — an "
                  f"unreachable daemon is an outage, and scoring it would bank "
                  f"infrastructure failures as quality regressions.", file=sys.stderr)
            sys.exit(5)
        print(f"daemon serving at {probe} ✓", flush=True)

    for item in items:
        iid = item["id"]
        if args.only and iid != args.only:
            continue
        q = item["question"]
        print(f"→ {iid}: {q}", flush=True)
        try:
            p = subprocess.run([args.cli, "chat", "ask", q],
                               capture_output=True, text=True, timeout=900, env=env)
            stdout, stderr, rc = p.stdout, p.stderr, p.returncode
        except subprocess.TimeoutExpired as e:
            stdout = e.stdout or ""
            stderr = e.stderr or ""
            rc = -1
            (out / f"{iid}.txt").write_text("RUNNER TIMEOUT — no answer produced\n")
            print(f"  TIMEOUT {iid}", file=sys.stderr, flush=True)

        if rc != -1:
            answer = extract(stdout)
            if not answer:
                print(f"  WARNING empty answer for {iid} (rc={rc})",
                      file=sys.stderr, flush=True)
            (out / f"{iid}.txt").write_text(answer + "\n")
            (out / f"{iid}.raw.txt").write_text(stdout)
        else:
            answer = ""

        # stderr is the arming channel — the old runner threw it away.
        (out / f"{iid}.stderr.txt").write_text(stderr)

        rec = {
            "id": iid,
            "run": args.run_label,
            "ts": datetime.now(timezone.utc).isoformat(),
            "rc": rc,
            "question": q,
            **parse_record(stderr, answer),
        }
        with open(records, "a") as fh:
            fh.write(json.dumps(rec) + "\n")

        rg = rec["router_gate"]
        print(f"  {len(answer)} chars | sec_facts_fired={rec['sec_facts_fired']} "
              f"audit_bare_armed={rec['audit_bare_armed']} "
              f"violations={len(rec['violations'])} "
              f"gate_action={rec['gate_action']} "
              f"router_gate={'passes=' + str(rg['passes']) if rg else 'not-scored'}",
              flush=True)


if __name__ == "__main__":
    main()
