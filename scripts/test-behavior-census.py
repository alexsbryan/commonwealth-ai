#!/usr/bin/env python3
"""Find behaviorally related Rust tests and their shared SCIP boundaries.

This is a read-only spike, not a duplicate detector.  SCIP owns test identity,
source ranges, and resolved references.  The source range contributes only the
test's prose and assertion messages, which are the behavior signal.  A human
still decides whether a candidate should become a shared contract, a
table-driven test, or remain a layered witness.
"""

from __future__ import annotations

import argparse
import collections
import json
import math
import re
import sqlite3
from dataclasses import asdict, dataclass
from pathlib import Path


DEFAULT_GRAPH = Path.home() / ".svrnmesh" / "indexes" / "commonwealth-ai" / "scip_graph.db"
STOP_WORDS = {
    "test", "tests", "should", "must", "never", "with", "from", "that",
    "this", "when", "then", "into", "same", "only", "after", "before",
    "does", "returns", "return", "true", "false", "assert", "error", "errors",
    "called", "call", "one", "two", "three", "line", "lines", "record",
    "records", "value", "values", "the", "and", "for", "not", "its",
}
FIXTURE_CALLS = {
    "key", "keys", "signed", "record", "member", "mesh_with", "proof_mesh",
    "fresh_local", "admitted", "applied", "submission", "op", "who", "origin",
    "dialer", "fixture", "setup", "build", "harness", "kind", "assert", "vec",
}
ACTION_NAMES = {
    "admit", "append", "compact", "complete", "create", "delete", "execute",
    "fold", "ingest", "insert", "merge", "merge_from", "publish", "read",
    "reconcile", "register", "remove", "resolve", "run", "submit", "update",
    "verify", "write",
}
TEST_QNAME_MARKERS = ("/tests/", "/test/", "/tests.rs/", "/test.rs/")
TEST_SUPPORT_MARKERS = ("/tests_support/", "/test_support/", "/tests_fixture/")
TEST_FILE_MARKERS = (
    "/tests/", "/test/", "/tests.rs", "/test.rs", "_test.rs", "_tests.rs", "/test_",
)


@dataclass(frozen=True)
class TestWitness:
    qualified_name: str
    name: str
    file_path: str
    line_start: int
    line_end: int
    behavior: str
    tokens: tuple[str, ...]
    production_boundaries: tuple[tuple[str, str], ...]


@dataclass(frozen=True)
class Candidate:
    score: float
    left: TestWitness
    right: TestWitness
    shared_boundaries: tuple[tuple[str, str], ...]
    recommendation: str


def is_test_symbol(qualified_name: str, file_path: str) -> bool:
    return (
        any(marker in qualified_name for marker in TEST_QNAME_MARKERS)
        or any(marker in file_path for marker in TEST_FILE_MARKERS)
    )


def display_callee(qualified_name: str) -> str:
    tail = qualified_name.rsplit("/", 1)[-1].rstrip("().")
    # SCIP method descriptors carry the receiver in `impl#[Type]method`.
    return tail.rsplit("]", 1)[-1]


def source_lines(root: Path, file_path: str, cache: dict[str, list[str]]) -> list[str]:
    if file_path in cache:
        return cache[file_path]
    try:
        lines = (root / file_path).read_text(errors="replace").splitlines()
    except OSError:
        lines = []
    cache[file_path] = lines
    return lines


def has_test_attribute(root: Path, file_path: str, line_start: int, cache: dict[str, list[str]]) -> bool:
    lines = source_lines(root, file_path, cache)
    cursor = line_start - 1
    for _ in range(8):
        if cursor < 0:
            break
        line = lines[cursor].strip()
        if re.search(r"#\[(?:tokio::)?test(?:\]|\(|\s)", line):
            return True
        if line and not line.startswith("#") and not line.startswith("///"):
            break
        cursor -= 1
    return False


def is_setup_reference(
    root: Path, file_path: str, line: int, cache: dict[str, list[str]]
) -> bool:
    """Exclude a resolved call when the following line proves setup only."""
    lines = source_lines(root, file_path, cache)
    window = "\n".join(lines[max(0, line):line + 4])
    return '.expect("register")' in window


