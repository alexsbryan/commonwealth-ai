#!/usr/bin/env python3
"""co_liveness.py — does this backlog item still reproduce at HEAD?

THE PROBLEM THIS EXISTS FOR. `VETTED` has never meant "still true".
It means clean header + Done-when + Evidence + a stated Approach — all
properties of the ITEM, none of the CODE. So an item stays vetted, ranked
and pullable long after the defect it describes was fixed. Measured
2026-08-12: three of the top four vetted items were already closed, two of
them by a commit that landed three days BEFORE the item was filed (seat
finding 14e2bcb3). A worker spawned on any of them would have gone hunting
a bug that no longer exists.

LEVEL-TRIGGERED, BY CONSTRUCTION. The only question asked here is "does
this still reproduce at HEAD?", and the answer depends on nothing but the
working tree in front of you. There is no mark, no cursor, no "commits
since last run", and therefore no queue that grows while nobody is
looking. Skipping this for a month costs exactly one run to recover, and
that run does the same work as a run today would (operator constraint,
2026-08-12: "every system we build has to be resilient to a messy process
that doesn't always orchestrate fully").

Contrast, deliberately, with `scripts/co-sweep.sh`: that one holds a
high-water mark and a 20-commit cap, and has been structurally unable to
catch up for six nights running. That shape is what this file must not be.

WORK IS PROPORTIONAL TO WHAT IS ABOUT TO BE TRUSTED. `--pull` verifies the
one-to-three items it is about to hand out, at the moment it hands them
out. A full-heap pass (`verify --all`) is a convenience that pre-populates
the ledger so the page can SHOW ages; it is never a precondition for a
correct pull.

    python3 scripts/co_liveness.py verify <id> [<id>...]   # judge N items
    python3 scripts/co_liveness.py verify --all            # the whole heap
    python3 scripts/co_liveness.py ledger                  # what is recorded
    python3 scripts/co_liveness.py candidates <commit>     # 2b, the accelerator

THE LEDGER IS THE SEAT'S EXISTING JUDGMENT LOG. Verdicts append to
`~/.sovereign/comaintainer/verdicts.jsonl` as records with
`kind="liveness"`, beside the landing verdicts co-review.sh already writes
there. No new store (principle 11 — the surface already existed and the
order forbids minting another). Reading it is level-triggered too: newest
record per item wins, an absent record means "never verified", and
deleting the whole file degrades to "nothing has been verified", which is
honest and costs one pull to recover. Nothing in it can fall behind
because nothing in it is a position.

NOTHING HERE AUTO-RETIRES. A `dead` verdict is a PROPOSAL. The seat
retires, with the citation this file produced as the pointer. An automated
retire is how a wrong judgment becomes invisible.

THE ENGINE IS THE LOCAL DAEMON, never Claude tokens (operator directive
63b8fa6e). `call_daemon` is imported from gym/comaintainer/score.py rather
than re-written, so there is one implementation of the schema-forced
model call and co-review.sh, the gym and this file cannot drift apart
(ARCH §10.6).
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# One implementation of the schema-forced daemon call, imported from where
# it already lived. co-review.sh takes the same import; the gym owns it.
sys.path.insert(0, str(REPO / "gym" / "comaintainer"))
try:
    from score import call_daemon  # noqa: E402
except Exception as _exc:  # pragma: no cover - named, never defaulted
    call_daemon = None
    _CALL_DAEMON_ERR = _exc
else:
    _CALL_DAEMON_ERR = None


# --- the ledger -----------------------------------------------------------
#
# The seat's own append-only judgment log, which already exists and is
# already read by the seat. `kind` discriminates: co-review.sh writes
# landing verdicts and overrides into the same file.

LIVENESS_KIND = "liveness"
CANDIDATE_KIND = "closure-candidate"

VERDICTS = ("alive", "dead", "could-not-judge")


def verdicts_log() -> Path:
    """CO_VERDICTS_LOG is the test override, and exists for the same
    reason CO_BACKLOG_NOTES_DB does: a self-test must not be able to
    append to the operator's real log."""
    env = os.environ.get("CO_VERDICTS_LOG")
    if env:
        return Path(env).expanduser()
    return Path.home() / ".sovereign" / "comaintainer" / "verdicts.jsonl"


