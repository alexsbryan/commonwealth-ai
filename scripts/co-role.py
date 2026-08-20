#!/usr/bin/env python3
"""Drive one seat role on the local stack. One harness, seven cards.

    scripts/co-role.py R0 --input "start a campaign on X"   # route, don't run
    scripts/co-role.py R5 --input finding.txt
    scripts/co-role.py R3 --input financial-corpora
    scripts/co-role.py R1 --input intent.txt --draft
    scripts/co-role.py --lint                       # cards only, no model

The cards are DATA in `gym/comaintainer/roles/`; this file is the only
code that reads them. A card change needs no code change (ARCH §6).

WHY THIS EXISTS. Every role's output already has a consumer in this
repo — the campaign loader, the backlog ruler, the verdict parser, the
liveness judge. The consumer IS the verifier: it either accepts the
output or it does not, so no role needs a judge grading a judge. What
was missing was a single place that sends a role's card, constrains the
reply to that role's schema, hands the result to that role's consumer,
and records what happened. Six ad-hoc call sites were doing four of
those five things each, differently.

GATE CLASSES (plan §313, operator-set — this file does not choose them):

  draft   R1 R2 R6   the output is queued as a pending directive for the
                     operator to approve, edit or reject. Nothing lands.
  auto    R3 R5      consumer-validated; a wrong item costs one heap row.
  charter R4         the charter's existing landing gate, unchanged.
  propose R0         the router. Returns steps and queues NOTHING — a route
                     is a reading of intent, not a decision anyone approves.

`--draft` / `--auto` override the card, and the override is recorded in
the audit row. Overriding `charter` is refused: R4's gate is ratified
and is not a command-line flag.

THE AUDIT ROW. One JSONL record per invocation to
`~/.sovereign/comaintainer/role-runs.jsonl` — role, pin, served model,
schema-accepted, consumer-accepted, gate, outcome. Not a new store in
the sense the plan forbids: it sits beside directives.jsonl and
verdicts.jsonl and is what lets a campaign leave a trail without prose.

REFUSAL IS A FIRST-CLASS OUTCOME. Engine unreachable, reply not
well-formed, consumer rejection, drift — each is a named `could-not-judge`
with its reason, and the process exits non-zero. Nothing here defaults
to a pass (§18.2: four verdicts, not two).
"""
from __future__ import annotations

import argparse
import datetime as dt
import importlib.util
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
GYM = REPO / "gym" / "comaintainer"
CARDS = GYM / "roles"
sys.path.insert(0, str(GYM))

import markers as M                                    # noqa: E402
from score import (                                    # noqa: E402
    EngineDrift, basis_gate, call_daemon, extract_verdict, first_json_object,
)

AUDIT = Path.home() / ".sovereign" / "comaintainer" / "role-runs.jsonl"
DIRECTIVE_LOG = REPO / "scripts" / "co-directive-log.sh"

# A card is a prompt. ~800 tokens is the cap the roles README states, and
# the failure it prevents is the cards growing into a second constitution
# — which is the thing that already does not fit in this model's window.
# 4 chars/token is the usual rough conversion; this is a guard rail, not a
# tokenizer, and it says so when it fires.
CARD_TOKEN_CAP = 800
CHARS_PER_TOKEN = 4

# `propose` is R0's, and it is a fourth class rather than a reuse of
# `draft` because a draft QUEUES a directive for the operator to approve.
# A route is not a decision anyone approves — it is a reading of intent
# that the operator either acts on or ignores. Queueing it would put a
# row in directives.jsonl for every question asked, and the edit rate
# would start counting them.
GATES = ("draft", "auto", "charter", "propose")
ENGINES = ("model", "script")

# Which directive kind a drafting role queues under. co-directive-log.sh
# validates this set; a role mapped to an unknown kind fails there, loudly.
DIRECTIVE_KIND = {"R1": "order", "R2": "decision", "R6": "decision"}


class RoleRefusal(RuntimeError):
    """A named could-not-judge. Carries the reason the operator needs."""


