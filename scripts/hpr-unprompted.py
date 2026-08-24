#!/usr/bin/env python3
"""hpr-unprompted — did anyone reach for the derived flag form WITHOUT being told?

THE CLAIM UNDER TEST (`hot-path-reuse` / `hpr-unprompted`): make reuse cheaper
and agents adopt it unprompted. Floor 0, target 3, over a rolling 30-day window.

THE FLOOR IS A REAL NATURAL EXPERIMENT, not an assumption. `cli_shared::args`
shipped 2026-06-09 in 6b2f4e04 and had 15 users by 2026-08-21: the minting
commit, and 14 files converted by three campaign rungs in a single day
(ae0ec58c nc-22b, 74313f10 nc-25, 580a652e nc-26). Zero voluntary in 73 days.
`--form shared-args --gate-zero` re-derives exactly that from git, which is how
this instrument is checked against a number measured independently of it.

PROVENANCE IS THE WHOLE DIFFICULTY, AND ONE METHOD IS KNOWN-WRONG. A first
attempt at this measurement intersected one commit's `git show --name-only`
list with the set of files using the form and reported "9 voluntary adopters".
It was false: `--name-only` lists every file a commit touched, not the files in
which the form appeared. THE METHOD HERE IS THE PER-FILE PICKAXE WALK —

    git log -S'<form>' [--pickaxe-regex] --reverse -- <one file>

whose FIRST row is the commit at which the form actually entered that file. A
commit is credited with a file only when it IS that first row. The commit-level
scan and the per-commit diff scan are pre-filters for speed; neither is ever
allowed to decide.

WHAT IS COUNTED. Files whose form-introducing commit is inside the window and is
neither a rung of this campaign nor the form's own minting commit. The value is
that file count; the commits behind it are listed in the JSON, and so is every
commit that was EXCLUDED, with the reason.

EXCLUSIONS, and why each exists.
  rung     A campaign cannot manufacture its own uptake. Rungs are identified by
           campaign tag in the commit subject or body (default `hpr-`, plus every
           order id under `.sovereign/features/hpr-*`), and by explicit
           `--rung-commit` shas. The kill clause on this bar is that uptake
           rising ONLY inside rungs means the form did not win on cost.
  minting  Introducing a form is not adopting it. The globally-first commit
           containing the form is found from history, never hardcoded.

A MOVE IS NOT AN ADOPTION, and that is a decision the two stages encode. The
commit-level pickaxe is rename-AWARE, so a commit that merely relocates a file
already using the form changes no occurrence count and never becomes a
candidate. The per-file walk on its own is rename-BLIND and would credit the
mover. This is not hypothetical: `580a652e` (nc-26) shows up in a per-file walk
as introducing `cli_shared::args` into eight `awareness_cmd` files, and its diff
is `R100`/`R09x` — a pure crate move of files that already used it. The
campaign's floor_basis, measured by the per-file walk alone, therefore reads
"14 rung conversions" spread over three commits; the truth is 14 over two
(nc-22b 5, nc-25 9) and nc-26 converted nothing. The floor itself — zero
voluntary — is unaffected.

DATES ARE PASSED AS EXPLICIT TIMESTAMPS, and that is load-bearing. `git log
--since=2026-08-21` does NOT mean midnight: approxidate fills in the CURRENT
TIME OF DAY, so the same window returned two rung commits in the morning and one
in the afternoon. Gate zero caught it. Every window is normalised to an ISO
instant here, and `--since-as-filter` is used where git supports it so a
non-monotonic timestamp cannot truncate the walk.

PRECONDITION (else exit 3, ARCH §7/§18.2/§18.3): `hpr-cheaper` must have a
newest measurement row of verdict `met`. A zero here while the cheaper form is
unbuilt is UNINTERPRETABLE — it says nothing about whether agents will reach for
something that does not yet exist. That is the exact reading `nc-adoption` got
wrong six times running, scoring `failed` against a treatment whose envelope was
never built. The verdict is READ from the co-lineage measurement store rather
than recomputed here: one decider, one name (§10.6).

GATE ZERO — `scripts/hpr-unprompted.py --gate-zero`. Both controls, discovered
from history rather than hardcoded, so they keep working as history grows:
  POSITIVE  find a real non-rung commit that introduced the derived form into a
            file, replay a window bracketing it, and require it to be counted.
  NEGATIVE  find the campaign-rung commits that introduced `cli_shared::args`
            into 14 files on one day, replay a window bracketing them with rung
            exclusion ON, and require the numerator to stay 0 — then replay the
            SAME window with exclusion OFF and require a non-zero, because a
            zero that could not have been anything else proves nothing (§18.1).

Exit codes (co-lineage instrument contract): 0 value valid · 2 usage ·
3 precondition unmet (named on stderr, NO value printed) · 4 environment.
`--gate-zero` exits 0 when both controls pass, 1 when either fails, 3 when a
control cannot be CONSTRUCTED.
"""
from __future__ import annotations

