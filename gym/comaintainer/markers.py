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
    r"tripped|crashed|regressed|made things worse|no difference"
    r"|net.negative|off.by.one|deadlock(?:ed)?|hang(?:s|ed)?|OOM"
    r"|failed|fails\b|surfaced|materialized|buys nothing|does not (?:already )?provide"
    r"|worse|slower|no speedup|did not separate|not fixed"
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
INSTRUMENT_RE = re.compile(
    r"(?i)\b(?:"
    r"(?:svrn |sovereign )?bench [a-z0-9/_\-]+"
    r"|sovereign-ci-bench\.sh(?: --quick)?"
    r"|eval run[a-z0-9 /_\-]*|chaos.monkey|contract (?:census|nightly)"
    r"|retrieval-prod|[a-z0-9_\-]+ (?:soak|A/B)|paired (?:run|bank|slice)"
    r")\b"
)

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