# ---- cards -------------------------------------------------------------


def load_card(rid: str) -> dict:
    """-> {role, name, gate, engine, consumer, schema, prose, schema_obj}"""
    path = CARDS / f"{rid}.md"
    if not path.exists():
        raise RoleRefusal(f"no card for {rid} at {path}")
    text = path.read_text(encoding="utf-8")
    m = re.match(r"^---\n(.*?)\n---\n(.*)$", text, re.S)
    if not m:
        raise RoleRefusal(f"{path.name}: no frontmatter block")
    meta: dict = {}
    for line in m.group(1).splitlines():
        if ":" in line:
            k, v = line.split(":", 1)
            meta[k.strip()] = v.strip()
    body = m.group(2)

    for key in ("role", "name", "gate", "engine", "consumer", "schema"):
        if key not in meta:
            raise RoleRefusal(f"{path.name}: frontmatter missing {key!r}")
    if meta["role"] != rid:
        raise RoleRefusal(f"{path.name}: declares role {meta['role']!r}")
    if meta["gate"] not in GATES:
        raise RoleRefusal(f"{path.name}: gate {meta['gate']!r} not in {GATES}")
    if meta["engine"] not in ENGINES:
        raise RoleRefusal(f"{path.name}: engine {meta['engine']!r} not in {ENGINES}")

    # The schema is the fenced json block, and it is removed from the
    # prose: llguidance enforces it at decode time, so restating it in
    # the prompt spends context on an instruction the grammar already
    # guarantees (§7.6 — structure over instruction).
    schema_obj = None
    fence = re.search(r"```json\s*\n(.*?)\n```", body, re.S)
    if meta["schema"] == "inline":
        if not fence:
            raise RoleRefusal(f"{path.name}: schema: inline but no ```json block")
        try:
            schema_obj = json.loads(fence.group(1))
        except json.JSONDecodeError as e:
            raise RoleRefusal(f"{path.name}: schema block is not JSON: {e}")
    elif meta["schema"] == "markers.verdict_schema":
        schema_obj = M.verdict_schema()
    elif meta["schema"] != "none":
        raise RoleRefusal(f"{path.name}: unknown schema {meta['schema']!r}")
    prose = (body[:fence.start()] + body[fence.end():]) if fence else body

    meta["prose"] = prose.strip()
    meta["schema_obj"] = schema_obj
    meta["path"] = path
    return meta


def lint_cards() -> int:
    """Every card parses, and none exceeds the token cap. Exit code IS
    the verdict, so this can be a gate."""
    bad = 0
    for rid in ("R0", "R1", "R2", "R3", "R4", "R5", "R6"):
        try:
            card = load_card(rid)
        except RoleRefusal as e:
            print(f"  FAIL  {rid}: {e}")
            bad += 1
            continue
        approx = len(card["prose"]) // CHARS_PER_TOKEN
        over = approx > CARD_TOKEN_CAP
        bad += over
        print(f"  {'FAIL' if over else 'ok  '}  {rid} {card['name']:28} "
              f"~{approx:4d} tok  gate={card['gate']:7} engine={card['engine']:6} "
              f"schema={'yes' if card['schema_obj'] else 'none'}"
              + (f"   OVER the {CARD_TOKEN_CAP}-token cap" if over else ""))
    print()
    print(f"card lint: {7 - bad} ok, {bad} failing "
          f"(approximate tokens at {CHARS_PER_TOKEN} chars/token)")
    return 1 if bad else 0


# ---- consumers ---------------------------------------------------------
# Each returns (accepted: bool, detail: str). The consumer is the
# verifier; none of these calls a model.


def _load_py(path: Path, name: str):
    sp = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(sp)
    sys.modules[name] = mod
    try:
        sp.loader.exec_module(mod)
    except SystemExit:      # these scripts call main() behind __main__
        pass
    return mod


