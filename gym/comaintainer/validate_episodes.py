#!/usr/bin/env python3
"""Validate the comaintainer bank: structure, leakage, secrets, split.

Runs offline — no GPU, no network, no model weights (ARCH §12.4). The
one external store it consults is `~/.sovereign/notes.db`, and its
ABSENCE is a printed `NOTES_DB_ABSENT` banner with a snapshot fallback,
never a silent pass (§18.3).

Checks, in order:

  self-test    a seeded LEAKY fixture must make every linter check FIRE
               before the linter's verdict on the real bank means
               anything — a gate that has never been watched to fail is
               not a gate (§18.1)
  structural   schema completeness, arg-per-verdict, length bounds,
               unique ids/signatures, basis resolution (ARCH § / note
               hex / ledger slug / commit hex / transcript pointer)
  leakage      verdict markers, basis ids, expect-side shingles, ledger
               states, secrets — in any request block
  balance      class floor/ceiling, six sources non-empty, bank floor
  split        recomputed from `markers.split_of`; mismatch = exit 1

Exit codes: 0 valid · 1 failures · 4 empty bank.

    python3 gym/comaintainer/validate_episodes.py
    python3 gym/comaintainer/validate_episodes.py --audit-sample 80 \\
        --audit-out gym/comaintainer/AUDIT_SAMPLE.txt
"""

from __future__ import annotations

import argparse
import collections
import json
import re
import sqlite3
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import markers as M  # noqa: E402

REPO = HERE.parent.parent
LEDGER = REPO / "sovereign" / "DEFAULTS_LEDGER.md"
ARCH = REPO / "sovereign" / "ARCH_PRINCIPLES.md"
NOTES_DB = Path.home() / ".sovereign" / "notes.db"

PROBLEMS: list[tuple[str, str, str]] = []  # (episode id, check, detail)
COUNTS: collections.Counter = collections.Counter()


def problem(eid: str, check: str, detail: str = "") -> None:
    PROBLEMS.append((eid, check, detail))
    COUNTS[check] += 1


# ---- anchor resolution ------------------------------------------------


def arch_sections() -> set[str]:
    """§ tokens that resolve: every `## N.` heading plus every §N.M the
    document itself uses (subsection cites like §18.5 are prose-level)."""
    text = ARCH.read_text()
    secs = set(re.findall(r"^## (\d+)\.", text, re.M))
    secs |= set(re.findall(r"§\s?(\d+(?:\.\d+)?)", text))
    return secs


def ledger_slugs() -> set[str]:
    slugs = {"owed-a-row"}
    for line in LEDGER.read_text().splitlines():
        if line.startswith("### "):
            slugs.add(M.slugify(line[4:].strip()))
    return slugs


def known_commits(hexes: set[str]) -> set[str]:
    ok = set()
    for h in hexes:
        r = subprocess.run(["git", "-C", str(REPO), "cat-file", "-e", h],
                           capture_output=True)
        if r.returncode == 0:
            ok.add(h)
    return ok


def known_notes(prefixes: set[str], notes_db_present: bool) -> set[str]:
    if not notes_db_present:
        return set()
    ok = set()
    with sqlite3.connect(f"file:{NOTES_DB}?mode=ro", uri=True) as db:
        for p in prefixes:
            row = db.execute("SELECT 1 FROM notes WHERE id LIKE ? LIMIT 1",
                             (p + "%",)).fetchone()
            if row:
                ok.add(p)
    return ok


def resolve_bases(bank: list[dict]) -> None:
    arch = arch_sections()
    slugs = ledger_slugs()
    note_prefixes, commit_hexes = set(), set()
    for e in bank:
        for b in e["expect"]["basis"]:
            if m := re.fullmatch(r"note ([0-9a-f]{8})", b):
                note_prefixes.add(m.group(1))
            elif m := re.fullmatch(r"commit ([0-9a-f]{7,40})", b):
                commit_hexes.add(m.group(1))
    notes_db_present = NOTES_DB.exists()
    if not notes_db_present:
        print("NOTES_DB_ABSENT — note anchors resolve via committed "
              "snapshots only (§18.3: reported, not defaulted)")
    notes_ok = known_notes(note_prefixes, notes_db_present)
    commits_ok = known_commits(commit_hexes)

    for e in bank:
        for b in e["expect"]["basis"]:
            if m := re.fullmatch(r"ARCH §(\d+(?:\.\d+)?)", b):
                if m.group(1) not in arch:
                    problem(e["id"], "basis_unresolved_arch", b)
            elif m := re.fullmatch(r"ledger:([a-z0-9\-]+)", b):
                if m.group(1) not in slugs:
                    problem(e["id"], "basis_unresolved_ledger", b)
            elif m := re.fullmatch(r"note ([0-9a-f]{8})", b):
                if m.group(1) not in notes_ok:
                    snap = (e["provenance"].get("note_snapshot") or {})
                    if not snap.get("head200"):
                        problem(e["id"], "basis_unresolved_note", b)
                    elif notes_db_present:
                        # live db, note gone -> snapshot carries it, but
                        # say so rather than silently passing
                        COUNTS["basis_note_snapshot_fallback"] += 1
            elif m := re.fullmatch(r"commit ([0-9a-f]{7,40})", b):
                if m.group(1) not in commits_ok:
                    problem(e["id"], "basis_unresolved_commit", b)
            elif m := re.fullmatch(r"transcript:([0-9a-f]{8}):(\d+)", b):
                snap = (e["provenance"].get("note_snapshot") or {})
                if not snap.get("head200"):
                    problem(e["id"], "basis_unresolved_transcript", b)
            else:
                problem(e["id"], "basis_unknown_anchor_type", b)


