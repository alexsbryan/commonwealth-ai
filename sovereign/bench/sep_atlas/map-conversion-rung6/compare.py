#!/usr/bin/env python3
"""rung 6 verdict, and the EI7 stage-0 study scoreboard. One scorer, two calls.

    compare.py [arm ...]                        # rung 6, exactly as before
    compare.py study --runs <dir> --out <dir>   # the EI7 scoreboard
    compare.py --self-test

**rung 6** (default: armA armB; e.g. `compare.py armA armB armB2`; missing arms
are skipped). Columns: judge = synth.judge_fact_score.ratio (the headline);
kw = fact_score.ratio (keyword facts in the answer); rows = the walk row that
fired per question, read from <arm>.log in question order ("walking the map
... row=<kind> (<source>...)"). `README.md:63` calls this form.

**study** reads the runs `research/ontology-retrieval/harness/run_arm.py`
writes — `<dir>/<arm>/run-<N>/{eval.json,manifest.json}` — and emits
`scoreboard.json` plus a fixed-width category x arm table, and
`sidebyside.{json,md}`: one block per question, three columns (bare, ablation,
full), each carrying gold members found/missed, fabricated members
(`--vocabulary <truth.json>`; without one the metric reads never-ran),
numbered citations and the walk's evidence path. `--recipe <recipe.toml>` with
`[corpus].query_sharing = false` withholds the snippets and says so. It REFUSES arms that
are not comparable (PRE-REG-custom-ontology-and-raptor-2026-09-17, "Corpora and
held-out truth" > Freeze / Model and host, and check I3): that refusal is the
verdict `could-not-judge` and exit 4, never a number computed across two
different builds, models or hosts.

Two exclusion rules drop a question from EVERY arm, both reported by name on
the table: closed-book answered it without retrieving, or some retrieving arm
routed it away from the routes in `GROUNDED_ROUTES` (check I7 censuses the
same set).

No bar is read here. The per-category verdict answers judgeability only
(pre-reg "Bars": a kind under n = 20 after those exclusions is
could-not-judge); bars 1-5 are the operator's at ratification.
"""
import argparse, json, os, re, sys, time
from pathlib import Path

HERE = os.path.dirname(os.path.abspath(__file__))
BASE = os.path.join(HERE, '../../sep/baselines/questions-synth/2026-07-06.json')
NAMED = ['argument_consequence_against_compatibilism', 'dialectical_gettier_lottery',
         'position_summary_kripke_reference_causal_historical', 'comparative_berlin_liberty',
         'argument_aristotle_hylomorphism', 'contested_bioethics_principlism']

def load(p):
    if not os.path.exists(p):
        return None
    d = json.load(open(p))
    return {r['question_id']: r for r in d['results']}

def rows_from_log(p):
    """Question-ordered list of (row, source) — one per deep turn; '-' when the walk did not run."""
    if not os.path.exists(p):
        return []
    out, cur = [], None
    for raw in open(p, errors='replace'):
        line = re.sub(r'\x1b\[[0-9;]*m', '', raw)  # the log is ANSI-coloured
        m = re.search(r'walking the map .*?row=(\w+) \((\w+)', line)
        if m:
            cur = (m.group(1), m.group(2))
        if 'deep_turn_summary' in line:
            out.append(cur or ('-', '-'))
            cur = None
    return out

def ratio(r, *path):
    x = r
    for k in path:
        x = (x or {}).get(k) if isinstance(x, dict) else None
    return None if x is None else float(x)


def rung6(ARMS):
    """The rung 6 table, unchanged. `ARMS` was `sys.argv[1:] or [armA, armB]`."""
    arms = {'jul': load(BASE)}
    for name in ARMS:
        arms[name] = load(os.path.join(HERE, name + '.json'))
    rows = {name: rows_from_log(os.path.join(HERE, name + '.log')) for name in ARMS}
    present = [n for n, a in arms.items() if a]
    order = list(arms['jul'].keys())

    short = lambda n: n.replace('arm', '')
    hdr = f"{'question':<52}" + ''.join(f"{short(n)+'.judge':>10}{short(n)+'.kw':>8}" for n in present) + ''.join(f"{short(n)+'.row':>14}" for n in ARMS)
    print(hdr); print('-' * len(hdr))
    sums = {n: [0.0, 0.0, 0] for n in present}
    for i, q in enumerate(order):
        line = f"{('* ' if q in NAMED else '  ') + q:<52}"
        for n in present:
            r = arms[n].get(q)
            j = ratio(r, 'synth', 'judge_fact_score', 'ratio'); k = ratio(r, 'fact_score', 'ratio')
            line += f"{(f'{j:.2f}' if j is not None else '—'):>10}{(f'{k:.2f}' if k is not None else '—'):>8}"
            if j is not None:
                sums[n][0] += j; sums[n][1] += (k or 0); sums[n][2] += 1
        for n in ARMS:
            rw = rows[n][i] if i < len(rows[n]) else ('—', '')
            line += f"{(rw[0] + ('' if rw[1] in ('', '-') else '/' + rw[1][:4])):>14}"
        print(line)
    print('-' * len(hdr))
    line = f"{'mean (n)':<52}"
    for n in present:
        s = sums[n]
        line += f"{(s[0]/s[2] if s[2] else 0):>10.3f}{(s[1]/s[2] if s[2] else 0):>8.3f}" + ('' if s[2] == 21 else f" n={s[2]}")
    print(line)
    for n in ARMS:
        fired = sum(1 for r in rows[n] if r[0] not in ('-', 'unfiltered'))
        if rows[n]:
            print(f"{n}: rows fired on {fired}/{len(rows[n])} questions walked")
    print("* = the six questions named before the run. Bars: B.judge mean >= 0.916; B >= A on the six.")
    return 0


# ── the EI7 study scoreboard ─────────────────────────────────────────────────

SCHEMA = "ei7-scoreboard/v1"
CLOSED_BOOK_ARM = "closed-book"
BARE_ARM = "bare"
ABLATION_ARM = "ablation"
FULL_ARM = "full"
# Arms in the pre-reg's own order ("Arms — one shape for every corpus"); an arm
# the table does not name sorts after these, it is never dropped.
ARM_ORDER = [CLOSED_BOOK_ARM, BARE_ARM, "deep", ABLATION_ARM, FULL_ARM, "oracle"]

CLOSED_BOOK_KNOWN = 0.5   # pre-reg "Arms": judge >= 0.5 on ALL runs is excluded
BAND_FLOOR = 0.05         # check I4
MIN_N = 20                # pre-reg "Bars": below this the kind is could-not-judge

# The routes that retrieve (check I7). Written once, in the wire spelling.
# `deep_query` and `simple_query` dispatch to `handle_simple`, whose first act
# is `prepare_knowledge_context` (`sovereign-core/src/runtime/handlers/
# simple.rs:24-27`). Until 2026-09-21 this listed the first two only, and the
# pod pilot (`research/ontology-retrieval/pod/20260921T035355Z/`) failed I7 on
# five `DeepQuery` rows that had each retrieved 20-28 chunks. Retrieving is not
# being WALKED: those rows carry no `atlas_walk`, and check I2 is what reads
# that.
# `Intent::name()` stamps `routed_intent` in PascalCase while the intent
# table's `slug` is the same route in snake_case
# (`sovereign-contracts/src/types/routing.rs:305-332`), and the knowledge
# handler's DISPLAY label is the slug — so one route reaches this file under
# two spellings. `route_key` folds them onto one rather than listing each
# route twice (ARCH §8).
GROUNDED_ROUTES = ("knowledge_query", "comparison_query", "deep_query",
                   "simple_query")

# Identity fields every arm of one corpus must agree on. `chunks_listing_sha256`
# is deliberately NOT here: the oracle arm runs against a corpus holding only
# the attesting passages, so its chunks differ by construction. Chunks are
# compared where the pre-reg compares them — the ablation/full pair, check I3.
AGREE = ("recipe_sha256", "synth_model", "judge_model", "host")


class Refusal(Exception):
    """Arms that are not comparable. Verdict could-not-judge, exit 4."""

    def __init__(self, field, detail):
        super().__init__(f"{field}: {detail}")
        self.field = field
        self.detail = detail


def run_index(run_dir):
    """N from `run-N`, or None when the directory is not a run."""
    name = Path(run_dir).name
    if not name.startswith("run-"):
        return None
    try:
        return int(name[4:])
    except ValueError:
        return None


def load_runs(root):
    """`{arm: [{run, dir, eval, manifest}]}`, run-ordered, plus what was skipped.

    A run missing either file is listed in `incomplete` rather than dropped: a
    half-written run is a fact about the corpus, not an absence.
    """
    root = Path(root)
    arms, incomplete = {}, []
    if not root.is_dir():
        return arms, incomplete
    for arm_dir in sorted(p for p in root.iterdir() if p.is_dir()):
        runs = []
        for run_dir in sorted(arm_dir.iterdir()):
            idx = run_index(run_dir)
            if idx is None or not run_dir.is_dir():
                continue
            ev, mf = run_dir / "eval.json", run_dir / "manifest.json"
            if not (ev.is_file() and mf.is_file()):
                incomplete.append(str(run_dir))
                continue
            runs.append({"run": idx, "dir": str(run_dir),
                         "eval": json.loads(ev.read_text()),
                         "manifest": json.loads(mf.read_text())})
        runs.sort(key=lambda r: r["run"])
        if runs:
            arms[arm_dir.name] = runs
        else:
            incomplete.append(str(arm_dir))
    return arms, incomplete


def _label(arm, run):
    return f"{arm}/run-{run['run']}"


