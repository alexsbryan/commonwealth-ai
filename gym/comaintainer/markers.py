#!/usr/bin/env python3
"""One home for every regex, table, and shared helper in the comaintainer gym.

ARCH §10.6 (one decider, one name): the harvester, the validator, the
scorer and the seat all import from here. A verdict marker that lives in
two files will drift into two verdict markers; a split recomputed from a
second implementation cannot detect a stamping bug in the first.

Everything here is deterministic and model-free. No RNG anywhere in the
gym's data path — same inputs, same bank.
"""

from __future__ import annotations

import gzip
import json
import re
from pathlib import Path

HERE = Path(__file__).resolve().parent

# ---- the closed verdict set (ARCH §2: closed sets are enums) ----------

VERDICTS = (
    "approve",
    "revise",
    "measure-first",
    "split",
    "escalate",
    "could-not-judge",
)

# Which argument field each verdict must carry (COMAINTAINER.md §4.1).
# An unknown verdict string is `malformed_bad_verdict`, never coerced.
ARG_OF = {
    "approve": "citations",
    "revise": "ask",
    "measure-first": "instrument",
    "split": "scopes",
    "escalate": "question",
    "could-not-judge": "missing",
}

# The SHAPE of each argument field, and the minimum contract.txt demands
# of it: split needs ">=2 separable concerns", the rest need one of
# whatever they are. Keyed by argument name and pinned against ARG_OF
# below, so a seventh verdict cannot be added without deciding what its
# argument looks like (ARCH §7 — structural, not remembered).
ARG_SHAPE = {
    "citations": ("array", 1),
    "ask": ("string", 1),
    "instrument": ("string", 1),
    "scopes": ("array", 2),
    "question": ("string", 1),
    "missing": ("string", 1),
}
assert set(ARG_SHAPE) == set(ARG_OF.values()), (
    "ARG_SHAPE and ARG_OF disagree about the argument fields: "
    f"{sorted(set(ARG_SHAPE) ^ set(ARG_OF.values()))}"
)


def verdict_schema() -> dict:
    """contract.txt as a JSON Schema, one branch per verdict.

    Passed to the daemon as `response_format: {type: json_schema, ...}`,
    which llguidance turns into a sampling grammar — so `malformed_no_json`,
    `malformed_bad_verdict` and `malformed_missing_arg` all become
    UNREACHABLE rather than honestly reported after the fact. That was the
    2026-08-10 repair: 8 of 36 verdicts (22%) in verdicts.jsonl were
    could-not-judge, 6 of them from an unconstrained reply.

    Built from VERDICTS/ARG_OF/ARG_SHAPE rather than written out, because
    a schema hand-copied from the contract is a second decider that will
    drift from it (ARCH §10.6).

    MEASURED CONSTRAINT: each branch must carry its own `"type": "object"`.
    A oneOf whose branches declare only `properties`/`required` is DROPPED
    by the daemon silently — the call still returns 200 and the model
    generates free prose. Verified live 2026-08-10 on
    FINAL-Bench_Darwin-36B-Opus-Q6_K: untyped branches produced an essay,
    typed branches produced conforming JSON.

    `basis` is required as a KEY but not constrained to be non-empty.
    A grammar that forces an anchor forces the model to invent one when it
    has none, and the whole point of basis is that anchors are real
    (contract.txt); whether it is populated is measured by score.py's
    basis-exists / basis-bears numbers, not mandated by the sampler.
    """
    branches = []
    for verdict in VERDICTS:
        arg = ARG_OF[verdict]
        kind, minimum = ARG_SHAPE[arg]
        if kind == "array":
            arg_schema = {"type": "array", "items": {"type": "string"},
                          "minItems": minimum}
        else:
            arg_schema = {"type": "string", "minLength": minimum}
        branches.append({
            "type": "object",
            "properties": {
                "verdict": {"type": "string", "const": verdict},
                arg: arg_schema,
                "basis": {"type": "array", "items": {"type": "string"}},
                "rationale": {"type": "string", "minLength": 1},
            },
            "required": ["verdict", arg, "basis", "rationale"],
            "additionalProperties": False,
        })
    return {"oneOf": branches}


