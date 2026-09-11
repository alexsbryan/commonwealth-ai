#!/usr/bin/env python3
"""co-arch.py — per-commit architecture audit, one batched forced-choice call.

The cheapest honest shape, derived from this repo's own decode pricing
(PREREG_audit_economy_d7_decode_20260814.md, prereg 7cbce9e1):

  generative register   327ms + 4.65 ms/out_char   (~75-90% decode share)
  batched forced choice 1125-1328ms @ ~29k prompt chars, 29-44 out chars
                                                   (~0% decode share)

So: pay ONE prefill per commit, emit ~3 chars per judged rule, and never
generate a character code can supply. Concretely —

  1. BUNDLE is added code lines only (measured 4-11x smaller than the full
     diff on real commits: 512de1c3 260k -> 35k chars).
  2. A model-free GATE decides which rules can fire at all. A commit with
     no added code costs zero model calls. This is §7.6: never ask a model
     to guarantee what code can enforce.
  3. The gate's matched lines ARE the citation. The model contributes one
     letter per fired rule and nothing else, so there is no model-authored
     prose anywhere in the row (§18.1 — a citation the subject authors is
     not evidence).
  4. §2.1 (stringly dispatch) is decided by the counter, never by the
     model — code can count match arms.

Verdicts are A (no violation) / B (violation) / C (evidence cannot decide).
C is first-class: four verdicts, not two (§18.2).

Rows, never prose: appended to ~/.sovereign/comaintainer/verdicts.jsonl as
kind:"arch", shadow:true. Single-sample and shadow-only until the bars in
gym/comaintainer/PREREG_arch_probes_20260817.md are cleared (§18.5 — one
run is not a measurement).

  scripts/co-arch.py <sha>          audit one commit (invoked by co-sweep.sh)
  scripts/co-arch.py --staged [-m MSG]
                                    gate only, on the INDEX: what would fire on
                                    the commit about to be made. No model. The
                                    engine behind .claude/hooks/commit-smells.py
  scripts/co-arch.py --self-test    offline, model-free: exit 0 behaved / 4 not
  scripts/co-arch.py --self-test-live  planted-B + planted-A through the
                                    daemon; exit 5 = engine unusable, which
                                    is NEVER a quality verdict

Run-path exit is 0 always (advisory shadow work, like co-review.sh and
co-drift.py): an engine failure writes could-not-judge rows naming it.
"""
from __future__ import annotations

import argparse
import datetime as _dt
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

def _discover_repo() -> Path:
    """The repo under audit, not the repo this script happens to live in.

    Portability: a user may install these scripts anywhere. Ask git from
    the working directory first; fall back to the script's own parent so
    an in-repo invocation keeps working."""
    r = subprocess.run(["git", "rev-parse", "--show-toplevel"],
                       capture_output=True, text=True)
    if r.returncode == 0 and r.stdout.strip():
        return Path(r.stdout.strip())
    return Path(__file__).resolve().parent.parent


REPO = Path(os.environ.get("CO_ARCH_REPO") or _discover_repo())
STATE_DIR = Path(os.environ.get("CO_STATE_DIR")
                 or Path.home() / ".sovereign" / "comaintainer")
VERDICTS_LOG = STATE_DIR / "verdicts.jsonl"
DAEMON = os.environ.get("SOVEREIGN_DAEMON_URL", "http://localhost:9741")
PROFILE_PATH = Path(os.environ.get("CO_ARCH_PROFILE")
                    or REPO / "quality" / "arch-probes.toml")

BUNDLE_CAP = int(os.environ.get("CO_ARCH_BUNDLE_CAP", 24_000))
CALL_TIMEOUT = 180.0
WINDOW = 2          # lines of context each side of a gate match
MAX_SITES = 4       # match sites shown PER RULE; overflow is counted, not hidden
MAX_TOKENS = 96         # one letter per rule + JSON scaffolding; decode ~free
VERDICTS = ("A", "B", "C")


# --------------------------------------------------------------------------
# the rules — each owns its section anchor and its own model-free gate
# --------------------------------------------------------------------------
# `gate` returns the citation lines that let the rule fire; an empty list
# means the rule CANNOT fire on this commit and costs nothing. `decider`
# rules never reach the model at all.

def _lines_matching(added: list[tuple[str, str]], rxs: list) -> list[str]:
    """-> ["path: line", ...] for added lines matching any compiled pattern."""
    return [f"{path}: {line.strip()[:120]}"
            for path, line in added
            if any(rx.search(line) for rx in rxs)]


class ProfileError(RuntimeError):
    """The rule set could not be loaded. Named and refused, never quietly
    replaced by an inline second copy (SS10.6, SS18.3)."""


def _compile_all(pats, where: str) -> list:
    out = []
    for pat in pats or []:
        try:
            out.append(re.compile(pat))
        except re.error as e:
            raise ProfileError(f"{where}: bad regex {pat!r}: {e}") from e
    return out