def _spread(arms, field, skip=()):
    """`{value: [labels]}` for one identity field across the arms not skipped."""
    seen = {}
    for arm, runs in arms.items():
        if arm in skip:
            continue
        for r in runs:
            ident = r["manifest"].get("identity") or {}
            seen.setdefault(ident.get(field), []).append(_label(arm, r))
    return seen


def _name_the_difference(seen):
    return "; ".join(f"{v!r} in {', '.join(w)}"
                     for v, w in sorted(seen.items(), key=lambda kv: str(kv[0])))


def check_identity(arms):
    """Refuse arms that are not comparable, naming the differing field.

    Returns `(identity, i3)`. The pre-reg's rule, verbatim in two halves:
    every arm shares recipe hash, ontology hash, synth model, judge model and
    host — EXCEPT that ablation and full must differ in ontology hash and must
    match in chunks hash, which is what makes the ablation an ablation (I3).
    """
    identity = {}
    for field in AGREE:
        seen = _spread(arms, field)
        if len(seen) > 1:
            raise Refusal(field, _name_the_difference(seen))
        identity[field] = next(iter(seen))

    # The ablation is BUILT to differ here, so it is held out of the agreement
    # and judged against full below instead.
    seen = _spread(arms, "ontology_sha256", skip=(ABLATION_ARM,))
    if len(seen) > 1:
        raise Refusal("ontology_sha256", _name_the_difference(seen))
    identity["ontology_sha256"] = next(iter(seen)) if seen else None

    i3 = {"verdict": "never-ran", "reason": None}
    missing = [a for a in (ABLATION_ARM, FULL_ARM) if a not in arms]
    if missing:
        i3["reason"] = f"no {' and no '.join(missing)} arm under the runs dir"
        return identity, i3
    for arm in (ABLATION_ARM, FULL_ARM):
        for field in ("ontology_sha256", "chunks_listing_sha256"):
            within = _spread({arm: arms[arm]}, field)
            if len(within) > 1:
                raise Refusal(field, f"{arm}'s own runs differ: "
                                     f"{_name_the_difference(within)}")
    abl = arms[ABLATION_ARM][0]["manifest"].get("identity") or {}
    full = arms[FULL_ARM][0]["manifest"].get("identity") or {}
    if abl.get("ontology_sha256") == full.get("ontology_sha256"):
        raise Refusal("ontology_sha256",
                      f"ablation and full share {abl.get('ontology_sha256')!r} — "
                      f"the ablation is not an ablation")
    if abl.get("chunks_listing_sha256") != full.get("chunks_listing_sha256"):
        raise Refusal("chunks_listing_sha256",
                      f"ablation {abl.get('chunks_listing_sha256')!r} vs full "
                      f"{full.get('chunks_listing_sha256')!r} — the two builds do "
                      f"not hold the same chunks")
    i3.update(verdict="passed",
              reason=f"ablation ontology {str(abl.get('ontology_sha256'))[:8]} != full "
                     f"{str(full.get('ontology_sha256'))[:8]}; chunks match "
                     f"{str(full.get('chunks_listing_sha256'))[:8]}")
    identity["ablation_ontology_sha256"] = abl.get("ontology_sha256")
    return identity, i3


def rows_of(run):
    return (run["eval"] or {}).get("results") or []


def measured(row):
    """False when the run produced no measurement for this question.

    `error` is the runner's own marker for a turn that did not answer — a 503,
    a degraded classifier — and it doc-comments the rule this follows
    (`eval_cmd/runner.rs:58-76`: "Consumers must EXCLUDE these rows from a
    comparison rather than score them"). Scoring one is a 0.0 that looks
    exactly like a model that answered with nothing.
    """
    return not row.get("error")


def judge_ratio(row):
    return ratio(row, "synth", "judge_fact_score", "ratio")


def kw_ratio(row):
    return ratio(row, "fact_score", "ratio")


def judge_members(row):
    """`[(fact, present)]` per gold member, from `synth.judge_evidence`.

    The audit trail, not `judge_fact_score.matched`/`missing`. Both are written
    from the same judge call (`eval_cmd/runner.rs:1879-1889`, `score.rs:251`)
    and on the runs in this directory both are populated — `armA.json`'s first
    row carries 7 names in `matched` — so the pre-reg's reason for preferring
    the trail is not that the rollup is empty. It is that only the trail carries
    `fact`, `present` and the evidence quote TOGETHER, which is what the
    per-question side-by-side renders; reading found/missed off one source here
    and the quote off another is two deciders for one answer (ARCH §8).
    """
    ev = ((row.get("synth") or {}).get("judge_evidence")) or []
    return [(d.get("fact"), bool(d.get("present"))) for d in ev]


def route_key(route):
    """One key per route, whichever of its two spellings was recorded.

    `KnowledgeQuery` and `knowledge_query` are the PascalCase `name` and the
    snake_case `slug` of ONE row in the intent table, so they must not count
    as two routes. Anything that is not a string has no key.
    """
    return route.replace("_", "").lower() if isinstance(route, str) else None


GROUNDED_ROUTE_KEYS = frozenset(route_key(r) for r in GROUNDED_ROUTES)


def route_of(row):
    """The route this row's turn took, or `None` when it recorded none.

    `synth.intent` is the runner's field for it, and it already prefers the
    handler-stamped `routed_intent` over the free-form display label
    (`eval_cmd/runner.rs:202-205`, `eval_cmd/routed_intent.rs`). A naked
    closed-book turn persists no metadata and carries `None` here — absent,
    never a route named "none" (ARCH §6).
    """
    r = (row.get("synth") or {}).get("intent")
    return r if isinstance(r, str) and r.strip() else None


def is_grounded_route(route):
    """True when this route retrieves. `None` is not grounded and not a route."""
    return route_key(route) in GROUNDED_ROUTE_KEYS


def retrieving_arms(arms):
    """Every arm but closed-book, whose naked turns take no route at all."""
    return {a: rs for a, rs in arms.items() if a != CLOSED_BOOK_ARM}


def ungrounded_route_exclusions(arms):
    """Questions that took a non-retrieving route in ANY retrieving arm.

    Modelled on `closed_book_exclusions` above: a question is in or out of the
    whole board, never per-arm — an arm that retrieved compared against an arm
    that answered from the model is not the comparison the pre-reg asks for.

    A row carrying NO route is `unrouted` and is NOT excluded: "recorded no
    route" is not "took a bad one" (ARCH §6), and check I7 is where that
    absence is read as could-not-judge.
    """
    retrieving = retrieving_arms(arms)
    if not retrieving:
        return {"verdict": "never-ran", "count": None, "excluded": [],
                "routes": {}, "unrouted": [], "per_category": {}, "arms": [],
                "reason": f"every arm under the runs dir is `{CLOSED_BOOK_ARM}`"}
    offenders, unrouted, category = {}, set(), {}
    for arm, runs in sorted(retrieving.items()):
        for r in runs:
            for row in rows_of(r):
                if not measured(row):
                    continue
                qid = row.get("question_id")
                category.setdefault(qid, row.get("category"))
                route = route_of(row)
                if route is None:
                    unrouted.add(qid)
                elif not is_grounded_route(route):
                    offenders.setdefault(qid, set()).add(f"{arm}:{route}")
    per_category = {}
    for qid in offenders:
        cat = category.get(qid)
        per_category[cat] = per_category.get(cat, 0) + 1
    return {"verdict": "passed", "count": len(offenders),
            "excluded": sorted(offenders),
            "routes": {q: sorted(v) for q, v in sorted(offenders.items())},
            "unrouted": sorted(unrouted), "per_category": per_category,
            "arms": sorted(retrieving), "reason": None}


def closed_book_exclusions(arms):
    """Questions closed-book answered (judge >= 0.5) on EVERY run.

    A question one run did not measure is `unmeasured`, never a 0.0 that quietly
    keeps it in the bank: "did not answer" is not "answered: no" (ARCH §6).
    With no closed-book arm the count is never-ran, not zero.
    """
    runs = arms.get(CLOSED_BOOK_ARM)
    if not runs:
        return {"verdict": "never-ran", "count": None, "excluded": [],
                "unmeasured": [], "runs": 0,
                "reason": f"no `{CLOSED_BOOK_ARM}` arm under the runs dir"}
    scores, seen = {}, set()
    for r in runs:
        for row in rows_of(r):
            qid = row.get("question_id")
            seen.add(qid)
            j = judge_ratio(row) if measured(row) else None
            scores.setdefault(qid, []).append(j)
    excluded, unmeasured = [], []
    for qid in sorted(seen):
        got = scores.get(qid, [])
        if len(got) < len(runs) or any(j is None for j in got):
            unmeasured.append(qid)
        elif all(j >= CLOSED_BOOK_KNOWN for j in got):
            excluded.append(qid)
    return {"verdict": "passed", "count": len(excluded), "excluded": excluded,
            "unmeasured": unmeasured, "runs": len(runs), "reason": None}


def _mean(xs):
    return sum(xs) / len(xs) if xs else None


def per_run_category_means(runs, excluded):
    """`{category: [per-run mean judge]}` and the same for the keyword view."""
    judge, kw, found, missed, dropped = {}, {}, {}, {}, 0
    for r in runs:
        j_run, k_run = {}, {}
        for row in rows_of(r):
            if row.get("question_id") in excluded:
                continue
            cat = row.get("category")
            if not measured(row):
                dropped += 1
                continue
            j, k = judge_ratio(row), kw_ratio(row)
            if j is not None:
                j_run.setdefault(cat, []).append(j)
            if k is not None:
                k_run.setdefault(cat, []).append(k)
            for _fact, present in judge_members(row):
                (found if present else missed).setdefault(cat, 0)
                if present:
                    found[cat] += 1
                else:
                    missed[cat] += 1
        for cat, xs in j_run.items():
            judge.setdefault(cat, []).append(_mean(xs))
        for cat, xs in k_run.items():
            kw.setdefault(cat, []).append(_mean(xs))
    return judge, kw, found, missed, dropped


