#!/usr/bin/env python3
"""M0 of the symbol lane — a PROBE (sovereign/docs/specs/NEXT_EDIT_SYMBOL_LANE.md).

Writes no daemon code and changes no behaviour. It answers one question:

  when a developer edits a function's SIGNATURE, does the SCIP graph built
  from the last save name the call sites the author then edited?

Pre-registered bars (set before this ran, see the spec):
  site recall     >= 80%
  site precision  >= 60%
  trigger yield   >= 25 episodes, else report the CI and rank nothing

Ground truth is the author's own commit and is INDEPENDENT of the graph:
a call site is "one the author edited" iff the commit changed that line
AND the line names the symbol. Nothing in that test consults `refs`.

`derive_episodes` is the ONE derivation of that population. M1a's
classifier (classify_overoffer.py) imports it rather than re-deriving,
so the two cannot drift apart — the episode counts printed here and the
population classified there are the same objects.
"""
from __future__ import annotations

import argparse
import collections
import os
import re
import sqlite3
import subprocess
from pathlib import Path

DB = os.path.expanduser("~/.svrnmesh/indexes/commonwealth-ai/scip_graph.db")
# A signature spans a few lines when it wraps; the declaration starts at
# the symbol's own line_start.
SIG_SPAN = 4


def git(*a: str) -> str:
    return subprocess.run(["git", *a], capture_output=True, text=True,
                          errors="replace").stdout


def last_touching(window: int) -> dict[str, str]:
    out = git("log", "--no-merges", "--format=%x00%H", "--name-only", f"-n{window}")
    last, cur = {}, None
    for line in out.split("\n"):
        if line.startswith("\x00"):
            cur = line[1:].strip()
            continue
        p = line.strip()
        if p and p not in last and cur:
            last[p] = cur
    return last


def changed_new_lines(commit: str) -> dict[str, set[int]]:
    """{path -> 0-based NEW-side line numbers the commit touched}."""
    out = git("show", "--unified=0", "--no-renames", "--format=", commit)
    per: dict[str, set[int]] = collections.defaultdict(set)
    path = None
    for line in out.split("\n"):
        if line.startswith("+++ b/"):
            path = line[6:].strip()
        elif line.startswith("@@") and path:
            m = re.search(r"\+(\d+)(?:,(\d+))?", line)
            if m:
                start = int(m.group(1)) - 1
                for i in range(int(m.group(2) or 1)):
                    per[path].add(start + i)
    return per


class Corpus:
    """The git/source accessors the derivation needs, memoised once.

    One accessor per path (ARCH §10.6): the classifier reuses these
    rather than opening its own, so "is this file aligned?" has a single
    answer in both scripts.
    """

    def __init__(self) -> None:
        self._moved: dict[str, set[str]] = {}
        self._diff: dict[str, dict[str, set[int]]] = {}
        self._src: dict[str, list[str]] = {}

    def aligned(self, path: str, commit: str) -> bool:
        """Is this file byte-identical between that commit and HEAD? Only
        then do the graph's HEAD line numbers describe the commit's."""
        if commit not in self._moved:
            # ONE subprocess per commit, not per file: everything that
            # differs between the commit and HEAD, so "aligned" is a set
            # lookup. Doing it per file shelled out thousands of times
            # and did not finish.
            out = git("diff", "--name-only", "--no-renames", commit, "HEAD")
            self._moved[commit] = {l.strip() for l in out.split("\n") if l.strip()}
        return path not in self._moved[commit] and Path(path).exists()

    def touched(self, commit: str) -> dict[str, set[int]]:
        if commit not in self._diff:
            self._diff[commit] = changed_new_lines(commit)
        return self._diff[commit]

    def lines(self, p: str) -> list[str]:
        if p not in self._src:
            self._src[p] = Path(p).read_text(encoding="utf-8", errors="replace").split("\n")
        return self._src[p]