def load_profile(path=None) -> dict:
    """Load the rule set from TOML — the ONLY definition of the rules.

    There is deliberately no inline fallback copy: two definitions of one
    rule set is the duplicate-decider smell this audit exists to catch
    (ARCH SS10.6). If the profile cannot be read, the run refuses and says
    why (SS18.3)."""
    path = Path(path or PROFILE_PATH)
    try:
        import tomllib
    except ModuleNotFoundError as e:   # py<3.11, e.g. launchd's system python
        raise ProfileError(
            f"tomllib unavailable on {sys.executable} (needs Python 3.11+); "
            f"cannot read {path}. Use a newer interpreter (SOVEREIGN_PYTHON) "
            f"— the rule set is not duplicated in code.") from e
    try:
        raw = tomllib.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError as e:
        raise ProfileError(f"no probe profile at {path} "
                           f"(set CO_ARCH_PROFILE)") from e
    except tomllib.TOMLDecodeError as e:
        raise ProfileError(f"{path}: {e}") from e

    prof = raw.get("profile", {})
    rules = []
    for r in raw.get("rule", []):
        for field in ("id", "anchor", "question"):
            if not r.get(field):
                raise ProfileError(f"rule {r.get('id', '?')} missing {field}")
        g = r.get("gate", {})
        if not (g.get("any") or g.get("new_file")):
            raise ProfileError(f"rule {r['id']}: needs gate.any or "
                               f"gate.new_file, else it fires on everything")
        rules.append({
            "id": r["id"], "sec": r["anchor"], "q": r["question"],
            "any": _compile_all(g.get("any"), f"rule {r['id']} gate.any"),
            "none": _compile_all(g.get("none"), f"rule {r['id']} gate.none"),
            "new_file": (re.compile(g["new_file"], re.I)
                         if g.get("new_file") else None),
            "new_file_suffix": g.get("new_file_suffix") or [],
        })
    deciders = []
    for d in raw.get("decider", []):
        kind = d.get("kind")
        if kind == "count":
            deciders.append({
                "id": d["id"], "sec": d["anchor"], "kind": "count",
                "rx": _compile_all([d["pattern"]], f"decider {d['id']}")[0],
                "max": int(d.get("max", 0)), "per": d.get("per", "file"),
                "message": d.get("message", "matches"),
            })
        elif kind == "message_symbols":
            deciders.append({
                "id": d["id"], "sec": d["anchor"], "kind": "message_symbols",
                "max_symbols": int(d.get("max_symbols", 12)),
                "message": d.get("message", "named in the message, absent from the tree"),
            })
        elif kind == "comment_ratio":
            deciders.append({
                "id": d["id"], "sec": d["anchor"], "kind": "comment_ratio",
                "max_ratio": float(d.get("max_ratio", 0.5)),
                "min_comments": int(d.get("min_comments", 8)),
                "message": d.get("message", "comment lines per code line"),
            })
        elif kind == "dead_citation":
            deciders.append({
                "id": d["id"], "sec": d["anchor"], "kind": "dead_citation",
                "max": int(d.get("max", 0)),
                "message": d.get("message", "cited path is in neither tree"),
            })
        else:
            raise ProfileError(f"decider {d.get('id', '?')}: unknown kind "
                               f"{kind!r} (known: count, message_symbols, "
                               f"comment_ratio, dead_citation)")
    ids = [r["id"] for r in rules] + [d["id"] for d in deciders]
    if len(ids) != len(set(ids)):
        raise ProfileError(f"duplicate rule ids in {path}: {ids}")
    if not ids:
        raise ProfileError(f"{path} declares no rules — an audit that can "
                           f"judge nothing is not an audit")
    return {"id": prof.get("id", path.stem),
            "globs": prof.get("globs") or ["*"],
            # The evidence/cost trade, measured 2026-08-17: prefill is
            # ~7-8ms per prompt token here, so a wider window costs real
            # seconds — and too narrow a window makes the judge answer C.
            # Both directions are failures; this is the knob between them.
            "window": int(prof.get("window", WINDOW)),
            "max_sites": int(prof.get("max_sites", MAX_SITES)),
            "rules": rules, "deciders": deciders,
            "refused": raw.get("refused", []), "path": path}


def gate_rule(rule: dict, added, files) -> list[str]:
    """-> citation lines that let this rule fire. [] means it CANNOT fire
    and costs no model call — this is where the speed comes from."""
    if rule["new_file"] is not None:
        hits = [f for f in files.get("added_files", [])
                if rule["new_file"].search(Path(f).stem)
                or Path(f).suffix in rule["new_file_suffix"]]
        return [f"new file: {f}" for f in hits]
    hits = _lines_matching(added, rule["any"]) if rule["any"] else []
    if not hits:
        return []
    # A suppressor makes the rule about ABSENCE (branches added AND nothing
    # traced). If it matches, this hunk is not the target.
    if rule["none"] and _lines_matching(added, rule["none"]):
        return []
    return hits


# A backticked token in a commit message that has the SHAPE of a code
# symbol. CLI verbs (`mesh join`), files (`AGENTS.md`), flags and TOML
# fragments are backticked in this repo's messages too (measured over the
# last 8 commits, 2026-09-01) and none of those is a claim about code, so
# the shapes are deliberately narrow: a `::` path, CamelCase with two humps,
# snake_case with an underscore, SCREAMING_CASE, a call `f()` or macro `m!`.
_TICK = re.compile(r"`([^`\n]{2,80})`")
_SYMBOL_SHAPES = [
    re.compile(r"^[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_<>]*)+(?:\(\))?!?$"),
    re.compile(r"^[A-Z][a-z0-9]+(?:[A-Z][a-z0-9]+)+$"),
    re.compile(r"^[a-z][a-z0-9]*(?:_[a-z0-9]+)+(?:\(\))?!?$"),
    re.compile(r"^[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+$"),
]


def symbol_needles(msg: str) -> list[tuple[str, str]]:
    """-> [(as written, grep needle)] for the code symbols a message names.
    The needle is the last path segment, bare of generics, `()` and `!`."""
    out: list[tuple[str, str]] = []
    for tok in _TICK.findall(msg or ""):
        tok = tok.strip()
        if not any(rx.match(tok) for rx in _SYMBOL_SHAPES):
            continue
        needle = re.sub(r"<[^>]*>", "", tok).rstrip("!").removesuffix("()")
        needle = needle.split("::")[-1]
        if len(needle) >= 3 and (tok, needle) not in out:
            out.append((tok, needle))
    return out


def _in_tree(needle: str, tree: str):
    """True/False = found/absent in `tree` (a ref, or "--cached" for the
    index); None = git could not answer (no such ref), which the caller
    treats as absent-but-unproven rather than as found."""
    args = ["grep", "-q", "-w", "-F", "-e", needle]
    args += ["--cached"] if tree == "--cached" else [tree]
    rc = _git_rc(*args)
    return {0: True, 1: False}.get(rc)


# --------------------------------------------------------------------------
# comment-shaped deciders (ARCH SS15, SS11.1)
# --------------------------------------------------------------------------
# `size_gate.rs` counting rule 1 deliberately does not count comments, so
# stripping documentation buys no room under a ceiling. The consequence is
# that comments are the one dimension nine ratchets leave unmetered, and the
# workspace runs 287k comment lines against 860k of code. These three meter
# it in the DIFF instead of on the stock: deleting an old comment cannot buy
# room here either, so rule 1's incentive is preserved and the growth is
# still priced at the moment it is written.

_COMMENT_RX = re.compile(r"^\s*//")
_PATH_TICK = re.compile(
    r"`([A-Za-z0-9_][A-Za-z0-9_./-]*/[A-Za-z0-9_./-]*"
    r"\.(?:rs|md|toml|py|sh|mjs|ts|svelte))(?:[:#][0-9A-Za-z_.-]*)?`")
