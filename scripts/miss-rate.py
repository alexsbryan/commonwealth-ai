#!/usr/bin/env python3
"""miss-rate.py — THE BAR of `quality/REFACTOR_FACTORY.md`, measured retrospectively.

    MISS RATE — of the new symbols agents create, the fraction that duplicate
    something which already existed and was reachable.

    misses / all-new-symbols, over commits this initiative did not direct.

This is the FLOW bar. It counts DECISIONS, not code. Converging the existing
stock moves it by exactly zero; that is the point (REFACTOR_FACTORY.md, "the
five attacks"). It exists to be run BEFORE any intervention lands, so the
baseline cannot later be chosen to flatter a result.

THE WINDOW IS FIVE MONTHS, NOT SIX, AND THAT IS NOT A CHOICE.
`quality/REFACTOR_FACTORY.md` asks for "six months of baseline". The repository's
first commit is bb93a424, 2026-03-31. There is no sixth month. The window here
is 2026-04-01 -> HEAD and the script reports its own span rather than accepting
a requested one, because silently honouring "six months" over five months of
history is the exit-0 wrong answer ARCH §18.3 exists to catch. March is measured
and reported but is a PARTIAL month (one day) and is excluded from the headline.

TWO GIT TRAPS, BOTH MEASURED HERE, BOTH ROUTED AROUND.

  1. `--since` TRUNCATES THE WALK. It is a traversal limit, not a filter: git
     stops at the first commit older than the bound. Measured on this repo at
     63c72af8: `rev-list --count --since=2026-08-01 --until=2026-08-24` returns
     ZERO while the very same range's diff contains 590 added type definitions.
     Every date-bounded number is therefore wrong by an unpredictable amount.
     THIS SCRIPT PASSES NO DATE FLAG TO GIT AT ALL. It walks all of history once
     and buckets by committer date in Python. (`hpr-unprompted.py` hit the same
     trap from the other side and documents `--since-as-filter`.)

  2. A MOVE IS NOT A BIRTH. `git log -p` without rename detection reports every
     line of a relocated file as an addition, so a crate move would read as
     hundreds of new types. The walk runs with `-M -C`, under which a pure
     rename carries no content lines and contributes nothing.

WHAT A MISS IS, EXACTLY. The adjudicator is `svrn code converge shape` at FROZEN
settings (below) — IDF-weighted field-set cosine, which never compares a type
NAME and so catches the renamed fork a name census structurally cannot see. For
each group of duplicate shapes, members are ordered by BIRTH. The earliest
member is the ORIGINAL. Every later-born member is a MISS: at the moment it was
written, a type of that shape already existed in another crate. Members born in
the same commit as the original are NOT misses — nothing existed to reuse.

WHY THIS IS A FLOOR, STATED UP FRONT. The detector runs on the CURRENT graph, so
a duplicate that was created and then converged or deleted inside the window is
invisible to it. Those are real misses this instrument cannot see. The true miss
rate is therefore >= what this prints, never <=. The same is true of the shape
gates: types with fewer than 2 named fields never enter the population at all.

EXCLUSIONS, and why each exists.
  initiative  A campaign cannot manufacture its own result. Commits belonging to
              this initiative are dropped from the NUMERATOR AND THE DENOMINATOR
              (REFACTOR_FACTORY.md attack 5). Identified by campaign tag in the
              subject/body and by every order id under `.sovereign/features/`.
  vendored    Paths matching the detector's own scope exclusions, so numerator
              and denominator are drawn from one population.
"""
import json, re, subprocess, sys, collections, pathlib, argparse

# ---------------------------------------------------------------------------
# FROZEN DETECTOR SETTINGS — REFACTOR_FACTORY.md, "the one attack that DOES work"
#
# Loosening equivalence is the single way to move this bar without anything
# improving. These are the settings the baseline was taken at. Changing any of
# them invalidates the series and the series RESTARTS. They are recorded in the
# output of every run so a later run cannot quietly disagree with an earlier one.
# ---------------------------------------------------------------------------
DETECTOR = {
    "tool": "svrn code converge shape",
    "threshold": 0.50,      # --threshold default
    "min_shared": 3,        # --min-shared default
    "rare_df": 20,          # --rare-df default
    "min_fields": 2,        # --min-fields default
    "names_only": False,
    "frozen_at": "2026-08-23",
    "frozen_reason": "baseline taken before any refactor-factory intervention",
}