import argparse
import datetime as _dt
import json
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
STORE = Path.home() / ".sovereign" / "comaintainer" / "bar-measurements.jsonl"
CAMPAIGN = "hot-path-reuse"
PRECONDITION_BAR = "hpr-cheaper"
PRECONDITION_VERDICT = "met"

# Closed set (§2.1). A form is (pickaxe pattern, whether the pattern is a regex).
# `derived` is the treatment this campaign shipped; `shared-args` is the 73-day
# natural experiment that set the floor and is what gate zero replays.
FORMS = {
    "derived": (r"#\[derive\([^)]*Parser", True),
    "shared-args": ("cli_shared::args", False),
}
# `rf-` ADDED 2026-08-23, BEFORE THE FIRST refactor-factory RUNG LANDED, and the
# timing is the point. The exclusion rule is "a campaign cannot manufacture its
# own uptake" — but the rule was written when this campaign was the only one
# touching the form, so it named only its own prefix. refactor-factory converts
# hand-rolled arg loops to the SAME derived form under an `rf-` tag; without this
# line those conversions would have been counted as VOLUNTARY adopters and this
# bar would have reported a target it did not earn. That is spec attack #5 (do
# the reuse ourselves) landing on a NEIGHBOURING campaign's bar, where nobody
# would look for it.
#
# THIS IS THE ONLY LEGITIMATE DIRECTION FOR A MID-EXPERIMENT INSTRUMENT EDIT: it
# makes the bar STRICTER (fewer commits can count as voluntary), never looser.
# Loosening it mid-window would invalidate the series and restart it.
DEFAULT_RUNG_PREFIXES = ("hpr-", "rf-")
WINDOW_DAYS = 30


class Absent(Exception):
    """Reported, never defaulted (§18.3)."""


def _git(*argv: str) -> str:
    proc = subprocess.run(["git", *argv], cwd=REPO, capture_output=True, text=True)
    if proc.returncode != 0:
        raise Absent(f"git {' '.join(argv[:3])} … exited {proc.returncode}: "
                     f"{proc.stderr.strip()[:300]}")
    return proc.stdout


def _pickaxe(pattern: str, is_regex: bool) -> list[str]:
    return ([f"-S{pattern}", "--pickaxe-regex"] if is_regex else [f"-S{pattern}"])


def _local_tz() -> _dt.timezone:
    return _dt.datetime.now().astimezone().tzinfo


def as_instant(spec: str) -> _dt.datetime:
    """Any accepted window bound -> an explicit local-time instant.

    A BARE DATE IS THE TRAP. `git log --since=2026-08-21` is approxidate and
    fills in the current time of day, so a window that contained two rung
    commits at 09:00 contained one at 12:00. Bare dates are pinned to midnight
    here and every git call receives an ISO instant, never a date."""
    spec = spec.strip()
    if spec in ("now", ""):
        return _dt.datetime.now(tz=_local_tz())
    m = re.fullmatch(r"(\d+)\.days\.ago", spec)
    if m:
        return _dt.datetime.now(tz=_local_tz()) - _dt.timedelta(days=int(m.group(1)))
    if re.fullmatch(r"\d{4}-\d{2}-\d{2}", spec):
        return _dt.datetime.fromisoformat(spec + "T00:00:00").replace(
            tzinfo=_local_tz())
    try:
        d = _dt.datetime.fromisoformat(spec)
    except ValueError:
        raise Absent(f"cannot read {spec!r} as a window bound — pass an ISO "
                     f"instant, a YYYY-MM-DD date, `N.days.ago`, or `now`")
    return d if d.tzinfo else d.replace(tzinfo=_local_tz())