# An illustrative name is not a citation. `tests/foo.rs` in a doc block is a
# worked example; flagging it would train the reader to skip the finding.
_PLACEHOLDER = re.compile(
    r"(?:^|/)(?:foo|bar|baz|qux|x|y|example|sub)\.\w+$|\.\.\.|[<>{}]")
_TREE_CACHE: dict = {}


def split_comment_code(added) -> dict:
    """-> {path: (n_comment, n_code)} over ADDED lines. Blank lines count as
    neither: a ratio a blank line can move is a ratio you can pad past."""
    out: dict = {}
    for path, line in added:
        if not line.strip():
            continue
        c, k = out.get(path, (0, 0))
        out[path] = (c + 1, k) if _COMMENT_RX.match(line) else (c, k + 1)
    return out


def _decide_comment_ratio(dec: dict, added) -> tuple:
    """Comment lines added per code line added, per file. `min_comments`
    keeps it off small edits, where the ratio is noise rather than a habit."""
    cites = []
    for path, (c, k) in sorted(split_comment_code(added).items()):
        if c < dec["min_comments"]:
            continue
        ratio = c / max(k, 1)
        if ratio > dec["max_ratio"]:
            cites.append(f"{path}: +{c} comment / +{k} code = {ratio:.2f} "
                         f"{dec['message']}")
    return ("B", cites) if cites else ("A", [])


def _tree_paths(tree: str):
    """Every path in `tree` ("--cached" = the index). None = git could not
    answer, which the caller reports as could-not-judge, never as clean."""
    if tree not in _TREE_CACHE:
        out = (_git("ls-files") if tree == "--cached"
               else _git("ls-tree", "-r", "--name-only", tree))
        _TREE_CACHE[tree] = (set(out.split("\n")) - {""}) or None
    return _TREE_CACHE[tree]


def _cited_paths(added) -> list:
    """-> [(citing file, cited path)] for repo-shaped paths named in ADDED
    comment lines."""
    out: list = []
    for path, line in added:
        if not _COMMENT_RX.match(line):
            continue
        for m in _PATH_TICK.finditer(line):
            cited = m.group(1).lstrip("./")
            if _PLACEHOLDER.search(cited) or (path, cited) in out:
                continue
            out.append((path, cited))
    return out


def _resolves(cited: str, from_file: str, tree: set) -> bool:
    """A comment writes a path from the repo root, from its own directory, or
    from any ancestor of it (a crate root, most often) -- all three resolve.
    The trailing suffix match keeps recall low on purpose: this decider is
    only allowed to report a path that exists NOWHERE."""
    if cited in tree:
        return True
    parts = from_file.split("/")[:-1]
    while True:
        base = "/".join(parts)
        if os.path.normpath(f"{base}/{cited}" if base else cited) in tree:
            return True
        if not parts:
            break
        parts = parts[:-1]
    return any(t.endswith("/" + cited) for t in tree)


def _decide_dead_citation(dec: dict, added, files) -> tuple:
    """ARCH SS11.1 -- cite, don't recall -- at the one place code can check a
    COMMENT: a path a comment names must exist in the tree the commit
    produces, or the one it started from (citing a file the commit deletes is
    legitimate). In neither, the comment was already wrong the day it landed.
    `files["trees"]` = (after, before); collect()/collect_staged() set it."""
    trees = files.get("trees")
    if not trees:
        return "C", ["no tree refs supplied; comment citations not checked"]
    after, before = trees
    listings = [t for t in (_tree_paths(after), _tree_paths(before)) if t]
    if not listings:
        return "C", [f"neither tree ({after}, {before}) could be listed"]
    cites = [f"{src}: `{cited}` -- {dec['message']}"
             for src, cited in _cited_paths(added)
             if not any(_resolves(cited, src, t) for t in listings)]
    return ("B", cites) if len(cites) > dec["max"] else ("A", [])


def _decide_message_symbols(dec: dict, files: dict, msg: str) -> tuple:
    """ARCH SS11.1, cite-don't-recall, at the one place code can check it: a
    symbol the message names in backticks must exist in the tree the commit
    produces OR the tree it started from (a removed or renamed name is a
    legitimate thing to cite). Absent from both, it was recalled, not cited.
    `files["trees"]` = (after, before); collect()/collect_staged() set it."""
    trees = files.get("trees")
    if not trees:
        return "C", ["no tree refs supplied; message symbols not checked"]
    after, before = trees
    cites = []
    for tok, needle in symbol_needles(msg)[: dec["max_symbols"]]:
        if _in_tree(needle, after) or _in_tree(needle, before):
            continue
        cites.append(f"message names `{tok}` — {dec['message']}")
    return ("B", cites) if cites else ("A", [])


def run_decider(dec: dict, added, files, msg: str = "") -> tuple:
    """Code decides; the model is never consulted (ARCH SS7.6). Counting is
    arithmetic, and a model asked to count gives a worse answer slower.
    `msg` is only read by message-shaped deciders; line-shaped ones ignore it."""
    if dec["kind"] == "message_symbols":
        return _decide_message_symbols(dec, files, msg)
    if dec["kind"] == "comment_ratio":
        return _decide_comment_ratio(dec, added)
    if dec["kind"] == "dead_citation":
        return _decide_dead_citation(dec, added, files)
    groups: dict = {}
    for path, line in added:
        if dec["rx"].search(line):
            key = path if dec["per"] == "file" else "*"
            groups.setdefault(key, []).append(line.strip()[:120])
    hits = {k: v for k, v in groups.items() if len(v) > dec["max"]}
    if not hits:
        return "A", []
    return "B", [f"{k}: {len(v)} {dec['message']}" for k, v in hits.items()]


# --------------------------------------------------------------------------
# bundle
# --------------------------------------------------------------------------

def _git(*args: str) -> str:
    r = subprocess.run(["git", "-C", str(REPO), *args],
                       capture_output=True, text=True)
    return r.stdout


def _git_rc(*args: str) -> int:
    return subprocess.run(["git", "-C", str(REPO), *args],
                          capture_output=True, text=True).returncode


