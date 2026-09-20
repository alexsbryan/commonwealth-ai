#!/usr/bin/env python3
"""Checks I1-I7 of PRE-REG-custom-ontology-and-raptor-2026-09-17, over a runs dir.

    checks.py --runs <dir> [--out <dir>]
    checks.py --runs <dir> --plant all        # or --plant I3
    checks.py --self-test

`--runs` is the tree `run_arm.py` writes: `<dir>/<arm>/run-<N>/{eval,manifest}.json`.
Output is a seven-row table and `checks.jsonl` (one JSON object per check) in
`--out`, which defaults to the runs dir.

**Four verdicts, never two.** Each check reads `passed`, `failed`,
`could-not-judge` or `never-ran`, with a one-line reason. A check whose arm was
not run reads `never-ran` — it is not a pass, and this script does not decide
whose job it is to care. That policy belongs to the caller, which knows which
arms it meant to run: `research/ontology-retrieval/pilot/run-pilot.sh` declares
the arms it cannot build and refuses any OTHER never-ran (ARCH §12 — the line
is drawn by what each side owns about itself).

**No bar is read here.** I5's 0.8 and I2's two 50% floors are the pre-reg's own
instrument thresholds, pre-registered before any data; they say whether the
instrument worked, never whether the ontology helped.

**`--plant`** copies the runs dir, corrupts the copy in the way one check must
catch, and asserts that check flips to `failed`. It runs the check on the clean
copy FIRST: a plant against a check that was already red proves nothing (ARCH
§7 — validate the instrument before the result), and that reads
`could-not-judge`, never a plant that "worked". A plant materialises the arm it
needs when the runs dir has none, so enforcement is proven whatever was run.
Nothing under `--runs` and nothing under any real index dir is ever written.

Exit codes. 0 = every check was issued and none read `failed` (plant mode: every
plant flipped its check to `failed`). 1 = at least one `failed` (plant mode: at
least one plant did not flip). 2 = a premise refusal; no check was issued.

I3 and I4 are not re-derived here. The ablation/full hash rule and the bare-arm
noise band each have exactly one implementation, in `compare.py`, and this file
imports it (ARCH §8 — one decider, one name). The kind names come from
`attest.py`, which writes them.
"""

import argparse
import importlib.util
import json
import os
import shutil
import sys
import tempfile
from pathlib import Path

HARNESS = Path(__file__).resolve().parent
REPO = HARNESS.parents[2]
COMPARE_PY = REPO / "sovereign" / "bench" / "sep_atlas" / "map-conversion-rung6" / "compare.py"
ATTEST_PY = HARNESS / "attest.py"

PASSED, FAILED, CNJ, NEVER = "passed", "failed", "could-not-judge", "never-ran"
CHECK_IDS = ("I1", "I2", "I3", "I4", "I5", "I6", "I7")

# Pre-reg instrument thresholds ("Checks"). Fixed before any data; not knobs.
WALK_FIRED_MIN = 0.5     # I2: full's walk fired on at least half the questions
SUMMARY_K4_MIN = 0.5     # I2: a summary appended on at least half of K4
ORACLE_MIN = 0.8         # I5: below this the truth set or the scorer is broken

# The three provenance tags `RetrievedChunk.source` can carry
# (`eval_cmd/runner.rs:242-247`). Bare must carry none of them.
STRUCTURAL_SOURCES = ("atlas", "atom-enum", "raptor")
# Both bags are `Vec<RetrievedChunk>` and both carry `source`
# (`runner.rs:81`, `:138`). `atlas_navigation` is atlas-surfaced by
# construction, so a bare arm holding one is exactly what I2 asks about; it is
# pulled out separately only because it does not compete for passage slots.
CHUNK_BAGS = ("retrieved", "atlas_navigation")


class Refused(Exception):
    """A premise this script cannot proceed under. Exit 2, issue nothing."""


def _load(path, name):
    if not Path(path).is_file():
        raise Refused(f"no module at {path}")
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


CMP = _load(COMPARE_PY, "ei7_compare")
ATT = _load(ATTEST_PY, "ei7_attest")


def check(cid, verdict, reason, **evidence):
    return {"check": cid, "verdict": verdict, "reason": reason, **evidence}


# ── I1: closed-book retrieved nothing ────────────────────────────────────────

def i1_closed_book_retrieved_is_empty(arms):
    """Closed-book returns an empty `retrieved[]` (pre-reg I1).

    A naked turn persists no metadata, so `retrieved` is empty by construction
    (`eval_cmd/runner.rs:1740-1745`). A non-empty one means the arm was not
    naked and every closed-book exclusion downstream is unearned.
    """
    runs = arms.get(CMP.CLOSED_BOOK_ARM)
    if not runs:
        return check("I1", NEVER, f"no `{CMP.CLOSED_BOOK_ARM}` arm under the runs dir")
    rows = 0
    offenders = []
    for r in runs:
        for row in CMP.rows_of(r):
            rows += 1
            n = len(row.get("retrieved") or [])
            if n:
                offenders.append(f"{CMP._label(CMP.CLOSED_BOOK_ARM, r)}"
                                 f"/{row.get('question_id')}={n}")
    if not rows:
        return check("I1", NEVER,
                     f"`{CMP.CLOSED_BOOK_ARM}` produced no row over "
                     f"{len(runs)} run(s)", rows=0)
    if offenders:
        return check("I1", FAILED,
                     f"{len(offenders)} of {rows} closed-book row(s) retrieved "
                     f"chunks: {', '.join(offenders[:4])}",
                     rows=rows, offenders=offenders)
    return check("I1", PASSED,
                 f"{rows} closed-book row(s) over {len(runs)} run(s), every "
                 f"`retrieved[]` empty", rows=rows)


# ── I2: every arm is what it says it is ──────────────────────────────────────

def _structural_hits(row):
    """`[(bag, source)]` for every structural provenance tag on one row."""
    hits = []
    for bag in CHUNK_BAGS:
        for c in row.get(bag) or []:
            src = c.get("source")
            if src in STRUCTURAL_SOURCES:
                hits.append((bag, src))
    return hits


def _i2_bare(arms):
    runs = arms.get(CMP.BARE_ARM)
    if not runs:
        return NEVER, f"no `{CMP.BARE_ARM}` arm under the runs dir", {}
    rows, offenders = 0, []
    for r in runs:
        for row in CMP.rows_of(r):
            rows += 1
            for bag, src in _structural_hits(row):
                offenders.append(f"{CMP._label(CMP.BARE_ARM, r)}"
                                 f"/{row.get('question_id')}:{bag}.{src}")
    if not rows:
        return NEVER, f"`{CMP.BARE_ARM}` produced no row", {"bare_rows": 0}
    if offenders:
        return (FAILED,
                f"bare carried {len(offenders)} structural source tag(s): "
                f"{', '.join(sorted(set(offenders))[:4])}",
                {"bare_rows": rows, "bare_offenders": offenders})
    return (PASSED, f"bare: {rows} row(s), no atlas/atom-enum/raptor tag",
            {"bare_rows": rows})