_SINCE_AS_FILTER: bool | None = None


def _since_flag() -> str:
    """`--since-as-filter` (git >= 2.37) filters WITHOUT stopping the walk; plain
    `--since` truncates history at the first out-of-order timestamp."""
    global _SINCE_AS_FILTER
    if _SINCE_AS_FILTER is None:
        probe = subprocess.run(
            ["git", "log", "-1", "--since-as-filter=1970-01-01T00:00:00",
             "--format=%H"], cwd=REPO, capture_output=True, text=True)
        _SINCE_AS_FILTER = probe.returncode == 0
    return "--since-as-filter" if _SINCE_AS_FILTER else "--since"


# --------------------------------------------------------------------------
# the per-file walk — the only thing allowed to decide when a form entered
# --------------------------------------------------------------------------


def introducing_commit(pattern: str, is_regex: bool, path: str) -> str | None:
    out = _git("log", *_pickaxe(pattern, is_regex), "--reverse", "--format=%H",
               "--", path)
    lines = [ln.strip() for ln in out.splitlines() if ln.strip()]
    return lines[0] if lines else None


def minting_commit(pattern: str, is_regex: bool) -> tuple[str, str] | None:
    out = _git("log", *_pickaxe(pattern, is_regex), "--reverse",
               "--format=%H%x09%s", "--", "*.rs")
    for ln in out.splitlines():
        if ln.strip():
            sha, _, subj = ln.partition("\t")
            return sha.strip(), subj.strip()
    return None


def candidate_commits(pattern: str, is_regex: bool, since: _dt.datetime,
                      until: _dt.datetime) -> list[dict]:
    """Commits whose diff CHANGES the number of occurrences of the form in some
    `.rs` file. Rename-aware by construction, so a pure move is not a candidate
    (see the module docstring) — and the per-file walk downstream is what
    actually credits a file."""
    out = _git("log", *_pickaxe(pattern, is_regex),
               f"{_since_flag()}={since.isoformat()}",
               f"--until={until.isoformat()}",
               "--format=%H%x1f%ad%x1f%an%x1f%s%x1e", "--date=short",
               "--", "*.rs")
    rows = []
    for rec in out.split("\x1e"):
        rec = rec.strip("\n")
        if not rec.strip():
            continue
        sha, date, author, subject = (rec.split("\x1f") + ["", "", "", ""])[:4]
        rows.append({"commit": sha, "date": date, "author": author,
                     "subject": subject})
    return rows


ADDED = re.compile(r"^\+(?!\+\+)")
DIFF_FILE = re.compile(r"^\+\+\+ b/(.*)$")


def files_gaining(sha: str, pattern: str, is_regex: bool) -> list[str]:
    """PRE-FILTER ONLY: `.rs` files in this commit's diff with an added line
    matching the form. Cheap; never the decider — `introducing_commit` is."""
    diff = _git("show", sha, "--unified=0", "--format=", "--no-color",
                "--diff-filter=ACMR", "--", "*.rs")
    rx = re.compile(pattern) if is_regex else None
    cur, hits = None, []
    for line in diff.splitlines():
        m = DIFF_FILE.match(line)
        if m:
            cur = m.group(1)
            continue
        if cur and ADDED.match(line):
            body = line[1:]
            if (rx.search(body) if rx else pattern in body):
                hits.append(cur)
                cur = None                   # one hit per file is enough
    return hits


# --------------------------------------------------------------------------
# rung identification
# --------------------------------------------------------------------------


def campaign_order_ids() -> list[str]:
    d = REPO / ".sovereign" / "features"
    if not d.is_dir():
        return []
    return sorted(p.name for p in d.iterdir()
                  if p.is_dir() and p.name.startswith("hpr"))


