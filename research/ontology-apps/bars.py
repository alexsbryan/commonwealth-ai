#!/usr/bin/env python3
"""The ontology-layer campaign's bar instruments: one reader per bar (quality/campaigns/ontology-layer.toml).

    bars.py <bar-id>

Each reader prints one JSON line, {"value": N, "commit": <sha or null>, "artifact": <path>}, as
scripts/co-lineage.py's _parse_value reads it; or exits 3 (the artifact it reads does not exist yet) or 1
(it exists and cannot be judged) with a one-line reason on stderr. Absence is never read as zero.

Selection rules, one per artifact kind; a reader never takes "something close":

  job run     a directory runs/<name>/ holding job.sh, or side jobs job-<side>.sh, and at least one leg. Newest =
              the latest mtime of any of its legs' DONE files.
  leg         a subdirectory of a job run, or of a side job's runs/<name>/<side>/, holding DONE (key=value lines) and a job.log whose
              "job start: name=... corpus=... recipe=<path>" line names its corpus and recipe. A leg whose
              name contains "smoke" is never chosen. Of a corpus's remaining legs the one with the most
              sections (DONE sections=) is chosen; a tie goes to the newer DONE.
  system      gvc, ward or uv. A recipe under a blind round is the system its round subdirectory names;
              otherwise a corpus id maps through CORPUS_SYSTEM (cdcr-gvc, crm-ward, uv-support).
  blind round ~/blind-author-<YYYYMMDD>[-r<N>]/ (no suffix is round 1); newest = the greatest (date, N).
              ~/blind-author-material-*/ is the authors' input, not a round, and never matches.
  blind recipe in a round's <system>/ directory, of recipe.toml and recipe-author.toml, the one file
              with a [corpus] table: recipe.toml is what BRIEF.md step 2 has the author write, and
              recipe-author.toml is the recipe-author skill ([skill]) the author worked from. Not exactly
              one such file is could-not-judge.
  qualifying  a run counts toward an identity bar only when its recipe path resolves under a blind round
              of that system. Our frozen-recipe runs (runs/scaffold-3sys, runs/e2-gvc-resolve-alone) never do.

Per bar:

  layer-default-path   the newest job run; per system (system_of: a blind recipe's round dir, else the corpus id;
                       blind GVC's corpus is `gvc`), its chosen leg counts when DONE has resolve_exit=0
                       AND ladder.run_system judges it (status "judged"). Read-tier: on runs/scaffold-3sys the
                       whole reader took 3.4 s warm, and the three ladders 7.1 s summed as separate cold
                       processes (2026-10-09); a fold big enough to pass 10 s reads could-not-judge (timeout).
  layer-invariants     run-tier (timeout_s): runs the contract tests (scripts/with-cargo-lock.sh scripts/sovereign-test.sh
                       --package sovereign-enrichment-build --filter layer_contract_tests), then counts the
                       contracts C1-C6 with at least one passing "contracts::cN_" test and no failing one in
                       target/sovereign-test/latest/cargo.jsonl. A cargo.jsonl not rewritten by this run, or
                       one holding no contract test, is could-not-judge (a build failure lands here).
  layer-identity-gvc   the newest (er-score.json mtime) runs/**/summary.json of resolve-statements whose
                       "recipe" qualifies for gvc; value = conll_f1 of the er-score.json in its parent dir.
  layer-identity-ward  the newest job run holding a qualifying non-smoke ward (uv) leg, that run's chosen
  layer-identity-uv    qualifying leg; value = ladder identity B3 F1 (resolve_exit must be 0, ladder judged).
                       The bar is defined on the tune fold: a leg whose sections leave out any of the
                       identity's gold units (ladder identity.gold_left_out != 0, e.g. uv's fixed third) is
                       could-not-judge, never read as the bar row.
  layer-estimator      per system, the newest runs/**/score_resolve.json carrying an `estimator` block (system
                       read through its "run"'s summary.json recipe); value = the greatest largest_gap over
                       the three systems, as score_resolve.py decided each. Any system missing: exit 3.
  layer-no-tuning      the newest blind round's three blind recipes; one per identity_evidential ENTRY and one
                       per document_reading / document_reader key, at any depth. (The floor's 13 over the
                       three frozen baseline recipes: 9 entries + 4 keys; test_bars.py proves the unit.)

  layer-author-gap     agreement.py outputs under blind-author/. The reference is reference/agreement.json (our
                       recipe against itself across two builds; both sides ours, else could-not-judge). The reading
                       is the */agreement.json whose blind side ran the newest blind round and whose ours side is
                       ours, newest by the latest DONE of its ours runs. Per row (gvc, ward, uv, gvc-resolve-alone)
                       ratio = reading B3 F1 / reference B3 F1; value = the least ratio. A row unjudged on either
                       side, or a zero reference, is could-not-judge naming the row; no reference or no reading of
                       the newest round is absent.
"""
import json, pathlib, re, subprocess, sys, time, tomllib

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parents[1]
RUNS = REPO / "runs"
HOME = pathlib.Path.home()