def _i2_full(arms):
    """Full's walk fired, on questions and on K4 summaries.

    Below either floor the ARM reads never-ran, not the check failed: a walk
    that did not fire says the arm was not the arm, which is an absent
    measurement rather than a violated invariant (the row's own rule).
    """
    runs = arms.get(CMP.FULL_ARM)
    if not runs:
        return NEVER, f"no `{CMP.FULL_ARM}` arm under the runs dir", {}
    rows = fired = k4 = k4_summed = 0
    for r in runs:
        for row in CMP.rows_of(r):
            if not CMP.measured(row):
                continue
            rows += 1
            verdict, _lines = CMP.walk_path(row)
            if verdict == PASSED:
                fired += 1
            if row.get("category") == ATT.K4:
                k4 += 1
                walk = row.get("atlas_walk")
                if isinstance(walk, dict) and (walk.get("summaries_appended") or 0) > 0:
                    k4_summed += 1
    ev = {"full_rows": rows, "walk_fired": fired, "k4_rows": k4,
          "k4_summaries": k4_summed}
    if not rows:
        return NEVER, f"`{CMP.FULL_ARM}` measured no row", ev
    share = fired / rows
    ev["walk_fired_share"] = share
    if share < WALK_FIRED_MIN:
        return (NEVER,
                f"full's walk fired on {fired}/{rows} ({share:.0%}) < "
                f"{WALK_FIRED_MIN:.0%} — the arm is never-ran, not a null", ev)
    if not k4:
        return (NEVER,
                f"full's walk fired on {fired}/{rows} ({share:.0%}), but no "
                f"{ATT.K4} question was measured — the summary half never ran", ev)
    k4_share = k4_summed / k4
    ev["k4_summary_share"] = k4_share
    if k4_share < SUMMARY_K4_MIN:
        return (NEVER,
                f"a summary was appended on {k4_summed}/{k4} {ATT.K4} "
                f"({k4_share:.0%}) < {SUMMARY_K4_MIN:.0%} — the arm is never-ran", ev)
    return (PASSED,
            f"full: walk fired {fired}/{rows} ({share:.0%}), summary on "
            f"{k4_summed}/{k4} {ATT.K4} ({k4_share:.0%})", ev)


def i2_every_arm_is_what_it_says(arms):
    bare_v, bare_r, bare_e = _i2_bare(arms)
    full_v, full_r, full_e = _i2_full(arms)
    ev = {"bare": {"verdict": bare_v, "reason": bare_r, **bare_e},
          "full": {"verdict": full_v, "reason": full_r, **full_e}}
    if FAILED in (bare_v, full_v):
        beaten = [r for v, r in ((bare_v, bare_r), (full_v, full_r)) if v == FAILED]
        return check("I2", FAILED, "; ".join(beaten), **ev)
    if NEVER in (bare_v, full_v):
        absent = [r for v, r in ((bare_v, bare_r), (full_v, full_r)) if v == NEVER]
        return check("I2", NEVER, "; ".join(absent), **ev)
    return check("I2", PASSED, f"{bare_r}; {full_r}", **ev)


# ── I3: ablation and full differ only in the named thing ─────────────────────

# The hash fields I3 itself is about. A refusal on one of these is the check
# failing; a refusal on a model, host or recipe means the runs were never
# comparable, which makes I3 unjudgeable rather than false.
I3_FIELDS = ("ontology_sha256", "chunks_listing_sha256")


def i3_ablation_differs_only_in_the_atlas(arms):
    """`compare.check_identity` is the one implementation (pre-reg I3)."""
    try:
        _identity, i3 = CMP.check_identity(arms)
    except CMP.Refusal as e:
        if e.field in I3_FIELDS:
            return check("I3", FAILED, f"{e.field}: {e.detail}", field=e.field)
        return check("I3", CNJ,
                     f"the arms are not comparable at all — {e.field}: {e.detail}",
                     field=e.field)
    return check("I3", i3["verdict"], i3["reason"])


# ── I4: the noise band exists for every category ─────────────────────────────

def _categories(arms, excluded):
    cats = set()
    for runs in arms.values():
        for r in runs:
            for row in CMP.rows_of(r):
                if row.get("question_id") not in excluded:
                    cats.add(row.get("category"))
    return sorted(c for c in cats if c is not None)


def i4_a_band_for_every_category(arms, excluded):
    """Bare's max - min per category over its runs, floor 0.05 (pre-reg I4).

    A category bare never scored has NO band, and a comparison that reads a
    delta there is reading it against nothing. One run gives a raw spread of
    0.0 and the floor stands in — that is the floor's job, and `floored` says
    so in the JSON.
    """
    cats = _categories(arms, excluded)
    if not cats:
        return check("I4", NEVER, "no category survived the closed-book exclusions")
    bands, band_arm = CMP.noise_bands(arms, excluded, cats)
    if band_arm["verdict"] != PASSED:
        return check("I4", NEVER, f"no `{CMP.BARE_ARM}` arm under the runs dir — "
                                  f"no band was measured", categories=cats)
    missing = [c for c in cats if bands[c].get("band") is None]
    ev = {"categories": cats,
          "bands": {c: bands[c].get("band") for c in cats},
          "floored": [c for c in cats if bands[c].get("floored")],
          "band_runs": band_arm["runs"]}
    if missing:
        return check("I4", FAILED,
                     f"no band for {len(missing)} of {len(cats)} categories: "
                     f"{', '.join(missing)} — `{CMP.BARE_ARM}` scored none of them",
                     **ev)
    shown = ", ".join(f"{c}={bands[c]['band']:.3f}"
                      f"{'*' if bands[c].get('floored') else ''}" for c in cats)
    return check("I4", PASSED,
                 f"a band for all {len(cats)} categories over "
                 f"{band_arm['runs']} bare run(s): {shown} (* = at the "
                 f"{CMP.BAND_FLOOR} floor)", **ev)


# ── I5: the oracle reaches its own ceiling ───────────────────────────────────

I5_CATEGORIES = (ATT.K0, ATT.K2)


def i5_oracle_scores_its_floor(arms, excluded):
    """Oracle at least 0.8 on K0 and K2; below that the truth set or the scorer
    is broken (pre-reg I5). K1 and K4 are the window ceiling, never a floor, so
    they are not judged here.
    """
    runs = arms.get("oracle")
    if not runs:
        return check("I5", NEVER, "no `oracle` arm under the runs dir")
    judge, _kw, _f, _m, _d = CMP.per_run_category_means(runs, excluded)
    scores, absent, below = {}, [], []
    for cat in I5_CATEGORIES:
        xs = [x for x in judge.get(cat, []) if x is not None]
        if not xs:
            absent.append(cat)
            continue
        mean = CMP._mean(xs)
        scores[cat] = mean
        if mean < ORACLE_MIN:
            below.append(f"{cat}={mean:.3f}")
    ev = {"scores": scores, "unscored": absent, "floor": ORACLE_MIN,
          "runs": len(runs)}
    if below:
        return check("I5", FAILED,
                     f"oracle below the {ORACLE_MIN} floor: {', '.join(below)}",
                     **ev)
    if not scores:
        return check("I5", NEVER,
                     f"oracle scored no {' and no '.join(absent)} question", **ev)
    got = ", ".join(f"{c}={scores[c]:.3f}" for c in sorted(scores))
    if absent:
        return check("I5", NEVER,
                     f"oracle {got} >= {ORACLE_MIN}, but scored no "
                     f"{' and no '.join(absent)} question", **ev)
    return check("I5", PASSED,
                 f"oracle {got} >= {ORACLE_MIN} over {len(runs)} run(s)", **ev)


# ── I6: the build census ─────────────────────────────────────────────────────