def derive_episodes(con: sqlite3.Connection, window: int, cross_file_only: bool):
    """The M0 population. Returns (episodes, counters, corpus).

    Each episode carries the SITE SETS, not just their sizes, so a caller
    can classify them. M0 itself only needs the cardinalities.
    """
    corpus = Corpus()
    last = last_touching(window)
    counters: collections.Counter = collections.Counter()

    # PASS 1 — find the signature edits. `symbols` is indexed on file_path
    # so these lookups are cheap.
    triggers = []
    seen_trigger: set[tuple[str, str]] = set()
    for path, commit in sorted(last.items()):
        if not path.endswith(".rs") or not corpus.aligned(path, commit):
            continue
        touched = corpus.touched(commit)
        if path not in touched:
            continue
        counters["files_scanned"] += 1
        for line in sorted(touched[path]):
            rows = con.execute(
                """select name, qualified_name, line_start, line_end from symbols
                   where file_path=? and line_start<=? and line_end>=?
                     and qualified_name like '%().'
                   order by (line_end-line_start) asc limit 1""",
                (path, line, line)).fetchall()
            if not rows:
                continue
            name, qual, ls, le = rows[0]
            if not name or not (ls <= line < ls + SIG_SPAN):
                continue                 # edit was in the body, not the signature
            # ...and the line must actually DECLARE it. Span proximity alone
            # counts body edits near the top of any short function, which
            # inflated this 15176-fold on the first run.
            src = corpus.lines(path)
            if not any(re.search(rf"\bfn\s+{re.escape(name)}\b", src[l])
                       for l in range(ls, min(ls + SIG_SPAN, len(src)))):
                continue
            counters["signature_edits"] += 1
            key = (commit, qual)
            if key in seen_trigger:
                continue                 # a wrapped signature is ONE episode
            seen_trigger.add(key)
            triggers.append((path, commit, line, name, qual, ls, le))

    # A textual truth cannot tell `Foo::new(` from `Bar::new(`. Where the
    # repo defines the same NAME on more than one symbol, the heuristic
    # would count another type's call sites as this author's and depress
    # recall for a reason that has nothing to do with the graph. Those
    # episodes are EXCLUDED and counted, not silently scored.
    counters["episodes_deduped"] = len(triggers)
    kept = []
    for t in triggers:
        n = con.execute("select count(distinct qualified_name) from symbols where name=?",
                        (t[3],)).fetchone()[0]
        if n > 1:
            counters["excluded_ambiguous_name"] += 1
        else:
            kept.append(t)
    triggers = kept

    # PASS 2 — ONE scan of `refs` for every callee at once. There is NO
    # index on callee_qualified (only on the empty callee_symbol column),
    # so a per-symbol query is a full scan of 1.36M rows each time.
    wanted = {t[4] for t in triggers}
    calls: dict[str, list[tuple[str, int]]] = collections.defaultdict(list)
    if wanted:
        for q, f, l in con.execute("select callee_qualified, file_path, line from refs"):
            if q in wanted:
                calls[q].append((f, l))
    counters["callees_resolved"] = len(calls)

    episodes = []
    for path, commit, line, name, qual, ls, le in triggers:
        pred = {(f, l) for f, l in calls.get(qual, [])
                if not (f == path and ls <= l <= le)}
        # Comparable only where the call-site file is ALSO byte-identical to
        # its state in this commit; elsewhere HEAD lines are not the
        # commit's lines. Dropped sites are counted, never guessed.
        comparable = {(f, l) for f, l in pred if corpus.aligned(f, commit)}
        counters["pred_sites_dropped_unaligned"] += len(pred) - len(comparable)

        # AUTHOR TRUTH, graph-independent: a line this commit changed that
        # names the symbol with a call paren.
        author = set()
        for f, lines in corpus.touched(commit).items():
            if not f.endswith(".rs") or not corpus.aligned(f, commit):
                continue
            if f == path and cross_file_only:
                continue
            fsrc = corpus.lines(f)
            for l in lines:
                if f == path and ls <= l < ls + SIG_SPAN:
                    continue      # the declaration edit is the TRIGGER, not a target
                if not (0 <= l < len(fsrc)):
                    continue
                if re.search(rf"\bfn\s+{re.escape(name)}\b", fsrc[l]):
                    continue              # a declaration, not a call site
                if re.search(rf"\b{re.escape(name)}\s*\(", fsrc[l]):
                    author.add((f, l))
        if not author:
            counters["no_call_site_edits"] += 1
            continue
        episodes.append({"symbol": name, "qualified": qual, "commit": commit,
                         "decl_path": path, "decl_start": ls, "decl_end": le,
                         "author_sites": author, "pred_sites": comparable})
    return episodes, counters, corpus


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--window", type=int, default=8000)
    ap.add_argument("--db", default=DB)
    ap.add_argument("-v", "--verbose", action="store_true")
    # Same-file call sites are just as valid a target: the rule lane cannot
    # induce a same-file signature fanout either, because the declaration
    # edit and the call-site edits are still different text. Excluding them
    # measures a narrower thing than the lane would actually do, so the
    # faithful default counts them. --cross-file-only reproduces the first
    # run for comparison.
    ap.add_argument("--cross-file-only", action="store_true")
    args = ap.parse_args()
    con = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)

    episodes, counters, _ = derive_episodes(con, args.window, args.cross_file_only)

    if args.verbose:
        for e in episodes:
            hit = e["author_sites"] & e["pred_sites"]
            print(f"  {e['symbol']:28s} author={len(e['author_sites']):>2} "
                  f"pred={len(e['pred_sites']):>3} hit={len(hit):>2}  "
                  f"{e['decl_path'].split('/')[-1]}")

    print(f"\nM0 — symbol lane probe (signature_fanout, rust, index-aligned)")
    print(f"  files scanned                  {counters['files_scanned']}")
    print(f"  signature-edit LINES           {counters['signature_edits']} (pre-dedup)")
    print(f"  distinct (commit,symbol)       {counters['episodes_deduped']}")
    print(f"  ...with call-site edits        {len(episodes)}   <- TRIGGER YIELD (bar: >=25)")
    print(f"  ...without any                 {counters['no_call_site_edits']}")
    print(f"  excluded, name not unique      {counters['excluded_ambiguous_name']}")
    print(f"  predicted sites dropped as unaligned: {counters['pred_sites_dropped_unaligned']}")
    if not episodes:
        raise SystemExit("\n  no episodes — nothing to measure, and no rate is published.")
    A = sum(len(e["author_sites"]) for e in episodes)
    P = sum(len(e["pred_sites"]) for e in episodes)
    H = sum(len(e["author_sites"] & e["pred_sites"]) for e in episodes)
    print(f"\n  author-edited call sites       {A}")
    print(f"  sites the graph named          {P}")
    print(f"  overlap                        {H}")
    print(f"\n  SITE RECALL     {H}/{A} = {H/A*100:.1f}%   (bar >= 80%)  "
          f"{'PASS' if H/A >= .8 else 'FAIL'}")
    print(f"  SITE PRECISION  {H}/{P} = {H/P*100:.1f}%   (bar >= 60%)  "
          f"{'PASS' if P and H/P >= .6 else 'FAIL'}")


if __name__ == "__main__":
    main()
