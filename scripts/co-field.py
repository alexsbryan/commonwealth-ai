#!/usr/bin/env python3
"""co-field.py — the seat's fieldglass reader (docs/FIELD_VERDICTS.md).

One decider for every seat surface that reads the field:

  co-field.py evidence <ref> [--repo DIR]
      Scene 1: changed-file evidence from the STANDING sidecar, for the
      landing bundle. Changed paths arrive on stdin, one per line.

  co-field.py diff <scratch-sidecar.json> <ref> [--repo DIR]
      Scene 2 (artifact C): landing diff, scratch render vs standing
      sidecar — three checks only (growth/offender transitions, coupling,
      SCIP freshness). Prints human lines to stdout and, with --json-out,
      writes the field_evidence object for the verdict record. A headline
      finding (offender transition or violation edge touching a changed
      crate) auto-mints an episode skeleton to
      ~/.sovereign/comaintainer/field-episodes.jsonl (tier A by
      construction, status unaudited).

Reads only the renderer's DECIDED fields — no re-derived thresholds
(ARCH §10.6). Every absent input is named, never omitted (§18.3); an
unknown age or lag renders as unknown, never as fresh (§18.2).
Exit code is always 0 for evidence (the bundle must assemble even when
the field is dark); the missing input is reported in the output itself.
"""

from __future__ import annotations

import argparse
import datetime
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


def _changed_files(repo: Path, ref: str) -> list[str]:
    r = subprocess.run(["git", "-C", str(repo), "diff-tree", "-r",
                        "--name-only", "--no-commit-id", ref],
                       capture_output=True, text=True)
    return [ln for ln in r.stdout.splitlines() if ln.strip()]


def _scip_covers(repo: Path, ref: str, scip_head: str) -> bool:
    """The index describes `ref` iff ref is an ancestor of (or equal to)
    the indexed commit."""
    if not scip_head:
        return False
    return subprocess.run(
        ["git", "-C", str(repo), "merge-base", "--is-ancestor", ref,
         scip_head], capture_output=True).returncode == 0


def _mint_episode(repo: Path, ref: str, ev: dict) -> None:
    """A headline contradiction candidate is a tier-A label by
    construction (the instrument settled it) — mint the skeleton;
    audit and promotion into the committed golden set stay manual."""
    subj = subprocess.run(["git", "-C", str(repo), "log", "-1",
                           "--format=%s", ref], capture_output=True,
                          text=True).stdout.strip()
    rec = {"ts": datetime.datetime.now(datetime.timezone.utc).isoformat(),
           "ref": ref, "context": subj, "field_evidence": ev,
           "seat_verdict": None,  # join to verdicts.jsonl on ref
           "tier": "A", "status": "unaudited"}
    p = Path.home() / ".sovereign" / "comaintainer" / "field-episodes.jsonl"
    p.parent.mkdir(parents=True, exist_ok=True)
    with open(p, "a") as fh:
        fh.write(json.dumps(rec, ensure_ascii=False) + "\n")
    print(f"  episode skeleton minted -> {p} (tier A, unaudited)")