def consume_r1(out: dict, _inp: str) -> tuple[bool, str]:
    """`co-order.sh check` — this is G2.

    That check exits 1 on a missing or empty `Done when:` and on a
    `serves:` naming a campaign or bar nobody declared. It has always
    exited non-zero and NOTHING CALLED IT, so R1's whole contract was
    guaranteed by prose. Now a non-falsifiable order blocks the draft.

    `check` takes an order ID and resolves `$FEATURES/<id>/order.md`, so
    the order is written under CO_FEATURES — a throwaway tree — rather
    than into the real orders directory where `co-order.sh list` would
    show the operator an order that does not exist.
    """
    today = dt.date.today().isoformat()
    order = ("---\nschema: work-order/v1\nid: co-role-draft\n"
             f"status: open\ndrafted: {today}\n"
             "lane: (none)\nengine: (none)\nbudget: (none)\n---\n\n"
             f"# Order: {out['objective'][:60]}\n\n"
             f"## Objective\n{out['objective']}\n\n"
             f"Done when: {out['done_when']}\n"
             f"Not worth continuing if: {out['not_worth_continuing_if']}\n\n"
             "## Lane\n(none)\n\n"
             "## Scope\n" + "\n".join(f"- {s}" for s in out["scope"]) + "\n\n"
             "## Engine\n(none)\n\n## Budget\n(none)\n\n## Seams\n(none)\n")
    with tempfile.TemporaryDirectory(prefix="co-role-r1-") as tmp:
        oid = "co-role-draft"
        d = Path(tmp) / oid
        d.mkdir(parents=True)
        (d / "order.md").write_text(order, encoding="utf-8")
        env = dict(os.environ, CO_FEATURES=tmp)
        r = subprocess.run(["bash", str(REPO / "scripts" / "co-order.sh"),
                            "check", oid],
                           capture_output=True, text=True, cwd=REPO,
                           timeout=120, env=env)
    lines = [ln for ln in ((r.stdout or "") + (r.stderr or "")).splitlines()
             if ln.strip()]
    if r.returncode == 0:
        return True, next((ln.strip() for ln in lines
                           if ln.startswith("ready:")), "co-order.sh check passed")
    # Report the PROBLEM lines, not the banner — the banner says a check
    # failed, the problems say which promise the order did not make.
    probs = [ln.strip(" -") for ln in lines if ln.startswith("  - ")]
    return False, "co-order.sh check: " + ("; ".join(probs) or f"exit {r.returncode}")


def consume_r2(out: dict, _inp: str) -> tuple[bool, str]:
    """The real campaign loader. A bar the loader rejects is not a bar."""
    toml = ('id = "co-role"\nobjective = "x"\nspec = "s"\n'
            'declared = "2026-08-19"\nstatus = "active"\n')
    for b in out["bars"]:
        toml += (f'\n[[bar]]\nid = {json.dumps(b["id"])}\n'
                 f'one_line = {json.dumps(b["one_line"])}\n'
                 f'derives_from = {json.dumps(b["derives_from"])}\n'
                 f'status = "open"\n')
    lin = _load_py(REPO / "scripts" / "co-lineage.py", "co_lineage_r2")
    with tempfile.NamedTemporaryFile("w", suffix=".toml", delete=False) as fh:
        fh.write(toml)
        p = Path(fh.name)
    try:
        camp = lin.load_campaign_file(p)
        return True, f"{len(camp.bars)} bars accepted by the real loader"
    except Exception as e:                       # noqa: BLE001
        return False, f"loader rejected: {e}"
    finally:
        p.unlink(missing_ok=True)