# ---- leakage linter ---------------------------------------------------


def lint_episode(e: dict) -> None:
    """One linter, shared with the harvester: `markers.lint_leaks`.
    Twins arrive as separate episodes, so they are re-linted
    independently by construction (leak_twin is not a separate check)."""
    for check, detail in M.lint_leaks(e):
        problem(e["id"], check, detail)


# ---- structural -------------------------------------------------------


def check_structure(bank: list[dict]) -> None:
    ids = collections.Counter(e.get("id") for e in bank)
    sigs = collections.Counter()
    for e in bank:
        eid = e.get("id", "<missing-id>")
        if ids[eid] > 1:
            problem(eid, "duplicate_id")
        for key in ("id", "source", "tier", "split", "request", "expect",
                    "provenance"):
            if key not in e:
                problem(eid, "schema_missing_key", key)
        if e.get("tier") not in ("A", "B", "C"):
            problem(eid, "bad_tier", str(e.get("tier")))
        if e.get("split") not in ("dev", "holdout"):
            problem(eid, "bad_split", str(e.get("split")))
        req = e.get("request") or {}
        for f in ("situation", "proposal", "evidence"):
            if not (req.get(f) or "").strip():
                problem(eid, "empty_request_field", f)
            elif len(req[f]) > M.LEN_BOUNDS[f] + 20:
                problem(eid, "request_field_over_bound",
                        f"{f}: {len(req[f])}")
        x = e.get("expect") or {}
        v = x.get("verdict")
        if v not in M.VERDICTS:
            problem(eid, "bad_verdict", str(v))
            continue
        arg = x.get(M.ARG_OF[v])
        if arg is None or (isinstance(arg, (str, list)) and not arg):
            problem(eid, "missing_arg_field", M.ARG_OF[v])
        if v == "split" and (not isinstance(arg, list) or len(arg) < 2):
            problem(eid, "split_scopes_not_list2")
        if v != "approve" and not x.get("basis"):
            problem(eid, "basis_empty_nonapprove")
        if not (x.get("rationale") or "").strip():
            problem(eid, "empty_rationale")
        sig = M.signature(e)
        if sigs[sig]:
            problem(eid, "duplicate_signature", str(sig[1])[:60])
        sigs[sig] += 1


def check_balance(bank: list[dict]) -> None:
    v_ctr = collections.Counter(e["expect"]["verdict"] for e in bank)
    for v in M.VERDICTS:
        if v_ctr.get(v, 0) < M.CLASS_FLOOR:
            problem("<bank>", "class_under_floor", f"{v}: {v_ctr.get(v, 0)}")
        if v_ctr.get(v, 0) > M.CLASS_CAPS[v]:
            problem("<bank>", "class_over_cap", f"{v}: {v_ctr[v]}")
    if bank and max(v_ctr.values()) / len(bank) > M.CLASS_CEILING_SHARE + 1e-9:
        problem("<bank>", "class_over_ceiling",
                f"{max(v_ctr.values())}/{len(bank)}")
    src_ctr = collections.Counter(e["source"] for e in bank)
    for src in M.REQUIRED_SOURCES:
        if not src_ctr.get(src):
            problem("<bank>", "source_empty", src)
    if len(bank) < M.BANK_FLOOR:
        problem("<bank>", "bank_under_floor", str(len(bank)))


def check_split(bank: list[dict]) -> None:
    strata: dict[tuple, list[dict]] = collections.defaultdict(list)
    for e in bank:
        strata[(e["source"], e["tier"], e["expect"]["verdict"])].append(e)
    empty = []
    for key, group in strata.items():
        group.sort(key=lambda e: e["id"])
        for i, e in enumerate(group):
            want = M.split_of(*key, i)
            if e["split"] != want:
                problem(e["id"], "split_mismatch", f"{e['split']} != {want}")
        if not any(e["split"] == "holdout" for e in group):
            empty.append(key)
    if empty:
        print(f"\nHOLDOUT-EMPTY STRATA ({len(empty)}) — named before any "
              f"headline is printed (§18.2):")
        for key in sorted(empty):
            print(f"  {key}  (n={len(strata[key])})")


# ---- linter self-test (§18.1) ----------------------------------------

