#!/usr/bin/env python3
"""Harvest FIM eval cases from real source in this repo.

Walks Rust / TypeScript / Python sources, picks real functions, and
masks them at a line boundary into (prefix, suffix, expected) triples:

  single  — cut mid-statement: expected is the rest of that line
  multi   — cut right after a block-opener line: expected is the next
            1-4 lines (the "body head" a ghost-text block completion
            should reproduce)

Output: gym/fim/cases.jsonl — one JSON object per line:
  {id, language, path, kind, prefix, suffix, expected, expected_first_line}

Deterministic (seeded) so the bank is stable across runs; re-run with
--n to resize. The bank is ~60 cases by default (25 rust / 20 ts / 15 py).
"""

from __future__ import annotations

import argparse
import json
import random
import re
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]

RUST_ROOTS = ["sovereign/crates", "commonwealth/crates"]
TS_ROOTS = ["packages/chat-ui/src", "sovereign/crates/sovereign-desktop/src"]
PY_ROOTS = ["scripts", "gym"]

# File filters: skip tests/benches/generated — completions there are
# unrepresentative boilerplate.
SKIP_RE = re.compile(r"(target/|/tests?/|_test\.|\.test\.|/benches?/|node_modules)")

FN_RE = {
    "rust": re.compile(r"^\s*(pub(\([^)]*\))?\s+)?(async\s+)?fn\s+\w+"),
    "typescript": re.compile(r"^\s*(export\s+)?(async\s+)?(function\s+\w+|[\w]+\s*\([^)]*\)\s*[:\{])"),
    "python": re.compile(r"^\s*(async\s+)?def\s+\w+"),
}

LANG_OF = {".rs": "rust", ".ts": "typescript", ".py": "python"}

MAX_PREFIX_CHARS = 6000   # keep prompts in the server's clamp window
MAX_SUFFIX_CHARS = 1500
MIN_FN_LINES = 6          # toy functions teach nothing


def functions_in(path: Path, lang: str) -> list[tuple[int, list[str]]]:
    """(start_line_idx, lines) for each function-like region."""
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return []
    out = []
    for i, line in enumerate(lines):
        if FN_RE[lang].match(line):
            out.append((i, lines))
    return out


def make_case(rng: random.Random, lang: str, path: Path, start: int, lines: list[str]) -> dict | None:
    """One masked case from the function starting at `start`."""
    if len(lines) - start < MIN_FN_LINES:
        return None
    kind = rng.choice(["single", "multi"])
    if kind == "single":
        # Cut mid-line inside the function body (not the opener line,
        # not a blank/comment line).
        cands = [
            i for i in range(start + 1, min(start + MIN_FN_LINES, len(lines)))
            if len(lines[i].strip()) >= 12 and not lines[i].strip().startswith(("//", "#", "*"))
        ]
        if not cands:
            return None
        cut = rng.choice(cands)
        col = rng.randrange(len(lines[cut]) // 3, 2 * len(lines[cut]) // 3)
        prefix_lines = lines[:cut] + [lines[cut][:col]]
        expected = lines[cut][col:]
        suffix_lines = lines[cut + 1:]
    else:
        # Cut right after a block-opener line inside the function.
        cands = [
            i for i in range(start, min(start + MIN_FN_LINES - 1, len(lines) - 2))
            if lines[i].rstrip().endswith(("{", "(", "[", ":"))
            and i + 2 < len(lines)
            and lines[i + 1].strip()
        ]
        if not cands:
            return None
        cut = rng.choice(cands)
        prefix_lines = lines[:cut + 1]
        take = rng.randrange(1, 4)
        expected = "\n".join(lines[cut + 1:cut + 1 + take])
        suffix_lines = lines[cut + 1 + take:]

    prefix = "\n".join(prefix_lines)[-MAX_PREFIX_CHARS:]
    suffix = "\n".join(suffix_lines)[:MAX_SUFFIX_CHARS]
    expected_first = expected.strip().splitlines()[0] if expected.strip() else ""
    if not expected.strip():
        return None
    rel = path.relative_to(REPO).as_posix()
    return {
        "id": f"{rel}:{start}",
        "language": lang,
        "path": path.name,
        "kind": kind,
        "prefix": prefix,
        "suffix": suffix,
        "expected": expected,
        "expected_first_line": expected_first,
    }


def harvest(lang: str, roots: list[str], n: int, rng: random.Random) -> list[dict]:
    exts = {k for k, v in LANG_OF.items() if v == lang}
    files = []
    for root in roots:
        r = REPO / root
        if not r.exists():
            continue
        for p in sorted(r.rglob("*")):
            if p.suffix in exts and not SKIP_RE.search(p.as_posix()):
                files.append(p)
    rng.shuffle(files)
    cases = []
    for p in files:
        if len(cases) >= n:
            break
        fns = functions_in(p, lang)
        rng.shuffle(fns)
        for start, lines in fns:
            if len(cases) >= n:
                break
            case = make_case(rng, lang, p, start, lines)
            if case:
                cases.append(case)
    return cases


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--rust", type=int, default=25)
    ap.add_argument("--ts", type=int, default=20)
    ap.add_argument("--py", type=int, default=15)
    ap.add_argument("--seed", type=int, default=20260721)
    ap.add_argument("--out", type=Path, default=Path(__file__).with_name("cases.jsonl"))
    args = ap.parse_args()

    rng = random.Random(args.seed)
    cases = (
        harvest("rust", RUST_ROOTS, args.rust, rng)
        + harvest("typescript", TS_ROOTS, args.ts, rng)
        + harvest("python", PY_ROOTS, args.py, rng)
    )
    rng.shuffle(cases)
    with args.out.open("w", encoding="utf-8") as fh:
        for c in cases:
            fh.write(json.dumps(c, ensure_ascii=False) + "\n")
    kinds = {}
    for c in cases:
        kinds[(c["language"], c["kind"])] = kinds.get((c["language"], c["kind"]), 0) + 1
    print(f"wrote {len(cases)} cases to {args.out}")
    for (lang, kind), n in sorted(kinds.items()):
        print(f"  {lang:10s} {kind:6s} {n}")


if __name__ == "__main__":
    main()