def consume_r5(out: dict, inp: str) -> tuple[bool, str]:
    """co-backlog's ruler. VETTED per scripts/BACKLOG.md: clean header,
    Done-when, Evidence, and an Approach that is not 'unknown'."""
    body = (f"Title: {out['title']}\nObjective: {out['objective']}\n"
            f"Value: {out['value']} — {out['value_line']}\nCost: {out['cost']}\n"
            f"Approach: {out['approach']}\nDone-when: {out['done_when']}\n"
            f"Evidence: {out['evidence']}\n")
    bl = _load_py(REPO / "scripts" / "co-backlog.py", "co_backlog_r5")
    try:
        item = bl.parse_item("co-role", dt.datetime.now(dt.timezone.utc)
                             .isoformat(), body, [])
    except Exception as e:                       # noqa: BLE001
        return False, f"ruler rejected: {e}"
    fields = getattr(item, "fields", {}) or {}
    problems = getattr(item, "problems", []) or []
    vetted = (not problems
              and all(k in fields for k in ("Done-when", "Evidence", "Approach"))
              and "unknown" not in str(fields.get("Approach", "")).lower())
    # The item must still be about the finding. A well-formed item that
    # dropped the worker's citation is the failure mode this role has —
    # it parses, it reads sensibly, and it is not actionable.
    kept = _keeps_a_citation(out["evidence"], inp)
    return (vetted and kept,
            f"parsed by co-backlog; vetted={vetted}"
            + (f", ruler problems={problems}" if problems else "")
            + f", keeps a citation from the finding={kept}")


def _keeps_a_citation(evidence: str, finding: str) -> bool:
    """Does `evidence` reuse a concrete token the finding actually named?

    Deliberately lexical and deliberately narrow: paths, numbers,
    flag-ish and dotted identifiers. A word-overlap check would pass on
    'the script' and defeat the point.
    """
    concrete = set(re.findall(r"[\w./-]*[/.][\w./-]+|\b\d{3,}\b|--[a-z-]+",
                              finding))
    return any(tok in evidence for tok in concrete if len(tok) > 3)


def consume_r4(out: dict, _inp: str) -> tuple[bool, str]:
    """Two checks the model cannot do for itself: the verdict is one of
    the six and carries its required argument, and every cited anchor
    resolves (G1)."""
    gate = basis_gate(out)
    if gate.get("basis_unresolved"):
        return False, ("cited anchors that do not resolve: "
                       + ", ".join(gate["basis_unresolved"]))
    checked = "resolved" if gate["basis_checked"] else "NOT verified (resolver down)"
    return True, f"verdict {out['verdict']!r}, {len(gate['basis'])} anchors {checked}"


def consume_r0(out: dict, _inp: str) -> tuple[bool, str]:
    """The six-role set is the consumer: a proposed step must name a role
    that exists and carry an input that role could actually take.

    The schema masks `role` to the enum, so a non-member is unreachable
    at decode time rather than caught here — this checks what the grammar
    cannot: that R6 was not routed unbounded, and that a step's input is
    not the operator's question echoed back.
    """
    steps = out.get("steps") or []
    if not steps:
        # Not a failure. "I could not read this" is the answer R0's card
        # asks for when the intent is vague or needs an id it was not
        # given, and it is reported as could-not-judge by the caller.
        raise RoleRefusal(f"no route — {out.get('note', 'no reason given')}")
    for s in steps:
        rid, text = s.get("role"), (s.get("input") or "").strip()
        if rid not in {f"R{i}" for i in range(1, 7)}:
            return False, f"proposed an unknown role {rid!r}"
        if not text:
            return False, f"{rid} proposed with an empty input"
        if rid == "R6" and not re.search(r"[0-9a-f]{6,}", text):
            # R6's own refusal would catch this, but catching it here
            # means the operator never sees a proposed step that cannot
            # run. `--all` sweeps ~282 items and times out reporting
            # nothing, which reads as "nothing to retire".
            return False, "R6 proposed without item ids — it must be bounded"
    return True, "; ".join(f"{s['role']} <- {s['input'][:48]}" for s in steps)


CONSUMERS = {"R0": consume_r0, "R1": consume_r1, "R2": consume_r2,
             "R4": consume_r4, "R5": consume_r5}


# ---- script roles ------------------------------------------------------