def cmd_diff(args: argparse.Namespace) -> int:
    repo = Path(args.repo).resolve()
    ev: dict = {"ref": args.ref,
                "dup": "skipped at landing - surfaces at next glance"}

    def finish() -> int:
        if args.json_out:
            Path(args.json_out).write_text(json.dumps(ev, ensure_ascii=False))
        # The one honesty line that prints on every run, unconditionally.
        print("  dup tier skipped at landing — surfaces at next glance")
        return 0

    scratch = _load(Path(args.scratch))
    if scratch is None:
        ev.update(status="could-not-judge",
                  missing=f"scratch sidecar at {args.scratch}")
        print(f"  could-not-judge — missing: {ev['missing']}")
        return finish()
    ev["status"] = "ok"

    standing_p, how = M.sidecar_path(repo)
    standing = _load(standing_p) if standing_p else None
    ev["baseline"] = ({"head": standing.get("head"),
                       "unix": standing.get("generated_unix"),
                       "how": how} if standing else None)

    changed = _changed_files(repo, args.ref)
    s_files = {f.get("path"): f for f in scratch.get("files", [])}
    b_files = {f.get("path"): f for f in (standing or {}).get("files", [])}
    headline: list[str] = []

    # Check 1 — growth (walked live at render time; never SCIP-gated).
    growth = []
    for p in changed:
        sf = s_files.get(p)
        if sf is None:
            continue
        bf = b_files.get(p)
        g = {"path": p,
             "lines_before": bf.get("lines") if bf else None,
             "lines_after": sf.get("lines"),
             "offender_before": bool(bf.get("offender")) if bf else None,
             "offender_after": bool(sf.get("offender"))}
        growth.append(g)
        if g["offender_after"] and g["offender_before"] is False:
            headline.append(f"OFFENDER TRANSITION: {p} "
                            f"({g['lines_before']} -> {g['lines_after']} lines)")
        elif bf and g["lines_after"] != g["lines_before"]:
            print(f"  {p}: {g['lines_before']} -> {g['lines_after']} lines")
    ev["growth"] = growth
    if standing is None:
        print("  no baseline sidecar (first field render) — absolute values "
              "only, no transitions")

    # Check 2 — coupling (SCIP-fed; §18.2: unjudgeable is said, not zeroed).
    scip_head = (scratch.get("honesty", {}) or {}).get("scip_head") or ""
    ev["scratch_scip_head"] = scip_head
    if not _scip_covers(repo, args.ref, scip_head):
        ev["coupling"] = {"status": "could-not-judge",
                          "missing": f"SCIP at {args.ref[:9]} "
                                     f"(index at {scip_head[:9] or 'unknown'})"}
        print(f"  coupling: could-not-judge — missing SCIP at "
              f"{args.ref[:9]}, index at {scip_head[:9] or 'unknown'} "
              "(re-run --field in ~1 min)")
    else:
        crates = {s_files[p].get("crate") for p in changed if p in s_files}
        crates.discard(None)

        def vio(sc: dict) -> set:
            return {(e.get("from"), e.get("to"))
                    for e in sc.get("flow_edges", [])
                    if e.get("kind") in M.FLOW_VIOLATION_KINDS}

        new_v = sorted(v for v in vio(scratch) - vio(standing or {})
                       if v[0] in crates or v[1] in crates)
        fan = [{"path": p, "before": b_files[p].get("fan_in"),
                "after": s_files[p].get("fan_in")}
               for p in changed if p in s_files and p in b_files
               and s_files[p].get("fan_in") != b_files[p].get("fan_in")]
        ev["coupling"] = {"status": "ok",
                          "new_violation_edges": [list(v) for v in new_v],
                          "fan_in_changes": fan}
        for v in new_v:
            headline.append(f"NEW LAYER VIOLATION: {v[0]} -> {v[1]}")
        for f in fan:
            print(f"  fan-in {f['path']}: {f['before']} -> {f['after']}")

    ev["headline"] = headline
    on_field = [p for p in changed if p in s_files]
    ev["off_field_paths"] = len(changed) - len(on_field)
    for h in headline:
        print(f"  {h}")
    if not headline and not any(
            g["lines_before"] != g["lines_after"] for g in growth):
        if on_field:
            print("  no structural delta on the changed files")
        else:
            # "No delta" and "nothing measurable" are different verdicts
            # (§18.2) — an all-docs/python landing must not read as
            # measured-clean.
            print(f"  none of the {len(changed)} changed path(s) are on "
                  "the field (non-code, or deleted) — nothing measurable")
    if ev["off_field_paths"] and on_field:
        print(f"  ({ev['off_field_paths']} changed path(s) not on the field)")
    if headline:
        _mint_episode(repo, args.ref, ev)
    return finish()


def cmd_corpus(args: argparse.Namespace) -> int:
    corpus = M.registry_corpus(Path(args.repo).resolve())
    if corpus is None:
        return 1
    print(corpus)
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)
    ev = sub.add_parser("evidence")
    ev.add_argument("ref")
    ev.add_argument("--repo", default=str(HERE.parent))
    df = sub.add_parser("diff")
    df.add_argument("scratch")
    df.add_argument("ref")
    df.add_argument("--repo", default=str(HERE.parent))
    df.add_argument("--json-out", default="")
    cp = sub.add_parser("corpus")
    cp.add_argument("--repo", default=str(HERE.parent))
    args = ap.parse_args()
    if args.cmd == "evidence":
        return cmd_evidence(args)
    if args.cmd == "diff":
        return cmd_diff(args)
    if args.cmd == "corpus":
        return cmd_corpus(args)
    return 2


if __name__ == "__main__":
    sys.exit(main())