def normalize_tokens(text: str) -> tuple[str, ...]:
    # Split snake_case and CamelCase without parsing Rust.
    text = re.sub(r"([a-z0-9])([A-Z])", r"\1 \2", text)
    words = re.findall(r"[a-z0-9]+", text.lower())
    return tuple(word for word in words if len(word) > 3 and word not in STOP_WORDS)


def source_behavior(
    root: Path,
    witness: tuple[str, str, str, int, int],
    cache: dict[str, list[str]] | None = None,
) -> tuple[str, tuple[str, ...]]:
    _, name, file_path, line_start, line_end = witness
    lines = source_lines(root, file_path, cache if cache is not None else {})
    if not lines:
        return name, normalize_tokens(name)

    start = max(0, line_start)
    end = min(len(lines), line_end + 1)
    comments: list[str] = []
    cursor = start - 1
    while cursor >= 0 and (
        not lines[cursor].strip()
        or lines[cursor].lstrip().startswith("///")
        or lines[cursor].lstrip().startswith("#[")
    ):
        line = lines[cursor].strip()
        if line.startswith("///"):
            comments.append(re.sub(r"^///!?\s?", "", line))
        cursor -= 1
    comments.reverse()
    body = "\n".join(lines[start:end])
    # Assertion messages are a useful supplement to prose; multiline Rust
    # parsing is intentionally delegated to SCIP rather than approximated here.
    messages = re.findall(
        r"(?:assert(?:_eq|_ne|_matches)?|panic|unreachable)!\s*\([^\n]*?,\s*\"([^\"]+)\"",
        body,
    )
    behavior = " ".join([name.replace("_", " "), *comments, *messages]).strip()
    return behavior, normalize_tokens(behavior)


def load_witnesses(root: Path, graph_path: Path, scope: str) -> list[TestWitness]:
    db = sqlite3.connect(f"file:{graph_path}?mode=ro", uri=True)
    try:
        params: list[str] = []
        query = (
            "SELECT qualified_name, name, file_path, line_start, line_end "
            "FROM symbols WHERE language = 'rust' AND qualified_name LIKE '%().'"
        )
        if scope:
            query += " AND file_path LIKE ?"
            params.append(scope.rstrip("/") + "/%")
        source_cache: dict[str, list[str]] = {}
        rows = [
            row for row in db.execute(query, params)
            if is_test_symbol(row[0], row[2])
            and has_test_attribute(root, row[2], row[3], source_cache)
        ]
        test_qnames = {row[0] for row in rows}
        symbol_files = {
            qualified_name: file_path
            for qualified_name, _, file_path, _, _ in db.execute(
                "SELECT qualified_name, name, file_path, line_start, line_end "
                "FROM symbols WHERE language = 'rust' AND qualified_name LIKE '%().'"
            )
        }
        boundaries: dict[str, set[tuple[str, str]]] = collections.defaultdict(set)
        # The current graph has no caller-qualified index. Scan refs once and
        # filter against the in-memory test set; this avoids a nested join when
        # a workspace has thousands of test symbols.
        ref_rows = db.execute(
            "SELECT caller_qualified, callee_qualified, file_path, line FROM refs "
            "WHERE callee_qualified LIKE '%().'"
        )
        for caller, callee, file_path, line in ref_rows:
            if caller not in test_qnames:
                continue
            if is_setup_reference(root, file_path, line, source_cache):
                continue
            boundaries[caller].add((callee, display_callee(callee)))

        witnesses: list[TestWitness] = []
        for row in rows:
            qname, name, file_path, line_start, line_end = row
            behavior, tokens = source_behavior(root, row, source_cache)
            production = tuple(
                sorted(
                    boundary for boundary in boundaries.get(qname, ())
                    if boundary[1] in ACTION_NAMES
                    and boundary[1] not in FIXTURE_CALLS
                    and boundary[0] in symbol_files
                    and (root / symbol_files[boundary[0]]).is_file()
                    and not is_test_symbol(boundary[0], symbol_files[boundary[0]])
                    and not any(marker in boundary[0] for marker in TEST_SUPPORT_MARKERS)
                )
            )
            witnesses.append(TestWitness(
                qname, name, file_path, line_start, line_end, behavior, tokens, production,
            ))
        return witnesses
    finally:
        db.close()