def noise_bands(arms, excluded, categories):
    """Bare's max - min per category over its runs, floor 0.05 (check I4).

    With no bare arm there is no band, and the answer is never-ran rather than
    the floor: a floor nobody measured reads exactly like a measured 0.05.
    """
    runs = arms.get(BARE_ARM)
    if not runs:
        return ({c: {"band": None, "verdict": "never-ran",
                     "reason": f"no `{BARE_ARM}` arm under the runs dir"}
                 for c in categories},
                {"verdict": "never-ran", "runs": 0})
    judge, _kw, _f, _m, _d = per_run_category_means(runs, excluded)
    out = {}
    for cat in categories:
        xs = [x for x in judge.get(cat, []) if x is not None]
        if not xs:
            out[cat] = {"band": None, "verdict": "never-ran",
                        "reason": f"`{BARE_ARM}` scored no question in {cat}"}
            continue
        raw = max(xs) - min(xs)
        out[cat] = {"band": max(raw, BAND_FLOOR), "raw": raw, "runs": len(xs),
                    "floored": raw < BAND_FLOOR, "verdict": "passed"}
    return out, {"verdict": "passed", "runs": len(runs)}


def build_scoreboard(arms, incomplete, identity, i3, runs_root):
    closed = closed_book_exclusions(arms)
    ungrounded = ungrounded_route_exclusions(arms)
    excluded = set(closed["excluded"]) | set(ungrounded["excluded"])

    questions = {}   # category -> set of question ids, after exclusions
    for runs in arms.values():
        for r in runs:
            for row in rows_of(r):
                qid = row.get("question_id")
                if qid in excluded:
                    continue
                questions.setdefault(row.get("category"), set()).add(qid)
    categories = sorted(questions)
    bands, band_arm = noise_bands(arms, excluded, categories)

    per_arm = {}
    for arm, runs in arms.items():
        judge, kw, found, missed, dropped = per_run_category_means(runs, excluded)
        per_arm[arm] = {"judge": judge, "kw": kw, "found": found,
                        "missed": missed, "unmeasured_rows": dropped,
                        "runs": len(runs)}

    cats = {}
    for cat in categories:
        n = len(questions[cat])
        # Counted here as well as in the `ungrounded_route` block so a reader
        # of one category row can see what left it; both read the one map.
        entry = {"n": n, "band": bands[cat], "arms": {},
                 "excluded_ungrounded_route": ungrounded["per_category"].get(cat, 0)}
        if n < MIN_N:
            entry["judgeable"] = "could-not-judge"
            entry["reason"] = f"n = {n} < {MIN_N} after closed-book exclusions"
        else:
            entry["judgeable"] = "passed"
            entry["reason"] = None
        for arm in order_arms(arms):
            a = per_arm[arm]
            js = [x for x in a["judge"].get(cat, []) if x is not None]
            ks = [x for x in a["kw"].get(cat, []) if x is not None]
            entry["arms"][arm] = {
                "judge": _mean(js), "kw": _mean(ks),
                "runs_scored": len(js), "runs": a["runs"],
                "found": a["found"].get(cat, 0), "missed": a["missed"].get(cat, 0),
                "verdict": "passed" if js else "never-ran",
            }
        cats[cat] = entry

    return {
        "schema": SCHEMA,
        "runs": str(runs_root),
        "generated_at_unix": int(time.time()),
        "verdict": None,
        "refusal": None,
        "identity": identity,
        "i3_ablation_vs_full": i3,
        "arms": {a: [r["run"] for r in rs] for a, rs in arms.items()},
        "incomplete": incomplete,
        "unmeasured_rows": {a: per_arm[a]["unmeasured_rows"] for a in per_arm},
        "closed_book": closed,
        "ungrounded_route": ungrounded,
        "band_arm": band_arm,
        "min_n": MIN_N,
        "categories": cats,
    }


def order_arms(arms):
    known = [a for a in ARM_ORDER if a in arms]
    return known + sorted(a for a in arms if a not in ARM_ORDER)


def fmt(x, width, places=3):
    return f"{('—' if x is None else format(x, f'.{places}f')):>{width}}"


def print_table(board, out=sys.stdout):
    arms = order_arms(board["arms"])
    # Column widths come from the arm NAMES, so `closed-book.judge` (17 chars)
    # cannot run into the column beside it and read as one number.
    w = {a: (max(13, len(a) + 7), max(11, len(a) + 4)) for a in arms}
    hdr = f"{'category':<20}{'judgeable':<17}{'n':>5}{'band':>11}"
    hdr += ''.join(f"{a + '.judge':>{w[a][0]}}{a + '.kw':>{w[a][1]}}" for a in arms)
    print(hdr, file=out)
    print('-' * len(hdr), file=out)
    for cat, e in board["categories"].items():
        band = e["band"]
        band_s = "never-ran" if band["band"] is None else f"{band['band']:.3f}"
        line = f"{cat:<20}{e['judgeable']:<17}{e['n']:>5}{band_s:>11}"
        for a in arms:
            c = e["arms"][a]
            line += fmt(c["judge"], w[a][0]) + fmt(c["kw"], w[a][1])
        print(line, file=out)
    print('-' * len(hdr), file=out)

    cb = board["closed_book"]
    if cb["count"] is None:
        print(f"closed-book exclusions: never-ran — {cb['reason']}", file=out)
    else:
        extra = f", {len(cb['unmeasured'])} unmeasured" if cb["unmeasured"] else ""
        print(f"closed-book exclusions: {cb['count']} over {cb['runs']} run(s){extra}",
              file=out)
    ug = board["ungrounded_route"]
    if ug["count"] is None:
        print(f"ungrounded-route exclusions: never-ran — {ug['reason']}", file=out)
    else:
        per = ", ".join(f"{c}={n}" for c, n in
                        sorted(ug["per_category"].items(), key=lambda kv: str(kv[0])))
        unrouted = f", {len(ug['unrouted'])} unrouted" if ug["unrouted"] else ""
        print(f"ungrounded-route exclusions: {ug['count']} over "
              f"{len(ug['arms'])} retrieving arm(s)"
              f"{' (' + per + ')' if per else ''}{unrouted}", file=out)
    ba = board["band_arm"]
    if ba["verdict"] == "never-ran":
        print(f"noise band: never-ran — no `{BARE_ARM}` arm", file=out)
    else:
        floored = [c for c, e in board["categories"].items()
                   if e["band"].get("floored")]
        note = f" ({len(floored)} at the {BAND_FLOOR} floor)" if floored else ""
        print(f"noise band: `{BARE_ARM}` max-min per category over {ba['runs']} "
              f"run(s), floor {BAND_FLOOR}{note}", file=out)
    i3 = board["i3_ablation_vs_full"]
    print(f"I3 (ablation vs full): {i3['verdict']} — {i3['reason']}", file=out)
    ident = board["identity"]
    print(f"identity: host={ident.get('host')} synth={ident.get('synth_model')} "
          f"judge={ident.get('judge_model')} recipe={str(ident.get('recipe_sha256'))[:8]} "
          f"ontology={str(ident.get('ontology_sha256'))[:8]}", file=out)
    dropped = sum(board["unmeasured_rows"].values())
    if dropped:
        print(f"{dropped} row(s) carried the runner's `error` marker and were not "
              f"scored (runner.rs:58-76)", file=out)
    if board["incomplete"]:
        print(f"incomplete: {', '.join(board['incomplete'])}", file=out)
    print(f"a category under n = {MIN_N} reads could-not-judge (pre-reg \"Bars\"); "
          f"no bar is read here", file=out)


def write_board(out_dir, board):
    d = Path(out_dir)
    d.mkdir(parents=True, exist_ok=True)
    (d / "scoreboard.json").write_text(json.dumps(board, indent=2) + "\n")
    return d / "scoreboard.json"


def study(runs_root, out_dir, quiet=False, vocabulary=None, recipe=None):
    """`(exit_code, scoreboard)`. 0 clean · 2 nothing to compare · 4 refused.

    `sidebyside.json` / `sidebyside.md` land beside `scoreboard.json` on the
    clean path only: a refusal has no comparable arms to put in three columns.
    """
    arms, incomplete = load_runs(runs_root)
    if not arms:
        board = {"schema": SCHEMA, "runs": str(runs_root), "verdict": "never-ran",
                 "refusal": {"field": "runs",
                             "detail": f"no complete run under {runs_root}"},
                 "arms": {}, "incomplete": incomplete, "categories": {}}
        write_board(out_dir, board)
        if not quiet:
            print(f"study: never-ran — {board['refusal']['detail']}", file=sys.stderr)
        return 2, board
    try:
        identity, i3 = check_identity(arms)
    except Refusal as e:
        board = {"schema": SCHEMA, "runs": str(runs_root),
                 "verdict": "could-not-judge",
                 "refusal": {"field": e.field, "detail": e.detail},
                 "arms": {a: [r["run"] for r in rs] for a, rs in arms.items()},
                 "incomplete": incomplete, "categories": {}}
        write_board(out_dir, board)
        if not quiet:
            print(f"study: could-not-judge — arms differ in {e.field}: {e.detail}",
                  file=sys.stderr)
        return 4, board
    board = build_scoreboard(arms, incomplete, identity, i3, runs_root)
    write_board(out_dir, board)
    sbs = build_sidebyside(arms, board, runs_root, vocabulary=vocabulary,
                           recipe=recipe)
    path = write_sidebyside(out_dir, sbs)
    if not quiet:
        print_table(board)
        miss = (f", never-ran: {', '.join(sbs['missing_arms'])}"
                if sbs["missing_arms"] else "")
        print(f"side-by-side: {len(sbs['questions'])} question(s) x "
              f"{len(sbs['arms'])} arm(s){miss} -> {path}")
        if not sbs["snippets"]["shown"]:
            print(f"snippets withheld — {sbs['snippets']['reason']}")
        if sbs["vocabulary"]["verdict"] != "passed":
            print(f"fabricated members: never-ran — {sbs['vocabulary']['reason']}")
    return 0, board