def is_rung(sha: str, subject: str, prefixes: tuple[str, ...],
            order_ids: list[str], rung_shas: set[str]) -> str | None:
    """-> reason string when this commit is a rung, else None."""
    if sha in rung_shas or sha[:12] in rung_shas:
        return "named by --rung-commit"
    body = subject + "\n" + _git("log", "-1", "--format=%b", sha)
    for oid in order_ids:
        if oid in body:
            return f"campaign order id {oid!r} in the commit message"
    for pfx in prefixes:
        if re.search(r"\b" + re.escape(pfx) + r"[0-9A-Za-z]", body):
            return f"campaign tag {pfx!r} in the commit message"
    return None


# --------------------------------------------------------------------------


def count_adoptions(form: str, since: _dt.datetime, until: _dt.datetime,
                    prefixes: tuple[str, ...] = DEFAULT_RUNG_PREFIXES,
                    rung_shas: set[str] | None = None,
                    exclude_rungs: bool = True) -> dict:
    pattern, is_regex = FORMS[form]
    rung_shas = rung_shas or set()
    order_ids = campaign_order_ids()
    cands = candidate_commits(pattern, is_regex, since, until)
    # The minting walk is a second full-history pickaxe (~13s on this repo).
    # An empty window needs no mint, and an empty window is the common case.
    mint = minting_commit(pattern, is_regex) if cands else None
    mint_sha = mint[0] if mint else None

    adoptions, excluded = [], []
    for c in cands:
        credited = [f for f in files_gaining(c["commit"], pattern, is_regex)
                    if introducing_commit(pattern, is_regex, f) == c["commit"]]
        if not credited:
            continue
        reason = None
        if c["commit"] == mint_sha:
            reason = "the form's own minting commit — introducing is not adopting"
        elif exclude_rungs:
            r = is_rung(c["commit"], c["subject"], prefixes, order_ids, rung_shas)
            if r:
                reason = f"campaign rung: {r}"
        if reason:
            excluded.append({**c, "files": credited, "reason": reason})
        else:
            adoptions.append({**c, "files": credited})

    files = sorted({f for a in adoptions for f in a["files"]})
    return {
        "value": float(len(files)),
        "form": form,
        "pattern": pattern,
        "window": {"since": since.isoformat(), "until": until.isoformat(),
                   "since_flag": _since_flag()},
        "rung_exclusion": exclude_rungs,
        "rung_prefixes": list(prefixes),
        "order_ids": order_ids,
        "minting_commit": mint_sha,
        # Absence is reported, never defaulted (§18.3): a null here means the
        # walk was SKIPPED because the window held no candidate, not that the
        # form has no minting commit.
        "minting_commit_status": ("resolved" if mint_sha else
                                  "not-needed: no candidate commit in window"
                                  if not cands else "not-found"),
        "adopted_files": files,
        "adoptions": adoptions,
        "excluded": excluded,
    }


# --------------------------------------------------------------------------
# precondition — read the verdict, never recompute it (§10.6)
# --------------------------------------------------------------------------


def cheaper_verdict(store: Path) -> tuple[str, str]:
    """-> (verdict, human explanation). Absence of rows IS never-attempted."""
    if not store.exists():
        return "never-attempted", f"no measurement store at {store}"
    newest = None
    for line in store.read_text(encoding="utf-8", errors="replace").splitlines():
        if not line.strip():
            continue
        try:
            d = json.loads(line)
        except json.JSONDecodeError:
            continue
        if d.get("campaign") == CAMPAIGN and d.get("bar") == PRECONDITION_BAR:
            newest = d
    if newest is None:
        return "never-attempted", (f"no {CAMPAIGN}/{PRECONDITION_BAR} row in "
                                   f"{store} — the bar has never been measured")
    return newest.get("verdict", "could-not-judge"), (
        f"newest {PRECONDITION_BAR} row: verdict={newest.get('verdict')} "
        f"value={newest.get('value')} ts={newest.get('ts')}")


# --------------------------------------------------------------------------
# gate zero
# --------------------------------------------------------------------------