class Liveness:
    """One judgment about one item. `verdict` is one of VERDICTS; a
    `could-not-judge` carries what was MISSING, never a defaulted
    'alive' (ARCH §18.3)."""

    def __init__(self, short, verdict, citation="", rationale="", engine="",
                 at=None, probes=0):
        self.short = short
        self.verdict = verdict if verdict in VERDICTS else "could-not-judge"
        self.citation = citation or ""
        self.rationale = rationale or ""
        self.engine = engine or ""
        self.at = at or dt.datetime.now(dt.timezone.utc).timestamp()
        self.probes = probes

    @property
    def age_days(self) -> float:
        now = dt.datetime.now(dt.timezone.utc).timestamp()
        return max(0.0, (now - self.at) / 86400.0)

    def to_record(self) -> dict:
        return {
            "ts": dt.datetime.fromtimestamp(self.at, dt.timezone.utc).isoformat(),
            "kind": LIVENESS_KIND,
            "item": self.short,
            "verdict": self.verdict,
            "citation": self.citation,
            "rationale": self.rationale,
            "engine": self.engine,
            "probes": self.probes,
        }

    @classmethod
    def from_record(cls, rec: dict):
        try:
            at = dt.datetime.fromisoformat(rec["ts"]).timestamp()
        except (KeyError, ValueError):
            return None
        if not rec.get("item"):
            return None
        return cls(rec["item"], rec.get("verdict", "could-not-judge"),
                   rec.get("citation", ""), rec.get("rationale", ""),
                   rec.get("engine", ""), at, int(rec.get("probes", 0) or 0))


def append_record(rec: dict, log: Path = None) -> Path:
    log = log or verdicts_log()
    log.parent.mkdir(parents=True, exist_ok=True)
    with open(log, "a", encoding="utf-8") as fh:
        fh.write(json.dumps(rec, ensure_ascii=False) + "\n")
    return log


def read_ledger(log: Path = None) -> dict:
    """{item_short: newest Liveness}. An absent or unreadable log is an
    EMPTY ledger — every item reads as never-verified, which is the honest
    degradation and costs one pull to recover. It is never an error, and
    it never blocks anything."""
    log = log or verdicts_log()
    out = {}
    try:
        text = log.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return out
    for line in text.splitlines():
        line = line.strip()
        if not line or LIVENESS_KIND not in line:
            continue
        try:
            rec = json.loads(line)
        except json.JSONDecodeError:
            continue
        if rec.get("kind") != LIVENESS_KIND:
            continue
        lv = Liveness.from_record(rec)
        if lv is None:
            continue
        prev = out.get(lv.short)
        if prev is None or lv.at >= prev.at:
            out[lv.short] = lv
    return out


# --- current-state probes -------------------------------------------------
#
# Everything below reads the WORKING TREE AT HEAD and nothing else. No
# `git log <mark>..HEAD`, no diff against a stored position — those are the
# queue-shaped questions this file exists to avoid.

FILE_CITE = re.compile(
    r"\b([\w][\w./-]*\.(?:rs|py|toml|md|sh|ts|tsx|js|json|ya?ml|html))"
    r"(?::(\d+)(?:\s*-\s*(\d+))?)?")
SHA_CITE = re.compile(r"\b([0-9a-f]{7,40})\b")
# Distinctive names worth grepping: Rust paths (`Foo::Bar`), CamelCase
# types, snake_case functions with an underscore. Bare English words are
# deliberately excluded — grepping "the" tells nobody anything.
SYMBOL = re.compile(r"\b(?:[A-Za-z_][\w]*::)+[A-Za-z_][\w]*\b"
                    r"|\b[A-Z][a-z0-9]+(?:[A-Z][A-Za-z0-9]*)+\b"
                    r"|\b[a-z][a-z0-9]*(?:_[a-z0-9]+){1,}\b")
QUOTED = re.compile(r"[\"'`]([^\"'`\n]{12,80})[\"'`]")

# Bounded so the judge call stays cheap however long the item is. An
# evidence bundle that grows with the item would make a long item cost
# more to verify than a short one, for no extra signal.
MAX_FILE_CITES = 6
MAX_SYMBOLS = 10
MAX_QUOTES = 3
MAX_SHAS = 4
MAX_AMBIGUOUS = 2      # a bare basename may name several files; probe this many
SYMBOL_HITS = 5
CONTEXT_BEFORE, CONTEXT_AFTER = 5, 12
DEF_BEFORE, DEF_AFTER = 2, 16
EVIDENCE_CHARS = 22000

# A grep hit that looks like the DEFINITION of the thing, not a mention of
# it. When the item's Done-when names a function, the definition is the
# evidence; the call sites usually are not.
DEFINITION = re.compile(r"\b(fn|struct|enum|trait|impl|type|const|static|def|"
                        r"class|function)\b")

_NOISE_SYMBOLS = {
    "Done_when", "Chunks_with", "Scored_by", "Verified_at", "co_backlog",
    "co_review", "co_sweep", "session_chunks", "session_chunk",
}