# ── the three-way side-by-side ───────────────────────────────────────────────

SIDEBYSIDE_SCHEMA = "ei7-sidebyside/v1"
# The pre-reg's three columns ("What the study must emit" — bare, generic,
# custom; `generic` is the ablation build and `custom` is full).
SIDEBYSIDE_ARMS = [BARE_ARM, ABLATION_ARM, FULL_ARM]
# The md's cap only. `sidebyside.json` carries the run's own snippet text,
# which the runner already truncates to ~600 chars (`runner.rs:239-241`).
SNIPPET_CHARS = 240
NO_WALK = "no walk"
WALK_REACHED_NOTHING = "walk ran, reached nothing"
# An atom whose `subtype` is empty carries no declared type. On the ablation
# build that is every atom, by construction — so the word is a reading of the
# arm, not a blank to hide.
UNTYPED = "untyped"

HARNESS = Path(HERE).parents[3] / "research" / "ontology-retrieval" / "harness"


def load_fabricated_scorer():
    """`(fabricated_members, None)` from the harness, or `(None, reason)`.

    Imported, never re-derived. The matching rules the count depends on —
    case-folded, word-bounded, longest name first — are one decider and it
    lives in `research/ontology-retrieval/harness/fabricated.py` (ARCH §8). A
    checkout without the harness reports the absence; it does not score 0.
    """
    import importlib.util
    path = HARNESS / "fabricated.py"
    if not path.is_file():
        return None, f"no fabricated-member scorer at {path}"
    spec = importlib.util.spec_from_file_location("ei7_fabricated", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod.fabricated_members, None


def load_vocabulary(path):
    """`(names, note)` from a truth JSON — `(None, note)` when there is none.

    The shape is the pre-reg's ("A2. Typed reach ... in the
    `sovereign-recipes/wessex-hoard/truth.json` shape"): `entities` maps each
    declared type to a list of members carrying `name`. A bare JSON list of
    names is read too, because that is the literal of the scorer's own
    `vocabulary: list[str]` parameter.

    A file that yields NO name is `never-ran`, not an empty vocabulary: an
    empty list scores every answer 0 fabricated, which reads exactly like a
    clean run (ARCH §6).
    """
    note = {"verdict": "never-ran", "path": None if path is None else str(path),
            "names": 0, "reason": "no `--vocabulary` was given"}
    if path is None:
        return None, note
    p = Path(os.path.expanduser(str(path)))
    if not p.is_file():
        note["reason"] = f"no such vocabulary file: {p}"
        return None, note
    try:
        doc = json.loads(p.read_text())
    except (ValueError, OSError) as e:
        note["reason"] = f"{p} did not parse: {type(e).__name__}: {e}"
        return None, note
    names = []
    if isinstance(doc, list):
        names = [str(x) for x in doc if isinstance(x, str) and x.strip()]
    elif isinstance(doc, dict):
        for members in (doc.get("entities") or {}).values():
            for m in members or []:
                name = m.get("name") if isinstance(m, dict) else m
                if isinstance(name, str) and name.strip():
                    names.append(name)
    seen, unique = set(), []
    for n in names:
        if n.casefold() not in seen:
            seen.add(n.casefold())
            unique.append(n)
    if not unique:
        note["reason"] = (f"{p} holds no name — expected `entities` in the "
                          f"truth.json shape, or a list of names")
        return None, note
    note.update(verdict="passed", names=len(unique), reason=None)
    return unique, note


def snippet_policy(recipe):
    """Whether the side-by-side may print chunk text, and why.

    `[corpus].query_sharing` is the recipe's answer to "may a peer search this
    and receive cited snippets back?" (`corpus-engine/src/recipe.rs:785-799`),
    and the pre-reg hands the renderer that same flag: "when it is false, the
    renderer withholds snippets and says so". `None` there falls back to
    `mesh_sharing`, which is the field's own documented back-compat rule.

    With no `--recipe`, no custody declaration was read at all: that is said in
    the header rather than resolved to `true`, and the snippets are printed —
    withholding on a flag nobody read would be the same invented answer facing
    the other way.
    """
    import tomllib
    pol = {"shown": True, "verdict": "never-ran", "recipe": None,
           "query_sharing": None, "source": None,
           "reason": "no `--recipe` was given — custody was not read"}
    if recipe is None:
        return pol
    p = Path(os.path.expanduser(str(recipe)))
    pol["recipe"] = str(p)
    if not p.is_file():
        pol["reason"] = f"no such recipe: {p} — custody was not read"
        return pol
    try:
        doc = tomllib.loads(p.read_text())
    except (tomllib.TOMLDecodeError, OSError) as e:
        pol["reason"] = f"{p} did not parse: {type(e).__name__}: {e}"
        return pol
    corpus = doc.get("corpus") or {}
    flag, source = corpus.get("query_sharing"), "query_sharing"
    if flag is None:
        flag, source = corpus.get("mesh_sharing"), "mesh_sharing"
    if flag is None:
        pol["reason"] = (f"{p} declares neither `query_sharing` nor "
                         f"`mesh_sharing` under [corpus]")
        return pol
    pol.update(shown=bool(flag), verdict="passed", query_sharing=bool(flag),
               source=source,
               reason=(f"{source} = {bool(flag)} in {p.name}"))
    return pol


def walk_path(row):
    """`(verdict, lines)` for one row's evidence path.

    Three outcomes, not two (ARCH §6):

      - no `atlas_walk` key   → `no walk`. The walk did not run for this row,
        which is the truth for every bare run and is not "reached nothing".
      - `atlas_walk`, no nodes → `walk ran, reached nothing`.
      - nodes → one line each, in the echo's own order (highest walk weight
        first, `types.rs:219-221`): a seed is `name (subtype)` and a hop is
        `from (subtype) —via→ name (subtype)`.

    A `from` the echo does not carry is beyond `MAP_NODE_CAP` — the atom id is
    printed as it stands rather than dropping the hop.
    """
    walk = row.get("atlas_walk")
    if not isinstance(walk, dict):
        return NO_WALK, []
    nodes = walk.get("nodes") or []
    if not nodes:
        return WALK_REACHED_NOTHING, []
    by_id = {n.get("atom_id"): n for n in nodes}

    def named(n):
        return f"{n.get('name')} ({n.get('subtype') or UNTYPED})"

    lines = []
    for n in nodes:
        src = n.get("from")
        if not src:
            lines.append(named(n))
            continue
        parent = by_id.get(src)
        head = named(parent) if parent else f"{src} (beyond the node cap)"
        lines.append(f"{head} —{n.get('via') or 'edge'}→ {named(n)}")
    return "passed", lines


def citations_of(row, show_snippets):
    """Numbered references from `retrieved[].title`, in retrieval order."""
    out = []
    for i, c in enumerate(row.get("retrieved") or [], 1):
        out.append({"n": i, "title": c.get("title"),
                    "corpus_id": c.get("corpus_id"),
                    "snippet": (c.get("snippet") if show_snippets else None)})
    return out


def first_measured(runs, qid):
    """`(run, row)` for the lowest-numbered run that MEASURED `qid`.

    A block shows one answer's text, citations and path, and there is no mean
    of those — so the column names a run rather than averaging. The rule is the
    lowest index that measured it, and the run rides in the column, so the pick
    is checkable rather than a choice. A row the runner marked `error` is
    skipped here exactly as the scoreboard skips it.
    """
    for r in runs:
        row = next((x for x in rows_of(r) if x.get("question_id") == qid), None)
        if row is not None and measured(row):
            return r, row
    return None, None


def sidebyside_column(arm, runs, qid, vocab, fab, show_snippets):
    """One arm's cell set for one question."""
    if not runs:
        return {"verdict": "never-ran", "run": None,
                "reason": f"no `{arm}` arm under the runs dir"}
    run, row = first_measured(runs, qid)
    if row is None:
        return {"verdict": "never-ran", "run": None,
                "reason": f"no run of `{arm}` measured {qid}"}
    gold = [{"fact": f, "present": p} for f, p in judge_members(row)]
    answer = ((row.get("synth") or {}).get("answer")) or ""
    if vocab is None or fab is None:
        fabricated = {"verdict": "never-ran", "names": None, "count": None}
    else:
        names = fab(answer, vocab, [g["fact"] for g in gold if g["fact"]])
        fabricated = {"verdict": "passed", "names": names, "count": len(names)}
    verdict, lines = walk_path(row)
    return {
        "verdict": "passed", "run": run["run"], "reason": None,
        "judge": judge_ratio(row), "kw": kw_ratio(row),
        "gold": gold,
        "found": sum(1 for g in gold if g["present"]),
        "missed": sum(1 for g in gold if not g["present"]),
        "fabricated": fabricated,
        "citations": citations_of(row, show_snippets),
        "path": {"verdict": verdict, "lines": lines},
    }


def build_sidebyside(arms, board, runs_root, vocabulary=None, recipe=None):
    """One block per question, three columns (bare, ablation, full)."""
    vocab, vocab_note = load_vocabulary(vocabulary)
    fab, fab_reason = load_fabricated_scorer()
    if fab is None and vocab_note["verdict"] == "passed":
        vocab_note.update(verdict="never-ran", reason=fab_reason)
    snippets = snippet_policy(recipe)
    excluded = set(((board or {}).get("closed_book") or {}).get("excluded") or [])

    meta = {}
    for arm in SIDEBYSIDE_ARMS:
        for r in arms.get(arm, []):
            for row in rows_of(r):
                meta.setdefault(row.get("question_id"),
                                (row.get("category"), row.get("question")))
    questions = []
    for qid in sorted(meta, key=lambda q: (str(meta[q][0]), str(q))):
        category, text = meta[qid]
        questions.append({
            "question_id": qid, "category": category, "question": text,
            # Kept in the file and MARKED: a block is a reading, not a mean,
            # and an excluded question is the one a reader most wants to see.
            "closed_book_excluded": qid in excluded,
            "columns": {arm: sidebyside_column(arm, arms.get(arm, []), qid,
                                               vocab, fab, snippets["shown"])
                        for arm in SIDEBYSIDE_ARMS},
        })
    return {
        "schema": SIDEBYSIDE_SCHEMA,
        "runs": str(runs_root),
        "generated_at_unix": int(time.time()),
        "arms": list(SIDEBYSIDE_ARMS),
        "missing_arms": [a for a in SIDEBYSIDE_ARMS if not arms.get(a)],
        "vocabulary": vocab_note,
        "snippets": snippets,
        "questions": questions,
    }


def _cell(lines):
    """A markdown table cell. `|` would end the cell and a newline the row."""
    if not lines:
        return "—"
    return "<br>".join(str(x).replace("|", "\\|").replace("\n", " ")
                       for x in lines)


def _citation_lines(col):
    return [f"[{c['n']}] {c['title'] or '(untitled — ' + str(c['corpus_id']) + ')'}"
            for c in col.get("citations") or []]


def _fabricated_lines(col):
    f = col.get("fabricated") or {}
    if f.get("verdict") != "passed":
        return ["never-ran"]
    return [f"{len(f['names'])}: " + ", ".join(f["names"])] if f["names"] else ["0"]


def render_sidebyside_md(sbs):
    """The same blocks as `sidebyside.json`, as one markdown file."""
    arms = sbs["arms"]
    v, s = sbs["vocabulary"], sbs["snippets"]
    out = ["# EI7 stage-0 — three-way side-by-side", ""]
    out.append(f"runs: `{sbs['runs']}`")
    out.append(f"arms: {', '.join(arms)}"
               + (f" (never-ran: {', '.join(sbs['missing_arms'])})"
                  if sbs["missing_arms"] else ""))
    out.append("fabricated members: " + (
        f"vocabulary of {v['names']} name(s) from `{v['path']}`"
        if v["verdict"] == "passed" else f"never-ran — {v['reason']}"))
    out.append("snippets: " + (
        f"shown — {s['reason']}" if s["shown"]
        else f"WITHHELD — {s['reason']}"))
    out.append("")
    out.append("A column names the run it read: the lowest-numbered run of that "
               "arm that measured the question. `no walk` = the row carries no "
               "`atlas_walk`, which is the truth for bare; it is not "
               f"`{WALK_REACHED_NOTHING}`.")
    for q in sbs["questions"]:
        out += ["", f"## {q['question_id']} — {q['category']}"
                    + ("  · closed-book excluded" if q["closed_book_excluded"] else ""),
                "", f"> {q['question']}", ""]
        cols = [q["columns"][a] for a in arms]
        out.append("| | " + " | ".join(arms) + " |")
        out.append("|---" * (len(arms) + 1) + "|")

        def line(label, fn, why=False):
            # A never-ran column states its reason ONCE, on the `run` row; the
            # rows under it still read `never-ran` rather than a dash, because
            # a dash in a score column reads as a zero.
            out.append(f"| {label} | " + " | ".join(
                (_cell([f"never-ran — {c['reason']}" if why else "never-ran"])
                 if c["verdict"] != "passed" else fn(c)) for c in cols) + " |")

        line("run", lambda c: f"run-{c['run']}", why=True)
        line("judge / kw", lambda c: f"{fmt(c['judge'], 0).strip()} / "
                                     f"{fmt(c['kw'], 0).strip()}")
        line("gold members", lambda c: _cell(
            [("+ " if g["present"] else "- ") + str(g["fact"]) for g in c["gold"]]))
        line("found / missed", lambda c: f"{c['found']} / {c['missed']}")
        line("fabricated", lambda c: _cell(_fabricated_lines(c)))
        line("citations", lambda c: _cell(_citation_lines(c)))
        line("evidence path", lambda c: _cell(
            c["path"]["lines"] or [c["path"]["verdict"]]))
        if not sbs["snippets"]["shown"]:
            out += ["", f"snippets withheld — {sbs['snippets']['reason']}"]
            continue
        quoted = []
        for arm, c in zip(arms, cols):
            for cite in (c.get("citations") or []) if c["verdict"] == "passed" else []:
                if cite["snippet"]:
                    text = re.sub(r"\s+", " ", cite["snippet"]).strip()
                    if len(text) > SNIPPET_CHARS:
                        text = text[:SNIPPET_CHARS] + "…"
                    quoted.append(f"- {arm} [{cite['n']}] {text}")
        if quoted:
            out += ["", f"snippets (first {SNIPPET_CHARS} chars):"] + quoted
    return "\n".join(out) + "\n"


def write_sidebyside(out_dir, sbs):
    d = Path(out_dir)
    d.mkdir(parents=True, exist_ok=True)
    (d / "sidebyside.json").write_text(json.dumps(sbs, indent=2) + "\n")
    (d / "sidebyside.md").write_text(render_sidebyside_md(sbs))
    return d / "sidebyside.md"


# ── self-test ────────────────────────────────────────────────────────────────

def self_test():
    """The failing inputs this script is named for.

    Each case carries a PLANT: an input the obvious-but-wrong implementation
    gets wrong. The verdicts are the four the queue names; a case that could not
    be set up reads `could-not-judge`, never `passed`.
    """
    import shutil, tempfile

    results = []

    def case(name, fn):
        try:
            ok, detail = fn()
        except Exception as e:                     # noqa: BLE001 — a broken fixture
            results.append((name, "could-not-judge", f"{type(e).__name__}: {e}"))
            return
        results.append((name, "passed" if ok else "failed", detail))

    IDENT = {"corpus": "c", "host": "http://localhost:9741",
             "synth_model": "m-synth", "judge_model": "m-judge",
             "recipe_sha256": "r" * 64, "ontology_sha256": "o" * 64,
             "chunks_listing_sha256": "k" * 64}

    def row(qid, category, judge, kw, members=None, error=None, answer="a",
            retrieved=None, walk=None, intent="KnowledgeQuery"):
        """One eval.json result. `members` is [(fact, present)].

        `walk=None` writes NO `atlas_walk` key at all, which is what a bare run
        looks like — distinct from `walk={"nodes": []}`, a walk that ran.

        `intent` defaults to the PascalCase spelling the handlers stamp;
        `intent=None` writes NO key, which is what a naked turn looks like.
        """
        ev = [{"fact": f, "present": p, "evidence": "q" if p else "(absent)"}
              for f, p in (members or [])]
        r = {"question_id": qid, "category": category, "question": qid,
             "retrieved": retrieved or [],
             "fact_score": {"matched": [], "missing": [],
                            "total_expected": 1, "ratio": kw},
             "synth": {"answer": answer,
                       # PLANT: the rollup's name lists are EMPTY while the
                       # audit trail is not. found/missed read off `matched`
                       # would be 0 here and the table would not notice.
                       "judge_fact_score": {"matched": [], "missing": [],
                                            "total_expected": len(ev) or 1,
                                            "ratio": judge},
                       "judge_evidence": ev}}
        if intent is not None:
            r["synth"]["intent"] = intent
        if error:
            r["error"] = error
        if walk is not None:
            r["atlas_walk"] = walk
        return r

    def fixture(root, arm, run, rows, ident=None):
        d = Path(root) / arm / f"run-{run}"
        d.mkdir(parents=True, exist_ok=True)
        (d / "eval.json").write_text(json.dumps(
            {"bank_name": "t", "corpus": "c", "limit": 0,
             "started_at_unix": 0, "results": rows}))
        (d / "manifest.json").write_text(json.dumps(
            {"schema": "ei7-arm-manifest/v1",
             "identity": {"arm": arm, **IDENT, **(ident or {})},
             "run": run, "verdict": None, "never_ran_reasons": []}))

    QIDS = [f"q{i:02d}" for i in range(24)]
    ARM_JUDGE = {CLOSED_BOOK_ARM: 0.10, BARE_ARM: 0.40,
                 ABLATION_ARM: 0.55, FULL_ARM: 0.70}

    def clean(root, qids=QIDS):
        """closed-book, bare, ablation, full; 3 runs; one category, n = 24."""
        for arm, j in ARM_JUDGE.items():
            ident = {"ontology_sha256": "a" * 64} if arm == ABLATION_ARM else {}
            # Closed-book is naked and records no route; every other arm went
            # through the router. That is the producer's shape, and it is what
            # makes "closed-book has no route" a fixture rather than a claim.
            intent = None if arm == CLOSED_BOOK_ARM else "KnowledgeQuery"
            for r in (1, 2, 3):
                fixture(root, arm, r,
                        [row(q, "K1", j + 0.01 * r, j / 2, intent=intent,
                             members=[("f1", True), ("f2", False)]) for q in qids],
                        ident=ident)
        return root

    def clean_set_passes():
        with tempfile.TemporaryDirectory() as t:
            code, b = study(clean(Path(t) / "runs"), Path(t) / "out", quiet=True)
            k1 = b["categories"]["K1"]
            wrote = (Path(t) / "out" / "scoreboard.json").is_file()
            ok = (code == 0 and b["verdict"] is None and wrote
                  and k1["n"] == 24 and k1["judgeable"] == "passed"
                  and b["closed_book"]["count"] == 0
                  # closed-book records no route and must not be read as 24
                  # ungrounded questions — it is the one arm with no router.
                  and b["ungrounded_route"]["count"] == 0
                  and b["ungrounded_route"]["unrouted"] == []
                  and k1["excluded_ungrounded_route"] == 0
                  and b["i3_ablation_vs_full"]["verdict"] == "passed"
                  and abs(k1["arms"][FULL_ARM]["judge"] - 0.72) < 1e-9)
            return ok, (f"exit={code} n={k1['n']} full.judge="
                        f"{k1['arms'][FULL_ARM]['judge']:.3f} "
                        f"i3={b['i3_ablation_vs_full']['verdict']}")

    def band_is_bare_spread_over_its_runs():
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            # bare spans 0.41 .. 0.43 above; widen run 3 so the band clears the
            # floor and the FLOOR is not what is being read.
            fixture(root, BARE_ARM, 3,
                    [row(q, "K1", 0.80, 0.2, members=[("f1", True)]) for q in QIDS])
            _code, b = study(root, Path(t) / "out", quiet=True)
            band = b["categories"]["K1"]["band"]
            ok = (abs(band["band"] - (0.80 - 0.41)) < 1e-6 and not band["floored"]
                  and band["runs"] == 3)
            return ok, f"band={band['band']:.3f} floored={band['floored']}"

    def recipe_hash_mismatch_refuses():
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            # PLANT: one arm rebuilt from an edited recipe.
            fixture(root, FULL_ARM, 2,
                    [row(q, "K1", 0.7, 0.35) for q in QIDS],
                    ident={"recipe_sha256": "z" * 64})
            code, b = study(root, Path(t) / "out", quiet=True)
            ok = (code == 4 and b["verdict"] == "could-not-judge"
                  and b["refusal"]["field"] == "recipe_sha256")
            return ok, f"exit={code} field={b['refusal']['field']}"

    def synth_model_mismatch_refuses():
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            # PLANT: one arm ran after the primary slot moved. The closed-book
            # exclusion set is a property of ONE model (pre-reg, "Model and host").
            fixture(root, BARE_ARM, 3,
                    [row(q, "K1", 0.4, 0.2) for q in QIDS],
                    ident={"synth_model": "another-model"})
            code, b = study(root, Path(t) / "out", quiet=True)
            ok = (code == 4 and b["verdict"] == "could-not-judge"
                  and b["refusal"]["field"] == "synth_model")
            return ok, f"exit={code} field={b['refusal']['field']}"

    def host_mismatch_refuses():
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            # PLANT: half the arms ran on the rented pod (:9841), half at home.
            fixture(root, FULL_ARM, 3,
                    [row(q, "K1", 0.7, 0.35) for q in QIDS],
                    ident={"host": "http://127.0.0.1:9841",
                           "ontology_sha256": "o" * 64})
            code, b = study(root, Path(t) / "out", quiet=True)
            ok = code == 4 and b["refusal"]["field"] == "host"
            return ok, f"exit={code} field={b['refusal']['field']}"

    def ablation_sharing_fulls_ontology_refuses():
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            # PLANT (I3): the "ablation" was built from the same ontology, so it
            # is a second full arm wearing the ablation's name.
            for r in (1, 2, 3):
                fixture(root, ABLATION_ARM, r,
                        [row(q, "K1", 0.55, 0.27) for q in QIDS],
                        ident={"ontology_sha256": "o" * 64})
            code, b = study(root, Path(t) / "out", quiet=True)
            ok = code == 4 and b["refusal"]["field"] == "ontology_sha256"
            return ok, f"exit={code} detail={b['refusal']['detail'][:60]}"

    def ablation_with_other_chunks_refuses():
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            # PLANT (I3): the ablation was rebuilt over a different chunk set, so
            # a delta against full is not the ontology's.
            for r in (1, 2, 3):
                fixture(root, ABLATION_ARM, r,
                        [row(q, "K1", 0.55, 0.27) for q in QIDS],
                        ident={"ontology_sha256": "a" * 64,
                               "chunks_listing_sha256": "x" * 64})
            code, b = study(root, Path(t) / "out", quiet=True)
            ok = code == 4 and b["refusal"]["field"] == "chunks_listing_sha256"
            return ok, f"exit={code} field={b['refusal']['field']}"

    def closed_book_excludes_only_on_every_run():
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            # PLANT: q00-q02 are known on ALL three runs (excluded); q03 is known
            # on two of three and stays in the bank.
            for r in (1, 2, 3):
                rows = []
                for i, q in enumerate(QIDS):
                    known = i < 3 or (i == 3 and r < 3)
                    rows.append(row(q, "K1", 0.9 if known else 0.1, 0.05))
                fixture(root, CLOSED_BOOK_ARM, r, rows)
            code, b = study(root, Path(t) / "out", quiet=True)
            cb = b["closed_book"]
            ok = (code == 0 and cb["count"] == 3
                  and cb["excluded"] == ["q00", "q01", "q02"]
                  and b["categories"]["K1"]["n"] == 21)
            return ok, f"excluded={cb['excluded']} n={b['categories']['K1']['n']}"

    def closed_book_unmeasured_is_not_a_zero():
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            # PLANT: q00 is known on runs 1-2 and its third run carries the
            # runner's `error` marker. A scored 0.0 there un-excludes it.
            for r in (1, 2, 3):
                rows = [row(q, "K1", 0.9 if q == "q00" else 0.1, 0.05,
                            error=("503 host busy" if q == "q00" and r == 3 else None))
                        for q in QIDS]
                fixture(root, CLOSED_BOOK_ARM, r, rows)
            code, b = study(root, Path(t) / "out", quiet=True)
            cb = b["closed_book"]
            ok = code == 0 and cb["count"] == 0 and cb["unmeasured"] == ["q00"]
            return ok, f"count={cb['count']} unmeasured={cb['unmeasured']}"

    def no_closed_book_arm_is_never_ran_not_zero():
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            shutil.rmtree(root / CLOSED_BOOK_ARM)
            code, b = study(root, Path(t) / "out", quiet=True)
            cb = b["closed_book"]
            ok = code == 0 and cb["verdict"] == "never-ran" and cb["count"] is None
            return ok, f"verdict={cb['verdict']} count={cb['count']!r}"

    def no_bare_arm_band_is_never_ran_not_the_floor():
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            shutil.rmtree(root / BARE_ARM)
            code, b = study(root, Path(t) / "out", quiet=True)
            band = b["categories"]["K1"]["band"]
            ok = (code == 0 and band["verdict"] == "never-ran"
                  and band["band"] is None)
            return ok, f"verdict={band['verdict']} band={band['band']!r}"

    def category_under_20_is_could_not_judge():
        with tempfile.TemporaryDirectory() as t:
            # PLANT: 15 questions — the pre-reg's bank size per (corpus, kind)
            # for every kind but K2, which pools to 20 only across corpora.
            root = clean(Path(t) / "runs", qids=QIDS[:15])
            code, b = study(root, Path(t) / "out", quiet=True)
            k1 = b["categories"]["K1"]
            ok = (code == 0 and k1["n"] == 15
                  and k1["judgeable"] == "could-not-judge"
                  and "15" in k1["reason"])
            return ok, f"n={k1['n']} judgeable={k1['judgeable']} ({k1['reason']})"

    def found_missed_comes_from_the_audit_trail():
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            _code, b = study(root, Path(t) / "out", quiet=True)
            cell = b["categories"]["K1"]["arms"][FULL_ARM]
            # 24 questions x 3 runs x one present + one absent member each.
            ok = cell["found"] == 72 and cell["missed"] == 72
            return ok, f"found={cell['found']} missed={cell['missed']} (matched[] is empty)"

    def errored_row_is_not_scored_as_zero():
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            # PLANT: run 2 of full lost three questions to a busy host. Scoring
            # their 0.0 drags the arm's mean and reads as a regression.
            rows = [row(q, "K1", 0.0 if i < 3 else 0.71, 0.35,
                        members=[("f1", True), ("f2", False)],
                        error=("503 host busy" if i < 3 else None))
                    for i, q in enumerate(QIDS)]
            fixture(root, FULL_ARM, 2, rows)
            code, b = study(root, Path(t) / "out", quiet=True)
            cell = b["categories"]["K1"]["arms"][FULL_ARM]
            # runs 1 and 3 are 0.71 / 0.73; run 2's surviving rows are 0.71.
            ok = (code == 0 and abs(cell["judge"] - (0.71 + 0.71 + 0.73) / 3) < 1e-9
                  and b["unmeasured_rows"][FULL_ARM] == 3)
            return ok, (f"full.judge={cell['judge']:.4f} "
                        f"unmeasured={b['unmeasured_rows'][FULL_ARM]}")

    def empty_runs_dir_is_never_ran():
        with tempfile.TemporaryDirectory() as t:
            code, b = study(Path(t) / "nothing-here", Path(t) / "out", quiet=True)
            ok = code == 2 and b["verdict"] == "never-ran"
            return ok, f"exit={code} verdict={b['verdict']}"

    def half_written_run_is_reported_not_dropped():
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            # PLANT: a run killed between eval.json and manifest.json. Silently
            # skipping it makes a 2-run arm look like the 3 runs the band needs.
            d = root / FULL_ARM / "run-4"
            d.mkdir(parents=True)
            (d / "eval.json").write_text(json.dumps({"results": []}))
            code, b = study(root, Path(t) / "out", quiet=True)
            ok = code == 0 and any("run-4" in p for p in b["incomplete"])
            return ok, f"incomplete={b['incomplete']}"

    def table_renders_every_arm():
        import io
        with tempfile.TemporaryDirectory() as t:
            _code, b = study(clean(Path(t) / "runs"), Path(t) / "out", quiet=True)
            buf = io.StringIO()
            print_table(b, out=buf)
            text = buf.getvalue()
            # PLANT: `closed-book.judge` is 17 chars. A per-arm column narrower
            # than its own name runs into the one beside it and the header reads
            # `bandclosed-book.judge` — a table nobody can parse by eye.
            head = text.splitlines()[0].split()
            want = ["category", "judgeable", "n", "band"] + [
                f"{a}.{c}" for a in order_arms(b["arms"]) for c in ("judge", "kw")]
            ok = (head == want and "closed-book exclusions:" in text
                  and "I3 (" in text)
            return ok, f"header={len(head)} tokens, {len(text.splitlines())} lines"

    # ── the three-way side-by-side ───────────────────────────────────────────

    WALK = {"kind": "K1", "seeds": 1, "edges_followed": 2, "nodes_reached": 3,
            "requests": 2, "summaries_appended": 0, "added": 2, "considered": 4,
            "nodes": [
                {"atlas": "a", "atom_id": "n1", "name": "Vale dossier",
                 "kind": "entity", "subtype": "dossier", "hop": 0,
                 "via": None, "from": None, "score": 0.9},
                {"atlas": "a", "atom_id": "n2", "name": "Marcus Vale",
                 "kind": "entity", "subtype": "", "hop": 1,
                 "via": "mentions", "from": "n1", "score": 0.5},
                {"atlas": "a", "atom_id": "n3", "name": "Kepler cell",
                 "kind": "claim", "subtype": "operation", "hop": 2,
                 "via": "supports", "from": "n9", "score": 0.2}]}
    CHUNKS = [{"corpus_id": "c", "title": "Dossier 4", "url": None, "score": 0.8,
               "snippet": "Vale   signed\nthe Kepler order."},
              {"corpus_id": "c", "title": None, "url": None, "score": 0.4,
               "snippet": "An untitled fragment."}]

    def sbs_of(t, root, **kw):
        """`(exit, sidebyside.json, sidebyside.md)` — read off disk, so the
        files landing is part of every assertion below."""
        code, _b = study(root, Path(t) / "out", quiet=True, **kw)
        return (code,
                json.loads((Path(t) / "out" / "sidebyside.json").read_text()),
                (Path(t) / "out" / "sidebyside.md").read_text())

    def block(j, qid):
        return next(q for q in j["questions"] if q["question_id"] == qid)

    def vocabulary_file(t):
        p = Path(t) / "truth.json"
        p.write_text(json.dumps({
            "corpus_id": "c",
            "entities": {"person": [{"name": "Elena Ward"}, {"name": "Marcus Vale"}],
                         "operation": [{"name": "Dmitri Kel"}]}}))
        return p

    def recipe_file(t, sharing):
        p = Path(t) / f"recipe-{sharing}.toml"
        p.write_text(f'[corpus]\nid = "c"\nmesh_sharing = true\n'
                     f'query_sharing = {str(bool(sharing)).lower()}\n')
        return p

    def block_per_question_three_columns():
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            code, j, md = sbs_of(t, root)
            b = block(j, "q00")
            ok = (code == 0 and len(j["questions"]) == 24
                  and j["arms"] == [BARE_ARM, ABLATION_ARM, FULL_ARM]
                  and sorted(b["columns"]) == sorted(j["arms"])
                  and all(c["verdict"] == "passed" for c in b["columns"].values())
                  and b["columns"][FULL_ARM]["found"] == 1
                  and b["columns"][FULL_ARM]["missed"] == 1
                  and "| | bare | ablation | full |" in md
                  and md.count("## q") == 24)
            return ok, (f"{len(j['questions'])} block(s), "
                        f"found/missed={b['columns'][FULL_ARM]['found']}/"
                        f"{b['columns'][FULL_ARM]['missed']}")

    def missing_arm_is_a_never_ran_column():
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            # PLANT: no ablation arm — the pilot's own case. Dropping the column
            # would render a two-arm block that reads like a three-way study.
            shutil.rmtree(root / ABLATION_ARM)
            _code, j, md = sbs_of(t, root)
            col = block(j, "q00")["columns"][ABLATION_ARM]
            ok = (j["missing_arms"] == [ABLATION_ARM]
                  and col["verdict"] == "never-ran"
                  and ABLATION_ARM in col["reason"]
                  and "| | bare | ablation | full |" in md)
            return ok, f"ablation: {col['verdict']} — {col['reason']}"

    def no_vocabulary_is_never_ran_not_zero():
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            # PLANT: no --vocabulary. A 0 here reads "nothing was fabricated",
            # which is a measurement nobody made (ARCH §6).
            _code, j, md = sbs_of(t, root)
            fab = block(j, "q00")["columns"][FULL_ARM]["fabricated"]
            ok = (fab["verdict"] == "never-ran" and fab["count"] is None
                  and j["vocabulary"]["verdict"] == "never-ran"
                  and "never-ran" in md.split("## q")[0])
            return ok, f"fabricated={fab['verdict']} count={fab['count']!r}"

    def fabricated_counts_vocabulary_non_gold_only():
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            # PLANT: the answer states one gold name (never fabricated), one
            # vocabulary non-gold name (counted once), and one name in no
            # vocabulary at all (the scorer's stated limit — not counted).
            for r in (1, 2, 3):
                fixture(root, FULL_ARM, r,
                        [row(q, "K1", 0.7, 0.35,
                             members=[("Elena Ward", True), ("Dmitri Kel", False)],
                             answer="Elena Ward met Marcus Vale, and Marcus Vale "
                                    "briefed Colonel Sandoval.") for q in QIDS])
            _code, j, _md = sbs_of(t, root, vocabulary=vocabulary_file(t))
            fab = block(j, "q00")["columns"][FULL_ARM]["fabricated"]
            ok = (j["vocabulary"]["verdict"] == "passed"
                  and j["vocabulary"]["names"] == 3
                  and fab["names"] == ["Marcus Vale"] and fab["count"] == 1)
            return ok, f"names={fab['names']} (vocabulary={j['vocabulary']['names']})"

    def bare_has_no_walk_and_full_renders_the_path():
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            for r in (1, 2, 3):
                fixture(root, FULL_ARM, r,
                        [row(q, "K1", 0.7, 0.35, members=[("f1", True)],
                             retrieved=CHUNKS, walk=WALK) for q in QIDS])
            _code, j, md = sbs_of(t, root)
            cols = block(j, "q00")["columns"]
            path = cols[FULL_ARM]["path"]
            ok = (cols[BARE_ARM]["path"]["verdict"] == NO_WALK
                  and not cols[BARE_ARM]["path"]["lines"]
                  and path["lines"] == [
                      "Vale dossier (dossier)",
                      "Vale dossier (dossier) —mentions→ Marcus Vale (untyped)",
                      "n9 (beyond the node cap) —supports→ Kepler cell (operation)"]
                  and NO_WALK in md
                  and "[2] (untitled — c)" in md)
            return ok, f"bare={cols[BARE_ARM]['path']['verdict']}; full: {path['lines'][1]}"

    def walk_that_reached_nothing_is_not_no_walk():
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            # PLANT: the walk RAN and reached no atom. Reading that as `no walk`
            # loses the one distinction the echo exists to carry
            # (`runner.rs:158-160`: absent is never "reached nothing").
            for r in (1, 2, 3):
                fixture(root, FULL_ARM, r,
                        [row(q, "K1", 0.7, 0.35, members=[("f1", True)],
                             walk={**WALK, "nodes": [], "nodes_reached": 0})
                         for q in QIDS])
            _code, j, md = sbs_of(t, root)
            path = block(j, "q00")["columns"][FULL_ARM]["path"]
            ok = (path["verdict"] == WALK_REACHED_NOTHING and not path["lines"]
                  and WALK_REACHED_NOTHING in md)
            return ok, f"verdict={path['verdict']!r}"

    def query_sharing_false_withholds_snippets():
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            for r in (1, 2, 3):
                fixture(root, FULL_ARM, r,
                        [row(q, "K1", 0.7, 0.35, members=[("f1", True)],
                             retrieved=CHUNKS) for q in QIDS])
            # PLANT: a restricted-text corpus. The citation titles stay (they
            # are the reference), the chunk text does not.
            _c, closed, closed_md = sbs_of(t, root, recipe=recipe_file(t, False))
            _c, open_, open_md = sbs_of(t, root, recipe=recipe_file(t, True))
            shut = block(closed, "q00")["columns"][FULL_ARM]["citations"]
            lets = block(open_, "q00")["columns"][FULL_ARM]["citations"]
            ok = (closed["snippets"]["shown"] is False
                  and closed["snippets"]["query_sharing"] is False
                  and all(c["snippet"] is None for c in shut)
                  and [c["title"] for c in shut] == ["Dossier 4", None]
                  and "snippets withheld" in closed_md
                  and "Vale signed the Kepler order." not in closed_md
                  and open_["snippets"]["shown"] is True
                  and lets[0]["snippet"].startswith("Vale")
                  and "Vale signed the Kepler order." in open_md)
            return ok, (f"withheld={not closed['snippets']['shown']} "
                        f"titles kept={[c['title'] for c in shut]}")

    def column_falls_to_the_next_run_that_measured():
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            # PLANT: run 1 of full lost q00 to a busy host. A column that reads
            # run 1 regardless shows a 0.0 answer the model never gave; one that
            # gives up shows never-ran while run 2 holds the answer.
            fixture(root, FULL_ARM, 1,
                    [row(q, "K1", 0.0, 0.0, members=[("f1", False)],
                         error="503 host busy" if q == "q00" else None)
                     for q in QIDS])
            _code, j, _md = sbs_of(t, root)
            first, other = block(j, "q00")["columns"][FULL_ARM], \
                block(j, "q01")["columns"][FULL_ARM]
            ok = (first["verdict"] == "passed" and first["run"] == 2
                  and abs(first["judge"] - 0.72) < 1e-9 and other["run"] == 1)
            return ok, f"q00 read run-{first['run']} judge={first['judge']}"

    def ungrounded_route_excludes_the_question_from_every_arm():
        """PLANT: `full` routed q00 to GenerativeQuery, which retrieves nothing
        by design. Dropping that row from `full` alone would leave every other
        arm averaging a question one arm never looked anything up for."""
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            for r in (1, 2, 3):
                fixture(root, FULL_ARM, r,
                        [row(q, "K1", 0.7, 0.35, members=[("f1", True)],
                             intent=("GenerativeQuery" if q == "q00"
                                     else "KnowledgeQuery")) for q in QIDS])
            _code, b = study(root, Path(t) / "out", quiet=True)
            ug, k1 = b["ungrounded_route"], b["categories"]["K1"]
            ok = (ug["excluded"] == ["q00"] and ug["per_category"] == {"K1": 1}
                  and ug["routes"]["q00"] == [f"{FULL_ARM}:GenerativeQuery"]
                  and k1["excluded_ungrounded_route"] == 1 and k1["n"] == 23
                  and k1["arms"][BARE_ARM]["runs_scored"] == 3)
            return ok, (f"excluded={ug['excluded']} n={k1['n']} "
                        f"per_category={ug['per_category']}")

    def an_unrouted_row_is_not_an_exclusion():
        """PLANT: `full` recorded NO route on q00 — the shape of a transcript
        banked before the handlers stamped the key. Excluding on an absent
        field would shrink the bank over a router that did nothing wrong; the
        absence is reported instead (ARCH §6), and check I7 reads it."""
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            for r in (1, 2, 3):
                fixture(root, FULL_ARM, r,
                        [row(q, "K1", 0.7, 0.35, members=[("f1", True)],
                             intent=None if q == "q00" else "KnowledgeQuery")
                         for q in QIDS])
            _code, b = study(root, Path(t) / "out", quiet=True)
            ug, k1 = b["ungrounded_route"], b["categories"]["K1"]
            ok = (ug["excluded"] == [] and ug["count"] == 0
                  and ug["unrouted"] == ["q00"] and k1["n"] == 24)
            return ok, f"unrouted={ug['unrouted']} excluded={ug['excluded']} n={k1['n']}"

    def the_two_spellings_of_one_route_are_one_route():
        """PLANT: `full` recorded `comparison_query`, the wire slug, where the
        handlers stamp `ComparisonQuery`. A literal match against either
        spelling alone excludes 24 questions that all retrieved."""
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            for r in (1, 2, 3):
                fixture(root, FULL_ARM, r,
                        [row(q, "K1", 0.7, 0.35, members=[("f1", True)],
                             intent="comparison_query") for q in QIDS])
            _code, b = study(root, Path(t) / "out", quiet=True)
            ug = b["ungrounded_route"]
            ok = (ug["count"] == 0 and ug["unrouted"] == []
                  and b["categories"]["K1"]["n"] == 24)
            return ok, f"count={ug['count']} n={b['categories']['K1']['n']}"

    def a_deep_query_row_retrieved_and_a_generative_one_did_not():
        """PLANT: `full` routed q00 to DeepQuery and q01 to GenerativeQuery.
        The first retrieves through `handle_simple` and stays; the second
        retrieves nothing and goes. A set widened until nothing is excluded
        would pass the first half of this and fail the second."""
        with tempfile.TemporaryDirectory() as t:
            root = clean(Path(t) / "runs")
            route = {"q00": "DeepQuery", "q01": "GenerativeQuery"}
            for r in (1, 2, 3):
                fixture(root, FULL_ARM, r,
                        [row(q, "K1", 0.7, 0.35, members=[("f1", True)],
                             intent=route.get(q, "KnowledgeQuery")) for q in QIDS])
            _code, b = study(root, Path(t) / "out", quiet=True)
            ug = b["ungrounded_route"]
            ok = ug["excluded"] == ["q01"] and b["categories"]["K1"]["n"] == 23
            return ok, f"excluded={ug['excluded']} n={b['categories']['K1']['n']}"

    def no_retrieving_arm_is_never_ran_not_zero():
        """PLANT: only closed-book ran. A count of 0 reads exactly like a board
        on which every question retrieved."""
        with tempfile.TemporaryDirectory() as t:
            root = Path(t) / "runs"
            for r in (1, 2, 3):
                fixture(root, CLOSED_BOOK_ARM, r,
                        [row(q, "K1", 0.1, 0.05, intent=None) for q in QIDS])
            ug = ungrounded_route_exclusions(load_runs(root)[0])
            ok = (ug["verdict"] == "never-ran" and ug["count"] is None
                  and ug["arms"] == [])
            return ok, f"{ug['verdict']} count={ug['count']}"

    case("clean-set-passes", clean_set_passes)
    case("band-is-bare-spread-over-its-runs", band_is_bare_spread_over_its_runs)
    case("recipe-hash-mismatch-refuses", recipe_hash_mismatch_refuses)
    case("synth-model-mismatch-refuses", synth_model_mismatch_refuses)
    case("host-mismatch-refuses", host_mismatch_refuses)
    case("ablation-sharing-ontology-refuses", ablation_sharing_fulls_ontology_refuses)
    case("ablation-other-chunks-refuses", ablation_with_other_chunks_refuses)
    case("closed-book-excludes-on-every-run", closed_book_excludes_only_on_every_run)
    case("closed-book-unmeasured-is-not-zero", closed_book_unmeasured_is_not_a_zero)
    case("no-closed-book-arm-is-never-ran", no_closed_book_arm_is_never_ran_not_zero)
    case("no-bare-arm-band-is-never-ran", no_bare_arm_band_is_never_ran_not_the_floor)
    case("category-under-20-could-not-judge", category_under_20_is_could_not_judge)
    case("found-missed-from-audit-trail", found_missed_comes_from_the_audit_trail)
    case("errored-row-is-not-a-zero", errored_row_is_not_scored_as_zero)
    case("empty-runs-dir-is-never-ran", empty_runs_dir_is_never_ran)
    case("half-written-run-is-reported", half_written_run_is_reported_not_dropped)
    case("table-renders-every-arm", table_renders_every_arm)
    case("sidebyside-block-per-question", block_per_question_three_columns)
    case("sidebyside-missing-arm-is-never-ran", missing_arm_is_a_never_ran_column)
    case("fabricated-no-vocabulary-is-never-ran", no_vocabulary_is_never_ran_not_zero)
    case("fabricated-non-gold-vocabulary-only", fabricated_counts_vocabulary_non_gold_only)
    case("path-bare-no-walk-full-renders", bare_has_no_walk_and_full_renders_the_path)
    case("path-reached-nothing-is-not-no-walk", walk_that_reached_nothing_is_not_no_walk)
    case("query-sharing-false-withholds", query_sharing_false_withholds_snippets)
    case("column-falls-to-next-measured-run", column_falls_to_the_next_run_that_measured)
    case("ungrounded-route-excludes-every-arm", ungrounded_route_excludes_the_question_from_every_arm)
    case("deep-query-retrieved-generative-did-not", a_deep_query_row_retrieved_and_a_generative_one_did_not)
    case("unrouted-row-is-not-an-exclusion", an_unrouted_row_is_not_an_exclusion)
    case("route-spellings-are-one-route", the_two_spellings_of_one_route_are_one_route)
    case("no-retrieving-arm-is-never-ran", no_retrieving_arm_is_never_ran_not_zero)

    for name, verdict, detail in results:
        print(f"  {name:<38} {verdict:<16} {detail}", file=sys.stderr)
    bad = [n for n, v, _ in results if v != "passed"]
    print(f"self-test: {len(results) - len(bad)}/{len(results)} passed",
          file=sys.stderr)
    return 1 if bad else 0


def main(argv):
    # The rung 6 call takes bare arm NAMES, so the subcommand is chosen on the
    # first token rather than by handing argparse the whole line: `compare.py
    # armA armB` must keep working exactly as README.md:63 calls it.
    if argv[:1] == ["--self-test"]:
        return self_test()
    if argv[:1] != ["study"]:
        return rung6(argv or ['armA', 'armB'])
    ap = argparse.ArgumentParser(prog="compare.py study",
                                 description="the EI7 stage-0 scoreboard")
    ap.add_argument("--runs", required=True,
                    help="runs root: <dir>/<arm>/run-<N>/{eval,manifest}.json")
    ap.add_argument("--out", required=True, help="where scoreboard.json lands")
    ap.add_argument("--vocabulary", default=None,
                    help="truth JSON naming the corpus's members; without it "
                         "the fabricated-member metric reads never-ran")
    ap.add_argument("--recipe", default=None,
                    help="the corpus recipe; `[corpus].query_sharing = false` "
                         "withholds snippets from the side-by-side")
    args = ap.parse_args(argv[1:])
    code, _board = study(os.path.expanduser(args.runs), os.path.expanduser(args.out),
                         vocabulary=args.vocabulary, recipe=args.recipe)
    return code


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