def run_script_role(rid: str, inp: str, timeout: float) -> tuple[bool, str, str]:
    """-> (accepted, detail, stdout). R3 and R6 are owned by scripts that
    already exist; this harness drives them, it does not reimplement
    them (§19)."""
    if rid == "R3":
        argv = [sys.executable, str(REPO / "scripts" / "co-lineage.py"),
                "coverage", inp]
    elif rid == "R6":
        if not inp.strip():
            raise RoleRefusal(
                "R6 needs explicit item ids or a bounded slice — `--all` timed "
                "out at 900s over ~282 items, and a sweep that times out "
                "reports nothing, which reads as 'no dead items'")
        argv = [sys.executable, str(REPO / "scripts" / "co_liveness.py"),
                "verify", "--dry-run"] + inp.split()
    else:
        raise RoleRefusal(f"{rid} is not a script role")
    r = subprocess.run(argv, capture_output=True, text=True, cwd=REPO,
                       timeout=timeout)
    out = r.stdout or ""
    if r.returncode != 0:
        return False, f"{Path(argv[1]).name} exited {r.returncode}: " \
                      f"{(r.stderr or '').strip()[:200]}", out
    if rid == "R3":
        ok = "bar" in out.lower() or "uncovered" in out.lower()
        return ok, (f"{len(out.splitlines())} lines of coverage" if ok
                    else "coverage output named no bars"), out
    verdicts = sum(out.lower().count(w) for w in ("alive", "dead", "could-not-judge"))
    return verdicts > 0, f"{verdicts} liveness verdict(s), nothing retired", out


# ---- the gate ----------------------------------------------------------


def queue_draft(rid: str, card: dict, payload: dict, citations: list) -> str:
    """A drafting role does not land; it queues a pending directive the
    operator approves, edits or rejects. Same log, same shape, same
    `--resolve` path as a hand-written directive — the console and this
    harness must be indistinguishable in the record."""
    kind = DIRECTIVE_KIND.get(rid)
    if kind is None:
        raise RoleRefusal(f"{rid} has gate 'draft' but no directive kind mapped")
    draft = json.dumps(payload, ensure_ascii=False, indent=2)
    argv = ["bash", str(DIRECTIVE_LOG), "--pending", "--kind", kind,
            "--draft", f"{rid} ({card['name']}) drafted:\n{draft}"]
    if citations:
        argv += ["--citations", ",".join(citations)]
    r = subprocess.run(argv, capture_output=True, text=True, cwd=REPO,
                       timeout=60)
    if r.returncode != 0:
        raise RoleRefusal(f"could not queue the draft: "
                          f"{(r.stderr or r.stdout or '').strip()[:200]}")
    return (r.stdout or "").strip().splitlines()[-1] if r.stdout else "(no id)"


class CanaryHalt(RuntimeError):
    """The role gave the forbidden answer to a question with a known one."""


def run_canary(rid: str, timeout: float, max_tokens: int) -> dict | None:
    """-> {verdict, forbidden, why} or None when the role has no canary.

    Schemas check form and parsers check shape; neither checks judgment,
    and this is the only thing here that does. One extra call alongside
    real work rather than a 46-episode bank — a bank measures better and
    is not running while you work, so the instrument can rot between
    measurements with nothing saying so (§18.1, made continuous).
    """
    path = GYM / "canaries" / f"{rid}.md"
    if not path.exists():
        return None
    text = path.read_text(encoding="utf-8")
    m = re.match(r"^---\n(.*?)\n---\n(.*)$", text, re.S)
    if not m:
        raise RoleRefusal(f"canary {path.name}: no frontmatter block")
    meta = {}
    for line in m.group(1).splitlines():
        if ":" in line:
            k, v = line.split(":", 1)
            meta[k.strip()] = v.strip()
    forbidden = meta.get("forbid_verdict")
    if not forbidden:
        raise RoleRefusal(f"canary {path.name}: no forbid_verdict")

    card = load_card(rid)
    raw, _model = call_daemon(card["prose"] + "\n\n=== INPUT ===\n" + m.group(2),
                              timeout, max_tokens, schema=card["schema_obj"],
                              schema_name=f"{rid.lower()}canary")
    parsed, _malformed = extract_verdict(raw or "")
    got = (parsed or {}).get("verdict")
    # A MALFORMED canary reply is not a pass. It means the instrument did
    # not answer, and "it did not answer" must not read as "it did not
    # fail" (§18.2 — could-not-judge is its own outcome).
    if got is None:
        raise RoleRefusal(f"canary {rid}: no well-formed verdict came back, so "
                          f"the instrument is unverified for this run")
    if got == forbidden:
        raise CanaryHalt(
            f"canary {rid}: the role answered {got!r} to a planted defect whose "
            f"correct answer is anything but that — {meta.get('why', '')}. "
            f"The real output of this run is NOT recorded as trustworthy.")
    return {"verdict": got, "forbidden": forbidden, "why": meta.get("why", "")}