def _git(repo: Path, *args, timeout=20) -> str:
    try:
        out = subprocess.run(["git", "-C", str(repo), *args],
                             capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired):
        return ""
    return out.stdout if out.returncode == 0 else ""


def _resolve_path(repo: Path, cited: str) -> list:
    """-> [Path]. A citation may be a repo-relative path or a bare
    basename (`peer_inference.rs:1956`). Both resolve against the tracked
    file list at HEAD — never against a remembered location.

    AMBIGUITY IS NOT ABSENCE, and conflating the two is a defect this
    function had and was watched to cause: `brief.rs` matches several
    tracked files, the first cut returned None for that, and the bundle
    then told the judge "NO SUCH FILE AT HEAD" about a file that exists.
    The judge believed it, and a live item came back could-not-judge on a
    fabricated probe. An empty list here means genuinely absent; a list
    longer than one means ambiguous, and the caller says which."""
    direct = repo / cited
    if direct.is_file():
        return [direct]
    if "/" in cited:
        return []
    listed = _git(repo, "ls-files", f"*/{cited}", cited).splitlines()
    return [repo / p for p in listed if (repo / p).is_file()]


def _slice(path: Path, line: int) -> str:
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return ""
    lo = max(0, line - 1 - CONTEXT_BEFORE)
    hi = min(len(lines), line + CONTEXT_AFTER)
    return "\n".join(f"{n + 1}: {lines[n]}" for n in range(lo, hi))


def _field(body: str, key: str) -> str:
    m = re.search(rf"^{key}:[ \t]*(.*)$", body, re.M)
    return m.group(1).strip() if m else ""


def _priority_text(body: str) -> str:
    """The lines that carry the falsifiable claim, first.

    An item's `Done-when` IS the thing being tested, so the symbols and
    literals in it are the ones worth probing; the discovery prose below
    the header is background. Probing in body order spent the symbol
    budget on the prose and left the Done-when's own names unprobed —
    which is how a live item came back could-not-judge."""
    return "\n".join(x for x in (_field(body, "Done-when"),
                                 _field(body, "Evidence"),
                                 _field(body, "Approach"),
                                 _field(body, "Objective")) if x)


def _probe_symbol(repo: Path, sym: str) -> str:
    """One symbol, as it reads at HEAD: the count, the first few sites,
    and — when one of them looks like the DEFINITION — the code around
    it. A count alone settles nothing, and the judge says so."""
    hits = _git(repo, "grep", "-n", "-F", "--", sym).splitlines()
    if not hits:
        return f"--- symbol `{sym}`: NOT PRESENT anywhere at HEAD."
    out = [f"--- symbol `{sym}`: {len(hits)} occurrence(s) at HEAD:"]
    out.extend(hits[:SYMBOL_HITS])
    for h in hits[:20]:
        parts = h.split(":", 2)
        if len(parts) < 3 or not DEFINITION.search(parts[2]):
            continue
        p = repo / parts[0]
        if not p.is_file():
            continue
        try:
            lines = p.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError:
            break
        n = int(parts[1])
        lo, hi = max(0, n - 1 - DEF_BEFORE), min(len(lines), n + DEF_AFTER)
        out.append(f"  definition site {parts[0]}:{n} AS IT IS NOW:")
        out.extend(f"  {i + 1}: {lines[i]}" for i in range(lo, hi))
        break
    return "\n".join(out)


def dirty_files(repo: Path = REPO) -> set:
    """Paths with uncommitted changes right now.

    THE PROBES READ THE WORKING TREE, NOT HEAD. `_slice` opens the file
    on disk and `git grep` without a rev searches the worktree, so on a
    clean checkout "at HEAD" is exactly true and on a dirty one it is
    not. That gap produced a wrong verdict the first time this ran:
    item 71e845e1 came back `dead` citing
    `sovereign-core/src/runtime/grounding/mod.rs:633-661` while a
    concurrent worker had that very file modified and uncommitted — the
    fix it was judged against had not landed.

    Naming the file is the honest fix and it is cheap. Reading HEAD
    through `git show` instead would make the function's name true and
    is the better fix; it is a real refactor of every probe and is filed
    rather than smuggled in here."""
    out = set()
    for line in _git(repo, "status", "--porcelain").splitlines():
        p = line[3:].strip()
        if p:
            out.add(p)
    return out