SYSTEMS = ("gvc", "ward", "uv")
CORPUS_SYSTEM = {"cdcr-gvc": "gvc", "crm-ward": "ward", "uv-support": "uv"}
sys.path.insert(0, str(HERE))
import ladder  # noqa: E402  (the one decider for blind rounds, runs and ladders)

ROUND = ladder.ROUND
JOB_START = re.compile(r"job start: name=(\S+) corpus=(\S+) recipe=(\S+)")
RECIPE_CANDIDATES = ("recipe.toml", "recipe-author.toml")
TUNING_KEYS = ("document_reading", "document_reader")
CONTRACT = re.compile(r"(?:^|::)contracts::c([1-6])_")
CONTRACT_TEST = ["scripts/with-cargo-lock.sh", "scripts/sovereign-test.sh",
                 "--package", "sovereign-enrichment-build", "--filter", "layer_contract_tests"]
CARGO_JSONL = REPO / "target/sovereign-test/latest/cargo.jsonl"
AUTHOR_GAP_ROWS = ("gvc", "ward", "uv", "gvc-resolve-alone")


class Absent(Exception):
    """The artifact a bar reads does not exist yet: exit 3."""


class CannotJudge(Exception):
    """The artifact exists and cannot be judged: exit 1."""


def emit(value, artifact, commit=None):
    print(json.dumps({"value": value, "commit": commit, "artifact": str(artifact)}))


def note(msg):
    print(msg, file=sys.stderr)


# ---------------------------------------------------------------- systems and blind rounds
def round_key(path):
    m = ROUND.match(path.name)
    return (m.group(1), int(m.group(2) or 1)) if m else None


def blind_rounds(home=HOME):
    """Every blind round directory, oldest first."""
    found = [p for p in home.glob("blind-author-*") if p.is_dir() and round_key(p)]
    return sorted(found, key=round_key)


def blind_system(recipe, home=HOME):
    """The system a recipe path belongs to when it lies under a blind round, else None (ladder.blind_of decides)."""
    found = ladder.blind_of(recipe, home)
    return found[1] if found else None


def system_of(recipe, home=HOME, corpus=None):
    """The one map from a recipe (and the corpus id a job named) to a system, or None."""
    s = blind_system(recipe, home)
    if s:
        return s
    if corpus is None:
        try:
            corpus = tomllib.loads(pathlib.Path(recipe).read_text()).get("corpus", {}).get("id")
        except (OSError, tomllib.TOMLDecodeError):
            return None
    return CORPUS_SYSTEM.get(corpus)


def head_of(path, runs=RUNS):
    """The commit the run's binary was built from (bin/head.txt, nearest ancestor under runs/), or None."""
    p = pathlib.Path(path).resolve()
    stop = runs.resolve()
    while p != stop and stop in p.parents:
        h = p / "bin/head.txt"
        if h.exists():
            return h.read_text().strip() or None
        p = p.parent
    return None


# ---------------------------------------------------------------- job runs and legs
def read_done(path):
    return dict(l.split("=", 1) for l in path.read_text().splitlines() if "=" in l)


def leg_of(d):
    """{"path", "name", "corpus", "recipe", "done", "mtime"} for a leg directory, else None."""
    done, log = d / "DONE", d / "job.log"
    if not (done.is_file() and log.is_file()):
        return None
    with open(log, errors="replace") as f:
        m = next((m for line in f for m in [JOB_START.search(line)] if m), None)
    if not m:
        return None
    return {"path": d, "name": d.name, "corpus": m.group(2), "recipe": m.group(3),
            "done": read_done(done), "mtime": done.stat().st_mtime}