LEAKY_FIXTURE = {
    "id": "cm-fixture-00000-leaky", "source": "ledger", "tier": "A",
    "split": "dev",
    "request": {
        "situation": "The flag was measured and rejected (stays off) after "
                     "the A/B; see note deadbeef and email leak@example.com.",
        "proposal": "Enable it anyway with api_key = 'sk-AAAAAAAAAAAAAAAAAAAA' "
                    "so the paired slice can rerun.",
        "evidence": "the flip condition separates at p<0.05 on the named bank",
    },
    "expect": {
        "verdict": "revise",
        "ask": "x", "basis": ["note deadbeef"],
        "rationale": "the flip condition separates at p<0.05 on the named bank",
    },
    "provenance": {"anchor": "leaky"},
}

SELF_TEST_MUST_FIRE = ("leak_verdict_marker", "leak_ledger_state",
                       "leak_basis_id", "leak_flip_condition", "leak_secret")


def self_test() -> bool:
    global PROBLEMS, COUNTS
    saved_p, saved_c = PROBLEMS, COUNTS
    PROBLEMS, COUNTS = [], collections.Counter()
    lint_episode(json.loads(json.dumps(LEAKY_FIXTURE)))
    fired = {c for _, c, _ in PROBLEMS}
    PROBLEMS, COUNTS = saved_p, saved_c
    missing = [c for c in SELF_TEST_MUST_FIRE if c not in fired]
    if missing:
        print(f"LINTER SELF-TEST FAILED — checks that did not fire on the "
              f"seeded leaky fixture: {missing}")
        print("A gate that has not been watched to fail is not a gate "
              "(§18.1). Refusing to validate anything.")
        return False
    print(f"linter self-test: all {len(SELF_TEST_MUST_FIRE)} seeded leak "
          f"classes fired on the fixture ✓")
    return True


# ---- audit sample -----------------------------------------------------


def write_audit(bank: list[dict], n: int, out: Path) -> None:
    transcripts = [e for e in bank if e["source"] == "transcript"]
    others = [e for e in bank if e["source"] != "transcript"]
    rest = max(0, n - len(transcripts))
    k = max(1, len(others) // rest) if rest else len(others) + 1
    sample = transcripts + others[::k][:rest]
    lines = [
        f"# AUDIT SAMPLE — {len(sample)} of {len(bank)} episodes",
        "# Every transcript-derived episode is included (their inclusion in",
        "# the committed bank is contingent on this pass); others every-k-th.",
        "# Per episode mark:  LABEL Y/N  (expect.verdict is the right call",
        "# given request alone + house law)   LEAK Y/N   notes free-form.",
        "",
    ]
    for e in sample:
        r, x = e["request"], e["expect"]
        lines += [
            "=" * 72,
            f"{e['id']}   source={e['source']} tier={e['tier']} "
            f"split={e['split']}",
            f"--- SITUATION\n{r['situation']}",
            f"--- PROPOSAL\n{r['proposal']}",
            f"--- EVIDENCE\n{r['evidence']}",
            f"--- EXPECT: {x['verdict']}   "
            f"{M.ARG_OF[x['verdict']]}={json.dumps(x.get(M.ARG_OF[x['verdict']]), ensure_ascii=False)}",
            f"    basis={x['basis']}",
            f"    rationale={x.get('rationale', '')}",
            "AUDIT: LABEL [ ]   LEAK [ ]   notes:",
            "",
        ]
    out.write_text("\n".join(lines))
    print(f"\naudit sample -> {out} ({len(sample)} episodes, "
          f"{len(transcripts)} transcript-derived, k={k})")


# ---- main -------------------------------------------------------------


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cases", default=str(HERE / "cases.jsonl.gz"))
    ap.add_argument("--audit-sample", type=int, default=0)
    ap.add_argument("--audit-out", default=str(HERE / "AUDIT_SAMPLE.txt"))
    args = ap.parse_args()

    if not self_test():
        sys.exit(1)

    bank = M.read_bank(args.cases)
    if not bank:
        print("EMPTY BANK — nothing was validated (exit 4, not a pass).")
        sys.exit(4)
    print(f"{len(bank)} episodes")

    check_structure(bank)
    resolve_bases(bank)
    for e in bank:
        lint_episode(e)
    check_balance(bank)
    check_split(bank)

    if args.audit_sample:
        write_audit(bank, args.audit_sample, Path(args.audit_out))

    if COUNTS:
        print(f"\n{'check':<36} {'n':>4}")
        for check, n in COUNTS.most_common():
            print(f"{check:<36} {n:>4}")
    if PROBLEMS:
        print(f"\n{len(PROBLEMS)} problems in {len(set(p[0] for p in PROBLEMS))} "
              f"episodes; first 15:")
        for eid, check, detail in PROBLEMS[:15]:
            print(f"  {eid}: {check} {detail[:70]}")
        sys.exit(1)
    print("\nvalid: structure, anchors, leakage, balance, split all clean.")


if __name__ == "__main__":
    main()