def gather_evidence(body: str, repo: Path = REPO) -> tuple[str, int, list]:
    """-> (bundle, probe_count, dirty_paths_probed). Every probe reads
    current state.

    An absent file, a missing symbol and an unresolvable commit are all
    REPORTED in the bundle by name. The judge never receives a bundle that
    quietly omits a probe that failed, because "this citation no longer
    resolves" is often the whole answer — but see `_resolve_path`: a probe
    that cannot tell absence from ambiguity is worse than no probe, and
    that one was watched to produce a wrong verdict."""
    parts, probes = [], 0
    seen_files, seen_syms = set(), set()
    dirty = dirty_files(repo)
    touched_dirty = set()
    priority = _priority_text(body)
    # The Done-when's own names first, then everything else the item says.
    search_order = priority + "\n" + body

    # 1. The item's own file:line citations, as they read TODAY.
    for m in FILE_CITE.finditer(search_order):
        if len(seen_files) >= MAX_FILE_CITES:
            break
        cited, line = m.group(1), m.group(2)
        key = (cited, line)
        if key in seen_files:
            continue
        seen_files.add(key)
        probes += 1
        found = _resolve_path(repo, cited)
        if not found:
            parts.append(f"--- {cited}: NO SUCH FILE AT HEAD. The citation "
                         "does not resolve today.")
            continue
        if len(found) > 1:
            names = ", ".join(str(p.relative_to(repo)) for p in found[:6])
            parts.append(f"--- {cited}: EXISTS at HEAD but the basename is "
                         f"ambiguous ({len(found)} tracked files: {names}). "
                         "Probing the first few.")
        for p in found[:MAX_AMBIGUOUS]:
            rel = p.relative_to(repo)
            if str(rel) in dirty:
                touched_dirty.add(str(rel))
                parts.append(f"--- WARNING: {rel} has UNCOMMITTED CHANGES. "
                             "What follows is the working tree, which may "
                             "include work that has not landed. Do not call "
                             "the item closed on the strength of it.")
            if line:
                text = _slice(p, int(line))
                parts.append(f"--- {rel} around line {line} AS IT IS NOW:\n"
                             + (text or "(line is past the end of the file "
                                        "today — the file has shrunk)"))
            else:
                # A file cited with no line: show the lines in it that
                # mention what the item is about. "It exists" settles
                # nothing and the judge is told to say so.
                inner = []
                for sym in list(dict.fromkeys(SYMBOL.findall(priority)))[:6]:
                    if len(sym) < 6 or sym in _NOISE_SYMBOLS:
                        continue
                    inner += _git(repo, "grep", "-n", "-F", "--", sym,
                                  "--", str(rel)).splitlines()[:4]
                if inner:
                    parts.append(f"--- {rel} AS IT IS NOW, lines mentioning "
                                 "what this item is about:\n"
                                 + "\n".join(dict.fromkeys(inner))[:2500])
                else:
                    parts.append(f"--- {rel}: exists at HEAD "
                                 f"({p.stat().st_size} bytes), but nothing in "
                                 "it mentions this item's own names.")

    # 2. The distinctive names the item leans on: do they still exist?
    for m in SYMBOL.finditer(search_order):
        if len(seen_syms) >= MAX_SYMBOLS:
            break
        sym = m.group(0)
        if sym in seen_syms or sym in _NOISE_SYMBOLS or len(sym) < 6:
            continue
        seen_syms.add(sym)
        probes += 1
        parts.append(_probe_symbol(repo, sym))

    # 3. Literal strings the item quotes (error messages, config keys).
    for m in list(QUOTED.finditer(search_order))[:MAX_QUOTES]:
        q = m.group(1).strip()
        if len(q) < 12:
            continue
        probes += 1
        hits = _git(repo, "grep", "-n", "-F", "--", q).splitlines()
        parts.append(f"--- literal {q!r}: "
                     + (f"{len(hits)} occurrence(s) at HEAD, first:\n"
                        + "\n".join(hits[:2]) if hits
                        else "NOT PRESENT anywhere at HEAD."))

    # 4. Commits the item names: do they exist, and what did they say?
    #    This is a lookup, not a range — `git log <mark>..HEAD` is the
    #    shape that grows, and it is not used here.
    shas = []
    for m in SHA_CITE.finditer(body):
        sha = m.group(1)
        if sha in shas or len(shas) >= MAX_SHAS:
            continue
        subject = _git(repo, "log", "-1", "--format=%h %ad %s",
                       "--date=short", sha).strip()
        if subject:
            shas.append(sha)
            probes += 1
            parts.append(f"--- commit {sha}: IN HISTORY — {subject}")

    if not parts:
        parts.append("--- no citation in this item resolved to anything "
                     "probeable at HEAD (no file, symbol, literal or commit).")
    bundle = "\n\n".join(parts)
    if touched_dirty:
        bundle = ("NOTE: this evidence was read from a DIRTY working tree. "
                  "Uncommitted files probed: " + ", ".join(sorted(touched_dirty))
                  + "\n\n" + bundle)
    return bundle[:EVIDENCE_CHARS], probes, sorted(touched_dirty)