def _added_from_diff(raw: str) -> list[tuple[str, str]]:
    """(path, line) for every added line in a unified diff, the one parse
    both the commit and the staged collectors use."""
    added: list[tuple[str, str]] = []
    path = "?"
    for ln in raw.splitlines():
        if ln.startswith("diff --git"):
            path = ln.split(" b/")[-1].strip()
        elif ln.startswith("+") and not ln.startswith("+++"):
            added.append((path, ln[1:]))
    return added


def _files_from_status(status: str) -> dict:
    rows = [l for l in status.splitlines() if l.strip()]
    return {"added_files": [l.split("\t")[-1] for l in rows if l.startswith("A")],
            "all_files": [l.split("\t")[-1] for l in rows]}


def collect(sha: str, globs=None) -> tuple[list[tuple[str, str]], dict, str]:
    """-> (added_lines, files, commit_message).

    `globs` comes from the profile: which files this codebase wants audited.
    NOTE the bundle is NOT built here — it is built from the gate's own
    matches by build_bundle(), after gating."""
    raw = _git("show", "--format=", "--no-color", sha, "--",
               *(globs or ["*"]))
    added = _added_from_diff(raw)
    status = _git("diff-tree", "-r", "--no-commit-id", "--name-status", sha)
    files = _files_from_status(status)
    files["trees"] = (sha, sha + "^")
    msg = _git("log", "-1", "--format=%s%n%n%b", sha).strip()
    return added, files, msg


def collect_staged(globs=None) -> tuple[list[tuple[str, str]], dict]:
    """-> (added_lines, files) for the INDEX: what `git commit` would record
    right now. Honours GIT_INDEX_FILE, so a caller can replay a pending
    `git add` into a temporary index first and audit THAT (the commit-smells
    hook does, because `git add X && git commit` is one Bash call and the
    real index has not seen X when the hook runs)."""
    raw = _git("diff", "--cached", "--no-color", "--", *(globs or ["*"]))
    files = _files_from_status(_git("diff", "--cached", "--name-status"))
    files["trees"] = ("--cached", "HEAD")
    return _added_from_diff(raw), files


def findings(added, files, msg: str, prof: dict) -> list[dict]:
    """The model-free layer's answer for one diff, in reading order: code-
    decided B verdicts, then the gated questions with their sites. This is
    what --staged prints and what the commit-smells hook hands the agent;
    the nightly path (rows_for) is the same gate with a model as judge."""
    out: list[dict] = []
    for d in prof["deciders"]:
        v, cites = run_decider(d, added, files, msg)
        if v == "B":
            out.append({"id": d["id"], "sec": d["sec"], "kind": "decided",
                        "text": d["message"], "cites": cites})
        elif v == "C":
            out.append({"id": d["id"], "sec": d["sec"], "kind": "unjudged",
                        "text": "; ".join(cites), "cites": []})
    for r in prof["rules"]:
        cites = gate_rule(r, added, files)
        if cites:
            out.append({"id": r["id"], "sec": r["sec"], "kind": "question",
                        "text": r["q"], "cites": cites})
    return out


def build_bundle(added, files, fired, msg: str, profile: dict = None) -> str:
    """Evidence windows around what the gate actually matched.

    THE COST IS PREFILL, and prefill is linear in prompt tokens: measured
    2026-08-17 at ~7-8ms per prompt token on this host (1,760 tokens ->
    12.4s; 8,169 -> 64.6s). Sending every added line to ask "does this one
    unwrap_or collapse an error?" pays for thousands of tokens the question
    does not use.

    The gate has already localised each rule to specific lines, so the
    evidence is those lines plus a small neighbourhood. What is left out is
    COUNTED in the header, never silently dropped (§18.3) — the judge can
    answer C if the window is too narrow to decide, which is what C is for.
    """
    window = (profile or {}).get("window", WINDOW)
    max_sites = (profile or {}).get("max_sites", MAX_SITES)
    idx_by_line = {}
    for i, (path, line) in enumerate(added):
        idx_by_line.setdefault((path, line), i)
    keep: set[int] = set()
    sampled: list[str] = []
    for rule, cites in fired:
        # Bounded per rule, so one noisy rule cannot drag the whole prompt
        # back up to full-diff size (the 70-site case that motivated this).
        # One violating site is enough to answer B; an A on a sample is
        # weaker, so the sampling is STATED in the header and the row.
        if len(cites) > max_sites:
            sampled.append(f"{rule['id']}: {MAX_SITES} of {len(cites)} "
                           f"matched sites shown")
        for c in cites[:max_sites]:
            if c.startswith("new file: "):
                continue
            path, _, frag = c.partition(": ")
            for i, (p2, l2) in enumerate(added):
                if p2 == path and l2.strip()[:120] == frag:
                    for j in range(max(0, i - window), min(len(added), i + window + 1)):
                        keep.add(j)
                    break
    if not keep:
        keep = set(range(min(len(added), window * 4)))
    shown = sorted(keep)
    lines, last = [], None
    for i in shown:
        if last is not None and i != last + 1:
            lines.append("    ...")
        path, line = added[i]
        # A single minified or generated line can be thousands of chars and
        # buys nothing at prefill prices.
        lines.append(f"{path}: {line[:200]}")
        last = i
    body = "\n".join(lines)
    if len(body) > BUNDLE_CAP:
        body = body[:BUNDLE_CAP] + f"\n[TRUNCATED at {BUNDLE_CAP} chars]"
    omitted = len(added) - len(shown)
    new_files = files.get("added_files") or []
    sample_note = ("\n=== SAMPLING ===\n" + "\n".join(sampled) + "\n"
                   if sampled else "")
    return (sample_note + f"=== COMMIT MESSAGE ===\n{msg}\n\n"
            f"=== NEW FILES ({len(new_files)}) ===\n"
            + ("\n".join(new_files) if new_files else "(none)") + "\n\n"
            f"=== ADDED CODE — the lines the rules matched, with context "
            f"({len(shown)} shown, {omitted} other added lines not shown) ===\n"
            f"{body}")


# --------------------------------------------------------------------------
# the one call
# --------------------------------------------------------------------------

def batch_schema(n: int) -> dict:
    return {"type": "object",
            "properties": {"v": {"type": "array", "minItems": n, "maxItems": n,
                                 "items": {"type": "string",
                                           "enum": list(VERDICTS)}}},
            "required": ["v"]}


