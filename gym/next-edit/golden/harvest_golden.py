#!/usr/bin/env python3
"""Stratified next-edit golden-set harvester.

Spec: `sovereign/docs/specs/NEXT_EDIT_BAKEOFF.md` §2. Taxonomy + sizing:
`gym/next-edit/golden/README.md`.

Mines many repositories for frontier-shape editing episodes and emits
cases in the daemon's wire contract, with ground truth taken from the
REST OF THE SAME COMMIT — the edits the author actually went on to make.
Labels are by construction: no teacher, no judge, no model.

Three things this does that `../harvest.py` does not:

1. **Stratifies by shape**, over the taxonomy in `shapes.py`, including
   shapes the consult gate declines. A bank drawn only from shapes the
   gate admits is a mirror of the gate and cannot measure a missed fire.
2. **Stratifies by language**, with a per-language ceiling, because a
   bank that is half Rust measures Rust.
3. **Cuts for contamination.** Every published candidate trained on
   public GitHub, so episodes are restricted to commits dated after
   `--since` (default past the latest candidate release) and, with
   `--repos-file`, to repositories chosen for obscurity. The collision
   risk is structural, not statistical, so the defence is too.

Deterministic: no RNG. Same repos + same `--since` -> same bank.

    python3 gym/next-edit/golden/harvest_golden.py --repo . --limit-per-shape 60
    python3 gym/next-edit/golden/harvest_golden.py --repos-file repos.txt --out golden.jsonl
"""

from __future__ import annotations

import argparse
import collections
import json
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))

from harvest import CTX_CHARS, MAX_FILE_BYTES, SKIP_SUBSTR, chars_to_u16, predict, strip_common  # noqa: E402
from shapes import FAR_LINES, LANG_OF, SHAPES, Edit, Episode, FileDiff, detect_all, diff_edits  # noqa: E402
from negatives import NEGATIVES, detect_negatives  # noqa: E402

# Past every candidate's publication (Sweep-1.5B Jan, Sweep-v2-7B May 14,
# Zeta-2 March, Mellum2 June). A commit authored after this cannot be in
# any of their training sets — the one contamination defence that does
# not depend on guessing a vendor's crawl.
DEFAULT_SINCE = "2026-07-01"

MAX_EDITS_PER_FILE = 200
MAX_TRUTH = 8
# Mirrors `routes_edit_predictions::MAX_UNIT_BYTES` — BYTES, not chars.
MAX_UNIT_BYTES = 2 * 1024

# Episodes dropped for reasons that are findings in their own right.
EXCLUDED: collections.Counter = collections.Counter()


def git(repo: Path, *args: str) -> bytes:
    return subprocess.run(
        ["git", "-C", str(repo), *args], capture_output=True
    ).stdout


# ---- case construction ------------------------------------------------


def apply_edits(old_lines: list[str], edits: list[Edit]) -> list[str]:
    """Apply edits (old-file coordinates) bottom-up so earlier indices
    stay valid."""
    out = old_lines[:]
    for e in sorted(edits, key=lambda e: e.old_start, reverse=True):
        out[e.old_start : e.old_end] = list(e.new_lines)
    return out


def line_offsets(lines: list[str]) -> list[int]:
    offs, acc = [], 0
    for l in lines:
        offs.append(acc)
        acc += len(l) + 1
    return offs


def unit_of(e: Edit, old: str, offs: list[int]) -> dict | None:
    """One coalesced edit unit as the extension would have captured it:
    a `{before, after}` replacement plus untouched context each side."""
    if e.old_start >= len(offs):
        return None
    at = offs[e.old_start]
    if e.kind == "replace" and len(e.old_lines) == 1 and len(e.new_lines) == 1:
        a, b = e.old_lines[0], e.new_lines[0]
        p, s = strip_common(a, b)
        before, after = a[p : len(a) - s], b[p : len(b) - s]
        at += p
        end = at + len(before)
    elif e.kind == "insert":
        before, after = "", e.new_text
        end = at
    elif e.kind == "delete":
        before, after = e.old_text, ""
        end = at + len(before)
    else:
        before, after = e.old_text, e.new_text
        end = at + len(before)
    if before == after:
        return None
    return {
        "before": before,
        "after": after,
        "left": old[max(0, at - CTX_CHARS) : at],
        "right": old[end : end + CTX_CHARS],
    }