# --- the judge ------------------------------------------------------------

JUDGE_SCHEMA = {
    "type": "object",
    "properties": {
        "verdict": {"type": "string", "enum": list(VERDICTS)},
        "citation": {"type": "string"},
        "rationale": {"type": "string"},
    },
    "required": ["verdict", "citation", "rationale"],
    "additionalProperties": False,
}

# Succinct and non-contradictory: this runs on a resident open-weight
# model, and a prompt that argues with itself is answered by whichever
# half the sampler reached first (memory: succinct-noncontradictory
# prompts).
JUDGE_PROMPT = """You judge ONE backlog item: does the problem it describes still exist in the code as it is TODAY?

THE ITEM was written some time ago. Its claims describe the past.
CURRENT STATE was read from the repository just now, using the item's own citations. Only CURRENT STATE is evidence about today.

Answer exactly one verdict:
  alive            the problem is still there.
  dead             it is already fixed, or no longer applies. Cite the file:line or symbol in CURRENT STATE that shows it.
  could-not-judge  CURRENT STATE does not settle it. Say in `citation` what evidence was missing.

Rules:
- Line numbers in THE ITEM may have moved. Judge the substance, not the line number.
- A citation that no longer resolves, or a symbol that is gone, is evidence the item may be dead.
- Code in CURRENT STATE that already does what the item's Done-when asks for means dead.
- If CURRENT STATE only shows that the file exists, that settles nothing: could-not-judge.
- Do not guess. could-not-judge is a correct and expected answer.
- Keep `rationale` under 60 words. A reply that runs long gets cut off mid-JSON and the verdict is lost.

=== THE ITEM ===
{item}

=== CURRENT STATE (read from HEAD just now) ===
{evidence}

Return the JSON verdict now."""


def judge(body: str, evidence: str, timeout: float = 300.0) -> tuple[dict, str]:
    """-> (parsed, model). Raises nothing: an engine failure comes back as
    a could-not-judge naming the engine, never as a defaulted 'alive'."""
    if call_daemon is None:
        return ({"verdict": "could-not-judge",
                 "citation": f"engine unavailable: {_CALL_DAEMON_ERR}",
                 "rationale": "gym/comaintainer/score.py could not be imported"},
                "import-error")
    prompt = JUDGE_PROMPT.format(item=body.strip()[:9000], evidence=evidence)
    try:
        # 900, not 420. MEASURED: at 420 the grammar-forced JSON was cut
        # mid-`rationale` on 10 of 94 items, and each one landed as
        # could-not-judge("the engine reply was not a well-formed
        # verdict") — an INSTRUMENT limit wearing the costume of a
        # finding about the code. The degradation was honest (§18.3) but
        # the measurement was under-powered, which is the §18.4 failure:
        # validate the instrument before the result. The prompt also now
        # caps the rationale, so the ceiling is slack rather than the
        # thing being hit.
        text, model = call_daemon(prompt, timeout, 900, schema=JUDGE_SCHEMA,
                                  schema_name="liveness")
    except Exception as exc:
        return ({"verdict": "could-not-judge",
                 "citation": f"engine unavailable ({type(exc).__name__}: {exc})",
                 "rationale": "the local daemon did not answer"},
                "daemon-unavailable")
    try:
        parsed = json.loads(text)
    except (json.JSONDecodeError, TypeError):
        m = re.search(r"\{.*\}", text or "", re.S)
        try:
            parsed = json.loads(m.group(0)) if m else None
        except json.JSONDecodeError:
            parsed = None
    if not isinstance(parsed, dict) or parsed.get("verdict") not in VERDICTS:
        return ({"verdict": "could-not-judge",
                 "citation": "the engine reply was not a well-formed verdict",
                 "rationale": (text or "")[:300]}, model)
    return (parsed, model)


# A pointer good enough to close an item on: a file:line, or a named
# commit. Anything else is a claim, not a citation.
POINTER = re.compile(
    r"[\w./-]+\.(?:rs|py|toml|md|sh|ts|tsx|js|json|ya?ml|html):\d+"
    r"|\bcommit\s+[0-9a-f]{7,40}\b")
# Sentences the probes themselves emit when a probe found NOTHING. A
# verdict resting on one of these is resting on absence of evidence.
# `NOT PRESENT` is matched bare, not as "NOT PRESENT anywhere". Watched:
# the narrower form let 58c74c5d through on the citation "…:226 (symbol
# run_lane exists, but the specific error string … is NOT PRESENT)" — a
# real file:line wrapped around an absence claim, which is the exact
# shape the gate exists to reject. "The bad code is gone" is sometimes
# genuine closure, but the safe direction is not symmetric: a wrong
# could-not-judge costs a line in a report, a wrong dead deletes live
# work.
ABSENCE_CLAIM = re.compile(
    r"NOT PRESENT|not present anywhere|nothing in it mentions"
    r"|does not resolve today|no citation in this item"
    r"|NO SUCH FILE AT HEAD|do(?:es)? not exist", re.I)