WINDOW_START = "2026-04"          # first whole month of repository history

# GATE ZERO — the instrument is checked before its result is believed (ARCH §18.4).
#
# POSITIVE CONTROL. These monthly new-type-definition counts were measured
# INDEPENDENTLY of this script and recorded in quality/REFACTOR_FACTORY.md before
# it existed. An instrument that cannot re-derive a number it did not produce is
# not measuring what it claims to.
#
# THE CONTROL RUNS AGAINST THE UNFILTERED BIRTH COUNT, AND THAT DISTINCTION IS
# THE POINT. Gate zero's first run failed here — filtered births drifted from the
# anchor by 13.2% / 9.1% / 26.4% in May / July / August. The cause was not a bug
# in birth detection: the anchor counts EVERY type definition, including tests,
# benches and examples, and matches the unfiltered walk within 2.6% in all five
# months. The BAR must use the filtered count, because the detector
# (`converge shape`) excludes those paths and a numerator and denominator drawn
# from different populations is meaningless. So the two numbers are both computed
# and each is used for exactly one job:
#   unfiltered -> gate zero, proving the birth MECHANISM is sound;
#   filtered   -> the bar, matching the detector's POPULATION.
# Checking the anchor against the filtered count would have forced a choice
# between a broken control and a broken bar. There was no such choice.
ANCHOR_BIRTHS = {"2026-04": 1996, "2026-05": 1535, "2026-06": 1000,
                 "2026-07": 850, "2026-08": 643}
ANCHOR_TOLERANCE = 0.05
ANCHOR_SOURCE = "quality/REFACTOR_FACTORY.md, 'Baseline first, and it is computable TODAY'"
EXCLUDE_SEGMENTS = {"vendor", "node_modules", ".cargo-container", "research",
                    "external", "target", ".claude", "tests", "benches", "examples"}

DEF_RE   = re.compile(r'^\+\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:struct|enum)\s+([A-Z][A-Za-z0-9_]*)')
DEL_RE   = re.compile(r'^-\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:struct|enum)\s+([A-Z][A-Za-z0-9_]*)')
OLDPATH_RE = re.compile(r'^--- a/(.+)$')
PATH_RE  = re.compile(r'^\+\+\+ b/(.+)$')
GROUP_RE = re.compile(r'^([01]\.\d{3})\s+(.+)$')


def sh(args):
    return subprocess.run(args, capture_output=True, text=True, cwd=REPO).stdout


def cli():
    """`svrn` is the prod symlink; some dev hosts carry only `sovereign`. Both are
    the same binary — try in order rather than hardcoding one and failing on the
    other host."""
    for name in ("svrn", "sovereign"):
        if subprocess.run(["which", name], capture_output=True).returncode == 0:
            return name
    return "svrn"


def in_scope(path):
    if not path.endswith(".rs") or path.endswith("build.rs"):
        return False
    return not (set(pathlib.PurePath(path).parts) & EXCLUDE_SEGMENTS)


def crate_index():
    """dir -> crate name, from every Cargo.toml. Longest prefix wins."""
    idx = {}
    for line in sh(["git", "ls-files", "*Cargo.toml"]).splitlines():
        try:
            txt = (REPO / line).read_text(errors="ignore")
        except OSError:
            continue
        m = re.search(r'^\s*name\s*=\s*"([^"]+)"', txt, re.M)
        if m:
            idx[str(pathlib.PurePath(line).parent)] = m.group(1)
    return idx


def crate_of(path, idx):
    p = pathlib.PurePath(path)
    for i in range(len(p.parts) - 1, 0, -1):
        cand = str(pathlib.PurePath(*p.parts[:i]))
        if cand in idx:
            return idx[cand]
    return None