def cosine(left: collections.Counter[str], right: collections.Counter[str], idf: dict[str, float]) -> float:
    lv = {token: count * idf[token] for token, count in left.items()}
    rv = {token: count * idf[token] for token, count in right.items()}
    denominator = math.sqrt(sum(value * value for value in lv.values())) * math.sqrt(
        sum(value * value for value in rv.values())
    )
    if not denominator:
        return 0.0
    return sum(value * rv.get(token, 0.0) for token, value in lv.items()) / denominator


def recommendation(shared: tuple[tuple[str, str], ...], left: TestWitness, right: TestWitness) -> str:
    if not shared:
        return "retain layered witnesses: no shared production boundary"
    names = ", ".join(name for _, name in shared[:3])
    if left.file_path == right.file_path:
        return f"candidate table-driven consolidation at {names}"
    return f"candidate shared contract at {names}; retain layer-specific proof"


def candidates(witnesses: list[TestWitness], threshold: float) -> list[Candidate]:
    document_frequency = collections.Counter(token for witness in witnesses for token in set(witness.tokens))
    idf = {
        token: math.log((len(witnesses) + 1) / (frequency + 1)) + 1
        for token, frequency in document_frequency.items()
    }
    vectors = [collections.Counter(witness.tokens) for witness in witnesses]
    token_index: dict[str, set[int]] = collections.defaultdict(set)
    for index, witness in enumerate(witnesses):
        for token in set(witness.tokens):
            token_index[token].add(index)
    result: list[Candidate] = []
    for index, left in enumerate(witnesses):
        possible: set[int] = set()
        for token in set(left.tokens):
            possible.update(token_index[token])
        for right_index in sorted(possible):
            if right_index <= index:
                continue
            right = witnesses[right_index]
            if left.file_path == right.file_path:
                continue
            if len(set(left.tokens) & set(right.tokens)) < 2:
                continue
            shared_qnames = {qname for qname, _ in left.production_boundaries}
            shared = tuple(boundary for boundary in right.production_boundaries if boundary[0] in shared_qnames)
            score = cosine(vectors[index], vectors[right_index], idf)
            if score >= threshold:
                result.append(Candidate(
                    score, left, right, tuple(sorted(set(shared))), recommendation(tuple(sorted(set(shared))), left, right),
                ))
    # A textual match with no shared action is useful as a negative control,
    # but a match with a compiler-resolved boundary is the actionable spike.
    return sorted(
        result,
        key=lambda candidate: (
            not bool(candidate.shared_boundaries),
            -candidate.score,
            candidate.left.qualified_name,
        ),
    )


def witness_json(witness: TestWitness) -> dict:
    return asdict(witness)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--graph", type=Path, default=DEFAULT_GRAPH)
    parser.add_argument("--scope", default="", help="repo-relative source prefix, e.g. commonwealth/crates")
    parser.add_argument("--threshold", type=float, default=0.35)
    parser.add_argument("--limit", type=int, default=25)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    if not args.graph.is_file():
        parser.error(f"SCIP graph not found: {args.graph}")
    witnesses = load_witnesses(args.root, args.graph, args.scope)
    found = candidates(witnesses, args.threshold)
    found = found[:args.limit]
    if args.json:
        print(json.dumps({
            "graph": str(args.graph),
            "scope": args.scope,
            "witnesses": len(witnesses),
            "candidates": [{
                "score": candidate.score,
                "left": witness_json(candidate.left),
                "right": witness_json(candidate.right),
                "shared_boundaries": candidate.shared_boundaries,
                "recommendation": candidate.recommendation,
            } for candidate in found],
        }, indent=2))
        return 0
    print(f"Rust test witnesses: {len(witnesses)}")
    print(f"Candidates (threshold {args.threshold:.2f}): {len(found)}")
    for number, candidate in enumerate(found, 1):
        print(f"\nCANDIDATE {number}  score={candidate.score:.3f}")
        print(f"  A {candidate.left.file_path}:{candidate.left.line_start + 1}::{candidate.left.name}")
        print(f"    {candidate.left.behavior[:240]}")
        print(f"  B {candidate.right.file_path}:{candidate.right.line_start + 1}::{candidate.right.name}")
        print(f"    {candidate.right.behavior[:240]}")
        if candidate.shared_boundaries:
            print("  shared boundary")
            for qualified, name in candidate.shared_boundaries[:5]:
                print(f"    {name}  {qualified}")
        print(f"  recommendation: {candidate.recommendation}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