# The field each atom kind carries its DECLARED type in. Checked against the
# installed chaos-secret-agent atlas, 2026-09-19: Entity.entity_type,
# Event.event_type, Relation.relation_type, State.state_type,
# Question.question_type. Summary and Configuration declare no type, which is
# why the census counts `Summary` atoms separately rather than as a type.
DECLARED_TYPE_FIELD = {
    "entity": "entity_type", "event": "event_type", "relation": "relation_type",
    "state": "state_type", "question": "question_type",
}
SUMMARY_ATOM = "Summary"


def index_dirs_from_manifests(arms):
    """`{path: [labels]}` from `provenance.paths.index_dir` across every run."""
    seen = {}
    for arm, runs in arms.items():
        for r in runs:
            prov = (r["manifest"].get("provenance") or {})
            path = (prov.get("paths") or {}).get("index_dir")
            seen.setdefault(path, []).append(CMP._label(arm, r))
    return seen


def census(atlas):
    """`(census, reason)` from an atlas dir — `(None, reason)` when unreadable.

    `atoms.json` and `edges.json` are the atlas's own plain-JSON writes beside
    the lance tables, so the census needs no lance reader. A declared type with
    no atom is counted 0 and shown; a type value the ontology never declared is
    listed apart rather than folded into a total.
    """
    atlas = Path(atlas)
    try:
        ont = json.loads((atlas / "ontology.json").read_text())
    except (OSError, ValueError) as e:
        return None, f"ontology.json did not parse: {type(e).__name__}: {e}"
    types = ((ont.get("policies") or {}).get("shape") or {}).get("types") or []
    declared = [(t.get("name"), (t.get("kind") or "").lower()) for t in types
                if t.get("name")]
    try:
        atoms = json.loads((atlas / "atoms.json").read_text()).get("atoms") or []
    except (OSError, ValueError) as e:
        return None, f"atoms.json did not parse: {type(e).__name__}: {e}"
    try:
        edges = json.loads((atlas / "edges.json").read_text()).get("edges") or []
    except (OSError, ValueError) as e:
        return None, f"edges.json did not parse: {type(e).__name__}: {e}"

    per_type = {name: 0 for name, _k in declared}
    undeclared, summaries = {}, 0
    declared_names = set(per_type)
    for a in atoms:
        kind = (a.get("atom_type") or "")
        if kind == SUMMARY_ATOM:
            summaries += 1
        field = DECLARED_TYPE_FIELD.get(kind.lower())
        if not field:
            continue
        value = (a.get("data") or {}).get(field)
        if value is None:
            continue
        if value in declared_names:
            per_type[value] += 1
        else:
            undeclared[value] = undeclared.get(value, 0) + 1
    per_edge = {}
    for e in edges:
        key = e.get("edge_type")
        key = key if isinstance(key, str) else json.dumps(key, sort_keys=True)
        per_edge[key] = per_edge.get(key, 0) + 1
    return ({"atlas": str(atlas),
             "ontology_version": ont.get("ontology_version"),
             "pipeline_id": ont.get("pipeline_id"),
             "declared_types": [n for n, _k in declared],
             "atoms_per_declared_type": per_type,
             "atoms_per_undeclared_type": undeclared,
             "edges_per_kind": per_edge,
             "summary_atoms": summaries,
             "atoms_total": len(atoms), "edges_total": len(edges)}, None)


def i6_build_census(arms):
    """`ontology.json` and `atoms_ann.lance` present, then the census (I6)."""
    seen = index_dirs_from_manifests(arms)
    known = {p: labels for p, labels in seen.items() if p}
    if not known:
        return check("I6", NEVER,
                     "no manifest records `provenance.paths.index_dir` — the "
                     "build was not located, and it is not guessed")
    if len(known) > 1:
        return check("I6", CNJ,
                     "the runs name more than one index dir: "
                     + CMP._name_the_difference(known),
                     index_dirs=sorted(known))
    index_dir = Path(next(iter(known)))
    atlas = index_dir / "atlas"
    missing = [n for n in ("ontology.json", "atoms_ann.lance")
               if not (atlas / n).exists()]
    if missing:
        return check("I6", FAILED,
                     f"{atlas} is missing {', '.join(missing)}",
                     atlas=str(atlas), missing=missing)
    cen, reason = census(atlas)
    if cen is None:
        return check("I6", CNJ, f"{atlas}: {reason}", atlas=str(atlas))
    per = ", ".join(f"{n}={c}" for n, c in sorted(cen["atoms_per_declared_type"].items()))
    edges = ", ".join(f"{k}={c}" for k, c in sorted(cen["edges_per_kind"].items()))
    return check("I6", PASSED,
                 f"{cen['atoms_total']} atoms ({per or 'no declared type'}), "
                 f"{cen['edges_total']} edges ({edges or 'none'}), "
                 f"{cen['summary_atoms']} Summary", census=cen)


# ── I7: every retrieving arm took a retrieving route ─────────────────────────

def i7_route_census(arms):
    """Per arm x category, the routes the scored rows took that do not retrieve.

    Grounded is `knowledge_query` or `comparison_query`. `compare.py` owns the
    set, the reader and the fold over the route's two spellings (ARCH §8) —
    this check counts, it does not re-derive what counts.

    Closed-book is skipped: a naked turn persists no metadata, so it takes no
    route by construction (`eval_cmd/runner.rs:1740-1745`), and censusing it
    would read every pilot as could-not-judge.

    The exclusions this census motivates live in `compare.study`, never here:
    a census that first dropped the rows it is counting would read `passed` on
    every runs dir ever written.

    A row on a route that does not retrieve is a definite violation; a row
    with no route at all is an absent measurement. When both are present the
    verdict is `failed` and the reason names the unrouted count too — a known
    violation is not suppressed by someone else's absence (ARCH §5, §6).
    """
    retrieving = CMP.retrieving_arms(arms)
    if not retrieving:
        return check("I7", NEVER,
                     f"no arm but `{CMP.CLOSED_BOOK_ARM}` under the runs dir")
    rows = 0
    offenders, bad_qids, unrouted = [], set(), []
    other = {}   # arm -> category -> route -> count
    for arm, runs in sorted(retrieving.items()):
        for r in runs:
            for row in CMP.rows_of(r):
                if not CMP.measured(row):
                    continue
                rows += 1
                qid = row.get("question_id")
                route = CMP.route_of(row)
                if route is None:
                    unrouted.append(f"{CMP._label(arm, r)}/{qid}")
                    continue
                if CMP.is_grounded_route(route):
                    continue
                offenders.append(f"{CMP._label(arm, r)}/{qid}={route}")
                bad_qids.add(qid)
                bucket = other.setdefault(arm, {}).setdefault(row.get("category"), {})
                bucket[route] = bucket.get(route, 0) + 1
    ev = {"scored_rows": rows, "arms": sorted(retrieving),
          "grounded_routes": list(CMP.GROUNDED_ROUTES),
          "other_routes": other, "offenders": offenders,
          "question_ids": sorted(bad_qids), "unrouted": unrouted}
    if not rows:
        return check("I7", NEVER,
                     f"no scored row in {len(retrieving)} retrieving arm(s): "
                     f"{', '.join(sorted(retrieving))}", **ev)
    absent = (f"; {len(unrouted)} row(s) carried no route at all "
              f"({', '.join(unrouted[:4])})" if unrouted else "")
    if offenders:
        return check("I7", FAILED,
                     f"{len(bad_qids)} question(s) off a retrieving route: "
                     f"{', '.join(sorted(bad_qids)[:6])} — {_census_line(other)}"
                     + absent, **ev)
    if unrouted:
        return check("I7", CNJ,
                     f"{rows - len(unrouted)}/{rows} scored row(s) grounded, "
                     f"but {len(unrouted)} carried no route at all "
                     f"({', '.join(unrouted[:4])}) — the census cannot judge "
                     f"a route nobody recorded", **ev)
    return check("I7", PASSED,
                 f"{rows} scored row(s) over {len(retrieving)} retrieving "
                 f"arm(s) ({', '.join(sorted(retrieving))}), every one on "
                 f"{' or '.join(CMP.GROUNDED_ROUTES)}", **ev)