def job_runs(runs=RUNS):
    """[(run dir, [legs])] for every job run, newest first. A run of side jobs (job-<side>.sh, as runs/converge-s2's
    job-ours.sh and job-blind.sh) keeps each side's legs under <side>/."""
    out = []
    for r in sorted(p for p in runs.glob("*") if (p / "job.sh").is_file() or any(p.glob("job-*.sh"))):
        dirs = [d for d in sorted(r.iterdir()) if d.is_dir()]
        dirs += [d for s in sorted(j.stem[len("job-"):] for j in r.glob("job-*.sh")) if (r / s).is_dir()
                 for d in sorted((r / s).iterdir()) if d.is_dir()]
        legs = [l for d in dirs for l in [leg_of(d)] if l]
        if legs:
            out.append((r, legs))
    return sorted(out, key=lambda rl: max(l["mtime"] for l in rl[1]), reverse=True)


def sections(leg):
    try:
        return int(leg["done"].get("sections", ""))
    except ValueError:
        return -1


def choose_leg(legs):
    """The leg chosen among one corpus's legs: never a smoke leg; most sections, then newest DONE."""
    kept = [l for l in legs if "smoke" not in l["name"]]
    return max(kept, key=lambda l: (sections(l), l["mtime"])) if kept else None


def judge_leg(system, leg):
    """ladder.run_system over a leg: the one accessor for a system's ladder."""
    return ladder.run_system(system, leg["path"].resolve())


# ---------------------------------------------------------------- the readers
def default_path(runs=RUNS, judge=judge_leg, home=HOME):
    found = job_runs(runs)
    if not found:
        raise Absent(f"no job run (runs/*/job.sh with a leg) under {runs}")
    run, legs = found[0]
    n = 0
    for system in SYSTEMS:
        leg = choose_leg([l for l in legs if system_of(l["recipe"], home, l["corpus"]) == system])
        if leg is None:
            note(f"{system}: no non-smoke leg in {run.name}")
            continue
        rx = leg["done"].get("resolve_exit")
        r = judge(system, leg) if rx == "0" else {"status": "not judged", "reason": f"resolve_exit={rx}"}
        ok = rx == "0" and r.get("status") == "judged"
        n += ok
        note(f"{system}: leg {leg['name']} resolve_exit={rx} ladder={r.get('status')}"
             + ("" if ok else f" ({r.get('reason')})"))
    emit(n, run, head_of(run, runs))


def identity_leg(system, runs=RUNS, home=HOME, judge=judge_leg):
    """ward / uv: B3 F1 of the newest qualifying leg (blind recipe), through ladder identity."""
    for run, legs in job_runs(runs):
        mine = [l for l in legs if blind_system(l["recipe"], home) == system]
        leg = choose_leg(mine)
        if leg is None:
            continue
        rx = leg["done"].get("resolve_exit")
        if rx != "0":
            raise CannotJudge(f"{leg['path']}: resolve_exit={rx}")
        r = judge(system, leg)
        if r.get("status") != "judged":
            raise CannotJudge(f"{leg['path']}: ladder {r.get('status')}: {r.get('reason')}")
        left = (r.get("identity") or {}).get("gold_left_out")
        if left != 0:
            raise CannotJudge(f"{leg['path']}: its sections are not the bar's population (the tune fold): "
                              f"{left} of the identity's gold units lie outside them; a slice is never the bar row")
        emit(r["identity"]["b_cubed"], leg["path"], head_of(leg["path"], runs))
        return
    raise Absent(f"no {system} job leg under {runs} ran a blind author's recipe (~/blind-author-<round>/{system}/)")


def identity_gvc(runs=RUNS, home=HOME):
    """gvc: conll_f1 of the newest resolve-statements run over a blind gvc recipe."""
    best = None
    for summary in runs.rglob("summary.json"):
        try:
            recipe = json.loads(summary.read_text()).get("recipe")
        except (json.JSONDecodeError, OSError, AttributeError):
            continue
        if not recipe or blind_system(recipe, home) != "gvc":
            continue
        er = summary.parent.parent / "er-score.json"
        if not er.exists():
            raise Absent(f"{summary} ran a blind gvc recipe but {er} does not exist")
        if best is None or er.stat().st_mtime > best.stat().st_mtime:
            best = er
    if best is None:
        raise Absent(f"no resolve-statements summary.json under {runs} names a blind gvc recipe")
    v = json.loads(best.read_text()).get("conll_f1")
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        raise CannotJudge(f"{best} carries no numeric conll_f1")
    emit(round(v, 4), best, head_of(best, runs))