def build_prompt(fired: list[dict], bundle: str) -> str:
    rules = "\n".join(f"{i + 1}. [{r['sec']}] {r['q']}"
                      for i, r in enumerate(fired))
    return (
        "You audit one commit's added code against this repo's architecture "
        "rules.\nFor EACH numbered rule, answer with one letter:\n"
        "A = the change does not violate this rule\n"
        "B = the change violates this rule\n"
        "C = the shown evidence cannot decide it\n\n"
        "Judge only from the lines shown. Do not guess about code you "
        "cannot see — that is what C is for.\n\n"
        f"RULES:\n{rules}\n\n=== CHANGE UNDER AUDIT ===\n{bundle}\n\n"
        f'Return {{"v":[...]}} with exactly {len(fired)} letters, rule order.')


def call_daemon(prompt: str, n: int, timeout: float = CALL_TIMEOUT,
                model: str | None = None) -> tuple[list[str] | None, str, dict]:
    """-> (letters | None, model_id, telemetry). Never raises.

    `model` is for the BANK SCORER only (bar (e): per-rule agreement
    between engines must be stated before a cheap engine carries a rule).
    The run path leaves it None and takes the daemon's own routing."""
    body = {"messages": [{"role": "user", "content": prompt}],
            "temperature": 0, "max_tokens": MAX_TOKENS,
            "response_format": {"type": "json_schema",
                                "json_schema": {"name": "arch",
                                                "schema": batch_schema(n),
                                                "strict": True}}}
    if model:
        body["model"] = model
    req = urllib.request.Request(f"{DAEMON}/v1/chat/completions",
                                 data=json.dumps(body).encode(),
                                 headers={"content-type": "application/json"})
    t0 = time.monotonic()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            payload = json.loads(r.read())
    except Exception as e:                       # noqa: BLE001 — all reported
        return None, f"daemon-unavailable ({type(e).__name__})", {
            "wall_ms": round((time.monotonic() - t0) * 1000),
            "prompt_chars": len(prompt)}
    ms = round((time.monotonic() - t0) * 1000)
    text = (payload.get("choices") or [{}])[0].get("message", {}).get("content", "")
    usage = payload.get("usage") or {}
    tel = {"wall_ms": ms, "prompt_chars": len(prompt), "out_chars": len(text),
           "prompt_tokens": usage.get("prompt_tokens"),
           "completion_tokens": usage.get("completion_tokens")}
    return parse_letters(text, n), payload.get("model", "unknown"), tel


def parse_letters(text: str, n: int) -> list[str] | None:
    """Tolerant of trailing content: grammar binding is not guaranteed on
    every engine, and a reply that is right plus chatty is still right.
    A count mismatch is NOT repaired — it becomes could-not-judge."""
    if not text:
        return None
    start = text.find("{")
    if start >= 0:
        depth = 0
        for i, ch in enumerate(text[start:], start):
            depth += (ch == "{") - (ch == "}")
            if depth == 0:
                try:
                    v = json.loads(text[start:i + 1]).get("v")
                except json.JSONDecodeError:
                    break
                if isinstance(v, list) and len(v) == n \
                        and all(x in VERDICTS for x in v):
                    return [str(x) for x in v]
                break
    bare = re.findall(r"\b([ABC])\b", text.upper())
    return bare if len(bare) == n else None


# --------------------------------------------------------------------------
# rows
# --------------------------------------------------------------------------

def rows_for(sha: str, added, files, msg, engine_on: bool,
             profile: dict) -> tuple[list[dict], dict]:
    ts = _dt.datetime.now(_dt.timezone.utc).isoformat()
    base = {"ts": ts, "kind": "arch", "ref": sha, "shadow": True,
            "profile": profile["id"]}
    rows: list[dict] = []

    for d in profile["deciders"]:                     # model-free, always
        verdict, cites = run_decider(d, added, files, msg)
        rows.append({**base, "rule": d["id"], "sec": d["sec"],
                     "verdict": verdict, "citation": cites,
                     "decided_by": "code"})

    gated = [(r, gate_rule(r, added, files)) for r in profile["rules"]]
    fired = [(r, c) for r, c in gated if c]
    skipped = [r["id"] for r, c in gated if not c]
    tel: dict = {"rules_fired": [r["id"] for r, _ in fired],
                 "rules_gated_out": skipped, "added_lines": len(added)}

    if not fired:
        return rows, tel
    if not engine_on:
        rows += [{**base, "rule": r["id"], "sec": r["sec"],
                  "verdict": "C", "missing": "engine not consulted",
                  "citation": c, "decided_by": "none"} for r, c in fired]
        return rows, tel

    bundle = build_bundle(added, files, fired, msg, profile)
    prompt = build_prompt([r for r, _ in fired], bundle)
    letters, model, call_tel = call_daemon(prompt, len(fired))
    tel.update(call_tel)
    tel["model"] = model
    for i, (r, cites) in enumerate(fired):
        row = {**base, "rule": r["id"], "sec": r["sec"],
               "citation": cites, "decided_by": "model", "model": model,
               "wall_ms": call_tel.get("wall_ms")}
        if letters is None:
            row.update({"verdict": "C",
                        "missing": f"a well-formed engine reply ({model})"})
        else:
            row["verdict"] = letters[i]
        rows.append(row)
    return rows, tel


def append(rows: list[dict], log: Path = VERDICTS_LOG) -> None:
    log.parent.mkdir(parents=True, exist_ok=True)
    with open(log, "a", encoding="utf-8") as fh:
        for r in rows:
            fh.write(json.dumps(r, ensure_ascii=False) + "\n")


def report(sha: str, rows: list[dict], tel: dict) -> None:
    flagged = [r for r in rows if r["verdict"] == "B"]
    unk = [r for r in rows if r["verdict"] == "C"]
    print(f"co-arch {sha[:9]}: {len(rows)} rule(s) judged "
          f"({len(tel.get('rules_fired', []))} model, "
          f"{sum(1 for r in rows if r.get('decided_by') == 'code')} code, "
          f"{len(tel.get('rules_gated_out', []))} gated out) "
          f"in {tel.get('wall_ms', 0)}ms"
          f"  prompt={tel.get('prompt_chars', 0)}c out={tel.get('out_chars', 0)}c")
    for r in flagged:
        print(f"  B {r['rule']:18} {r['sec']:12} {(r['citation'] or [''])[0][:90]}")
    for r in unk:
        print(f"  C {r['rule']:18} {r['sec']:12} {r.get('missing', 'evidence')}")