def gate_closure_claim(verdict: str, citation: str, dirty: list = None) -> tuple:
    """-> (verdict, citation). A DEAD verdict must name where the fix
    IS. Absence of evidence is not evidence of closure.

    STRUCTURAL, NOT REMEMBERED (principle 10). Measured on the first full
    pass of 94 items: 21 of 46 `dead` verdicts rested on a probe that
    found nothing — "NO SUCH FILE AT HEAD", "nothing in it mentions this
    item's own names" — rather than on code that does the job. One of
    them was `3dfb8308`, an item the order under which this was built
    explicitly ruled out of scope; its "dead" came from an absolute path
    in the item body that the file probe could not resolve. Retiring on
    that would have deleted a live open question on a parsing artifact.

    So the gate lives here, in the one verifier, rather than in whatever
    script happens to be doing the retiring — a downstream filter is a
    rule someone has to remember, and the next caller will not."""
    if verdict != "dead":
        return verdict, citation
    # An item is closed when the fix has LANDED. Evidence read out of
    # somebody's uncommitted edits is not that. The failing input is
    # 71e845e1, judged dead against a peer's in-flight grounding/mod.rs.
    #
    # MATCH THE CITATION, NOT ONLY THE PROBES. The first cut marked
    # dirtiness only in the file-citation branch of gather_evidence, and
    # 71e845e1 came back `dead` anyway — the judge had cited
    # grounding/mod.rs from SYMBOL GREP output, a path the file branch
    # never touched. The thing that has to be clean is whatever the
    # verdict RESTS on, which is the citation.
    named_dirty = [d for d in (dirty or [])
                   if d in (citation or "") or Path(d).name in (citation or "")]
    if named_dirty:
        return ("could-not-judge",
                "the closure evidence names a file with UNCOMMITTED CHANGES ("
                + ", ".join(named_dirty[:3])
                + ") — the fix it cites may not have landed. Re-run on a "
                  f"clean tree. Judge said: {(citation or '')[:180]}")
    if POINTER.search(citation or "") and not ABSENCE_CLAIM.search(citation or ""):
        return verdict, citation
    return ("could-not-judge",
            "a `dead` verdict with no concrete pointer — the evidence was "
            "absence (a probe that found nothing), not code that closes the "
            f"item. Reported, not acted on. Judge said: {(citation or '')[:200]}")


def verify(short: str, body: str, repo: Path = REPO,
           timeout: float = 300.0) -> Liveness:
    """THE ONE VERIFIER (ARCH §10.6). `verify --all` and co-backlog.py's
    `--pull` both call exactly this, so a pull and a heap pass can never
    disagree about what liveness means."""
    evidence, probes, probed_dirty = gather_evidence(body, repo)
    parsed, model = judge(body, evidence, timeout)
    # The whole dirty set, not just what the file probes touched: the
    # judge can cite a path it saw in grep output.
    verdict, citation = gate_closure_claim(
        parsed["verdict"], parsed.get("citation", ""),
        sorted(dirty_files(repo) | set(probed_dirty)))
    return Liveness(short, verdict, citation,
                    parsed.get("rationale", ""), model, probes=probes)


# --- 2b: the accelerator --------------------------------------------------
#
# Given a commit, propose which open items it may have closed. This is
# EDGE-triggered and that is exactly why it is only an accelerator: it
# saves the level-triggered pass work when it runs, and costs nothing at
# all when it does not. co-sweep.sh may be behind, capped, or uninstalled;
# the heap is correct either way because `verify` above never consults it.

CANDIDATE_SCHEMA = {
    "type": "object",
    "properties": {
        "closes": {"type": "boolean"},
        "citation": {"type": "string"},
        "rationale": {"type": "string"},
    },
    "required": ["closes", "citation", "rationale"],
    "additionalProperties": False,
}

CANDIDATE_PROMPT = """Does this commit close this open backlog item?

Answer true only if the commit's own diff does what the item's Done-when asks for. A commit that merely touches the same file does not close it.

=== THE OPEN ITEM ===
{item}

=== THE COMMIT ===
{commit}

Return the JSON now."""

# How many items one commit is allowed to cost. The prefilter below is
# lexical and free; only the survivors reach the model.
MAX_CANDIDATES_PER_COMMIT = 3
PREFILTER_MIN_SCORE = 2