# The reply that actually failed, five times, in verdicts.jsonl between
# 2026-08-06 and 2026-08-09: a verdict with EVERY argument field present
# and all of them empty. extract_verdict reads ARG_OF[verdict], finds "",
# and returns malformed_missing_arg. This is the named failing input the
# schema exists to rule out (ARCH §18.1 — a check with no failing input
# you can name is not a check), kept here so the pin below can test the
# schema against the real thing rather than an invented one.
RECORDED_MISSING_ARG_REPLY = {
    "verdict": "approve", "citations": [], "ask": "", "instrument": "",
    "scopes": [], "question": "", "missing": "", "basis": [],
    "rationale": "…",
}


def verdict_schema_problems() -> list[str]:
    """Pin: the schema must actually forbid RECORDED_MISSING_ARG_REPLY.

    Run from validate_episodes.py alongside field_vocab_problems(). A
    schema that has drifted permissive — additionalProperties re-enabled,
    a minimum dropped, a branch missing its `type` (which the daemon drops
    SILENTLY, see verdict_schema) — would let the exact 2026-08 failure
    back in while every other test stayed green.
    """
    problems = []
    branches = verdict_schema().get("oneOf", [])
    if len(branches) != len(VERDICTS):
        problems.append(f"schema has {len(branches)} branches for "
                        f"{len(VERDICTS)} verdicts")
        return problems
    for verdict, branch in zip(VERDICTS, branches):
        arg = ARG_OF[verdict]
        props = branch.get("properties", {})
        where = f"branch {verdict!r}"
        if branch.get("type") != "object":
            problems.append(f"{where}: no \"type\": \"object\" — the daemon "
                            f"drops such a schema silently")
        if branch.get("additionalProperties") is not False:
            problems.append(f"{where}: additionalProperties is not false, so "
                            f"the all-fields-empty reply is still legal")
        if props.get("verdict", {}).get("const") != verdict:
            problems.append(f"{where}: verdict is not pinned to a const")
        for key in ("verdict", arg, "basis", "rationale"):
            if key not in branch.get("required", []):
                problems.append(f"{where}: {key!r} is not required")
        spec = props.get(arg, {})
        if spec.get("minItems", 0) < 1 and spec.get("minLength", 0) < 1:
            problems.append(f"{where}: {arg!r} may be empty — this is the "
                            f"malformed_missing_arg shape")
        # And the direct test: would the recorded failure pass this branch?
        recorded = RECORDED_MISSING_ARG_REPLY
        extra = set(recorded) - set(props)
        empty_arg = not recorded.get(arg)
        if not extra and not empty_arg:
            problems.append(f"{where}: accepts the recorded 2026-08 "
                            f"malformed reply verbatim")
    return problems


# Coarse triage bucketing (score.py's second metric): what the operator
# would DO with the verdict — land it, bounce it back, or defer it.
COARSE_OF = {
    "approve": "LAND",
    "revise": "BOUNCE",
    "split": "BOUNCE",
    "measure-first": "DEFER",
    "escalate": "DEFER",
    "could-not-judge": "DEFER",
}

# ---- field-evidence anchors (docs/FIELD_VERDICTS.md, Scene 1) --------
# Closed set (ARCH §2.1). The class names also appear as prose in
# contract.txt and CHARTER.md (wire contract, §2.2 alias form);
# field_vocab_problems() is the equivalence pin that keeps those copies
# honest (§10.6 — one decider). Add a class HERE; the pin fails until
# the prose copies agree.

FIELD_CLASSES = (
    "offender",
    "tollbooth",
    "bridge",
    "dup",
    "tax",
    "layer-violation",
)

FIELD_ANCHOR_RE = re.compile(
    r"field:(" + "|".join(re.escape(c) for c in FIELD_CLASSES) + r"):(\S+)")

# The renderer's own violation filter (code_fieldglass/mod.rs, headline
# count) — mirrored once, here, for every python consumer.
FLOW_VIOLATION_KINDS = ("upward", "forbidden")


def field_vocab_problems() -> list[str]:
    """Pin the contract/charter prose against FIELD_CLASSES. The exact
    `a|b|c` alternation string must appear verbatim in both files, so a
    class added in one home fails loudly everywhere the gym validates."""
    want = "|".join(FIELD_CLASSES)
    return [
        f"{name} does not declare the field classes `{want}` verbatim"
        for name in ("contract.txt", "CHARTER.md")
        if want not in (HERE / name).read_text()
    ]