def estimator(runs=RUNS, home=HOME):
    newest = {}
    for path in runs.rglob("score_resolve.json"):
        try:
            row = json.loads(path.read_text())
        except (json.JSONDecodeError, OSError):
            continue
        if not isinstance(row, dict) or not isinstance(row.get("estimator"), dict):
            continue
        summary = pathlib.Path(row.get("run", "")) / "summary.json"
        try:
            recipe = json.loads(summary.read_text()).get("recipe")
        except (json.JSONDecodeError, OSError, AttributeError):
            note(f"{path}: its run's summary.json ({summary}) is unreadable; its system is unknown, skipped")
            continue
        system = system_of(recipe, home) if recipe else None
        if system is None:
            note(f"{path}: recipe {recipe} maps to no system, skipped")
            continue
        if system not in newest or path.stat().st_mtime > newest[system][0].stat().st_mtime:
            newest[system] = (path, row["estimator"])
    missing = [s for s in SYSTEMS if s not in newest]
    if missing:
        raise Absent(f"no score_resolve.json estimator block for {', '.join(missing)}"
                     + (f" (have {', '.join(sorted(newest))})" if newest else ""))
    gaps = {}
    for s, (path, block) in newest.items():
        lg = block.get("largest_gap")
        if not (isinstance(lg, list) and lg and isinstance(lg[0], (int, float))):
            raise CannotJudge(f"{path}: estimator block has no largest_gap")
        gaps[s] = (lg[0], path)
        note(f"{s}: largest gap {lg[0]} ({lg[1] if len(lg) > 1 else '?'}) in {path}")
    worst = max(gaps, key=lambda s: gaps[s][0])
    emit(gaps[worst][0], gaps[worst][1], head_of(gaps[worst][1], runs))


def count_tuning(node):
    """Recipe fields carrying a measured number or a mechanism switch: one per identity_evidential entry, one
    per document_reading / document_reader key, at any depth."""
    if isinstance(node, list):
        return sum(count_tuning(x) for x in node)
    if not isinstance(node, dict):
        return 0
    n = 0
    for k, v in node.items():
        if k == "identity_evidential":
            n += len(v) if isinstance(v, list) else 1
        elif k in TUNING_KEYS:
            n += 1
        else:
            n += count_tuning(v)
    return n


def blind_recipe(system_dir):
    """The one recipe file a round's system directory holds: the candidate with a [corpus] table."""
    found = []
    for name in RECIPE_CANDIDATES:
        p = system_dir / name
        if p.is_file():
            try:
                doc = tomllib.loads(p.read_text())
            except tomllib.TOMLDecodeError as e:
                raise CannotJudge(f"{p} is not TOML: {e}")
            if "corpus" in doc:
                found.append((p, doc))
    if len(found) != 1:
        raise CannotJudge(f"{system_dir}: {len(found)} of {RECIPE_CANDIDATES} carry a [corpus] table; want one")
    return found[0]


def no_tuning(home=HOME):
    rounds = blind_rounds(home)
    if not rounds:
        raise Absent(f"no blind round (~/blind-author-<YYYYMMDD>[-rN]/) under {home}")
    rnd = rounds[-1]
    total = 0
    for system in SYSTEMS:
        d = rnd / system
        if not d.is_dir():
            raise Absent(f"{rnd} has no {system}/ directory")
        path, doc = blind_recipe(d)
        n = count_tuning(doc)
        note(f"{system}: {n} in {path}")
        total += n
    emit(total, rnd)


def contracts_held(jsonl):
    """How many of C1-C6 have a passing contracts::cN_ test and no failing one; None when none appears."""
    passed, failed = set(), set()
    for line in pathlib.Path(jsonl).read_text().splitlines():
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            continue
        m = CONTRACT.search(str(row.get("n", ""))) if isinstance(row, dict) else None
        if m:
            (passed if row.get("t") == "pass" else failed).add(int(m.group(1)))
    if not passed | failed:
        return None
    return len(passed - failed)


def invariants(jsonl=CARGO_JSONL, cmd=CONTRACT_TEST):
    started = time.time()
    proc = subprocess.run(cmd, cwd=REPO, capture_output=True, text=True, stdin=subprocess.DEVNULL)
    sys.stderr.write(proc.stdout + proc.stderr)  # stdout carries only the value line
    if not jsonl.exists() or jsonl.stat().st_mtime < started:
        raise CannotJudge(f"test run exited {proc.returncode} and did not rewrite {jsonl}")
    n = contracts_held(jsonl)
    if n is None:
        raise CannotJudge(f"test run exited {proc.returncode}; {jsonl} holds no contracts::cN_ test")
    emit(n, jsonl)


def run_path(p, repo=REPO):
    p = pathlib.Path(p)
    return p if p.is_absolute() else repo / p