def _commit_bundle(repo: Path, sha: str) -> tuple[str, list, str]:
    """-> (bundle_for_the_model, files, full_text_for_the_prefilter).

    THE TWO TEXTS ARE DIFFERENT ON PURPOSE. What the model reads is
    truncated, because tokens cost time. What the PREFILTER reads is the
    whole diff, because a local string scan is free — and truncating it
    was measured to hide the signal: fb4d0e0b's strongest tokens
    (`outcome_ctx`, `select_route`) sit past the 12k mark of commit
    7021a11f, so the truncated text ranked it fourth and the cap dropped
    it. Paying model tokens to widen a free scan is the wrong trade."""
    subject = _git(repo, "log", "-1", "--format=%H%n%s%n%n%b", sha)
    files = [f for f in _git(repo, "diff-tree", "-r", "--name-only",
                             "--no-commit-id", sha).splitlines() if f]
    diff = _git(repo, "show", "--format=", "--no-color", sha)
    head = (f"{subject}\n=== FILES ===\n" + "\n".join(files[:40]))
    return (head + f"\n=== DIFF (truncated) ===\n{diff[:12000]}",
            files, head + "\n" + diff)


# A symbol has to be this distinctive before an overlap means anything.
# Shorter names (`new`, `push`, `Result`) appear in every commit and every
# item, and scoring on them would put three arbitrary items in front of
# the model on every single commit.
PREFILTER_SYMBOL_CHARS = 8


def prefilter(items: list, files: list, commit_text: str) -> list:
    """Cheap lexical overlap, so a commit costs at most
    MAX_CANDIDATES_PER_COMMIT model calls however big the backlog is.
    Returns [(score, short, body)] best first.

    THE DIFF IS PART OF THE SIGNAL, not just the paths. Scoring on file
    stems alone missed fb4d0e0b — the item that was the whole reason this
    step exists — because its body names no file at all, only the symbols
    `select_route`, `outcome_ctx` and `NamedModelLocation::Unknown`. An
    item that describes a defect in terms of code rather than paths is
    the normal case, not the exception. Watched: with the diff term
    removed, fb4d0e0b scores 0 and never reaches the model."""
    stems = {Path(f).name for f in files} | {Path(f).stem for f in files}
    stems = {s for s in stems if len(s) > 3}
    syms = {s for s in SYMBOL.findall(commit_text)
            if len(s) >= PREFILTER_SYMBOL_CHARS}
    scored = []
    for short, body in items:
        score = 0
        for f in files:
            if f in body:
                score += 3
        for s in stems:
            if s in body:
                score += 1
        for s in syms:
            if s in body:
                score += 1
        if score >= PREFILTER_MIN_SCORE:
            scored.append((score, short, body))
    scored.sort(key=lambda t: (-t[0], t[1]))
    return scored[:MAX_CANDIDATES_PER_COMMIT]


def propose_closures(sha: str, items: list, repo: Path = REPO,
                     timeout: float = 300.0) -> list:
    """-> [record]. PROPOSALS ONLY — nothing here retires anything."""
    bundle, files, scan = _commit_bundle(repo, sha)
    out = []
    for score, short, body in prefilter(items, files, scan):
        prompt = CANDIDATE_PROMPT.format(item=body.strip()[:6000],
                                         commit=bundle)
        if call_daemon is None:
            continue
        try:
            text, model = call_daemon(prompt, timeout, 300,
                                      schema=CANDIDATE_SCHEMA,
                                      schema_name="closure")
            parsed = json.loads(text)
        except Exception:
            continue
        if not isinstance(parsed, dict) or not parsed.get("closes"):
            continue
        out.append({
            "ts": dt.datetime.now(dt.timezone.utc).isoformat(),
            "kind": CANDIDATE_KIND,
            "item": short,
            "ref": _git(repo, "rev-parse", sha).strip() or sha,
            "prefilter_score": score,
            "citation": parsed.get("citation", ""),
            "rationale": parsed.get("rationale", ""),
            "engine": model,
            "disposition": "proposed — the seat decides; nothing auto-retires",
        })
    return out


# --- reading the heap (borrowed, not re-implemented) ----------------------