def _census_line(other):
    """`arm/category: route=n, route=n` for every route that does not retrieve."""
    parts = []
    for arm in sorted(other):
        for cat in sorted(other[arm], key=str):
            counts = ", ".join(f"{r}={n}" for r, n in sorted(other[arm][cat].items()))
            parts.append(f"{arm}/{cat}: {counts}")
    return "; ".join(parts)


# ── running the seven ────────────────────────────────────────────────────────

def run_checks(runs_root):
    """The seven checks over one runs dir, in order. Raises `Refused` on no runs."""
    arms, incomplete = CMP.load_runs(runs_root)
    if not arms:
        raise Refused(f"no complete run under {runs_root} — "
                      f"expected <dir>/<arm>/run-<N>/{{eval,manifest}}.json")
    closed = CMP.closed_book_exclusions(arms)
    excluded = set(closed["excluded"])
    rows = [
        i1_closed_book_retrieved_is_empty(arms),
        i2_every_arm_is_what_it_says(arms),
        i3_ablation_differs_only_in_the_atlas(arms),
        i4_a_band_for_every_category(arms, excluded),
        i5_oracle_scores_its_floor(arms, excluded),
        i6_build_census(arms),
        i7_route_census(arms),
    ]
    meta = {"runs": str(runs_root), "arms": {a: [r["run"] for r in rs]
                                             for a, rs in arms.items()},
            "incomplete": incomplete,
            "closed_book_exclusions": closed["count"]}
    return rows, meta


def print_table(rows, meta, out=sys.stdout, title="checks"):
    arms = ", ".join(f"{a}x{len(rs)}" for a, rs in sorted(meta["arms"].items()))
    print(f"{title}: {meta['runs']}  [{arms or 'no arm'}]", file=out)
    if meta.get("incomplete"):
        print(f"  incomplete: {', '.join(meta['incomplete'])}", file=out)
    width = max(len(r["verdict"]) for r in rows)
    print(f"  {'check':<5} {'verdict':<{width}}  reason", file=out)
    for r in rows:
        print(f"  {r['check']:<5} {r['verdict']:<{width}}  {r['reason']}", file=out)
    tally = {}
    for r in rows:
        tally[r["verdict"]] = tally.get(r["verdict"], 0) + 1
    print("  " + " · ".join(f"{v}: {n}" for v, n in sorted(tally.items())), file=out)


def write_jsonl(out_dir, rows, meta, name="checks.jsonl"):
    d = Path(out_dir)
    d.mkdir(parents=True, exist_ok=True)
    path = d / name
    with path.open("w") as fh:
        for r in rows:
            fh.write(json.dumps({**r, "runs": meta["runs"]}) + "\n")
    return path


# ── plants ───────────────────────────────────────────────────────────────────

def _edit_runs(root, arm, fn):
    """Apply `fn(eval_doc)` to every run of `arm` under `root`."""
    for run_dir in sorted((Path(root) / arm).iterdir()):
        ev = run_dir / "eval.json"
        if not ev.is_file():
            continue
        doc = json.loads(ev.read_text())
        fn(doc)
        ev.write_text(json.dumps(doc))


def _materialise(root, arm, donor_order):
    """Copy the first available donor arm's runs to `<root>/<arm>`.

    Returns a note when a copy happened, `None` when the arm was already there.
    A plant for a check whose arm the caller did not run would otherwise assert
    against never-ran and prove nothing.
    """
    root = Path(root)
    if (root / arm).is_dir():
        return None
    donor = next((d for d in donor_order if (root / d).is_dir()), None)
    if donor is None:
        raise Refused(f"cannot plant: no `{arm}` arm and no donor among "
                      f"{', '.join(donor_order)}")
    shutil.copytree(root / donor, root / arm)
    for run_dir in sorted((root / arm).iterdir()):
        mf = run_dir / "manifest.json"
        if not mf.is_file():
            continue
        doc = json.loads(mf.read_text())
        (doc.setdefault("identity", {}))["arm"] = arm
        mf.write_text(json.dumps(doc))
    return f"materialised `{arm}` from `{donor}`"


def plant_i1(root):
    note = _materialise(root, CMP.CLOSED_BOOK_ARM, (CMP.BARE_ARM, CMP.FULL_ARM))

    def corrupt(doc):
        for row in doc.get("results") or []:
            row["retrieved"] = [{"corpus_id": "c", "title": "leaked",
                                 "score": 1.0, "snippet": "x"}]
            break
    _edit_runs(root, CMP.CLOSED_BOOK_ARM, corrupt)
    return _note("one closed-book row given a non-empty `retrieved[]`", note)


def plant_i2(root):
    note = _materialise(root, CMP.BARE_ARM, (CMP.FULL_ARM, CMP.CLOSED_BOOK_ARM))

    def corrupt(doc):
        for row in doc.get("results") or []:
            row.setdefault("retrieved", []).append(
                {"corpus_id": "c", "title": "t", "score": 1.0, "snippet": "x",
                 "source": "atlas"})
            break
    _edit_runs(root, CMP.BARE_ARM, corrupt)
    return _note("one bare row given an `atlas`-sourced chunk", note)


def plant_i3(root):
    """The ablation built on full's own atlas — not an ablation."""
    root = Path(root)
    if not (root / CMP.FULL_ARM).is_dir():
        raise Refused(f"cannot plant I3: no `{CMP.FULL_ARM}` arm to copy")
    if (root / CMP.ABLATION_ARM).is_dir():
        shutil.rmtree(root / CMP.ABLATION_ARM)
    shutil.copytree(root / CMP.FULL_ARM, root / CMP.ABLATION_ARM)
    for run_dir in sorted((root / CMP.ABLATION_ARM).iterdir()):
        mf = run_dir / "manifest.json"
        if not mf.is_file():
            continue
        doc = json.loads(mf.read_text())
        (doc.setdefault("identity", {}))["arm"] = CMP.ABLATION_ARM
        mf.write_text(json.dumps(doc))
    return "`ablation` copied from `full`, ontology hash and all"


def plant_i4(root):
    """Bare blinded to one category, so that category has no noise band."""
    root = Path(root)
    _materialise(root, CMP.BARE_ARM, (CMP.FULL_ARM, CMP.CLOSED_BOOK_ARM))
    seen = set()
    for run_dir in sorted((root / CMP.BARE_ARM).iterdir()):
        ev = run_dir / "eval.json"
        if ev.is_file():
            for row in json.loads(ev.read_text()).get("results") or []:
                if row.get("category"):
                    seen.add(row["category"])
    if not seen:
        raise Refused(f"cannot plant I4: `{CMP.BARE_ARM}` carries no category")
    target = sorted(seen)[0]
    # The category must survive elsewhere, or it leaves the board entirely and
    # I4 has nothing to miss.
    note = _materialise(root, CMP.FULL_ARM, (CMP.CLOSED_BOOK_ARM,))

    def corrupt(doc):
        doc["results"] = [r for r in doc.get("results") or []
                          if r.get("category") != target]
    _edit_runs(root, CMP.BARE_ARM, corrupt)
    return _note(f"every `{target}` row dropped from `{CMP.BARE_ARM}`", note)