def agreement_rows(path):
    """{row: (b3 f1, both_placed)} for every row of an agreement.json, a str where the row was not judged."""
    doc = json.loads(path.read_text())
    out = {}
    for row in AUTHOR_GAP_ROWS:
        a = doc.get(row, {}).get("agreement") if isinstance(doc.get(row), dict) else None
        f = a.get("b_cubed", {}).get("f1") if isinstance(a, dict) else None
        out[row] = (f, a.get("both_placed")) if isinstance(f, (int, float)) else str(a or "absent")
    return doc, out


def sides(doc, side):
    return {row: run_path(doc[row][side]["run"]) for row in AUTHOR_GAP_ROWS
            if isinstance(doc.get(row), dict) and isinstance(doc[row].get(side), dict) and doc[row][side].get("run")}


def round_of(runs, home):
    """The one blind round every run was made under, None when any run is ours, or why not (a str)."""
    found = {ladder.blind_of(r, home) if r else None for r in (ladder.run_recipe(p) for p in runs.values())}
    rounds = {f[0] if f else None for f in found}
    return next(iter(rounds)) if len(rounds) == 1 else f"runs span {sorted(map(str, rounds))}"


def author_gap(blind_author=HERE / "blind-author", home=HOME):
    ref_path = blind_author / "reference/agreement.json"
    if not ref_path.is_file():
        raise Absent(f"no reference {ref_path} (agreement.py, our recipe against itself across two builds)")
    rounds = blind_rounds(home)
    if not rounds:
        raise Absent(f"no blind round (~/blind-author-<YYYYMMDD>[-rN]/) under {home}")
    newest = rounds[-1].name
    readings = []
    for p in sorted(blind_author.glob("*/agreement.json")):
        if p == ref_path:
            continue
        doc = json.loads(p.read_text())
        rnd = round_of(sides(doc, "blind"), home)
        if rnd != newest:
            note(f"{p}: blind side {rnd or 'is ours'}, not the newest round {newest}; skipped")
            continue
        ours = sides(doc, "ours")
        if round_of(ours, home) is not None:
            note(f"{p}: its ours side is not our recipe; skipped")
            continue
        dones = [d / "DONE" for d in ours.values()]
        if not dones or not all(d.is_file() for d in dones):
            raise CannotJudge(f"{p}: an ours run has no DONE ({[str(d) for d in dones if not d.is_file()]})")
        readings.append((max(d.stat().st_mtime for d in dones), p))
    if not readings:
        raise Absent(f"no blind-author/*/agreement.json reads round {newest} against our runs")
    _, path = max(readings)
    ref_doc, ref = agreement_rows(ref_path)
    if round_of(sides(ref_doc, "blind"), home) is not None or round_of(sides(ref_doc, "ours"), home) is not None:
        raise CannotJudge(f"{ref_path}: the reference holds a blind run; it must be ours against ours")
    _, got = agreement_rows(path)
    ratios = {}
    for row in AUTHOR_GAP_ROWS:
        if isinstance(ref[row], str) or not ref[row][0]:
            raise CannotJudge(f"{row}: the reference {ref_path} reads {ref[row]}, no denominator")
        if isinstance(got[row], str):
            raise CannotJudge(f"{row}: {path} reads {got[row]}")
        ratios[row] = got[row][0] / ref[row][0]
        note(f"{row}: blind-vs-ours B3 {got[row][0]} ({got[row][1]} units) / ours-vs-ours {ref[row][0]} "
             f"({ref[row][1]} units) = {ratios[row]:.3f}")
    least = min(ratios, key=ratios.get)
    note(f"least: {least}")
    emit(round(ratios[least], 3), path, head_of(sides(json.loads(path.read_text()), "ours")["gvc"]))


READERS = {
    "layer-default-path": default_path,
    "layer-invariants": invariants,
    "layer-identity-gvc": identity_gvc,
    "layer-identity-ward": lambda: identity_leg("ward"),
    "layer-identity-uv": lambda: identity_leg("uv"),
    "layer-estimator": estimator,
    "layer-no-tuning": no_tuning,
    "layer-author-gap": author_gap,
}


def main(argv):
    if len(argv) != 2 or argv[1] not in READERS:
        note(f"usage: bars.py <{'|'.join(READERS)}>")
        return 2
    try:
        READERS[argv[1]]()
    except Absent as e:
        note(f"artifact-absent: {e}")
        return 3
    except CannotJudge as e:
        note(f"could-not-judge: {e}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