def audit(row: dict) -> None:
    AUDIT.parent.mkdir(parents=True, exist_ok=True)
    with AUDIT.open("a", encoding="utf-8") as fh:
        fh.write(json.dumps(row, ensure_ascii=False) + "\n")


# ---- main --------------------------------------------------------------


def run_role(rid: str, inp: str, gate_override: str | None,
             timeout: float, max_tokens: int,
             canary: bool = True) -> tuple[int, dict]:
    card = load_card(rid)
    gate = card["gate"]
    if gate_override:
        if gate == "charter":
            raise RoleRefusal(
                "R4's gate is the charter's ratified landing gate; it is not a "
                "command-line flag. Change the charter by operator-approved PR.")
        gate = gate_override

    row = {"ts": dt.datetime.now(dt.timezone.utc).isoformat(), "role": rid,
           "name": card["name"], "gate": gate,
           "gate_overridden": bool(gate_override), "engine": card["engine"],
           "consumer": card["consumer"], "pin": M.SEAT_ENGINE_OF_RECORD,
           "model": None, "schema_accepted": None, "consumer_accepted": None,
           "outcome": None, "detail": None}

    # BEFORE the real call, not after: a run whose instrument is broken
    # should not spend the real call at all, and a canary that only runs
    # on success is a canary that never sees a bad day.
    if canary:
        cres = run_canary(rid, timeout, max_tokens)
        row["canary"] = cres or "none for this role"
    else:
        row["canary"] = "SKIPPED (--no-canary)"

    if card["engine"] == "script":
        row["model"] = "(none — code)" if rid == "R3" else M.SEAT_ENGINE_OF_RECORD
        accepted, detail, stdout = run_script_role(rid, inp, timeout)
        row.update({"schema_accepted": None, "consumer_accepted": accepted,
                    "detail": detail})
        payload = {"stdout": stdout}
    else:
        try:
            raw, model = call_daemon(card["prose"] + "\n\n=== INPUT ===\n" + inp,
                                     timeout, max_tokens,
                                     schema=card["schema_obj"],
                                     schema_name=rid.lower())
        except EngineDrift:
            raise
        except Exception as e:                   # noqa: BLE001
            raise RoleRefusal(f"the engine did not answer "
                              f"({type(e).__name__}: {e})")
        row["model"] = model

        if rid == "R4":
            payload, malformed = extract_verdict(raw or "")
            if malformed:
                row["schema_accepted"] = False
                raise RoleRefusal(f"reply was not a well-formed verdict "
                                  f"({malformed})")
        else:
            payload = first_json_object(raw or "")
            if payload is None:
                row["schema_accepted"] = False
                raise RoleRefusal("reply contained no balanced JSON object")
        row["schema_accepted"] = True

        consumer = CONSUMERS.get(rid)
        if consumer is None:
            raise RoleRefusal(f"{rid} has engine 'model' but no consumer wired")
        accepted, detail = consumer(payload, inp)
        row.update({"consumer_accepted": accepted, "detail": detail})

    if not accepted:
        row["outcome"] = "rejected"
        audit(row)
        print(f"{rid} REJECTED by {card['consumer']} — {row['detail']}")
        return 1, row

    if gate == "propose":
        # Nothing queued, nothing landed. The payload rides in the audit
        # row so a caller (the console) reads structured steps rather
        # than scraping them back out of this printout.
        row.update({"outcome": "proposed", "payload": payload})
        audit(row)
        for s in payload.get("steps", []):
            print(f"  {s['role']}  {s['input'][:70]}")
            print(f"      why: {s['why'][:100]}")
        if payload.get("note"):
            print(f"  note: {payload['note'][:200]}")
        return 0, row

    if gate == "draft":
        did = queue_draft(rid, card, payload,
                          payload.get("basis", []) if isinstance(payload, dict) else [])
        row.update({"outcome": "queued", "directive_id": did})
        audit(row)
        print(f"{rid} QUEUED as pending directive {did} — {row['detail']}")
        print("   nothing landed; approve, edit or reject it from the console")
        return 0, row

    row["outcome"] = "accepted"
    audit(row)
    print(f"{rid} ACCEPTED ({gate}) — {row['detail']}")
    return 0, row


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(
        prog="co-role.py",
        description="Drive one seat role on the local stack.")
    ap.add_argument("role", nargs="?", help="R1..R6")
    ap.add_argument("--input", default="",
                    help="a file path, or the literal argument for a script "
                         "role (R3: campaign id; R6: item ids)")
    ap.add_argument("--draft", action="store_const", const="draft",
                    dest="gate", help="override the card's gate to draft")
    ap.add_argument("--auto", action="store_const", const="auto", dest="gate",
                    help="override the card's gate to auto")
    ap.add_argument("--lint", action="store_true",
                    help="parse every card and check the token cap; no model")
    ap.add_argument("--no-canary", action="store_true",
                    help="skip the planted-defect canary; the skip is recorded "
                         "in the audit row, never silent")
    ap.add_argument("--canary-only", action="store_true",
                    help="run just the canary and report what came back — how "
                         "you confirm it can fire at all")
    ap.add_argument("--timeout", type=float, default=600.0)
    ap.add_argument("--max-tokens", type=int, default=900)
    ap.add_argument("--json", action="store_true",
                    help="print the audit row on stdout")
    a = ap.parse_args(argv)

    if a.lint:
        return lint_cards()
    if not a.role:
        ap.error("a role (R1..R6) or --lint is required")
    rid = a.role.upper()

    inp = a.input
    p = Path(inp)
    if inp and p.exists() and p.is_file():
        inp = p.read_text(encoding="utf-8")

    try:
        if a.canary_only:
            res = run_canary(rid, a.timeout, a.max_tokens)
            if res is None:
                print(f"{rid} has no canary "
                      f"(gym/comaintainer/canaries/{rid}.md absent)")
                return 2
            print(f"{rid} canary OK — answered {res['verdict']!r}, "
                  f"forbidden is {res['forbidden']!r}")
            return 0
        code, row = run_role(rid, inp, a.gate, a.timeout, a.max_tokens,
                             canary=not a.no_canary)
    except CanaryHalt as e:
        audit({"ts": dt.datetime.now(dt.timezone.utc).isoformat(), "role": rid,
               "outcome": "canary-halt", "detail": str(e)})
        print(f"HALT — {e}", file=sys.stderr)
        return 4
    except EngineDrift as e:
        # Not a could-not-judge. The reply came from a model nobody asked
        # for, so there is nothing to judge (§18.3).
        print(f"{rid} VOID — {e}", file=sys.stderr)
        return 3
    except RoleRefusal as e:
        row = {"ts": dt.datetime.now(dt.timezone.utc).isoformat(), "role": rid,
               "outcome": "could-not-judge", "detail": str(e)}
        audit(row)
        print(f"{rid} COULD-NOT-JUDGE — {e}", file=sys.stderr)
        if a.json:
            print(json.dumps(row, ensure_ascii=False))
        return 2
    if a.json:
        print(json.dumps(row, ensure_ascii=False))
    return code


if __name__ == "__main__":
    raise SystemExit(main())