def open_items(db: Path = None) -> list:
    """[(short, body)] for every live backlog item. Uses co-backlog.py's
    own store reader so there is one query, not two (ARCH §10.6)."""
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import importlib.util
    spec = importlib.util.spec_from_file_location(
        "co_backlog", Path(__file__).resolve().parent / "co-backlog.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    read = mod.read_store(db or mod.notes_db_path())
    if read.error:
        print(f"co_liveness: {read.error}", file=sys.stderr)
        return []
    return [(r[0][:8], r[1] or "") for r in read.rows]


# --- CLI -------------------------------------------------------------------


def _cmd_verify(args) -> int:
    db = Path(args.db).expanduser() if args.db else None
    items = open_items(db)
    if args.ids:
        wanted = {i[:8] for i in args.ids}
        picked = [(s, b) for s, b in items if s in wanted]
        missing = wanted - {s for s, _ in picked}
        for m in sorted(missing):
            # Retired items are still verifiable — that is how the
            # instrument is validated against a KNOWN-DEAD control.
            body = _retired_body(db, m)
            if body:
                picked.append((m, body))
            else:
                print(f"co_liveness: {m} not found in the store", file=sys.stderr)
    elif args.all:
        picked = items
    else:
        print("co_liveness: name ids or pass --all", file=sys.stderr)
        return 2

    log = Path(args.log).expanduser() if args.log else verdicts_log()
    counts = {v: 0 for v in VERDICTS}
    for n, (short, body) in enumerate(picked, 1):
        lv = verify(short, body, REPO, args.timeout)
        counts[lv.verdict] += 1
        if not args.dry_run:
            append_record(lv.to_record(), log)
        print(f"[{n}/{len(picked)}] {short}  {lv.verdict.upper():16} "
              f"probes={lv.probes} engine={lv.engine}")
        print(f"    citation: {lv.citation[:220]}")
        if args.verbose:
            print(f"    rationale: {lv.rationale[:400]}")
        sys.stdout.flush()
    print(f"\nliveness pass: {counts['alive']} alive, {counts['dead']} dead, "
          f"{counts['could-not-judge']} could-not-judge "
          f"({len(picked)} judged)")
    if not args.dry_run:
        print(f"appended -> {log}")
    return 0


def _retired_body(db, short):
    import sqlite3
    if db is None:
        db = Path.home() / ".sovereign" / "notes.db"
    try:
        conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        row = conn.execute("SELECT content FROM notes WHERE id LIKE ? LIMIT 1",
                           (short + "%",)).fetchone()
        conn.close()
    except sqlite3.Error:
        return None
    return row[0] if row else None


def _cmd_ledger(args) -> int:
    log = Path(args.log).expanduser() if args.log else verdicts_log()
    led = read_ledger(log)
    if not led:
        print(f"co_liveness: no liveness records in {log} — every item reads "
              "as never-verified. That is honest, and one pull re-verifies "
              "what it hands out.")
        return 0
    for short, lv in sorted(led.items(), key=lambda kv: kv[1].at, reverse=True):
        print(f"{short}  {lv.verdict:16} {lv.age_days:6.1f}d ago  {lv.citation[:110]}")
    print(f"\n{len(led)} item(s) with a recorded liveness verdict in {log}")
    return 0


def _cmd_candidates(args) -> int:
    items = open_items(Path(args.db).expanduser() if args.db else None)
    log = Path(args.log).expanduser() if args.log else verdicts_log()
    recs = propose_closures(args.commit, items, REPO, args.timeout)
    if not recs:
        print(f"co_liveness: no closure candidate for {args.commit[:8]} "
              f"({len(items)} open item(s) considered)")
        return 0
    for rec in recs:
        if not args.dry_run:
            append_record(rec, log)
        print(f"CLOSURE CANDIDATE {rec['item']} <- {rec['ref'][:8]} "
              f"(prefilter {rec['prefilter_score']})")
        print(f"    {rec['citation'][:220]}")
    if not args.dry_run:
        print(f"appended {len(recs)} candidate(s) -> {log}")
    return 0


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(
        prog="co_liveness.py",
        description="Does a backlog item still reproduce at HEAD? "
                    "Level-triggered: no mark, no cursor, no catch-up.")
    ap.add_argument("--db", help="notes store (default: co-backlog.py's)")
    ap.add_argument("--log", help="verdicts log (default: the seat's)")
    ap.add_argument("--timeout", type=float, default=300.0)
    sub = ap.add_subparsers(dest="cmd", required=True)

    v = sub.add_parser("verify", help="judge items against HEAD")
    v.add_argument("ids", nargs="*")
    v.add_argument("--all", action="store_true")
    v.add_argument("--dry-run", action="store_true", help="judge, record nothing")
    v.add_argument("--verbose", action="store_true")
    v.set_defaults(fn=_cmd_verify)

    l = sub.add_parser("ledger", help="what liveness is recorded")
    l.set_defaults(fn=_cmd_ledger)

    c = sub.add_parser("candidates", help="which open items a commit may close")
    c.add_argument("commit")
    c.add_argument("--dry-run", action="store_true")
    c.set_defaults(fn=_cmd_candidates)

    args = ap.parse_args(argv)
    return args.fn(args)


if __name__ == "__main__":
    raise SystemExit(main())