def _introductions(form: str) -> list[dict]:
    """Every file carrying the form at HEAD, with the commit that introduced it."""
    pattern, is_regex = FORMS[form]
    grep = ["grep", "-l"]
    grep += (["-E", pattern] if is_regex else [pattern])
    out = _git(*grep, "HEAD", "--", "*.rs")
    rows = []
    for ln in out.splitlines():
        path = ln.split(":", 1)[1] if ln.startswith("HEAD:") else ln
        sha = introducing_commit(pattern, is_regex, path)
        if not sha:
            continue
        date, subject = _git("log", "-1", "--format=%ad%x1f%s", "--date=short",
                             sha).strip().split("\x1f")
        rows.append({"path": path, "commit": sha, "date": date, "subject": subject})
    return rows


def _span(dates: list[str]) -> tuple[_dt.datetime, _dt.datetime]:
    """[midnight of the earliest date, midnight after the latest) — explicit
    instants, never bare dates (see `as_instant`)."""
    lo = as_instant(min(dates))
    hi = as_instant((_dt.date.fromisoformat(max(dates))
                     + _dt.timedelta(days=1)).isoformat())
    return lo, hi


def gate_zero(out=sys.stdout) -> int:
    p = lambda s="": print(s, file=out)  # noqa: E731
    p("\n  hpr-unprompted — GATE ZERO\n")
    ok = True

    # ---- POSITIVE ---------------------------------------------------------
    p("  POSITIVE — a real, non-rung commit that introduced the DERIVED form.")
    p("  Replay a window bracketing it; it must be counted.\n")
    intros = _introductions("derived")
    mint = minting_commit(*FORMS["derived"])
    order_ids = campaign_order_ids()
    cands = [r for r in intros
             if r["commit"] != (mint[0] if mint else None)
             and not is_rung(r["commit"], r["subject"], DEFAULT_RUNG_PREFIXES,
                             order_ids, set())]
    if not cands:
        print("hpr-unprompted: no non-rung introduction of the derived form exists "
              "in history — the POSITIVE control CANNOT BE CONSTRUCTED",
              file=sys.stderr)
        return 3
    pick = sorted(cands, key=lambda r: r["date"])[0]
    since, until = _span([pick["date"]])
    got = count_adoptions("derived", since, until)
    hit = pick["path"] in got["adopted_files"]
    ok &= hit and got["value"] >= 1
    p(f"    replayed  {pick['commit'][:12]}  {pick['date']}  {pick['subject'][:44]}")
    p(f"    file      {pick['path']}")
    p(f"    window    {since.isoformat()} .. {until.isoformat()}")
    p(f"    predicted value >= 1 and the file named;  observed value = "
      f"{got['value']:g}, file counted = {hit}")
    p(f"    {'PASS' if (hit and got['value'] >= 1) else 'FAIL'}\n")

    # ---- NEGATIVE ---------------------------------------------------------
    p("  NEGATIVE — the campaign rungs that converted 14 files to")
    p("  `cli_shared::args` in one day. With rung exclusion ON the numerator")
    p("  must stay 0; with it OFF the same window must be non-zero, or the 0")
    p("  proves nothing (§18.1 — a zero count is not a control).\n")
    sa = _introductions("shared-args")
    rungs = [r for r in sa
             if is_rung(r["commit"], r["subject"], ("nc-",), [], set())]
    if not rungs:
        print("hpr-unprompted: no rung-introduced use of `cli_shared::args` found — "
              "the NEGATIVE control CANNOT BE CONSTRUCTED", file=sys.stderr)
        return 3
    since, until = _span([r["date"] for r in rungs])
    excl = count_adoptions("shared-args", since, until, prefixes=("nc-",))
    incl = count_adoptions("shared-args", since, until, prefixes=("nc-",),
                           exclude_rungs=False)
    neg_ok = excl["value"] == 0 and incl["value"] > 0
    ok &= neg_ok
    p(f"    rung commits  {', '.join(sorted({r['commit'][:8] for r in rungs}))}")
    p(f"    window        {since.isoformat()} .. {until.isoformat()}")
    p(f"    exclusion ON   predicted 0    observed {excl['value']:g}   "
      f"({len(excl['excluded'])} commit(s) excluded)")
    p(f"    exclusion OFF  predicted > 0  observed {incl['value']:g}   "
      f"(so the 0 above is a refusal, not an empty window)")
    for e in excl["excluded"]:
        p(f"      excluded {e['commit'][:8]} {len(e['files']):>2} file(s) — {e['reason']}")
    p(f"    {'PASS' if neg_ok else 'FAIL'}\n")

    p(f"  GATE ZERO {'PASSED' if ok else 'FAILED'}\n")
    return 0 if ok else 1