# --------------------------------------------------------------------------
# self-tests — a gate nobody watched fail is not a gate (§18.1)
# --------------------------------------------------------------------------

PLANTED_B = [
    ("src/thing.rs", "    let n = cfg.limit.unwrap_or(0);"),
    ("src/thing.rs", '    match kind { "a" => 1, "b" => 2, "c" => 3, "d" => 4, _ => 0 }'),
    ("src/thing.rs", '    "a" => 1,'), ("src/thing.rs", '    "b" => 2,'),
    ("src/thing.rs", '    "c" => 3,'), ("src/thing.rs", '    "d" => 4,'),
    ("src/thing.rs", "    let id = rows.len();"),
]
PLANTED_A = [
    ("src/clean.rs", "    let n = cfg.limit.ok_or(Error::Missing)?;"),
    ("src/clean.rs", "    tracing::debug!(?n, \"limit resolved\");"),
]


def self_test(profile_path=None) -> int:
    """Model-free layer. Exit 0 behaved, 4 misbehaved.

    Two halves. The FIRST validates whatever profile is loaded, so a user
    who writes their own rule set for their own codebase finds out here
    rather than at 03:30 — bad regex, missing question, duplicate id, a
    rule with no gate (which would fire on every commit and spend a call
    to learn nothing). The SECOND checks gate BEHAVIOUR, and only for the
    rules the loaded profile actually declares, so a custom profile is
    not failed for lacking this repo's rules.
    """
    bad: list[str] = []
    try:
        prof = load_profile(profile_path)
    except ProfileError as e:
        print(f"  MISBEHAVED: profile did not load: {e}")
        print("co-arch --self-test: 1 FAILURE(S)")
        return 4
    ids = {r["id"] for r in prof["rules"]} | {d["id"] for d in prof["deciders"]}
    print(f"  profile {prof['id']} ({prof['path']}): "
          f"{len(prof['rules'])} model rule(s), {len(prof['deciders'])} "
          f"code decider(s), {len(prof['refused'])} refused")
    for r in prof["rules"]:
        if len(r["q"]) < 20:
            bad.append(f"rule {r['id']}: question too short to be judgeable")
    for ref in prof["refused"]:
        if not ref.get("reason"):
            bad.append(f"refused rule {ref.get('id', '?')} carries no reason "
                       f"— it will be re-proposed")

    def has(rid):
        return rid in ids

    def gate_of(rid):
        return next(r for r in prof["rules"] if r["id"] == rid)

    # Comment-shaped deciders. Each gets a planted violation AND a planted
    # clean case: a check with no input that makes it fail is not a check
    # (ARCH SS18.1), and the clean case is what stops a rule that fires on
    # everything from reading as a working gate.
    if has("comment-ratio"):
        dec = next(d for d in prof["deciders"] if d["id"] == "comment-ratio")
        essay = [("r.rs", "//! line") for _ in range(20)] + \
                [("r.rs", "let x = 1;") for _ in range(4)]
        v, cites = run_decider(dec, essay, {})
        if v != "B" or not cites:
            bad.append(f"comment-ratio missed a 20:4 essay: {v} {cites}")
        documented = [("r.rs", "/// doc") for _ in range(10)] + \
                     [("r.rs", "let x = 1;") for _ in range(40)]
        v, _ = run_decider(dec, documented, {})
        if v != "A":
            bad.append(f"comment-ratio flagged documented code at 0.25: {v}")
        v, _ = run_decider(dec, [("r.rs", "// one"), ("r.rs", "// two")], {})
        if v != "A":
            bad.append("comment-ratio fired under min_comments")

    if has("inline-comment"):
        dec = next(d for d in prof["deciders"] if d["id"] == "inline-comment")
        v, cites = run_decider(dec, [("r.rs", "    // step")] * 11, {})
        if v != "B" or not cites:
            bad.append(f"inline-comment missed 11 body comments: {v} {cites}")
        exempt = [("r.rs", "// SPDX-License-Identifier: AGPL-3.0-or-later")] + \
                 [("r.rs", "        // SAFETY: the pointer is owned")] * 11 + \
                 [("r.rs", "/// doc")] * 11 + [("r.rs", "//! module")] * 11
        v, _ = run_decider(dec, exempt, {})
        if v != "A":
            bad.append(f"inline-comment flagged SPDX/SAFETY/doc lines: {v}")

    if has("dead-citation"):
        dec = next(d for d in prof["deciders"] if d["id"] == "dead-citation")
        trees = {"trees": ("HEAD", "HEAD")}
        v, cites = run_decider(
            dec, [("a/b.rs", "//! see `quality/no_such_file_here.toml`")],
            trees)
        if v != "B" or not cites:
            bad.append(f"dead-citation missed an absent path: {v} {cites}")
        v, _ = run_decider(
            dec, [("a/b.rs", "//! see `quality/arch-probes.toml`")], trees)
        if v != "A":
            bad.append("dead-citation flagged a path that exists")
        v, _ = run_decider(dec, [("a/b.rs", "//! see `tests/foo.rs`")], trees)
        if v != "A":
            bad.append("dead-citation flagged an illustrative placeholder")
        v, cites = run_decider(dec, [("a/b.rs", "//! `x/y.rs`")], {})
        if v != "C":
            bad.append(f"dead-citation claimed clean with no tree refs: {v}")

    if has("stringly"):
        dec = next(d for d in prof["deciders"] if d["id"] == "stringly")
        v, cites = run_decider(dec, PLANTED_B, {})
        if v != "B" or not cites:
            bad.append(f"stringly missed a planted 4-arm match: {v} {cites}")
        v, _ = run_decider(dec, PLANTED_A, {})
        if v != "A":
            bad.append(f"stringly flagged clean code: {v}")
        v, _ = run_decider(dec, [("f.rs", '"a" => 1,'), ("f.rs", '"b" => 2,')], {})
        if v != "A":
            bad.append(f"stringly flagged a 2-arm match (<= max is allowed): {v}")
    if has("silent-sub"):
        if not gate_rule(gate_of("silent-sub"), PLANTED_B, {}):
            bad.append("silent-sub gate missed unwrap_or")
        if gate_rule(gate_of("silent-sub"), PLANTED_A, {}):
            bad.append("silent-sub gate fired on ok_or (the correct idiom)")
    if has("addr-identity") and not gate_rule(gate_of("addr-identity"), PLANTED_B, {}):
        bad.append("addr-identity gate missed rows.len() as id")
    if has("untraced-branch") and gate_rule(gate_of("untraced-branch"), PLANTED_A, {}):
        bad.append("untraced gate fired on a hunk carrying tracing::debug!")
    if has("additive-bias") and not gate_rule(
            gate_of("additive-bias"), [], {"added_files": ["scripts/new_store.py"]}):
        bad.append("additive gate missed a new store script")
    if has("uncited-symbol"):
        dec = next(d for d in prof["deciders"] if d["id"] == "uncited-symbol")
        trees = {"trees": ("HEAD", "HEAD^")}
        # Spelled in two halves so this source file is not itself the
        # tree hit that would make the planted-B pass vacuously.
        ghost = "NoSuch" + "SymbolZq9"
        v, cites = run_decider(dec, [], trees, f"wire `{ghost}::frob` in")
        if v != "B" or not cites:
            bad.append(f"uncited-symbol missed a symbol absent from both trees: {v} {cites}")
        v, _ = run_decider(dec, [], trees, "extend `run_decider` and `ProfileError`")
        if v != "A":
            bad.append(f"uncited-symbol flagged symbols that exist at HEAD: {v}")
        v, _ = run_decider(dec, [], trees, "run `mesh join`, edit `AGENTS.md`, pass `--tighten`")
        if v != "A":
            bad.append(f"uncited-symbol treated a CLI verb / file / flag as a symbol: {v}")
        v, _ = run_decider(dec, [], {}, f"`{ghost}`")
        if v != "C":
            bad.append(f"uncited-symbol with no tree refs must say so (C), got {v}")
        if symbol_needles("`Foo::bar()` `baz_qux!` `HTTP_PORT` `x`") != [
                ("Foo::bar()", "bar"), ("baz_qux!", "baz_qux"), ("HTTP_PORT", "HTTP_PORT")]:
            bad.append(f"symbol_needles shape/needle mismatch: "
                       f"{symbol_needles('`Foo::bar()` `baz_qux!` `HTTP_PORT` `x`')}")
    try:
        st_added, st_files = collect_staged(prof["globs"])
        if st_files.get("trees") != ("--cached", "HEAD"):
            bad.append(f"collect_staged did not name its trees: {st_files.get('trees')}")
    except Exception as e:   # noqa: BLE001 — a self-test reports, never raises
        bad.append(f"collect_staged raised: {e}")

    for n, text, want in [
        (3, '{"v":["A","B","C"]}', ["A", "B", "C"]),
        (3, '{"v":["A","B","C"]}\nSome trailing prose.', ["A", "B", "C"]),
        (3, '{"v":["A","B"]}', None),          # count mismatch is not repaired
        (2, "garbage", None),
    ]:
        got = parse_letters(text, n)
        if got != want:
            bad.append(f"parse_letters({text[:28]!r}, {n}) -> {got}, want {want}")

    rows, tel = rows_for("deadbeef", PLANTED_A, {"added_files": []},
                         "planted clean commit", engine_on=False, profile=prof)
    if any(r["verdict"] == "B" for r in rows):
        bad.append("clean planted commit produced a B with the engine off")

    for b in bad:
        print(f"  MISBEHAVED: {b}")
    print(f"co-arch --self-test: {'BEHAVED' if not bad else f'{len(bad)} FAILURE(S)'}")
    return 0 if not bad else 4


