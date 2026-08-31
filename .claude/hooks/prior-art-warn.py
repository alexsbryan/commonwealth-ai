#!/usr/bin/env python3
"""PreToolUse advisory: name the prior art BEFORE a new concept is minted.

WHY. Measured on this repo 2026-08-08: the epistemic-state enum family reached
39 declarations over 138 days, and the MEDIAN member was written with 19
siblings already present. Commit 27c0fe031 minted SIX at once across 5 crates.
Every catch came from the operator, never from the builder's own process
(ARCH §19). The write path is where that is cheap to interrupt.

WHAT IT DOES. On an Edit/Write whose ADDED text declares a new type, it ranks
the existing per-symbol descriptions against that declaration and names the
closest prior art. It does not decide anything: ~63% recall@10 on independent
ground truth (note 5767f68b) is a WARN, nowhere near a gate, and the standing
decision is warn-before-gate — a false-positive machine gets switched off in a
week (concept_gate.rs).

RANKER. IDF-weighted lexical overlap over `summary + asks`, replicating
refactor_cmd/affinity.rs `query_terms`/`shortlist`. ONE DECIDER (ARCH §10.6):
if that ranker changes, this must change with it. Deliberately NOT embeddings —
measured 2026-08-31, lexical scored 62.8%/60.7% against embeddings' 60.5%/62.5%
on the same ground truth, so the vector path buys nothing and would put a
daemon round-trip in the hot path of every edit.

COST. Zero for the common case: an edit that declares no type exits before
reading anything. Only a real declaration pays the cache read.

NEVER BLOCKS. Always exit 0. Missing cache, unreadable payload, no daemon —
all mean silence, because a hook that fails loudly on every edit gets removed.
"""
import json
import math
import os
import re
import subprocess
import sys
from collections import Counter

MAX_HITS = 3
MIN_SCORE = 6.0          # one or two shared words is not prior art; silence beats a shrug
STOP = {"the","a","an","of","to","and","or","is","it","that","this","for","with",
        "into","from","in","on","by","as","be","are","was","one","any","all"}
DECL = re.compile(r'^\+?\s*(?:pub(?:\s*\([^)]*\))?\s+)?(enum|struct|trait)\s+([A-Z][A-Za-z0-9_]*)',
                  re.MULTILINE)


def terms(q):
    return [w.lower() for w in re.split(r'[^0-9A-Za-z]+', q)
            if len(w) > 2 and w.lower() not in STOP]


def tracked(root):
    """The git index decides source-vs-generated. Vendored trees and
    target/*/build are IN the corpus (note 0fb0f1c8); matching a new type
    against llama.cpp is a false positive about code we do not own."""
    try:
        out = subprocess.run(["git", "-C", root, "ls-files", "-z"],
                             capture_output=True, timeout=10)
        if out.returncode != 0:
            return None
        return set(out.stdout.decode("utf-8", "replace").split("\0"))
    except Exception:
        return None


def main():
    try:
        payload = json.load(sys.stdin)
    except Exception:
        return 0
    ti = payload.get("tool_input") or {}
    added = ti.get("new_string") or ti.get("content") or ""
    path = ti.get("file_path") or ""
    if not added or not path.endswith(".rs"):
        return 0
    # A type minted in a test, bench or example is a fixture, not sprawl. Firing
    # there is pure noise, and noise is how an advisory gets routed around.
    rel = path.replace("\\", "/")
    if re.search(r'(^|/)(tests?|benches|examples)/|/tests\.rs$|_test\.rs$', rel):
        return 0

    decls = DECL.findall(added)
    if not decls:
        return 0            # the common case: nothing minted, nothing read

    root = os.environ.get("CLAUDE_PROJECT_DIR") or os.getcwd()
    idx = os.path.expanduser("~/.svrnmesh/indexes")
    corpus = os.path.basename(os.path.realpath(root))
    cache = os.path.join(idx, corpus, "code_intel_cache.json")
    if not os.path.exists(cache):
        return 0
    try:
        with open(cache) as fh:
            entries = json.load(fh)
        entries = list(entries.values()) if isinstance(entries, dict) else entries
    except Exception:
        return 0

    keep = tracked(root)
    rows = []
    for e in entries:
        m = e.get("meta") or {}
        # TYPES ONLY. "Has this concept been declared before" is a question
        # about declarations, and the cache is ~87% functions (19,363 callable
        # to 2,497 type on this corpus). Ranking against the whole cache
        # answered a query about index staleness with `index_declarations`,
        # `symbol_lane` and `assert_daemon_is_ours` — plausible prose overlap,
        # useless advice, and exactly the false-positive noise that gets a warn
        # switched off. Routed on the SCIP descriptor suffix, the same decider
        # `code_intel::prompt_kind_for` uses: `Name#` is a type, `name().` a
        # callable (ARCH §10.6).
        if not m.get("qualified_name", "").rstrip().endswith("#"):
            continue
        fp = m.get("file_path", "")
        if keep is not None and fp not in keep:
            continue
        if fp == os.path.relpath(path, root):
            continue        # never cite the file being edited back at itself
        s = (e.get("summary") or "").strip()
        if not s:
            continue
        rows.append((m.get("name", "?"), fp, m.get("line_start", 0),
                     (s + " " + " ".join(e.get("asks") or [])).lower(), s))
    if not rows:
        return 0

    blocks = []
    for kw, name in decls[:2]:
        # The declaration itself is the query, minus its own name so the match
        # is on what it MEANS rather than what it is spelled.
        body = re.sub(r'\b%s\b' % re.escape(name), " ", added)
        ts = set(terms(body))
        if not ts:
            continue
        df = Counter()
        for _, _, _, hay, _ in rows:
            for t in ts:
                if t in hay:
                    df[t] += 1
        n = max(len(rows), 1)
        scored = []
        for nm, fp, ln, hay, summary in rows:
            sc = sum(math.log(n / max(df.get(t, 1), 1)) for t in ts if t in hay)
            if sc > MIN_SCORE:
                scored.append((sc, nm, fp, ln, summary))
        scored.sort(key=lambda x: -x[0])
        if not scored:
            continue
        lines = [f"`{kw} {name}` may already exist:"]
        for sc, nm, fp, ln, summary in scored[:MAX_HITS]:
            clipped = summary if len(summary) <= 110 else summary[:110].rsplit(" ", 1)[0] + "…"
            lines.append(f"  {nm} — {fp}:{ln}")
            lines.append(f"    {clipped}")
        blocks.append("\n".join(lines))

    if not blocks:
        return 0
    msg = ("\n\n".join(blocks)
           + "\n\nReuse or converge if one is the same concept; otherwise proceed. "
             "Advisory (~63% recall), not a gate.")
    print(json.dumps({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "additionalContext": msg,
    }}))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception:
        sys.exit(0)     # never block a write on this hook's own failure