def initiative_commits():
    """Campaign rungs + every order id on disk. A campaign cannot count its own work."""
    ids = {d.name for d in (REPO / ".sovereign" / "features").glob("*") if d.is_dir()}
    tags = [r'\bnc-\d', r'\bhpr-\d', r'noun-convergence', r'hot-path-reuse', r'refactor-factory']
    pat = re.compile("|".join(tags + [re.escape(i) for i in ids]), re.I)
    out, sha = set(), None
    for line in sh(["git", "log", "--format=@@C %H%n%s%n%b"]).splitlines():
        if line.startswith("@@C "):
            sha = line[4:].strip()
        elif sha and pat.search(line):
            out.add(sha)
    return out


def walk_births():
    """Births AND deaths in ONE pass. No date flags; `-M -C` so a move is neither.

    NEGATIVE FLOW is not decoration. Three things depend on it:
      - net flow: a population whose death rate is ~0 accumulates no matter what
        the miss rate does, so births alone cannot say whether the codebase is
        converging or just growing more slowly;
      - the miss rate's own BLIND SPOT: a duplicate born and killed inside the
        window never reaches the current graph, so the detector cannot adjudicate
        it. Counting those births BOUNDS the floor instead of merely declaring it;
      - who did the subtracting: deaths inside initiative commits are OUR
        convergence work, not the codebase's own closure loop, and conflating
        them lets the campaign take credit for a habit it did not create.
    """
    proc = subprocess.Popen(
        ["git", "log", "--format=@@C %H %cI", "-M", "-C", "-p", "HEAD", "--", "*.rs"],
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, cwd=REPO)
    births, per_commit, deaths, per_commit_all = {}, [], [], []
    sha = date = path = oldpath = None
    for line in proc.stdout:
        if line.startswith("@@C "):
            _, sha, date = line.split()
            path = oldpath = None
        elif line.startswith("+++ b/"):
            path = PATH_RE.match(line.rstrip()).group(1)
        elif line.startswith("--- a/"):
            oldpath = OLDPATH_RE.match(line.rstrip()).group(1)
        elif line.startswith("+"):
            m = DEF_RE.match(line)
            if not (m and sha and path):
                continue
            name = m.group(1)
            if path.endswith(".rs"):
                per_commit_all.append((sha, date, path, name))   # gate-zero control
            if not in_scope(path):
                continue
            per_commit.append((sha, date, path, name))
            key = (name, path)
            if key not in births or date < births[key][0]:
                births[key] = (date, sha, path)
        elif line.startswith("-") and not line.startswith("--- "):
            m = DEL_RE.match(line)
            src = oldpath or path
            if m and sha and src and in_scope(src):
                deaths.append((sha, date, src, m.group(1)))
    proc.wait()
    return births, per_commit, deaths, per_commit_all


def live_at_head():
    """Type NAMES present at HEAD, in scope.

    KEYED ON NAME, NOT (name, path), AND THAT IS THE WHOLE CORRECTNESS ARGUMENT.
    Keying on the path counts a type that merely MOVED FILE as having died — the
    same error `-M -C` exists to prevent in the diff walk. Measured: the path-keyed
    version reported 1,625 in-window births missing at HEAD against only 737
    observed deaths, an impossibility that is entirely relocation. Name-keyed is
    conservative in the right direction: a type is "gone" only when no definition
    of that name survives anywhere in scope.
    """
    out = set()
    txt = sh(["git", "grep", "-n", "-E",
              r'^[[:space:]]*(pub([[:space:]]*\([^)]*\))?[[:space:]]+)?(struct|enum)[[:space:]]+[A-Z]',
              "HEAD", "--", "*.rs"])
    pat = re.compile(r'^HEAD:([^:]+):\d+:\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:struct|enum)\s+([A-Z][A-Za-z0-9_]*)')
    for line in txt.splitlines():
        m = pat.match(line)
        if m and in_scope(m.group(1)):
            out.add(m.group(2))
    return out


def parse_groups(text):
    groups = []
    for line in text.splitlines():
        m = GROUP_RE.match(line)
        if not m:
            continue
        members = []
        for part in m.group(2).split("=="):
            part = part.strip()
            if "::" in part:
                crate, name = part.rsplit("::", 1)
                members.append((crate.strip(), name.strip()))
        if len(members) > 1:
            groups.append({"score": float(m.group(1)), "members": members})
    return groups