def plant_i5(root):
    """An oracle that cannot answer its own K0."""
    note = _materialise(root, "oracle", (CMP.BARE_ARM, CMP.FULL_ARM,
                                         CMP.CLOSED_BOOK_ARM))

    def corrupt(doc):
        for row in doc.get("results") or []:
            row.pop("error", None)
            row["category"] = ATT.K0
            synth = row.setdefault("synth", {})
            synth.setdefault("judge_fact_score", {})["ratio"] = 0.1
            break
    _edit_runs(root, "oracle", corrupt)
    return _note(f"one oracle row scored 0.1 on {ATT.K0}", note)


def plant_i6(root):
    """An atlas with no `ontology.json`.

    The plant builds its OWN atlas inside the copy and repoints the manifests
    at it. The real index dir is never touched, and the plant therefore proves
    the check on a runs dir whose manifests name no index at all.
    """
    root = Path(root)
    atlas = root / "planted-index" / "atlas"
    atlas.mkdir(parents=True, exist_ok=True)
    (atlas / "atoms_ann.lance").mkdir(exist_ok=True)
    (atlas / "atoms.json").write_text(json.dumps({"atoms": []}))
    (atlas / "edges.json").write_text(json.dumps({"edges": []}))
    touched = 0
    for arm_dir in sorted(p for p in root.iterdir() if p.is_dir()):
        for run_dir in sorted(arm_dir.iterdir()):
            mf = run_dir / "manifest.json"
            if not mf.is_file():
                continue
            doc = json.loads(mf.read_text())
            prov = doc.setdefault("provenance", {})
            prov.setdefault("paths", {})["index_dir"] = str(atlas.parent)
            mf.write_text(json.dumps(doc))
            touched += 1
    if not touched:
        raise Refused("cannot plant I6: no manifest under the runs dir")
    return (f"{touched} manifest(s) repointed at an atlas with "
            f"`atoms_ann.lance` but no `ontology.json`")


def plant_i7(root):
    """One retrieving row rewritten onto a route that retrieves nothing.

    Written in the snake_case wire spelling while the fixture carries the
    PascalCase one, so the plant also proves the fold does not let a wrong
    route through on a spelling mismatch.
    """
    note = _materialise(root, CMP.FULL_ARM,
                        (CMP.BARE_ARM, "deep", CMP.ABLATION_ARM))

    def corrupt(doc):
        for row in doc.get("results") or []:
            row.setdefault("synth", {})["intent"] = "generative_query"
            break
    _edit_runs(root, CMP.FULL_ARM, corrupt)
    return _note(f"one `{CMP.FULL_ARM}` row's route rewritten to "
                 f"`generative_query`", note)


def _note(what, materialised):
    return f"{what} ({materialised})" if materialised else what


PLANTS = {"I1": plant_i1, "I2": plant_i2, "I3": plant_i3,
          "I4": plant_i4, "I5": plant_i5, "I6": plant_i6, "I7": plant_i7}


def plant_one(runs_root, cid, workdir):
    """Copy, check clean, corrupt, check again. One row, `before` and `after`.

    A plant is only evidence when the check was NOT already red: an
    already-failing check would read `failed` whatever the plant did, so that
    is `could-not-judge` (ARCH §7).
    """
    dest = Path(workdir) / cid
    shutil.copytree(runs_root, dest)
    before = next(r for r in run_checks(dest)[0] if r["check"] == cid)
    if before["verdict"] == FAILED:
        return {"check": cid, "verdict": CNJ, "plant": None,
                "before": FAILED, "after": None,
                "reason": f"already failed before the plant — {before['reason']}"}
    try:
        what = PLANTS[cid](dest)
    except Refused as e:
        return {"check": cid, "verdict": CNJ, "plant": None,
                "before": before["verdict"], "after": None, "reason": str(e)}
    after = next(r for r in run_checks(dest)[0] if r["check"] == cid)
    caught = after["verdict"] == FAILED
    return {"check": cid, "verdict": FAILED if caught else after["verdict"],
            "plant": what, "before": before["verdict"], "after": after["verdict"],
            "reason": (f"{what} -> {after['verdict']}: {after['reason']}")}


def run_plants(runs_root, ids):
    with tempfile.TemporaryDirectory(prefix="ei7-plant-") as work:
        return [plant_one(runs_root, cid, work) for cid in ids]


# ── self-test ────────────────────────────────────────────────────────────────