def build_case(fd: FileDiff, ep: Episode, cid: str) -> dict | None:
    """An episode -> one wire-contract case with held-out ground truth."""
    old_lines = fd.old.split("\n")
    offs = line_offsets(old_lines)

    negative = ep.shape in NEGATIVES
    exemplars = sorted(ep.exemplars, key=lambda e: e.old_start)
    truth = sorted(ep.truth, key=lambda e: e.old_start)[:MAX_TRUTH]
    if len(exemplars) < 2 or (not truth and not negative):
        # The consult gate needs an exemplar PAIR to induce anything
        # (`exemplar_pair`), so a one-exemplar episode can never reach
        # the model lane and is not a case, it is a hole.
        return None
    if ep.doc_mode == "apply" and any(
        a.old_end > b.old_start for a, b in zip(exemplars, exemplars[1:])
    ):
        return None
    if {id(e) for e in exemplars} & {id(e) for e in truth}:
        return None

    history = [unit_of(e, fd.old, offs) for e in exemplars]
    if any(u is None for u in history):
        return None
    # The wire caps each unit field at 2 KiB of UTF-8 (`validate_wire`),
    # and the extension enforces the same bound before sending, so an
    # episode whose units exceed it can never reach the daemon in
    # production — a multi-line block insert hits this easily. Excluded
    # from the bank, but COUNTED: "N% of real episodes are excluded by
    # the unit cap" is a finding about the contract, not a detail to
    # swallow (ARCH §18.3). Measured on this repo: 28/263, ~11%.
    if any(
        len(u[f].encode()) > MAX_UNIT_BYTES
        for u in history
        for f in ("before", "after", "left", "right")
    ):
        EXCLUDED["unit_cap"] += 1
        return None

    # The document the model sees: exemplars applied, truth still absent.
    mid_lines = old_lines[:] if ep.doc_mode == "old" else apply_edits(old_lines, exemplars)
    text = "\n".join(mid_lines)
    if len(text) > MAX_FILE_BYTES:
        return None

    # Rebase truth into mid-document coordinates. Exemplars and truth are
    # disjoint line ranges, so each truth edit shifts by the cumulative
    # line delta of the exemplars above it.
    mid_offs = line_offsets(mid_lines)
    sites, char_points = [], []
    for t in truth:
        shift = 0 if ep.doc_mode == "old" else sum(
            e.line_delta for e in exemplars if e.old_end <= t.old_start
        )
        s_line = t.old_start + shift
        e_line = t.old_end + shift
        if not (0 <= s_line <= e_line <= len(mid_lines)):
            return None
        # Verify the rebase landed on the text we expect to replace.
        if list(mid_lines[s_line:e_line]) != list(t.old_lines):
            return None
        start = mid_offs[s_line] if s_line < len(mid_offs) else len(text)
        end = (
            mid_offs[e_line] if e_line < len(mid_offs) else len(text)
        ) if e_line > s_line else start
        sites.append({"start_c": start, "end_c": end, "new_text": t.new_text})
        char_points += [start, end]

    last = exemplars[-1]
    cursor_c = offs[last.old_start] + sum(
        e.line_delta for e in exemplars[:-1]
    ) * 0  # cursor is derived below in mid coordinates
    cur_line = last.old_start + (0 if ep.doc_mode == "old" else sum(
        e.line_delta for e in exemplars[:-1]))
    cursor_c = mid_offs[min(cur_line, len(mid_offs) - 1)] if mid_offs else 0
    char_points.append(cursor_c)

    u16 = chars_to_u16(text, sorted(set(char_points)))
    wire_sites = [
        {"start": u16[s["start_c"]], "end": u16[s["end_c"]], "new_text": s["new_text"]}
        for s in sites
    ]

    # A negative whose correct answer is silence must be one the
    # DETERMINISTIC lane is already silent on — otherwise "it fired" is
    # ambiguous between "our label is wrong" and "the rule lane has a
    # bug", and an ambiguous negative cannot score anything. The rule
    # replica is the referee. `neg_literal_trap` is exempt BY DESIGN:
    # the whole point of that shape is that a text-only engine does
    # fire into comments and string literals, so its rule-lane fires
    # are the finding, not a mislabel.
    if negative and ep.shape != "neg_literal_trap":
        try:
            if predict(history, text, cursor_c)["edits"]:
                EXCLUDED[f"{ep.shape}:rule-lane-fires"] += 1
                return None
        except Exception:
            pass

    far = (min(t.old_start for t in truth) - last.old_end > FAR_LINES) if truth else False
    return {
        "id": cid,
        "shape": ep.shape,
        "kind": "negative" if negative else "positive",
        "language": fd.language,
        "gate": NEGATIVES[ep.shape]["why"] if negative else SHAPES[ep.shape]["gate"],
        "far_from_cursor": far,
        "provenance": {
            "repo": fd.repo,
            "commit": fd.commit,
            "date": fd.date,
            "path": fd.path,
            "note": ep.note,
        },
        "request": {
            "history": history,
            "text": text,
            "cursor": u16[cursor_c],
            "path": fd.path,
            "language": fd.language,
            "debug": True,
            "model_lane": True,
        },
        "expect": (
            {"fire": False, "why": NEGATIVES[ep.shape]["why"]}
            if negative
            else {"fire": True, "truth": wire_sites}
        ),
    }