def sidecar_path(repo: Path) -> tuple[Path | None, str]:
    """Resolve THIS repo's fieldglass sidecar — one decider for every
    seat surface (bundle, resolver, landing diff).

    Generalized, not home-context: multi-repo hosts are real. The
    project registry's root->corpus binding wins; a repo that IS
    registered but has no render resolves ABSENT (never borrow another
    repo's field); the newest-sidecar guess is allowed only for
    unregistered repos and is NAMED so a wrong-corpus grab is loud,
    never quiet.

    Returns (path or None, how), how in
    {"registry", "newest-fallback", "absent"}.
    """
    arch = Path.home() / ".sovereign" / "arch"
    corpus = registry_corpus(repo)
    if corpus is not None:
        cand = arch / corpus / "fieldglass.json"
        if cand.exists():
            return cand, "registry"
        return None, "absent"
    cands = sorted(arch.glob("*/fieldglass.json"),
                   key=lambda q: q.stat().st_mtime, reverse=True)
    if cands:
        return cands[0], "newest-fallback"
    return None, "absent"


def registry_corpus(repo: Path) -> str | None:
    """The project registry's corpus id for this repo root, or None when
    the repo is unregistered (or the registry is absent/unreadable)."""
    try:
        for p in json.loads(
                (Path.home() / ".sovereign" / "projects.json").read_text()):
            if Path(p.get("root", "")).resolve() == repo.resolve():
                return p["corpus_id"]
    except (OSError, json.JSONDecodeError, KeyError):
        pass
    return None


# ---- verdict-marker regexes ------------------------------------------
# These mine the HOUSE's own verdict language out of commits, notes and
# ledger rows. They are also the leakage linter's tripwire: any of these
# appearing in a `request` block is the answer key printed on the exam.

REJECT_RE = re.compile(
    r"(?i)\b("
    r"rejected|reverts?|reverted|overturn(?:ed|s)?|withdrawn?|withdrawal"
    r"|net.negative|dominated|did not separate|nothing separates"
    r"|no speedup|made things worse|worse,? not fixed|refuted"
    r"|stays? (?:off|unset|dark|0\.0|`?0`?)|do not re.litigate"
    r"|not promoted|kill(?:ed)? th(?:is|e) row|abandon(?:ed)?"
    r"|destroys? \d+%|off the table"
    r")\b"
)

APPROVE_RE = re.compile(
    r"(?i)\b("
    r"earned the default|flip condition (?:is )?met|graduated"
    r"|default (?:flipped )?on|promoted|proven|validated"
    r"|parity (?:held|proven|passed)|soak pass(?:ed)?|both halves .{0,20}met"
    r"|moved (?:to )?graduated"
    r")\b"
)

# Evidence that a MEASUREMENT backs the sentence — numbers with units,
# statistics, named instruments. Two independent hits are required to
# call a paragraph "evidence" (one number can be a version string).
MEASURE_RE = re.compile(
    r"(?i)("
    r"\b\d+(?:\.\d+)?\s*(?:%|pp|ms|s\b|sec|seconds|minutes?|min\b|t/s|tok/s|MB|GB|KB)"
    r"|\bp\s*=\s*0?\.\d+|\bn\s*=\s*\d+|\bCI\b|Wilson|p50|p90|p95|p99"
    r"|\b\d+\s*/\s*\d+\b|\bx\d+\b|\b\d+(?:\.\d+)?x\b|→|->"
    r"|\bbench\b|\bA/B\b|\bpaired\b|\bsoak\b|baseline|measured"
    r"|\bdelta\b|Δ|\bmean\b|\bmedian\b"
    r")"
)

# Lines that are the operator's to decide, never the comaintainer's
# (COMAINTAINER.md §4.4 boundaries).
ESCALATE_RE = re.compile(
    r"(?i)\b("
    r"operator call|operator directive|product call|operator.owned"
    r"|product priority|taste|budget call|privacy call|operator decides"
    r"|ask the operator|operator['’]s call"
    r")\b"
)

# Explicit could-not-judge language (rare in history; mostly constructed).
CNJ_RE = re.compile(
    r"(?i)\b("
    r"could.not.judge|cannot judge|could not be judged|unjudgeable"
    r"|no instrument exists|not measurable with|never.ran"
    r")\b"
)