def gate_zero(result, per_commit, deaths):
    """Refuse to report a number the instrument cannot defend. Exit 4 on failure."""
    fails, checks = [], []

    # 1. POSITIVE CONTROL against the independently-recorded anchor.
    got = result["births_unfiltered_for_gate_zero"]
    for mo, want in sorted(ANCHOR_BIRTHS.items()):
        have = got.get(mo)
        if have is None:
            fails.append(f"anchor month {mo} absent from series")
            continue
        drift = abs(have - want) / want
        ok = drift <= ANCHOR_TOLERANCE
        checks.append((f"births {mo} ~= anchor {want}", f"{have} ({drift*100:.1f}% drift)", ok))
        if not ok:
            fails.append(f"{mo}: births {have} vs anchor {want}, {drift*100:.1f}% > "
                         f"{ANCHOR_TOLERANCE*100:.0f}%")

    # 2. A BIRTH CANNOT VANISH WITHOUT A DEATH. Births counted as unadjudicable
    #    (in-window, absent at HEAD) can never exceed observed deaths. THIS CHECK
    #    IS NOT HYPOTHETICAL: the first version of live_at_head() keyed on
    #    (name, path), counted every RELOCATED type as dead, and reported 1,625
    #    unadjudicable against 822 total deaths. It was caught by hand. It would
    #    have been caught here.
    tot_u = result["totals"]["unadjudicable_births"]
    tot_deaths_all = result["totals"]["deaths"] + result["totals"]["deaths_initiative"]
    ok = tot_u <= tot_deaths_all
    checks.append(("unadjudicable births <= all deaths", f"{tot_u} <= {tot_deaths_all}", ok))
    if not ok:
        fails.append(f"{tot_u} births vanished but only {tot_deaths_all} deaths observed — "
                     f"the survival check is counting relocations as deaths")

    # 3. MISSES ARE A SUBSET OF BIRTHS. A miss that is not also a counted birth
    #    means numerator and denominator are drawn from different populations.
    ok = result["totals"]["misses"] <= result["totals"]["new_symbols"]
    checks.append(("misses <= births", f"{result['totals']['misses']} <= "
                                       f"{result['totals']['new_symbols']}", ok))
    if not ok:
        fails.append("numerator exceeds denominator — populations disagree")

    # 4. THE DETECTOR ACTUALLY RAN. Zero groups is exit 3 upstream, but zero
    #    ADJUDICABLE members would silently produce a miss rate of 0.0.
    ok = result["groups_adjudicated"] > 0 and result["totals"]["misses"] > 0
    checks.append(("detector produced adjudicated misses",
                   f"{result['groups_adjudicated']} groups, "
                   f"{result['totals']['misses']} misses", ok))
    if not ok:
        fails.append("no misses adjudicated — a miss rate of 0 here means the "
                     "detector or the birth join is broken, not that the code is clean")

    print("GATE ZERO")
    for name, val, ok in checks:
        print(f"  [{'PASS' if ok else 'FAIL'}] {name:<38} {val}")
    print(f"  anchor source: {ANCHOR_SOURCE}")
    if fails:
        print("\nGATE ZERO FAILED — the result is NOT reported:")
        for f in fails:
            print(f"  - {f}")
        return 4
    print("\ngate zero passed; the number below is defensible")
    return 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--gate-zero", action="store_true",
                    help="validate the instrument against independently-measured "
                         "numbers and internal invariants; exit 4 if it cannot defend itself")
    ap.add_argument("--shape-file", help="frozen `converge shape --limit 0` output")
    ap.add_argument("--emit", choices=["miss-rate", "negative-flow"], default="miss-rate",
                    help="which bar's scalar goes in `value`. The whole report is emitted "
                         "either way; only the headline scalar changes, so the two bars "
                         "cannot disagree about the run that produced them.")
    a = ap.parse_args()

    shape_txt = (pathlib.Path(a.shape_file).read_text() if a.shape_file
                 else sh([cli(), "code", "converge", "shape", "--limit", "0"]))
    groups = parse_groups(shape_txt)
    if not groups:
        print("PRECONDITION FAILED: detector returned no groups (ARCH §18.3)", file=sys.stderr)
        return 3

    idx = crate_index()
    initiative = initiative_commits()
    births, per_commit, deaths, per_commit_all = walk_births()

    # name+crate -> earliest birth, from the per-file birth table
    by_symbol = {}
    for (name, path), (date, sha, p) in births.items():
        c = crate_of(path, idx)
        if not c:
            continue
        k = (c, name)
        if k not in by_symbol or date < by_symbol[k][0]:
            by_symbol[k] = (date, sha, path)

    # DENOMINATOR: births per month, initiative commits removed
    denom = collections.Counter()
    for sha, date, path, name in per_commit:
        if sha in initiative:
            continue
        denom[date[:7]] += 1

    # unfiltered births, initiative commits INCLUDED — this is the quantity the
    # anchor measured, and gate zero's control must compare like with like
    denom_all = collections.Counter(date[:7] for _, date, _, _ in per_commit_all)

    # NEGATIVE FLOW: deaths per month, split by who did the subtracting
    died, died_initiative = collections.Counter(), collections.Counter()
    for sha, date, path, name in deaths:
        (died_initiative if sha in initiative else died)[date[:7]] += 1

    # BLIND SPOT: births inside the window that are gone at HEAD. The detector
    # runs on the current graph and can never adjudicate these, so they bound
    # the floor rather than leaving it an open-ended caveat.
    live = live_at_head()
    unadjudicable = collections.Counter()
    for sha, date, path, name in per_commit:
        if sha in initiative or date[:7] < WINDOW_START:
            continue
        if name not in live:
            unadjudicable[date[:7]] += 1

    # NUMERATOR: later-born members of a duplicate shape group
    misses, unresolved = [], 0
    for g in groups:
        dated = []
        for crate, name in g["members"]:
            b = by_symbol.get((crate, name))
            if b:
                dated.append((b[0], b[1], crate, name, b[2]))
            else:
                unresolved += 1
        if len(dated) < 2:
            continue
        dated.sort()
        origin_date, origin_sha, origin_crate, origin_name, _ = dated[0]
        for date, sha, crate, name, path in dated[1:]:
            if sha == origin_sha or sha in initiative or date[:7] < WINDOW_START:
                continue
            misses.append({"month": date[:7], "crate": crate, "name": name,
                           "path": path, "sha": sha[:12], "score": g["score"],
                           "duplicated": f"{origin_crate}::{origin_name}",
                           "origin_born": origin_date[:10]})

    miss_by_month = collections.Counter(m["month"] for m in misses)
    months = sorted(mo for mo in denom if mo >= WINDOW_START)
    series = [{"month": mo,
               "new_symbols": denom[mo],
               "deaths": died[mo],
               "deaths_initiative": died_initiative[mo],
               "net_flow": denom[mo] - died[mo],
               "misses": miss_by_month[mo],
               "miss_rate": round(miss_by_month[mo] / denom[mo], 5) if denom[mo] else None,
               "unadjudicable_births": unadjudicable[mo]}
              for mo in months]
    tot_n = sum(denom[mo] for mo in months)
    tot_m = sum(miss_by_month[mo] for mo in months)
    tot_d = sum(died[mo] for mo in months)
    tot_di = sum(died_initiative[mo] for mo in months)
    tot_u = sum(unadjudicable[mo] for mo in months)

    result = {
        "bar": "miss-rate",
        "value": round(tot_m / tot_n, 5) if tot_n else None,
        "is_floor": True,
        "floor_reason": "detector runs on the CURRENT graph; duplicates created and "
                        "then converged or deleted inside the window are invisible",
        "window": {"start": months[0] if months else None,
                   "end": months[-1] if months else None,
                   "months": len(months),
                   "requested_by_spec": 6,
                   "available": "repository's first commit is 2026-03-31; there is no sixth month"},
        # `commit`, not `head`: co-lineage's _parse_value reads this key to stamp
        # the measurement row's ref. Naming it anything else records an
        # unattributed row that looks fine.
        "commit": sh(["git", "rev-parse", "HEAD"]).strip(),
        "dirty": bool(sh(["git", "status", "--porcelain"]).strip()),
        "detector": DETECTOR,
        "totals": {"new_symbols": tot_n, "misses": tot_m, "deaths": tot_d,
                   "deaths_initiative": tot_di, "net_flow": tot_n - tot_d,
                   "unadjudicable_births": tot_u},
        "negative_flow": {
            "death_rate": round(tot_d / tot_n, 5) if tot_n else None,
            "note": "deaths in initiative commits are counted SEPARATELY: they are "
                    "the campaign's own convergence work, not the codebase's habit",
        },
        "floor_bound": {
            "unadjudicable_births": tot_u,
            "share_of_denominator": round(tot_u / tot_n, 5) if tot_n else None,
            "meaning": "born and gone before the current graph was taken; the detector "
                       "cannot adjudicate them, so the true miss rate lies between the "
                       "reported value and (misses + unadjudicable) / new_symbols",
            "upper_bound_if_all_were_misses": round((tot_m + tot_u) / tot_n, 5) if tot_n else None,
        },
        "series": series,
        "births_unfiltered_for_gate_zero": {mo: denom_all[mo] for mo in months},
        "groups_adjudicated": len(groups),
        "members_without_birth": unresolved,
        "initiative_commits_excluded": len(initiative),
        "misses_detail": sorted(misses, key=lambda m: m["month"]),
    }

    if a.gate_zero:
        return gate_zero(result, per_commit, deaths)

    if a.emit == "negative-flow":
        result["bar"] = "negative-flow"
        result["value"] = result["negative_flow"]["death_rate"]
        result["is_floor"] = False
        result["floor_reason"] = ("organic deaths only; deaths inside initiative commits "
                                  "are reported separately and never folded in")

    if a.json:
        # SINGLE LINE, deliberately. co-lineage reads the LAST non-empty stdout
        # line and json.loads it; a pretty-printed object ends in "}" and is
        # recorded as could-not-judge(unparseable-output). Measured that way once.
        print(json.dumps(result))
        return 0

    w = result["window"]
    print(f"MISS RATE — {w['start']} .. {w['end']}  ({w['months']} months)")
    print(f"  spec asked for 6 months; {w['available']}\n")
    print(f"  {'month':<9} {'births':>7} {'deaths':>7} {'net':>7} {'misses':>7} {'rate':>8}")
    for r in series:
        print(f"  {r['month']:<9} {r['new_symbols']:>7} {r['deaths']:>7} "
              f"{r['net_flow']:>+7} {r['misses']:>7} "
              f"{(r['miss_rate']*100 if r['miss_rate'] is not None else 0):>7.2f}%")
    print(f"  {'TOTAL':<9} {tot_n:>7} {tot_d:>7} {tot_n - tot_d:>+7} {tot_m:>7} "
          f"{(result['value']*100 if result['value'] else 0):>7.2f}%")
    print(f"\n  NEGATIVE FLOW: {tot_d} organic deaths ({tot_d/tot_n*100:.1f}% of births) "
          f"+ {tot_di} more inside initiative commits, counted separately.")
    print(f"  BLIND SPOT BOUNDED: {tot_u} in-window births ({tot_u/tot_n*100:.1f}%) are gone at "
          f"HEAD and cannot be adjudicated.")
    print(f"  True miss rate lies in [{result['value']*100:.2f}%, "
          f"{result['floor_bound']['upper_bound_if_all_were_misses']*100:.2f}%].")
    print(f"\n  THIS IS A FLOOR — {result['floor_reason']}.")
    print(f"  detector FROZEN {DETECTOR['frozen_at']}: threshold={DETECTOR['threshold']} "
          f"min_shared={DETECTOR['min_shared']} rare_df={DETECTOR['rare_df']} "
          f"min_fields={DETECTOR['min_fields']}")
    print(f"  {len(groups)} shape groups adjudicated · "
          f"{len(initiative)} initiative commits excluded from BOTH sides")
    if result["dirty"]:
        print("  !! DIRTY TREE — the ref does not fully describe what ran")
    return 0


if __name__ == "__main__":
    REPO = pathlib.Path(__file__).resolve().parent.parent
    sys.exit(main())