# ---- mining -----------------------------------------------------------


def mine_repo(
    repo: Path, since: str, max_commits: int, seen: set, quota: dict, args
) -> list[dict]:
    name = repo.resolve().name
    log = git(repo, "log", "--no-merges", f"--since={since}",
              "--format=%H %ad", "--date=short", f"--max-count={max_commits}")
    cases: list[dict] = []
    for line in log.decode(errors="replace").splitlines():
        if not line.strip():
            continue
        commit, _, date = line.partition(" ")
        names = git(repo, "diff-tree", "-r", "--no-renames", "--diff-filter=M",
                    "--name-only", commit).decode(errors="replace").splitlines()[1:]
        if len(names) > 25:
            continue
        for path in names:
            suf = "." + path.rsplit(".", 1)[-1] if "." in path else ""
            lang = LANG_OF.get(suf)
            if not lang or any(s in path for s in SKIP_SUBSTR):
                continue
            if quota["lang"][lang] >= args.limit_per_language:
                continue
            if quota["repolang"][(name, lang)] >= args.limit_per_repo_language:
                continue
            try:
                old = git(repo, "show", f"{commit}~1:{path}").decode()
                new = git(repo, "show", f"{commit}:{path}").decode()
            except Exception:
                continue
            if not old or not new or len(old) > MAX_FILE_BYTES:
                continue
            edits = diff_edits(old, new)
            if not edits or len(edits) > MAX_EDITS_PER_FILE:
                continue
            fd = FileDiff(name, commit, date, path, lang, old, new, edits)
            eps = detect_all(fd)
            if args.limit_per_negative:
                eps = eps + detect_negatives(fd)
            for ep in eps:
                cap = (args.limit_per_negative if ep.shape in NEGATIVES
                       else args.limit_per_shape)
                if quota["shape"][ep.shape] >= cap:
                    continue
                if quota["lang"][lang] >= args.limit_per_language:
                    break
                cid = f"g-{ep.shape}-{len(cases):05d}-{commit[:7]}"
                case = build_case(fd, ep, cid)
                if not case:
                    continue
                # Dedup on the induced intent so one popular refactor
                # cannot flood a shape with near-identical episodes.
                sig = (ep.shape, lang, ep.note, len(case["expect"].get("truth", ())))
                if sig in seen:
                    continue
                seen.add(sig)
                quota["shape"][ep.shape] += 1
                quota["lang"][lang] += 1
                quota["repolang"][(name, lang)] += 1
                cases.append(case)
    return cases


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", action="append", default=None,
                    help="path to a git repo (repeatable)")
    ap.add_argument("--repos-file", default=None,
                    help="file of repo paths, one per line")
    ap.add_argument("--since", default=DEFAULT_SINCE,
                    help=f"contamination cut; default {DEFAULT_SINCE}")
    ap.add_argument("--max-commits", type=int, default=4000)
    ap.add_argument("--limit-per-shape", type=int, default=60)
    ap.add_argument("--limit-per-negative", type=int, default=100,
                    help="cap per negative shape; 0 disables negatives")
    ap.add_argument("--limit-per-language", type=int, default=120)
    ap.add_argument("--limit-per-repo-language", type=int, default=35,
                    help="share per (repo, language) so the first repo mined "
                         "cannot exhaust a language quota for all the others")
    ap.add_argument("--out", default=str(HERE / "cases.jsonl"))
    args = ap.parse_args()

    repos = [Path(r) for r in (args.repo or [])]
    if args.repos_file:
        repos += [Path(l.strip()) for l in Path(args.repos_file).read_text().splitlines()
                  if l.strip() and not l.startswith("#")]
    if not repos:
        sys.exit("no repos: pass --repo or --repos-file")

    quota = {"shape": collections.Counter(), "lang": collections.Counter(),
             "repolang": collections.Counter()}
    seen: set = set()
    all_cases: list[dict] = []
    for repo in repos:
        if not (repo / ".git").exists() and not (repo / "HEAD").exists():
            print(f"  skip (not a git repo): {repo}", file=sys.stderr)
            continue
        got = mine_repo(repo, args.since, args.max_commits, seen, quota, args)
        print(f"  {repo}: +{len(got)}", file=sys.stderr)
        all_cases += got

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("w") as fh:
        for c in all_cases:
            fh.write(json.dumps(c) + "\n")

    print(f"\n{len(all_cases)} cases -> {out}\n")
    print(f"{'shape':<20} {'gate':>10} {'n':>5}")
    for name, spec in SHAPES.items():
        n = quota["shape"].get(name, 0)
        flag = "" if n >= args.limit_per_shape else "  UNDER QUOTA"
        print(f"{name:<20} {spec['gate']:>10} {n:>5}{flag}")
    print()
    print(f"{'negative shape':<20} {'n':>5}   correct answer")
    for name, spec in NEGATIVES.items():
        print(f"{name:<20} {quota['shape'].get(name, 0):>5}   {spec['why']}")
    print(f"\nlanguages: {dict(quota['lang'].most_common())}")
    if EXCLUDED:
        print(f"\nexcluded (reported, not swallowed): {dict(EXCLUDED)}")
    # A stratum that missed quota is reported, never silently accepted:
    # a bank that claims 12 shapes and carries 3 of them is a bank that
    # will be quoted as covering 12 (ARCH §18.3).
    short = [s for s in SHAPES if quota["shape"].get(s, 0) < args.limit_per_shape]
    npos = sum(quota['shape'][s] for s in SHAPES)
    nneg = sum(quota['shape'][s] for s in NEGATIVES)
    print(f"\npositives {npos} · negatives {nneg}")
    if short:
        print(f"\n{len(short)} shape(s) under quota — widen the corpus "
              f"(--repos-file) before treating this bank as stratified.")


if __name__ == "__main__":
    main()
