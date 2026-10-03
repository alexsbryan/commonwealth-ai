#!/usr/bin/env python3
"""The top-level-programs move as two functions: the one reader of
`quality/top-level-moves.toml`.

    back(path)     the path as it was spelled before the move
    forward(path)  the path as it is spelled now

A path the move did not touch comes back unchanged. Matching is on whole path
segments and the longest prefix wins, so `bench/lanes/cognitive/x` maps through
its own row (`sovereign/inquiries/cognitive`) and not through `bench/lanes`.

Use it as a module, from a script under `scripts/`:

    sys.path.insert(0, str(REPO / "scripts" / "lib"))
    from top_level_moves import Moves
    moves = Moves.at(REPO)
    moves.back("svrn/crates/sovereign-core/src/lib.rs")
    # -> "sovereign/crates/sovereign-core/src/lib.rs"

`python3 scripts/lib/top_level_moves.py --selftest` checks both directions on
the real table and exits non-zero on the first wrong answer.
"""
from __future__ import annotations

import sys
import tomllib
from pathlib import Path

REGISTRY = "quality/top-level-moves.toml"


def _map(path: str, rows: list[tuple[str, str]]) -> str:
    for src, dst in rows:
        if path == src:
            return dst
        if path.startswith(src + "/"):
            return dst + path[len(src):]
    return path


class Moves:
    """The table, with each direction's rows ordered longest prefix first."""

    def __init__(self, rows: list[tuple[str, str]], present: bool = True):
        self.rows = rows
        # False when the root carries no table: a fixture repo, or a checkout
        # from before the move. Then both directions are the identity, and a
        # caller that reports its inputs says so rather than implying a mapping.
        self.present = present
        self._fwd = sorted(rows, key=lambda r: -len(r[0]))
        self._back = sorted(((d, s) for s, d in rows), key=lambda r: -len(r[0]))

    @classmethod
    def at(cls, root: str | Path) -> "Moves":
        path = Path(root) / REGISTRY
        if not path.is_file():
            return cls([], present=False)
        with open(path, "rb") as f:
            data = tomllib.load(f)
        return cls([(r["from"], r["to"]) for r in data.get("move", [])])

    def forward(self, path: str) -> str:
        return _map(path, self._fwd)

    def back(self, path: str) -> str:
        return _map(path, self._back)


def _selftest() -> int:
    moves = Moves.at(Path(__file__).resolve().parents[2])
    cases = [
        # (pre-move, post-move)
        ("corpus-engine/src/lib.rs", "ingest/crates/corpus-engine/src/lib.rs"),
        ("corpus-engine/xtask/src/main.rs", "quality/xtask/src/main.rs"),
        ("sovereign/crates/sovereign-core/src/lib.rs", "svrn/crates/sovereign-core/src/lib.rs"),
        ("sovereign/inquiries/cognitive/bank.toml", "bench/lanes/cognitive/bank.toml"),
        ("sovereign/bench/sep/bench.toml", "bench/lanes/sep/bench.toml"),
        ("commonwealth/crates/commonwealth-core/src/lib.rs", "cmnwlth/crates/commonwealth-core/src/lib.rs"),
        ("studio/crates/sovereign-studio/src/main.rs", "clients/studio/src/main.rs"),
        ("sovereign/crates/sovereign-desktop/src-tauri/src/main.rs", "clients/desktop/src-tauri/src/main.rs"),
        ("kernel-types/src/lib.rs", "shared/crates/kernel-types/src/lib.rs"),
    ]
    failed = 0
    if not moves.present:
        print(f"top_level_moves: no {REGISTRY} at the repo root")
        return 1
    for old, new in cases:
        for got, want, how in ((moves.forward(old), new, "forward"), (moves.back(new), old, "back")):
            if got != want:
                print(f"  ✗ {how}: got {got!r}, want {want!r}")
                failed += 1
    # untouched paths are the identity in both directions
    for p in ("scripts/pre-push.sh", "vendor/llama-cpp-4/Cargo.toml", "corpus-engine-notes-fake/x"):
        if moves.forward(p) != p or moves.back(p) != p:
            print(f"  ✗ identity: {p!r}")
            failed += 1
    print(f"top_level_moves: {len(moves.rows)} rows, {len(cases) * 2 + 3 - failed} of {len(cases) * 2 + 3} checks hold")
    return 1 if failed else 0


if __name__ == "__main__":
    if "--selftest" in sys.argv:
        sys.exit(_selftest())
    sys.exit(__doc__)