# Ledger state parentheticals in headings — `(stays unset)`, `(off)`,
# `(state)` — are the label riding in the title. Always stripped from
# any text that reaches a request block.
LEDGER_STATE_RE = re.compile(
    r"\s*[—–-]?\s*\((?:stays? [^)]*|off|on|unset|default [^)]*|0\.\d+[^)]*)\)"
    r"|\s*→\s*\*\*[A-Z]+[^*]*\*\*"
)

# Result-marker sentences in attempt notes: the part that says how the
# attempt ENDED, which is expect-side material.
ATTEMPT_RESULT_RE = re.compile(
    r"(?i)\b("
    r"tripped|crashed|regress(?:ed|ion)|made things worse|no difference"
    r"|net.negative|off.by.one|deadlock(?:ed)?|hang(?:s|ed)?|OOM"
    r"|failed|fails\b|surfaced|materialized|buys nothing|does not (?:already )?provide"
    r"|worse|slower|no speedup|did not separate|not fixed"
    r"|corrupts?\b|do not use|rejects\b|exhausted"
    r")\b"
)

# ---- transcript mining markers ---------------------------------------
# Operator turns that are corrections vs go-aheads. Weakest signal class
# (d) in the mining ladder — capped low by the harvester.

CORRECTION_RE = re.compile(
    r"(?i)^(no\b|not\b|nope\b|actually\b|instead\b|wait\b|don'?t\b|stop\b"
    r"|rather\b|hold on\b|that'?s (?:not|wrong)\b|wrong\b|revert\b|undo\b)"
)

GOAHEAD_RE = re.compile(
    r"(?i)^(yes|yep|yeah|go ahead|proceed|lgtm|ok(?:ay)?|do it|ship it"
    r"|sounds good|approved|perfect|continue|sure)\b[^?]{0,25}$"
)

# Gate receipts in commit bodies — evidence the author ran the gates.
RECEIPT_RE = re.compile(
    r"(?i)("
    r"exit(?:s|ed)? 0|tests? pass(?:ed|ing)?|\d+ tests? pass|pass: \d+"
    r"|lint (?:clean|pass(?:ed)?)|both (?:scripts |gates )?(?:green|exit 0)"
    r"|workspace (?:green|clean)|cargo (?:test|check) (?:green|clean|passes)"
    r")"
)

# A concrete artifact path in evidence (for -t2 artifact-elided twins).
ARTIFACT_PATH_RE = re.compile(
    r"\b(?:target|sovereign|research|gym|scripts|docs|bench)"
    r"/[A-Za-z0-9_\-./]+\.(?:json|jsonl|md|txt|toml|log)\b"
    r"|`~/[A-Za-z0-9_\-./]+`"
)

# A named, addressable instrument (for -t1 evidence-stripped twins).
# `bench` needs a lane-shaped argument (a path or hyphenated lane name):
# an audit pass found the loose form matching prose fragments ("bench
# that", "Bench note") and stamping them as instruments.
INSTRUMENT_RE = re.compile(
    r"(?i)\b(?:"
    r"(?:svrn |sovereign )?bench [a-z0-9_]+(?:[/-][a-z0-9_\-]+)+"
    r"|(?:svrn |sovereign )?bench (?:all|enrichment-ablate|sep|summarize|governance)"
    r"|sovereign-ci-bench\.sh(?: --quick)?"
    r"|eval run(?: --[a-z\-]+)*|chaos.monkey|contract (?:census|nightly)"
    r"|retrieval-prod|[a-z0-9_\-]{4,} soak|controlled A/B|paired (?:run|bank|slice)"
    r")\b"
)

# Harness/system noise that must never become an episode's proposal —
# these are transport failures the transcript records, not agent
# proposals the operator judged.
HARNESS_NOISE_RE = re.compile(
    r"(?i)^\s*(API Error|Login expired|Prompt is too long|Context left|"
    r"Credit balance|\[Request interrupted)"
)

# ---- transcript context scope (operator caveat, 2026-08-06) ----------
# A transcript correction is either STANDING policy (it recurs across
# sessions, or it matches a recorded feedback memory — the judge is
# expected to know it) or a SITUATED one-off (it made sense only inside
# that session — scored in a tracked steering lane, never in dev
# exact-6). Closed set (§2); the flag rides each transcript episode.

SCOPE_FLAGS = ("standing", "situated")