def self_test():
    """The failing inputs this script is named for.

    Each case carries a PLANT: an input the obvious-but-wrong implementation
    gets wrong. The verdicts are the four the queue names; a case that could
    not be set up reads `could-not-judge`, never `passed`.
    """
    results = []

    def case(name, fn):
        try:
            ok, detail = fn()
        except Exception as e:                     # noqa: BLE001 — a broken fixture
            results.append((name, CNJ, f"{type(e).__name__}: {e}"))
            return
        results.append((name, PASSED if ok else FAILED, detail))

    IDENT = {"corpus": "c", "host": "http://localhost:9741",
             "synth_model": "m-synth", "judge_model": "m-judge",
             "recipe_sha256": "r" * 64, "ontology_sha256": "o" * 64,
             "chunks_listing_sha256": "k" * 64}

    def row(qid, category, judge, retrieved=None, walk=None, nav=None,
            intent="KnowledgeQuery"):
        """`intent=None` writes NO `synth.intent` key — a naked turn's shape."""
        r = {"question_id": qid, "category": category, "question": qid,
             "retrieved": retrieved if retrieved is not None else [],
             "fact_score": {"matched": [], "missing": [], "total_expected": 1,
                            "ratio": judge / 2},
             "synth": {"answer": "a",
                       "judge_fact_score": {"matched": [], "missing": [],
                                            "total_expected": 1, "ratio": judge},
                       "judge_evidence": [{"fact": "f1", "present": True}]}}
        if intent is not None:
            r["synth"]["intent"] = intent
        if walk is not None:
            r["atlas_walk"] = walk
        if nav is not None:
            r["atlas_navigation"] = nav
        return r

    def fixture(root, arm, run, rows, ident=None, index_dir=None):
        d = Path(root) / arm / f"run-{run}"
        d.mkdir(parents=True, exist_ok=True)
        (d / "eval.json").write_text(json.dumps(
            {"bank_name": "t", "corpus": "c", "limit": 0, "started_at_unix": 0,
             "results": rows}))
        mf = {"schema": "ei7-arm-manifest/v1",
              "identity": {"arm": arm, **IDENT, **(ident or {})},
              "run": run, "verdict": None, "never_ran_reasons": []}
        if index_dir is not None:
            mf["provenance"] = {"paths": {"index_dir": str(index_dir)}}
        (d / "manifest.json").write_text(json.dumps(mf))

    WALK = {"kind": "k", "nodes": [{"atom_id": "a1", "name": "A", "subtype": "person"}],
            "seeds": 1, "edges_followed": 1, "nodes_reached": 1, "requests": 1,
            "summaries_appended": 1, "added": 1, "considered": 1}
    QIDS = [f"q{i:02d}" for i in range(6)]

    def atlas_at(root, with_ontology=True, with_ann=True):
        """A minimal but REAL atlas dir, in the shapes the writers use."""
        atlas = Path(root) / "atlas"
        atlas.mkdir(parents=True, exist_ok=True)
        if with_ann:
            (atlas / "atoms_ann.lance").mkdir(exist_ok=True)
        if with_ontology:
            (atlas / "ontology.json").write_text(json.dumps(
                {"schema_version": "1.0", "ontology_version": 1,
                 "pipeline_id": "custom_atlas",
                 "policies": {"shape": {"types": [
                     {"name": "person", "kind": "entity"},
                     {"name": "place", "kind": "entity"},
                     {"name": "ghost", "kind": "entity"}]}}}))
        (atlas / "atoms.json").write_text(json.dumps({"atoms": [
            {"atom_type": "Entity", "data": {"id": "e1", "entity_type": "person"}},
            {"atom_type": "Entity", "data": {"id": "e2", "entity_type": "person"}},
            {"atom_type": "Entity", "data": {"id": "e3", "entity_type": "place"}},
            {"atom_type": "State", "data": {"id": "s1", "state_type": "unclassified"}},
            {"atom_type": "Summary", "data": {"id": "sm1", "level": 1}},
            {"atom_type": "Summary", "data": {"id": "sm2", "level": 2}},
        ]}))
        (atlas / "edges.json").write_text(json.dumps({"edges": [
            {"id": "x1", "edge_type": "Involves"},
            {"id": "x2", "edge_type": "Involves"},
            {"id": "x3", "edge_type": "Grounds"}]}))
        return Path(root)

    def clean(root):
        """closed-book, bare, full — the arms the pilot can build, one run each."""
        index = atlas_at(Path(root).parent / "index")
        cats = [ATT.K0, ATT.K0, ATT.K2, ATT.K2, ATT.K4, ATT.K4]
        # Closed-book is naked and records no route (`runner.rs:1740-1745`);
        # every other arm went through the router. The fixture says so rather
        # than leaving I7's skip resting on a claim.
        fixture(root, CMP.CLOSED_BOOK_ARM, 1,
                [row(q, c, 0.1, intent=None) for q, c in zip(QIDS, cats)],
                index_dir=index)
        fixture(root, CMP.BARE_ARM, 1,
                [row(q, c, 0.4) for q, c in zip(QIDS, cats)], index_dir=index)
        fixture(root, CMP.FULL_ARM, 1,
                [row(q, c, 0.7, retrieved=[{"corpus_id": "c", "score": 1.0,
                                            "snippet": "s", "source": "atlas"}],
                     walk=WALK) for q, c in zip(QIDS, cats)], index_dir=index)
        return root

    def tmp():
        return tempfile.TemporaryDirectory(prefix="ei7-checks-selftest-")

    def by_id(rows):
        return {r["check"]: r for r in rows}

    # ── the clean set ────────────────────────────────────────────────────────

    def clean_set_reads_five_passed_and_two_never_ran():
        """I3 and I5 have no arm here. `never-ran` is NOT a pass, and is said."""
        with tmp() as t:
            rows, meta = run_checks(clean(Path(t) / "runs"))
            got = by_id(rows)
            ok = (len(rows) == 7
                  and [r["check"] for r in rows] == list(CHECK_IDS)
                  and got["I1"]["verdict"] == PASSED
                  and got["I2"]["verdict"] == PASSED
                  and got["I3"]["verdict"] == NEVER
                  and got["I4"]["verdict"] == PASSED
                  and got["I5"]["verdict"] == NEVER
                  and got["I6"]["verdict"] == PASSED
                  and got["I7"]["verdict"] == PASSED
                  and meta["closed_book_exclusions"] == 0
                  and all(r["reason"] for r in rows))
            return ok, " ".join(f"{c}={got[c]['verdict']}" for c in CHECK_IDS)

    def every_check_writes_one_jsonl_line():
        with tmp() as t:
            rows, meta = run_checks(clean(Path(t) / "runs"))
            path = write_jsonl(Path(t) / "out", rows, meta)
            lines = [json.loads(x) for x in path.read_text().splitlines()]
            ok = (len(lines) == 7
                  and [x["check"] for x in lines] == list(CHECK_IDS)
                  and all(x["verdict"] in (PASSED, FAILED, CNJ, NEVER) for x in lines))
            return ok, f"{len(lines)} line(s) in {path.name}"

    def an_empty_runs_dir_refuses_rather_than_passing_seven():
        """PLANT: nothing to check. Seven vacuous `passed` rows would be the
        cheapest possible green, and it is the one this must not print."""
        with tmp() as t:
            empty = Path(t) / "nothing"
            empty.mkdir()
            try:
                run_checks(empty)
                return False, "run_checks returned rows for an empty dir"
            except Refused as e:
                return True, f"Refused: {str(e)[:48]}"

    # ── per-check plants, each against the clean set ─────────────────────────

    def each_plant_flips_its_own_check():
        with tmp() as t:
            root = clean(Path(t) / "runs")
            rows = run_plants(root, CHECK_IDS)
            got = by_id(rows)
            ok = all(r["verdict"] == FAILED for r in rows)
            return ok, " ".join(f"{c}:{got[c]['before']}->{got[c]['after']}"
                                for c in CHECK_IDS)

    def a_plant_leaves_the_real_runs_dir_untouched():
        """PLANT: a plant that corrupted `--runs` itself would pass its own
        assertion and destroy the evidence it was run on."""
        with tmp() as t:
            root = clean(Path(t) / "runs")
            before = {p.relative_to(root): p.read_bytes()
                      for p in sorted(root.rglob("*")) if p.is_file()}
            run_plants(root, CHECK_IDS)
            after = {p.relative_to(root): p.read_bytes()
                     for p in sorted(root.rglob("*")) if p.is_file()}
            ok = before == after
            return ok, (f"{len(before)} file(s) before, {len(after)} after, "
                        f"identical={ok}")

    def a_plant_on_an_already_red_check_is_could_not_judge():
        """PLANT: I1 already failing. Asserting `failed` after the plant would
        be true and would prove nothing about the plant."""
        with tmp() as t:
            root = clean(Path(t) / "runs")
            _edit_runs(root, CMP.CLOSED_BOOK_ARM, lambda d: d["results"][0].update(
                {"retrieved": [{"corpus_id": "c", "score": 1.0, "snippet": "x"}]}))
            with tempfile.TemporaryDirectory() as work:
                got = plant_one(root, "I1", work)
            ok = got["verdict"] == CNJ and got["before"] == FAILED
            return ok, f"{got['verdict']} before={got['before']}"

    # ── the checks' own discriminations ──────────────────────────────────────

    def i2_reads_a_quiet_full_arm_as_never_ran_not_failed():
        """PLANT: full's walk never fires. The row's rule is that the ARM is
        never-ran — reading this as `failed` would blame the check."""
        with tmp() as t:
            root = clean(Path(t) / "runs")
            _edit_runs(root, CMP.FULL_ARM, lambda d: [r.pop("atlas_walk", None)
                                                      for r in d["results"]])
            got = by_id(run_checks(root)[0])["I2"]
            ok = got["verdict"] == NEVER and got["full"]["verdict"] == NEVER
            return ok, f"{got['verdict']} — {got['reason'][:60]}"

    def i2_reads_an_unsummarised_k4_as_never_ran():
        """PLANT: the walk fires everywhere but appends no K4 summary. Half the
        rule met is not the rule met."""
        with tmp() as t:
            root = clean(Path(t) / "runs")

            def drop_summaries(doc):
                for r in doc["results"]:
                    if r.get("category") == ATT.K4:
                        r["atlas_walk"] = {**WALK, "summaries_appended": 0}
            _edit_runs(root, CMP.FULL_ARM, drop_summaries)
            got = by_id(run_checks(root)[0])["I2"]
            ok = (got["verdict"] == NEVER
                  and got["full"]["k4_summaries"] == 0
                  and got["full"]["walk_fired_share"] == 1.0)
            return ok, f"{got['verdict']} — {got['reason'][:60]}"

    def i2_counts_an_atlas_navigation_entry_on_bare():
        """PLANT: the structural hit is in `atlas_navigation`, not `retrieved`.
        A check reading only `retrieved` calls this bare arm clean."""
        with tmp() as t:
            root = clean(Path(t) / "runs")
            _edit_runs(root, CMP.BARE_ARM, lambda d: d["results"][0].update(
                {"atlas_navigation": [{"corpus_id": "c", "score": 1.0,
                                       "snippet": "s", "source": "atlas"}]}))
            got = by_id(run_checks(root)[0])["I2"]
            ok = (got["verdict"] == FAILED
                  and "atlas_navigation.atlas" in " ".join(got["bare"]["bare_offenders"]))
            return ok, f"{got['verdict']} — {got['reason'][:60]}"

    def i3_reads_a_model_mismatch_as_could_not_judge():
        """PLANT: the arms differ in judge model. That is not I3's claim being
        false — it is I3 being unjudgeable, and the two must not collapse."""
        with tmp() as t:
            root = clean(Path(t) / "runs")
            fixture(root, CMP.ABLATION_ARM, 1,
                    [row(q, ATT.K0, 0.5) for q in QIDS],
                    ident={"ontology_sha256": "a" * 64, "judge_model": "other"})
            got = by_id(run_checks(root)[0])["I3"]
            ok = got["verdict"] == CNJ and got["field"] == "judge_model"
            return ok, f"{got['verdict']} field={got.get('field')}"

    def i3_passes_a_real_ablation():
        with tmp() as t:
            root = clean(Path(t) / "runs")
            fixture(root, CMP.ABLATION_ARM, 1,
                    [row(q, ATT.K0, 0.5) for q in QIDS],
                    ident={"ontology_sha256": "a" * 64})
            got = by_id(run_checks(root)[0])["I3"]
            return got["verdict"] == PASSED, f"{got['verdict']} — {got['reason'][:60]}"

    def i4_floors_a_single_run_band_rather_than_reading_zero():
        """One run's spread is 0.0. A band of 0.0 would call every delta real."""
        with tmp() as t:
            got = by_id(run_checks(clean(Path(t) / "runs"))[0])["I4"]
            ok = (got["verdict"] == PASSED
                  and set(got["floored"]) == set(got["categories"])
                  and all(b == CMP.BAND_FLOOR for b in got["bands"].values()))
            return ok, f"{got['verdict']} bands={got['bands']}"

    def i5_reads_a_partial_oracle_as_never_ran_not_passed():
        """PLANT: the oracle clears 0.8 on K0 and never saw a K2. Averaging the
        one it has would print a pass for a floor half of which nobody measured."""
        with tmp() as t:
            root = clean(Path(t) / "runs")
            fixture(root, "oracle", 1, [row(q, ATT.K0, 0.95) for q in QIDS])
            got = by_id(run_checks(root)[0])["I5"]
            ok = (got["verdict"] == NEVER and got["unscored"] == [ATT.K2]
                  and got["scores"][ATT.K0] > ORACLE_MIN)
            return ok, f"{got['verdict']} — {got['reason'][:60]}"

    def i5_passes_an_oracle_that_clears_both():
        with tmp() as t:
            root = clean(Path(t) / "runs")
            fixture(root, "oracle", 1,
                    [row(q, c, 0.9) for q, c in zip(QIDS, [ATT.K0, ATT.K0, ATT.K2,
                                                           ATT.K2, ATT.K4, ATT.K4])])
            got = by_id(run_checks(root)[0])["I5"]
            return got["verdict"] == PASSED, f"{got['verdict']} {got['scores']}"

    def i6_counts_a_declared_type_with_no_atom_as_zero():
        """PLANT: `ghost` is declared and empty. A census keyed off the atoms
        would omit the row entirely, and an absent type reads as no opinion."""
        with tmp() as t:
            got = by_id(run_checks(clean(Path(t) / "runs"))[0])["I6"]
            cen = got.get("census") or {}
            per = cen.get("atoms_per_declared_type") or {}
            ok = (got["verdict"] == PASSED and per == {"person": 2, "place": 1, "ghost": 0}
                  and cen["summary_atoms"] == 2
                  and cen["edges_per_kind"] == {"Involves": 2, "Grounds": 1}
                  and cen["atoms_per_undeclared_type"] == {"unclassified": 1})
            return ok, f"{got['verdict']} per_type={per} summaries={cen.get('summary_atoms')}"

    def i6_is_never_ran_when_no_manifest_names_an_index():
        """PLANT: manifests with no `provenance`. Guessing `~/.svrnmesh/...`
        would census SOME build and report it as this run's."""
        with tmp() as t:
            root = Path(t) / "runs"
            fixture(root, CMP.BARE_ARM, 1, [row(q, ATT.K0, 0.4) for q in QIDS])
            got = by_id(run_checks(root)[0])["I6"]
            ok = got["verdict"] == NEVER and "not guessed" in got["reason"]
            return ok, f"{got['verdict']} — {got['reason'][:60]}"

    def i6_refuses_two_index_dirs_rather_than_picking_one():
        with tmp() as t:
            root = clean(Path(t) / "runs")
            other = atlas_at(Path(t) / "index2")
            _relabel = json.loads((root / CMP.BARE_ARM / "run-1" / "manifest.json").read_text())
            _relabel["provenance"] = {"paths": {"index_dir": str(other)}}
            (root / CMP.BARE_ARM / "run-1" / "manifest.json").write_text(json.dumps(_relabel))
            got = by_id(run_checks(root)[0])["I6"]
            ok = got["verdict"] == CNJ and len(got["index_dirs"]) == 2
            return ok, f"{got['verdict']} — {got['reason'][:60]}"

    def i7_censuses_only_the_retrieving_arms():
        """PLANT: closed-book's six rows carry no route, by construction. A
        census that read them would print could-not-judge on every pilot ever
        run and never once name a real routing fault."""
        with tmp() as t:
            got = by_id(run_checks(clean(Path(t) / "runs"))[0])["I7"]
            ok = (got["verdict"] == PASSED
                  and got["scored_rows"] == 12
                  and CMP.CLOSED_BOOK_ARM not in got["arms"]
                  and got["unrouted"] == [])
            return ok, f"{got['verdict']} rows={got['scored_rows']} arms={got['arms']}"

    def i7_fails_and_names_the_question_on_a_generative_route():
        """A route that retrieves nothing, on an arm whose whole claim is that
        it retrieved."""
        with tmp() as t:
            root = clean(Path(t) / "runs")
            _edit_runs(root, CMP.FULL_ARM, lambda d: d["results"][0]["synth"].update(
                {"intent": "GenerativeQuery"}))
            got = by_id(run_checks(root)[0])["I7"]
            ok = (got["verdict"] == FAILED
                  and got["question_ids"] == [QIDS[0]]
                  and got["other_routes"][CMP.FULL_ARM][ATT.K0] == {"GenerativeQuery": 1}
                  and ATT.K0 in got["reason"])
            return ok, f"{got['verdict']} — {got['reason'][:72]}"

    def i7_reads_a_routeless_row_as_could_not_judge():
        """PLANT: a transcript banked before the handlers stamped the key.
        Reading an absent field as "not grounded" blames the router for a row
        nobody recorded a route on (ARCH §6)."""
        with tmp() as t:
            root = clean(Path(t) / "runs")
            _edit_runs(root, CMP.FULL_ARM, lambda d: [r["synth"].pop("intent", None)
                                                      for r in d["results"]])
            got = by_id(run_checks(root)[0])["I7"]
            ok = (got["verdict"] == CNJ and len(got["unrouted"]) == 6
                  and got["question_ids"] == [])
            return ok, f"{got['verdict']} — {got['reason'][:72]}"

    def i7_folds_the_two_spellings_of_one_route():
        """PLANT: the wire slug `comparison_query` where the handlers stamp
        `ComparisonQuery`. Matching either spelling literally calls six rows
        that retrieved ungrounded."""
        with tmp() as t:
            root = clean(Path(t) / "runs")
            _edit_runs(root, CMP.FULL_ARM, lambda d: [r["synth"].update(
                {"intent": "comparison_query"}) for r in d["results"]])
            got = by_id(run_checks(root)[0])["I7"]
            ok = got["verdict"] == PASSED and got["scored_rows"] == 12
            return ok, f"{got['verdict']} rows={got['scored_rows']}"

    def i7_prefers_failed_over_could_not_judge_when_both():
        """PLANT: one row off-route and another with no route. Reading the
        absence first would hide a violation the census already proved."""
        with tmp() as t:
            root = clean(Path(t) / "runs")

            def corrupt(doc):
                doc["results"][0]["synth"]["intent"] = "GenerativeQuery"
                doc["results"][1]["synth"].pop("intent", None)
            _edit_runs(root, CMP.FULL_ARM, corrupt)
            got = by_id(run_checks(root)[0])["I7"]
            ok = (got["verdict"] == FAILED and got["question_ids"] == [QIDS[0]]
                  and len(got["unrouted"]) == 1
                  and "carried no route at all" in got["reason"])
            return ok, f"{got['verdict']} — {got['reason'][:72]}"

    case("clean-set-five-passed-two-never-ran", clean_set_reads_five_passed_and_two_never_ran)
    case("jsonl-one-line-per-check", every_check_writes_one_jsonl_line)
    case("empty-runs-dir-refuses", an_empty_runs_dir_refuses_rather_than_passing_seven)
    case("every-plant-flips-its-check", each_plant_flips_its_own_check)
    case("plant-never-touches-the-runs-dir", a_plant_leaves_the_real_runs_dir_untouched)
    case("plant-on-a-red-check-is-cnj", a_plant_on_an_already_red_check_is_could_not_judge)
    case("i2-quiet-full-is-never-ran", i2_reads_a_quiet_full_arm_as_never_ran_not_failed)
    case("i2-unsummarised-k4-is-never-ran", i2_reads_an_unsummarised_k4_as_never_ran)
    case("i2-atlas-navigation-counts", i2_counts_an_atlas_navigation_entry_on_bare)
    case("i3-model-mismatch-is-cnj", i3_reads_a_model_mismatch_as_could_not_judge)
    case("i3-real-ablation-passes", i3_passes_a_real_ablation)
    case("i4-single-run-band-is-floored", i4_floors_a_single_run_band_rather_than_reading_zero)
    case("i5-partial-oracle-is-never-ran", i5_reads_a_partial_oracle_as_never_ran_not_passed)
    case("i5-full-oracle-passes", i5_passes_an_oracle_that_clears_both)
    case("i6-empty-declared-type-is-zero", i6_counts_a_declared_type_with_no_atom_as_zero)
    case("i6-no-index-dir-is-never-ran", i6_is_never_ran_when_no_manifest_names_an_index)
    case("i6-two-index-dirs-refuse", i6_refuses_two_index_dirs_rather_than_picking_one)
    case("i7-censuses-retrieving-arms-only", i7_censuses_only_the_retrieving_arms)
    case("i7-generative-route-fails", i7_fails_and_names_the_question_on_a_generative_route)
    case("i7-routeless-row-is-cnj", i7_reads_a_routeless_row_as_could_not_judge)
    case("i7-two-spellings-one-route", i7_folds_the_two_spellings_of_one_route)
    case("i7-failed-beats-cnj", i7_prefers_failed_over_could_not_judge_when_both)

    width = max(len(n) for n, _v, _d in results)
    for name, verdict, detail in results:
        print(f"  {name:<{width}}  {verdict:<16}  {detail}")
    bad = [n for n, v, _d in results if v != PASSED]
    print(f"self-test: {len(results) - len(bad)}/{len(results)} passed"
          + (f" — not passed: {', '.join(bad)}" if bad else ""))
    return 1 if bad else 0


