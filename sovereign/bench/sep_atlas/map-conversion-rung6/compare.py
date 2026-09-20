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
`scoreboard.json` plus a fixed-width category x arm table. It REFUSES arms that
are not comparable (PRE-REG-custom-ontology-and-raptor-2026-09-17, "Corpora and
held-out truth" > Freeze / Model and host, and check I3): that refusal is the
verdict `could-not-judge` and exit 4, never a number computed across two
different builds, models or hosts.

No bar is read here. The per-category verdict answers judgeability only
(pre-reg "Bars": a kind under n = 20 after closed-book exclusions is
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
    excluded = set(closed["excluded"])

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
        entry = {"n": n, "band": bands[cat], "arms": {}}
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


def study(runs_root, out_dir, quiet=False):
    """`(exit_code, scoreboard)`. 0 clean · 2 nothing to compare · 4 refused."""
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
    if not quiet:
        print_table(board)
    return 0, board


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

    def row(qid, category, judge, kw, members=None, error=None):
        """One eval.json result. `members` is [(fact, present)]."""
        ev = [{"fact": f, "present": p, "evidence": "q" if p else "(absent)"}
              for f, p in (members or [])]
        r = {"question_id": qid, "category": category, "question": qid,
             "retrieved": [], "fact_score": {"matched": [], "missing": [],
                                             "total_expected": 1, "ratio": kw},
             "synth": {"answer": "a",
                       # PLANT: the rollup's name lists are EMPTY while the
                       # audit trail is not. found/missed read off `matched`
                       # would be 0 here and the table would not notice.
                       "judge_fact_score": {"matched": [], "missing": [],
                                            "total_expected": len(ev) or 1,
                                            "ratio": judge},
                       "judge_evidence": ev}}
        if error:
            r["error"] = error
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
            for r in (1, 2, 3):
                fixture(root, arm, r,
                        [row(q, "K1", j + 0.01 * r, j / 2,
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
    args = ap.parse_args(argv[1:])
    code, _board = study(os.path.expanduser(args.runs), os.path.expanduser(args.out))
    return code


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