# Function words only — domain words ("tests", "bench", "release") are
# exactly the signal the scope matcher needs, so they stay.
_SCOPE_STOPWORDS = frozenset(
    "this that with have from then than they them what were will would "
    "should could about just only also been into your does doing there "
    "their these those some more most over after before being very much "
    "here want wants need needs like when where which while whose still "
    "them because let's lets please actually instead rather don't dont "
    "stop wait hold".split()
)


def content_words(text: str) -> set[str]:
    """Lowercased tokens >=4 chars minus function words — the unit both
    scope tests (feedback-memory match, cross-session recurrence)
    compare on. One implementation (§10.6)."""
    return {t for t in re.findall(r"[a-z0-9_\-]{4,}", norm(text))
            if t not in _SCOPE_STOPWORDS}

# ---- secret / PII scrub (leak_secret) --------------------------------
# Zero hits required for a bank to commit. Applied to EVERY field of
# EVERY episode, request and expect alike.

SECRET_RES = [
    ("api_key", re.compile(r"\b(sk-[A-Za-z0-9]{16,}|ghp_[A-Za-z0-9]{20,}|gho_[A-Za-z0-9]{20,}|AKIA[0-9A-Z]{16})\b")),
    ("bearer", re.compile(r"(?i)\b(bearer|authorization)\s*[:=]\s*[A-Za-z0-9_\-\.]{16,}")),
    ("assignment", re.compile(r"(?i)\b(api_?key|secret|token|password|passwd)\s*[:=]\s*['\"]?[A-Za-z0-9_\-\.]{12,}")),
    ("credential_url", re.compile(r"[a-z][a-z0-9+.\-]*://[^\s/@]+:[^\s/@]+@")),
    ("private_key", re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----")),
    # Any email that is not the repo author's is PII we do not commit.
    ("email", re.compile(r"\b[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}\b")),
]
ALLOWED_EMAILS = {"alexbryan01@gmail.com", "noreply@anthropic.com"}


def secret_hits(text: str) -> list[str]:
    """Names of secret patterns present in `text` (empty = clean)."""
    hits = []
    for name, rx in SECRET_RES:
        for m in rx.finditer(text):
            if name == "email" and m.group(0).lower() in ALLOWED_EMAILS:
                continue
            hits.append(name)
            break
    return hits


# ---- engine of record -------------------------------------------------
# Every committed number in README.md was produced by this model at
# temp 0 (noise floor measured exactly zero ON THIS ENGINE — the
# pedigree does not transfer). score.py refuses a daemon serving
# anything else unless --allow-engine-drift names the substitution.
# Earned 2026-08-06: a daemon restart for unrelated mesh work swapped
# the primary to a Qwen 35B and a full dev run scored on the wrong
# judge before anyone noticed (§18.3: never silently substitute).
ENGINE_OF_RECORD = "Darwin-36B"   # substring of the served model id

# ---- caps and floors (validator enforces; harvester targets) ---------

SOURCE_CAPS = {
    "ledger": 20,
    "commit": 40,
    "attempt": 22,
    "decision": 120,
    "tripwire": 100,   # 50 planted/clean pairs
    "constructed": 60,
    "transcript": 80,
    "fixchain": 40,
    "twin": 65,        # 45 -t1 + 20 -t2
}

# revise and approve supply outstrip the other classes ~3:1. With the
# four small classes totalling ~97 episodes, the ceiling algebra pins
# both large caps: C <= 0.35*(2C + 97) => C <= 113. The plan's 130/120
# breached the 35% ceiling at the realized bank size — trimmed here,
# recorded in README.
CLASS_CAPS = {
    "revise": 113,
    "approve": 113,
    "measure-first": 80,
    "could-not-judge": 30,
    "escalate": 22,
    "split": 20,
}
CLASS_FLOOR = 12          # min episodes per verdict class
CLASS_CEILING_SHARE = 0.35  # max share of the bank one class may hold
BANK_FLOOR = 300

# Request-side length bounds (chars): situation / proposal / evidence.
LEN_BOUNDS = {"situation": 4000, "proposal": 8000, "evidence": 4000}

REQUIRED_SOURCES = (
    "ledger", "commit", "attempt", "decision", "tripwire",
    "constructed", "transcript", "fixchain",
)


# ---- shared helpers ---------------------------------------------------


def slugify(heading: str) -> str:
    """Ledger heading -> the slug both the harvester's basis strings and
    the validator's resolution table use. One implementation, or a
    stamped slug could never fail to resolve (§10.6). Cuts at the first
    em-dash/arrow (the flag/state tail), then drops backticks — cutting
    at the first backtick instead would slug a heading that STARTS with
    a flag name to empty."""
    base = LEDGER_STATE_RE.sub("", re.sub(r"[—–→].*", "", heading))
    return re.sub(r"[^a-z0-9]+", "-", base.replace("`", "").lower()).strip("-")[:40]


def lint_leaks(ep: dict) -> list[tuple[str, str]]:
    """Leakage + secret checks shared by the harvester (drop + count)
    and the validator (exit 1). One implementation — a linter the
    harvester and validator disagree about is two linters (§10.6).
    Returns (check_name, detail) pairs; empty = clean."""
    req = "\n".join((ep["request"]["situation"], ep["request"]["proposal"],
                     ep["request"]["evidence"]))
    hits: list[tuple[str, str]] = []
    for rx in (REJECT_RE, APPROVE_RE):
        m = rx.search(req)
        if m:
            hits.append(("leak_verdict_marker", m.group(0)[:60]))
    m = LEDGER_STATE_RE.search(req)
    if m:
        hits.append(("leak_ledger_state", m.group(0)[:60]))
    for b in ep["expect"]["basis"]:
        for hex8 in re.findall(r"\b([0-9a-f]{8})\b", b):
            if hex8 in req:
                hits.append(("leak_basis_id", hex8))
    req_sh = shingles(req)
    expect_texts = [("rationale", ep["expect"].get("rationale", ""))]
    arg = ep["expect"].get(ARG_OF[ep["expect"]["verdict"]])
    if isinstance(arg, str):
        expect_texts.append(("arg", arg))
    elif isinstance(arg, list):
        expect_texts += [("arg", a) for a in arg if isinstance(a, str)]
    for field, text in expect_texts:
        if not text:
            continue
        overlap = shingles(text) & req_sh
        if overlap:
            name = ("leak_flip_condition" if ep["source"] == "ledger"
                    else "leak_rationale_shingle")
            hits.append((name, f"{field}: " + " ".join(next(iter(overlap)))))
            break
    all_text = req + "\n" + "\n".join(t for _, t in expect_texts)
    for hit in secret_hits(all_text):
        hits.append(("leak_secret", hit))
    return hits


def norm(s: str) -> str:
    """Whitespace-agnostic normal form (mirrors next-edit's ruler)."""
    return re.sub(r"\s+", " ", s.replace("\r\n", "\n")).strip().lower()


def shingles(s: str, n: int = 6) -> set[tuple[str, ...]]:
    """n-token shingles of normalised text, for leakage + dedupe.

    6 tokens is long enough that a shared shingle means shared PROSE,
    not shared vocabulary."""
    toks = re.findall(r"[a-z0-9_§\.]+", norm(s))
    return {tuple(toks[i : i + n]) for i in range(max(0, len(toks) - n + 1))}


def split_of(source: str, tier: str, verdict: str, index: int) -> str:
    """Deterministic dev/holdout assignment.

    Stratum = (source, tier, expect.verdict); within a stratum the
    episodes are sorted by id and every third (i % 3 == 2) goes to
    holdout. Stamped at harvest, recomputed by the validator FROM THIS
    SAME FUNCTION — a mismatch cannot ship (ARCH §7)."""
    del source, tier, verdict  # stratum is the caller's grouping key
    return "holdout" if index % 3 == 2 else "dev"


def read_bank(path: str | Path) -> list[dict]:
    """Read a bank, gzipped or not (mirrors next-edit's read_cases)."""
    p = Path(path)
    if not p.exists() and Path(str(p) + ".gz").exists():
        p = Path(str(p) + ".gz")
    blob = p.read_bytes()
    if p.suffix == ".gz":
        blob = gzip.decompress(blob)
    return [json.loads(l) for l in blob.decode().splitlines() if l.strip()]


def write_bank(path: str | Path, episodes: list[dict]) -> None:
    blob = "".join(json.dumps(e, ensure_ascii=False) + "\n" for e in episodes).encode()
    p = Path(path)
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_bytes(gzip.compress(blob) if str(p).endswith(".gz") else blob)


def signature(ep: dict) -> tuple:
    """Dedupe key: same source-family + same normalised proposal prefix
    cannot enter the bank twice."""
    req = ep["request"]
    return (ep["source"], norm(req["proposal"])[:240], ep["expect"]["verdict"])