# ── entry point ──────────────────────────────────────────────────────────────

def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--runs", help="runs root: <dir>/<arm>/run-<N>/{eval,manifest}.json")
    ap.add_argument("--out", default=None,
                    help="where checks.jsonl lands (default: the runs dir)")
    ap.add_argument("--plant", default=None,
                    help=f"corrupt a COPY and assert the check goes failed: "
                         f"{'|'.join(CHECK_IDS)}|all")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()

    if args.self_test:
        return self_test()
    if args.runs is None:
        ap.error("missing required argument: --runs")
    runs_root = os.path.expanduser(args.runs)
    out_dir = os.path.expanduser(args.out) if args.out else runs_root

    try:
        if args.plant:
            wanted = (list(CHECK_IDS) if args.plant == "all"
                      else [args.plant.upper()])
            unknown = [c for c in wanted if c not in PLANTS]
            if unknown:
                raise Refused(f"no plant for {', '.join(unknown)} — "
                              f"expected {'|'.join(CHECK_IDS)}|all")
            rows = run_plants(runs_root, wanted)
            meta = {"runs": runs_root, "arms": CMP.load_runs(runs_root)[0],
                    "incomplete": []}
            meta["arms"] = {a: [r["run"] for r in rs] for a, rs in meta["arms"].items()}
            print_table(rows, meta, title="checks --plant")
            write_jsonl(out_dir, rows, meta, name="checks-planted.jsonl")
            unflipped = [r["check"] for r in rows if r["verdict"] != FAILED]
            if unflipped:
                print(f"  PLANT NOT CAUGHT: {', '.join(unflipped)} — the "
                      f"enforcement does not enforce", file=sys.stderr)
                return 1
            return 0

        rows, meta = run_checks(runs_root)
        print_table(rows, meta)
        path = write_jsonl(out_dir, rows, meta)
        print(f"  -> {path}")
        return 1 if any(r["verdict"] == FAILED for r in rows) else 0
    except Refused as e:
        print(f"checks: {e}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