def self_test_live(profile_path=None) -> int:
    """Planted-B through the real engine. Exit 5 = engine unusable, which
    is NEVER a quality verdict."""
    try:
        prof = load_profile(profile_path)
    except ProfileError as e:
        print(f"co-arch --self-test-live: profile did not load: {e}")
        return 4
    fired = [r for r in prof["rules"] if gate_rule(r, PLANTED_B, {})]
    if not fired:
        print("co-arch --self-test-live: planted-B fires no gate — fixture rot")
        return 4
    letters, model, tel = call_daemon(
        build_prompt(fired, "\n".join(f"{p}: {l}" for p, l in PLANTED_B)),
        len(fired))
    if letters is None and str(model).startswith("daemon-unavailable"):
        print(f"co-arch --self-test-live: engine unusable ({model}) — "
              f"NOT a quality verdict")
        return 5
    print(f"co-arch --self-test-live: model={model} {tel.get('wall_ms')}ms "
          f"prompt={tel.get('prompt_chars')}c out={tel.get('out_chars')}c")
    print(f"  planted-B fired={[r['id'] for r in fired]} letters={letters}")
    if letters is None:
        print("  MISBEHAVED: engine replied but no parseable letters")
        return 4
    if "B" not in letters:
        print("  MISBEHAVED: planted violations drew no B — the probe cannot "
              "catch what it exists to catch")
        return 4
    return 0


def rollup(hours: int, log: Path = VERDICTS_LOG) -> int:
    """The seat's read surface (briefing factor 4). Counts, sha pointers and
    the GATE's citation lines — no model authors a rendered line."""
    if not log.exists():
        print("co-arch rollup: no verdicts log — the audit has never run")
        return 0
    cutoff = _dt.datetime.now(_dt.timezone.utc) - _dt.timedelta(hours=hours)
    rows = []
    for line in log.read_text(encoding="utf-8", errors="replace").splitlines():
        if not line.strip():
            continue
        try:
            d = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(d, dict) or d.get("kind") != "arch":
            continue
        try:
            if _dt.datetime.fromisoformat(d["ts"]) >= cutoff:
                rows.append(d)
        except (KeyError, ValueError):
            continue
    if not rows:
        print(f"co-arch (shadow): no rows in {hours}h — audit last ran never, "
              f"or the sweep has not run since it was enabled")
        return 0
    tally: dict = {}
    for r in rows:
        tally[r.get("verdict", "?")] = tally.get(r.get("verdict", "?"), 0) + 1
    commits = len({r.get("ref") for r in rows})
    print(f"co-arch (shadow, {len(rows)} rule-verdict(s) over {commits} "
          f"commit(s), {hours}h): "
          + "  ".join(f"{k} {v}" for k, v in sorted(tally.items())))
    for r in [r for r in rows if r.get("verdict") == "B"][-12:]:
        cite = (r.get("citation") or [""])[0]
        print(f"  B {str(r.get('ref', ''))[:9]}  {r.get('rule', '?'):18}"
              f"{r.get('sec', ''):12} {str(cite)[:80]}")
    unjudged = [r for r in rows if r.get("verdict") == "C"]
    if unjudged:
        # NEVER let could-not-judge read as clean (ARCH §18.2).
        print(f"  C {len(unjudged)} rule-verdict(s) not judged "
              f"(evidence or engine) — deferred, NOT clean")
    return 0


