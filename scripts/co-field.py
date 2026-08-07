#!/usr/bin/env python3
"""co-field.py — the seat's fieldglass reader (docs/FIELD_VERDICTS.md).

One decider for every seat surface that reads the field:

  co-field.py evidence <ref> [--repo DIR]
      Scene 1: changed-file evidence from the STANDING sidecar, for the
      landing bundle. Changed paths arrive on stdin, one per line.

  co-field.py diff <scratch-sidecar.json> <ref> [--repo DIR]
      Scene 2 (artifact C): landing diff, scratch render vs standing
      sidecar. Added by artifact C.

Reads only the renderer's DECIDED fields — no re-derived thresholds
(ARCH §10.6). Every absent input is named, never omitted (§18.3); an
unknown age or lag renders as unknown, never as fresh (§18.2).
Exit code is always 0 for evidence (the bundle must assemble even when
the field is dark); the missing input is reported in the output itself.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "gym" / "comaintainer"))
import markers as M  # noqa: E402  (one home: FIELD_CLASSES, sidecar_path)


def _load(path: Path) -> dict | None:
    try:
        return json.loads(path.read_text())
    except (OSError, json.JSONDecodeError):
        return None


def _commits_ahead(repo: Path, base: str, ref: str) -> str:
    """How many commits `ref` is ahead of the sidecar's head — or the
    honest 'unknown' when history cannot relate them (§18.2)."""
    r = subprocess.run(["git", "-C", str(repo), "rev-list", "--count",
                        f"{base}..{ref}"], capture_output=True, text=True)
    if r.returncode != 0:
        return "unknown (sidecar head not in this history)"
    return r.stdout.strip()


def _sidecar_header(sc: dict, path: Path, how: str, repo: Path,
                    ref: str) -> None:
    head = (sc.get("head") or "?")[:12]
    gen = sc.get("generated_unix")
    age = f"{(time.time() - gen) / 3600:.1f}h old" if gen else "age unknown"
    print(f"sidecar: {path} (resolved via {how})")
    if how == "newest-fallback":
        print("WARNING: repo not in the project registry — sidecar is a "
              "newest-file guess; `svrn project register` pins it")
    print(f"field as of commit {head} ({age}); reviewed ref is "
          f"{_commits_ahead(repo, sc.get('head') or '', ref)} commit(s) "
          "ahead of it")


def cmd_evidence(args: argparse.Namespace) -> int:
    repo = Path(args.repo).resolve()
    changed = [ln.strip() for ln in sys.stdin if ln.strip()]
    sidecar, how = M.sidecar_path(repo)
    if sidecar is None:
        print("ABSENT — no fieldglass render for this repo "
              "(named, not omitted)")
        return 0
    sc = _load(sidecar)
    if sc is None:
        print(f"UNREADABLE — sidecar at {sidecar} did not parse "
              "(named, not omitted)")
        return 0
    _sidecar_header(sc, sidecar, how, repo, args.ref)

    files = {f.get("path"): f for f in sc.get("files", [])}
    att = sc.get("attention", {})
    toll = {t[0]: t for t in att.get("tollbooths", []) if t}
    tax = {d.get("path"): d for d in att.get("comprehension_tax", [])}

    findings = 0
    off_field = 0
    for path in changed:
        f = files.get(path)
        if f is None:
            off_field += 1
            continue
        flags = []
        if f.get("offender"):
            flags.append(f"OFFENDER ring ({f.get('lines')} lines)")
        if path in toll:
            flags.append(f"tollbooth ({toll[path][1]} commits/90d)")
        if f.get("bridge", 0) > 0:
            flags.append(f"bridge {f['bridge']:.2f}")
        if path in tax:
            d = tax[path]
            flags.append(f"comprehension tax ({d.get('read_tokens', 0)} "
                         f"read-tokens over {d.get('edits', 0)} edits)")
        if flags:
            findings += 1
            print(f"  {path}: " + "; ".join(flags))

    changed_set = set(changed)
    for a in sc.get("dup_arcs", []):
        if a.get("a") in changed_set or a.get("b") in changed_set:
            findings += 1
            print(f"  clone arc: {a.get('a')}:{a.get('a_line')} <-> "
                  f"{a.get('b')}:{a.get('b_line')} "
                  f"({a.get('lines')} lines, sim {a.get('sim'):.2f})")

    crates = {files[p].get("crate") for p in changed_set if p in files}
    crates.discard(None)
    for e in sc.get("flow_edges", []):
        if e.get("kind") in M.FLOW_VIOLATION_KINDS and (
                e.get("from") in crates or e.get("to") in crates):
            findings += 1
            print(f"  layer violation ({e.get('kind')}): "
                  f"{e.get('from')} -> {e.get('to')} ({e.get('refs')} refs)")

    if findings == 0:
        print("  no standing findings on the changed files")
    if off_field:
        print(f"  ({off_field} changed path(s) not on the field — new, "
              "deleted, or non-code)")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)
    ev = sub.add_parser("evidence")
    ev.add_argument("ref")
    ev.add_argument("--repo", default=str(HERE.parent))
    args = ap.parse_args()
    if args.cmd == "evidence":
        return cmd_evidence(args)
    return 2


if __name__ == "__main__":
    sys.exit(main())