# --------------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--form", choices=sorted(FORMS), default="derived")
    ap.add_argument("--since", help=f"default: {WINDOW_DAYS} days ago")
    ap.add_argument("--until", default="now")
    ap.add_argument("--rung-prefix", action="append", default=[],
                    help="campaign tag marking a rung commit (repeatable)")
    ap.add_argument("--rung-commit", action="append", default=[],
                    help="explicit rung sha (repeatable)")
    ap.add_argument("--no-rung-exclusion", action="store_true",
                    help="gate zero only: count rungs too, to prove a 0 is a refusal")
    ap.add_argument("--store", default=str(STORE))
    ap.add_argument("--gate-zero", action="store_true")
    ap.add_argument("--ignore-precondition", action="store_true",
                    help="gate zero only: stamps gate_zero=true on the output")
    args = ap.parse_args()

    try:
        if args.gate_zero:
            return gate_zero()

        verdict, why = cheaper_verdict(Path(args.store))
        control = args.ignore_precondition or args.no_rung_exclusion
        if verdict != PRECONDITION_VERDICT and not control:
            print(f"hpr-unprompted: PRECONDITION UNMET — {PRECONDITION_BAR} is "
                  f"{verdict!r}, not {PRECONDITION_VERDICT!r}. {why}.",
                  file=sys.stderr)
            print("hpr-unprompted: a zero adoption count while the cheaper form is "
                  "unproven is uninterpretable, so NO value is reported "
                  "(exit 3 = could-not-judge / artifact-absent).", file=sys.stderr)
            return 3

        since = as_instant(args.since or f"{WINDOW_DAYS}.days.ago")
        until = as_instant(args.until)
        prefixes = tuple(args.rung_prefix) or DEFAULT_RUNG_PREFIXES
        res = count_adoptions(args.form, since, until, prefixes=prefixes,
                              rung_shas=set(args.rung_commit),
                              exclude_rungs=not args.no_rung_exclusion)
        res["commit"] = _git("rev-parse", "HEAD").strip()
        res["gate_zero"] = bool(control)
        res["precondition"] = why
    except Absent as exc:
        print(f"hpr-unprompted: {exc}", file=sys.stderr)
        return 4

    if control:
        print("hpr-unprompted: CONTROL run — not a bar value "
              f"(ignore_precondition={args.ignore_precondition}, "
              f"rung_exclusion={not args.no_rung_exclusion})", file=sys.stderr)

    if args.json:
        print(json.dumps(res))
        return 0

    p = print
    p(f"\n  hpr-unprompted — voluntary adoptions of the {res['form']} form\n")
    p(f"  window     {res['window']['since']} .. {res['window']['until']}")
    p(f"  pattern    {res['pattern']}")
    p(f"  minting    {(res['minting_commit'] or '(none)')[:12]}  (always excluded)")
    p(f"\n  value {res['value']:g}   floor 0  target 3   higher is better\n")
    for a in res["adoptions"]:
        p(f"    + {a['commit'][:8]} {a['date']} {a['subject'][:50]}")
        for f in a["files"]:
            p(f"        {f}")
    if not res["adoptions"]:
        p("    (no voluntary adoption in this window)")
    if res["excluded"]:
        p("\n  excluded:")
        for e in res["excluded"]:
            p(f"    - {e['commit'][:8]} {len(e['files']):>2} file(s) — {e['reason']}")
    p("\n  If uptake rises ONLY inside rungs, the form did not win on cost and")
    p("  the honest outcome is to delete it. Never add a gate to rescue this.\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