def sweep(rng: str, limit: int, prof: dict) -> int:
    """Report-only: the CODE deciders over a commit range, nothing persisted
    and no model call. This is how a new decider earns a place in the live
    profile -- its false-positive rate is measured against real history
    first, because a gate nobody has watched fire is not a gate (ARCH
    SS18.1). The commits it fires on ARE the negative control: they were
    written before the rule existed, so a B here is a hit the rule could not
    have been fitted to."""
    shas = _git("rev-list", "--no-merges", rng).split()[:limit]
    if not shas:
        print(f"co-arch --range {rng}: no commits", file=sys.stderr)
        return 4
    tally: dict = {d["id"]: {"A": 0, "B": 0, "C": 0} for d in prof["deciders"]}
    sites: dict = {d["id"]: [] for d in prof["deciders"]}
    fired_commits: dict = {d["id"]: set() for d in prof["deciders"]}
    n_touched = 0
    for sha in shas:
        added, files, msg = collect(sha, prof["globs"])
        if not added:
            continue
        n_touched += 1
        for d in prof["deciders"]:
            v, cites = run_decider(d, added, files, msg)
            tally[d["id"]][v] += 1
            if v == "B":
                fired_commits[d["id"]].add(sha)
                sites[d["id"]].extend(f"{sha[:9]} {c}" for c in cites)
    print(f"co-arch --range {rng} (profile {prof['id']}, deciders only): "
          f"{len(shas)} commit(s), {n_touched} touching the profile globs\n")
    print(f"  {'decider':18} {'commits B':>9} {'rate':>7} {'sites':>7}   anchor")
    for d in prof["deciders"]:
        i = d["id"]
        n = len(fired_commits[i])
        rate = n / max(n_touched, 1)
        print(f"  {i:18} {n:9d} {rate:6.0%} {len(sites[i]):7d}   {d['sec']}")
        if tally[i]["C"]:
            # NEVER let could-not-judge read as clean (ARCH SS18.2).
            print(f"  {'':18} {tally[i]['C']} commit(s) NOT judged -- deferred,"
                  f" not clean")
    for d in prof["deciders"]:
        if not sites[d["id"]]:
            continue
        print(f"\n  --- {d['id']} ({len(sites[d['id']])} site(s)) ---")
        for line in sites[d["id"]][:12]:
            print(f"    {line[:150]}")
        if len(sites[d["id"]]) > 12:
            print(f"    ... {len(sites[d['id']]) - 12} more")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(add_help=True)
    ap.add_argument("sha", nargs="?")
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--self-test-live", action="store_true")
    ap.add_argument("--rollup", action="store_true",
                    help="seat-facing summary of recent arch rows")
    ap.add_argument("--range", dest="rng", default=None,
                    help="deciders-only sweep over a commit range (A..B): "
                         "what the code-decided rules would have said on "
                         "real history. No model call, nothing persisted.")
    ap.add_argument("--limit", type=int, default=200,
                    help="with --range: most commits to sweep (default 200)")
    ap.add_argument("--hours", type=int, default=24)
    ap.add_argument("--profile", default=None,
                    help="rule set TOML (default: <repo>/quality/arch-probes.toml, "
                         "or $CO_ARCH_PROFILE)")
    ap.add_argument("--dry-run", action="store_true",
                    help="gate only; name the rules that would fire, no call")
    ap.add_argument("--staged", action="store_true",
                    help="gate only, on the index: what would fire on the "
                         "commit about to be made (no model call)")
    ap.add_argument("-m", "--message", default="",
                    help="with --staged: the intended commit message, so "
                         "message-shaped deciders (uncited-symbol) can run")
    a = ap.parse_args()
    if a.self_test:
        return self_test(a.profile)
    if a.self_test_live:
        return self_test_live(a.profile)
    if a.rollup:
        return rollup(a.hours)
    if not a.sha and not a.staged and not a.rng:
        ap.error("a commit sha, --range or --staged is required")

    try:
        prof = load_profile(a.profile)
    except ProfileError as e:
        # Refused, named, and NOT replaced by a second copy of the rules.
        print(f"co-arch: {e}", file=sys.stderr)
        return 0

    if a.rng:
        return sweep(a.rng, a.limit, prof)

    if a.staged:
        added, files = collect_staged(prof["globs"])
        found = findings(added, files, a.message, prof)
        print(f"co-arch --staged (profile {prof['id']}): added_lines={len(added)} "
              f"finding(s)={len(found)}")
        mark = {"decided": "B", "question": "?", "unjudged": "C"}
        for f in found:
            head = f["cites"][0][:90] if f["cites"] else f["text"][:90]
            print(f"  {mark[f['kind']]} {f['id']:18} {f['sec']:12} "
                  f"{len(f['cites'])} site(s)  {head}")
        return 0

    sha = _git("rev-parse", a.sha).strip()
    if not sha:
        print(f"co-arch: unresolvable ref {a.sha}", file=sys.stderr)
        return 0
    added, files, msg = collect(sha, prof["globs"])
    if not added:
        print(f"co-arch {sha[:9]}: no added code lines in {prof['id']} globs "
              f"— zero model calls")
        return 0
    if a.dry_run:
        fired_dry = [(r, gate_rule(r, added, files)) for r in prof["rules"]]
        fired_dry = [(r, c) for r, c in fired_dry if c]
        bundle = build_bundle(added, files, fired_dry, msg, prof)
        print(f"co-arch {sha[:9]} (dry, profile {prof['id']}): "
              f"added_lines={len(added)} bundle={len(bundle)}c")
        for r in prof["rules"]:
            k = len(gate_rule(r, added, files))
            print(f"  {'FIRE' if k else 'gate'} {r['id']:18} {k} citation line(s)")
        return 0
    rows, tel = rows_for(sha, added, files, msg, True, prof)
    append(rows)
    report(sha, rows, tel)
    return 0


if __name__ == "__main__":
    sys.exit(main())
