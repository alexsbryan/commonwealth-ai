#!/usr/bin/env python3
"""Blind essay judge over arm runs: plot-point coverage, a 1-5 rubric, pairwise.

    essay_judge.py --runs raptor-proof/runs/<slug> --bank essay/bank-<slug>.toml \
        --daemon http://host:port --out <dir> [--model commonwealth/primary] [--seed 7]
    essay_judge.py --self-test            # no network; planted cases

Reads `<runs>/<arm>/run-<N>/eval.json` (harness/run_arm.py) for arms bare, deep,
full and, when present, closed-book. The reference is the bank's `answer1` (the
held-out plot summary, identical on every row).

Inventory checked first: `svrn eval run --essay-judge` scores the RETRIEVED SET
on four 0-3 axes with no reference and no answer in view; the run's own
`judge_fact_score` checks expected_facts, which are a formality in an essay
bank. Neither reads the answer against a held-out summary, so neither serves.

BLIND: no prompt names an arm, and `scrub` removes the `[Source: <corpus>]` tags
the synth writes (only `full` cites the raptor corpus). `arm_leaks` on the board
counts answers where an arm-revealing token survived the scrub.

Four verdicts per cell, never two: a missing arm or run, a never-ran manifest or
a row carrying `error` is never-ran (excluded, counted); a judge reply that
cannot be parsed is could-not-judge (excluded, counted); an EMPTY answer is a
real 0 and costs no judge call. Questions that took a non-retrieving route in
any retrieving arm leave the whole board, by compare.py's own rule.

Every judge exchange is saved under `<out>/raw/<kind>/`, keyed by a hash of the
prompt, and an existing record is reused: a killed window resumes for free, and
`points.json` / `relevance.json` are decided once, before any answer is read.
"""
import argparse, collections, hashlib, json, math, re, statistics, sys, tempfile, threading, time, tomllib
import urllib.error, urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
sys.path.insert(0, str(REPO / "sovereign/bench/sep_atlas/map-conversion-rung6"))
sys.path.insert(0, str(HERE.parent.parent / "harness"))
from compare import is_grounded_route, measured, route_of  # noqa: E402  one route rule, one error rule
from run_arm import _ALIAS_RE  # noqa: E402  one reading of /v1/models `owned_by`
import walk_reach  # noqa: E402  one definition of "did the walk reach this kind"

ARMS = ("bare", "deep", "full")          # the retrieving arms
CLOSED_BOOK = "closed-book"              # optional; reported, never paired
PAIRS = (("full", "bare"), ("full", "deep"))
RUBRIC = ("specificity", "synthesis", "faithfulness")
LABELS = ("correct", "contradicted", "absent")
MAX_ANSWER_CHARS = 12000                 # per answer shown to the judge; longer is cut and counted
BUSY_CODES = (429, 502, 503, 504)
BUSY_ATTEMPTS, BUSY_BACKOFF_S = 6, 5.0   # 5, 10, 20, 40, 80 s

P_POINTS = """Split this plot summary of a novel into atomic plot points: one event, fact or stated theme of the story per point, each a short sentence that names its characters.
Return only JSON: {{"points": ["...", "..."]}}

Summary:
{reference}"""
P_RELEVANCE = """Below are numbered plot points of a novel and an exam question about it.
Which points should a good answer to the question draw on?
Return only JSON: {{"relevant": ["P1", "P4"]}}

Question: {question}

Points:
{points}"""
P_COVERAGE = """Below are numbered plot points of a novel and an answer written about the novel.
Label every point by what the answer says about it:
"correct" = the answer states it
"contradicted" = the answer says something that conflicts with it
"absent" = the answer does not mention it
Return only JSON with one entry per point: {{"P1": "correct", "P2": "absent"}}

Points:
{points}

Answer:
{answer}"""
P_RUBRIC = """Grade this answer to an exam question about a novel. The reference is a trusted plot summary.
Score each from 1 (poor) to 5 (excellent):
specificity = names particular events and characters, not generalities
synthesis = connects events from across the book, not one scene
faithfulness = says nothing the reference contradicts
Return only JSON: {{"specificity": 3, "synthesis": 3, "faithfulness": 3}}

Question: {question}

Reference:
{reference}

Answer:
{answer}"""
P_PAIR = """Two answers to an exam question about a novel. The reference is a trusted plot summary.
Which answer is better: more specific about events, connects more of the story, and says nothing the reference contradicts? Ignore length and style.
Return only JSON: {{"winner": "A"}} or {{"winner": "B"}} or {{"winner": "tie"}}

Question: {question}

Reference:
{reference}

Answer A:
{a}

Answer B:
{b}"""
NUDGE = "\n\nReturn only the JSON object, nothing else."


class Busy(Exception):
    """The daemon could not take the call right now; worth waiting for."""


def extract_json(text):
    """The first JSON object in a reply, past think-blocks, fences and prose."""
    text = re.sub(r"<think>.*?</think>", "", text or "", flags=re.S)
    dec = json.JSONDecoder()
    for m in re.finditer(r"\{", text):
        try:
            obj, _ = dec.raw_decode(text[m.start():])
        except ValueError:
            continue
        if isinstance(obj, dict):
            return obj
    return None


def scrub(answer, corpus):
    """The answer as the judge sees it: source tags out, length capped."""
    a = re.sub(r"\s*\[Source:[^\]]*\]", "", answer or "").strip()
    return a[:MAX_ANSWER_CHARS], len(a) > MAX_ANSWER_CHARS, bool(re.search(r"(?i)raptor", a) or (corpus and corpus in a))


def http_transport(daemon, model, seed, max_tokens, timeout):
    url = daemon.rstrip("/") + "/v1/chat/completions"

    def send(prompt):
        body = {"model": model, "messages": [{"role": "user", "content": prompt}], "temperature": 0,
                "max_tokens": max_tokens, "seed": seed, "stream": False,
                "chat_template_kwargs": {"enable_thinking": False}}
        req = urllib.request.Request(url, data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                doc = json.loads(resp.read().decode())
        except urllib.error.HTTPError as e:
            if e.code in BUSY_CODES:
                raise Busy(f"HTTP {e.code}") from e
            raise
        except (urllib.error.URLError, TimeoutError, ConnectionError) as e:
            raise Busy(f"{type(e).__name__}: {e}") from e
        msg = (doc.get("choices") or [{}])[0].get("message") or {}
        return {"content": msg.get("content") or msg.get("reasoning_content") or "", "model": doc.get("model")}
    return send


class Judge:
    def __init__(self, transport, out, workers=1, sleep=time.sleep, salt=""):
        # `salt` (model|seed) is in every record's key: a second judge pointed at the same --out reuses nothing
        self.transport, self.raw, self.workers, self.sleep, self.salt = transport, Path(out) / "raw", workers, sleep, salt
        self.calls = self.cached = self.unparsed = 0
        self.models, self.lock = set(), threading.Lock()

    def ask(self, kind, key, prompt, check):
        """Parsed reply, or None = could-not-judge. Every attempt lands in the raw record."""
        path = self.raw / kind / f"{key}-{hashlib.sha256((self.salt + prompt).encode()).hexdigest()[:10]}.json"
        if path.exists():
            rec = json.loads(path.read_text())
            with self.lock:
                self.cached += 1
                self.models.update(a["model"] for a in rec["attempts"] if a.get("model"))
            return rec["parsed"]
        attempts, parsed = [], None
        for text in (prompt, prompt + NUDGE):   # temperature 0: resending the SAME prompt returns the same reply
            reply = self._send(text)
            got = extract_json(reply["content"])
            attempts.append({"nudged": text is not prompt, "reply": reply["content"], "model": reply.get("model")})
            if got is not None and check(got):
                parsed = got
                break
        with self.lock:
            self.unparsed += parsed is None
            self.models.update(a["model"] for a in attempts if a.get("model"))
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps({"kind": kind, "key": key, "prompt": prompt, "attempts": attempts, "parsed": parsed}, indent=1))
        return parsed

    def _send(self, text):
        for n in range(BUSY_ATTEMPTS):
            try:
                with self.lock:
                    self.calls += 1
                return self.transport(text)
            except Busy as e:
                if n == BUSY_ATTEMPTS - 1:
                    raise SystemExit(f"essay_judge: daemon busy {BUSY_ATTEMPTS} times in a row ({e}); rerun to resume from raw/")
                wait = BUSY_BACKOFF_S * 2 ** n
                print(f"essay_judge: {e}; retry in {wait:.0f}s", file=sys.stderr)
                self.sleep(wait)

    def ask_many(self, jobs):
        """jobs: [(kind, key, prompt, check)] -> [parsed], in job order."""
        with ThreadPoolExecutor(max_workers=self.workers) as pool:
            return list(pool.map(lambda j: self.ask(*j), jobs))


# ---- reading the runs -------------------------------------------------------

def load_runs(runs_dir):
    """{arm: {run_no: {qid: row}}}; an arm with no usable run is absent -> never-ran."""
    out, synth_models, skipped = {}, set(), []
    for arm in ARMS + (CLOSED_BOOK,):
        for d in sorted(Path(runs_dir, arm).glob("run-*")):
            m = re.fullmatch(r"run-(\d+)", d.name)
            try:
                manifest = json.loads((d / "manifest.json").read_text()) if (d / "manifest.json").exists() else {}
                rows = json.loads((d / "eval.json").read_text())["results"]
            except (OSError, ValueError, KeyError) as e:
                skipped.append(f"{arm}/{d.name}: {type(e).__name__}")
                continue
            if not m or manifest.get("verdict") == "never-ran":
                skipped.append(f"{arm}/{d.name}: manifest never-ran")
                continue
            sm = (manifest.get("identity") or {}).get("synth_model")
            if sm:
                synth_models.add(sm)
            out.setdefault(arm, {})[int(m.group(1))] = {r["question_id"]: r for r in rows if measured(r)}
    return out, synth_models, skipped


def factor_reach(runs):
    """Per arm: on how many DISTINCT questions did the walk reach each atom kind.

    The board reports coverage per arm and says nothing about whether the thing
    the arm is FOR was ever touched. On 2026-09-21 that gap produced a readable
    board with an unreadable result: `full` scored 0.287 against bare's 0.300
    while the walk reached a `Summary` on 2 of the 12 questions, because the
    `trajectory` navigation row seven of them land on seeds no summaries. A
    null there means "not reached", and on the board it was indistinguishable
    from "did not help".

    Counted as reached if ANY run of the arm reached it — deliberately the
    generous direction. If even the union is thin, the factor was not under
    test, and a verdict that does not say so is a claim the data cannot carry
    (ARCH §18.3).
    """
    out = {}
    for arm, by_run in runs.items():
        qids = set().union(*(set(r) for r in by_run.values())) if by_run else set()
        per_kind = collections.Counter()
        for qid in qids:
            rows_for_q = [r[qid] for r in by_run.values() if qid in r]
            _nodes, reached_here = walk_reach.reach_counts(rows_for_q)
            per_kind.update(reached_here.keys())
        out[arm] = {"questions": len(qids), "reached": dict(per_kind)}
    return out


def route_exclusions(runs):
    bad = {}
    for arm in ARMS:
        for n, rows in runs.get(arm, {}).items():
            for qid, row in rows.items():
                route = route_of(row)
                if route is not None and not is_grounded_route(route):
                    bad.setdefault(qid, set()).add(f"{arm}/run-{n}:{route}")
    return {q: sorted(v) for q, v in sorted(bad.items())}


# ---- scoring ----------------------------------------------------------------

def numbered(points):
    return "\n".join(f"P{i}. {p}" for i, p in enumerate(points, 1))


def score_coverage(parsed, n_points, relevant):
    """coverage over judged points; `relevant` restricts the denominator. None = could-not-judge."""
    if parsed is None:
        return None
    lab = {f"P{i}": str(parsed.get(f"P{i}", "")).strip().lower() for i in range(1, n_points + 1)}
    judged = {k: v for k, v in lab.items() if v in LABELS}
    if not judged:
        return None
    rel = [k for k in judged if k in relevant]
    return {"coverage_all": sum(v == "correct" for v in judged.values()) / len(judged),
            "coverage_relevant": (sum(judged[k] == "correct" for k in rel) / len(rel)) if rel else None,
            "contradictions": sum(v == "contradicted" for v in judged.values()),
            "unjudged_points": n_points - len(judged), "labels": lab}


def pair_outcome(first, second):
    """`first`: reply with OUR arm shown as A; `second`: our arm shown as B. A win needs both orders."""
    if first is None or second is None:
        return "could-not-judge"
    ours = (first["winner"] == "A", second["winner"] == "B")
    theirs = (first["winner"] == "B", second["winner"] == "A")
    return "win" if all(ours) else "loss" if all(theirs) else "tie"


def sign_test(wins, losses):
    """Two-sided exact binomial p on wins vs losses; ties carry no sign. None when there is nothing to test."""
    n, k = wins + losses, min(wins, losses)
    return None if n == 0 else min(1.0, 2 * sum(math.comb(n, i) for i in range(k + 1)) / 2 ** n)


def mean(xs):
    xs = [x for x in xs if x is not None]
    return statistics.fmean(xs) if xs else None


def judge_runs(judge, bank, runs, out):
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    questions = {q["id"]: q["question"] for q in bank["questions"]}
    refs = {q.get("answer1", "").strip() for q in bank["questions"]}
    if len(refs) != 1 or not next(iter(refs)):
        raise SystemExit("essay_judge: refused: the bank's rows must share ONE non-empty `answer1` reference")
    reference, corpus = refs.pop(), bank["bank"].get("corpus", "")

    # 1. points, once.  2. relevance, once per question, before any answer is read.
    got = judge.ask("points", "reference", P_POINTS.format(reference=reference),
                    lambda d: isinstance(d.get("points"), list) and len([p for p in d["points"] if str(p).strip()]) >= 2)
    if got is None:
        raise SystemExit("essay_judge: could-not-judge: the judge returned no plot points; see raw/points/")
    points = [str(p).strip() for p in got["points"] if str(p).strip()]
    (out / "points.json").write_text(json.dumps({"reference_words": len(reference.split()), "points": points}, indent=1))
    ids = {f"P{i}" for i in range(1, len(points) + 1)}
    rel = judge.ask_many([("relevance", qid, P_RELEVANCE.format(question=q, points=numbered(points)),
                           lambda d: isinstance(d.get("relevant"), list)) for qid, q in questions.items()])
    relevance = {qid: (None if r is None else sorted(ids & {str(x).strip() for x in r["relevant"]}, key=lambda s: int(s[1:])))
                 for qid, r in zip(questions, rel)}
    (out / "relevance.json").write_text(json.dumps(relevance, indent=1))

    excluded = route_exclusions(runs)
    included = [q for q in questions if q not in excluded]
    cells, jobs, leaks, cut = {}, [], 0, 0          # cells[(arm, run, qid)] = scrubbed answer
    for arm, by_run in runs.items():
        for n, rows in by_run.items():
            for qid in included:
                if qid in rows:
                    a, was_cut, leak = scrub((rows[qid].get("synth") or {}).get("answer"), corpus)
                    cells[(arm, n, qid)], leaks, cut = a, leaks + leak, cut + was_cut
                    if a:
                        key = f"{qid}--{arm}-r{n}"
                        jobs.append(("coverage", key, P_COVERAGE.format(points=numbered(points), answer=a),
                                     lambda d: any(str(d.get(k, "")).strip().lower() in LABELS for k in ids)))
                        jobs.append(("rubric", key, P_RUBRIC.format(question=questions[qid], reference=reference, answer=a),
                                     lambda d: all(isinstance(d.get(k), (int, float)) and 1 <= d[k] <= 5 for k in RUBRIC)))
    pair_cells = [(x, y, n, qid) for x, y in PAIRS for n in sorted(set(runs.get(x, {})) & set(runs.get(y, {})))
                  for qid in included if (x, n, qid) in cells and (y, n, qid) in cells]
    for x, y, n, qid in pair_cells:
        ax, ay = cells[(x, n, qid)], cells[(y, n, qid)]
        if ax and ay:
            for tag, a, b in (("ab", ax, ay), ("ba", ay, ax)):
                jobs.append(("pairwise", f"{qid}--{x}-vs-{y}-r{n}-{tag}",
                             P_PAIR.format(question=questions[qid], reference=reference, a=a, b=b),
                             lambda d: str(d.get("winner", "")).strip() in ("A", "B", "tie")))
    print(f"essay_judge: {len(points)} points, {len(included)}/{len(questions)} questions on the board, "
          f"{len(jobs)} answer/pair judge calls planned (raw/ records are reused)", file=sys.stderr)
    cov, rub, pw = {}, {}, {}
    for (kind, key, _, _), parsed in zip(jobs, judge.ask_many(jobs)):
        {"coverage": cov, "rubric": rub, "pairwise": pw}[kind][key] = parsed

    per_cell = {}
    for (arm, n, qid), a in cells.items():
        key = f"{qid}--{arm}-r{n}"
        if not a:   # an empty answer is a measured zero, not a missing measurement
            c = {"coverage_all": 0.0, "coverage_relevant": 0.0 if relevance.get(qid) else None, "contradictions": 0, "unjudged_points": 0}
            r, status = dict.fromkeys(RUBRIC, 0), "empty-answer"
        else:
            c, r = score_coverage(cov.get(key), len(points), set(relevance.get(qid) or [])), rub.get(key)
            status = "judged" if c and r else "could-not-judge"
        per_cell[key] = {"arm": arm, "run": n, "question_id": qid, "status": status, "answer_chars": len(a),
                         "coverage": c, "rubric": r and {k: r[k] for k in RUBRIC}}

    arms = {}
    for arm in ARMS + (CLOSED_BOOK,):
        if arm not in runs:
            arms[arm] = {"verdict": "never-ran", "runs": []}
            continue
        per_run = []
        for n in sorted(runs[arm]):
            cs = [c for c in per_cell.values() if c["arm"] == arm and c["run"] == n]
            row = {"run": n, "answers": len(cs), "empty": sum(c["status"] == "empty-answer" for c in cs),
                   "could_not_judge": sum(c["status"] == "could-not-judge" for c in cs),
                   "never_ran": len(included) - len(cs),
                   "contradictions": sum((c["coverage"] or {}).get("contradictions", 0) for c in cs),
                   "answer_chars": mean([c["answer_chars"] for c in cs])}
            for k in ("coverage_all", "coverage_relevant"):
                row[k] = mean([(c["coverage"] or {}).get(k) for c in cs])
            for k in RUBRIC:
                row[k] = mean([(c["rubric"] or {}).get(k) for c in cs])
            row["rubric_mean"] = mean([row[k] for k in RUBRIC])
            per_run.append(row)
        metrics = ("coverage_all", "coverage_relevant", "rubric_mean") + RUBRIC + ("answer_chars",)
        arms[arm] = {"verdict": "ran", "runs": per_run, "mean": {k: mean([r[k] for r in per_run]) for k in metrics},
                     "contradictions": sum(r["contradictions"] for r in per_run),
                     "band": {k: (max(v) - min(v) if len(v := [r[k] for r in per_run if r[k] is not None]) > 1 else None)
                              for k in metrics[:3]}}

    pairwise = {}
    for x, y in PAIRS:
        name = f"{x}_vs_{y}"
        if x not in runs or y not in runs:
            pairwise[name] = {"verdict": "never-ran", "missing": [a for a in (x, y) if a not in runs]}
            continue
        outcomes = {}
        for px, py, n, qid in pair_cells:
            if (px, py) == (x, y):
                ax, ay = cells[(x, n, qid)], cells[(y, n, qid)]
                k = f"{qid}--{x}-vs-{y}-r{n}"
                outcomes[f"{qid}/run-{n}"] = (pair_outcome(pw.get(f"{k}-ab"), pw.get(f"{k}-ba")) if ax and ay
                                              else "tie" if not ax and not ay else "win" if ax else "loss")
        tally = {o: sum(v == o for v in outcomes.values()) for o in ("win", "loss", "tie", "could-not-judge")}
        by_q = {}
        for k, v in outcomes.items():
            by_q.setdefault(k.split("/")[0], []).append(v)
        q_sign = {q: (v.count("win") > v.count("loss")) - (v.count("win") < v.count("loss")) for q, v in by_q.items()}
        qw, ql = sum(s > 0 for s in q_sign.values()), sum(s < 0 for s in q_sign.values())
        pairwise[name] = {"verdict": "ran", **tally, "n": len(outcomes), "sign_test_p": sign_test(tally["win"], tally["loss"]),
                          "per_question": {"win": qw, "loss": ql, "tie": len(q_sign) - qw - ql, "n": len(q_sign),
                                           "sign_test_p": sign_test(qw, ql)}, "outcomes": outcomes}
    return {"n_questions": len(included), "n_points": len(points), "reference_words": len(reference.split()),
            "route_exclusions": excluded, "relevance_could_not_judge": sorted(q for q, r in relevance.items() if r is None),
            "arms": arms, "bare_band": (arms["bare"].get("band") if arms["bare"]["verdict"] == "ran" else None),
            "pairwise": pairwise, "arm_leaks": leaks, "answers_cut": cut, "cells": per_cell,
            "judge": {"calls": judge.calls, "reused": judge.cached, "unparsed": judge.unparsed, "reported_models": sorted(judge.models)}}


# ---- the board --------------------------------------------------------------

def family(model_id):
    m = re.match(r"[A-Za-z]+", str(model_id or "").split("/")[-1])
    return m.group().lower() if m else None


def kinship_line(judge_ids, synth_ids):
    if not judge_ids or not synth_ids:
        return "judge/synth kinship: could-not-judge (a model id was not reported)"
    if set(judge_ids) & set(synth_ids):
        return f"judge and synth are the SAME MODEL ({', '.join(sorted(set(judge_ids) & set(synth_ids)))}): self-preference applies to every arm alike, not to one"
    shared = {family(j) for j in judge_ids} & {family(s) for s in synth_ids} - {None}
    if shared:
        return f"judge and synth are the same model family ({', '.join(sorted(shared))})"
    return "judge and synth are different model families"


def resolve_alias(daemon, model, timeout=10.0):
    """What `/v1/models` says `model` points at; None when it does not say (never defaulted)."""
    try:
        with urllib.request.urlopen(daemon.rstrip("/") + "/v1/models", timeout=timeout) as resp:
            data = json.loads(resp.read().decode()).get("data", [])
    except (urllib.error.URLError, OSError, ValueError):
        return None
    for m in data:
        if m.get("id") in (model, model.split("/")[-1]):
            hit = _ALIAS_RE.match(str(m.get("owned_by") or ""))
            return hit.group(1).strip() if hit else m["id"]
    return None


def fmt(x, nd=3):
    return "-" if x is None else f"{x:.{nd}f}" if isinstance(x, float) else str(x)


def board_md(b):
    L = [f"# Essay board: {b['bank']}", "",
         f"judge model (requested `{b['judge_model']['requested']}`): /v1/models -> {b['judge_model']['resolved'] or 'not reported'}; "
         f"replies carried {', '.join(b['judge_model']['reported']) or 'no model id'}",
         f"synth model(s) per run manifests: {', '.join(b['synth_models']) or 'not recorded'}", b["kinship"], "",
         f"n = {b['n_questions']} questions on the board, {b['n_points']} plot points from a {b['reference_words']}-word reference. "
         f"Route exclusions: {len(b['route_exclusions'])}. Arm-revealing tokens surviving the scrub: {b['arm_leaks']}. Answers cut at {MAX_ANSWER_CHARS} chars: {b['answers_cut']}.",
         "", "| arm | runs | cov (relevant) | cov (all) | contradictions | specificity | synthesis | faithfulness | rubric | chars | empty | could-not-judge | never-ran |",
         "|---|---|---|---|---|---|---|---|---|---|---|---|---|"]
    for arm, a in b["arms"].items():
        if a["verdict"] != "ran":
            L.append(f"| {arm} | never-ran | - | - | - | - | - | - | - | - | - | - | - |")
            continue
        m, rs = a["mean"], a["runs"]
        L.append(f"| {arm} | {len(rs)} | {fmt(m['coverage_relevant'])} | {fmt(m['coverage_all'])} | {a['contradictions']} | "
                 + " | ".join(fmt(m[k], 2) for k in RUBRIC) + f" | {fmt(m['rubric_mean'], 2)} | {fmt(m['answer_chars'], 0)} | "
                 f"{sum(r['empty'] for r in rs)} | {sum(r['could_not_judge'] for r in rs)} | {sum(r['never_ran'] for r in rs)} |")
    # What the arm's walk actually TOUCHED, beside what it scored. An arm whose
    # distinguishing evidence was reached on a handful of questions has not been
    # tested on this bank, and the coverage column above cannot say so.
    reach = b.get("factor_reach") or {}
    if reach:
        L += ["", "| arm | questions | Summary reached | Claim | Configuration | State | Entity |", "|---|---|---|---|---|---|---|"]
        for arm, a in reach.items():
            r, n = a["reached"], a["questions"]
            L.append(f"| {arm} | {n} | {r.get('summary', 0)} | {r.get('claim', 0)} | "
                     f"{r.get('configuration', 0)} | {r.get('state', 0)} | {r.get('entity', 0)} |")
        thin = [f"{arm} ({a['reached'].get('summary', 0)}/{a['questions']})"
                for arm, a in reach.items()
                if arm in ARMS and a["questions"] and a["reached"].get("summary", 0) * 2 < a["questions"]]
        if thin:
            L.append("")
            L.append("NOT TESTABLE on this bank — the walk reached a `Summary` on under half the "
                     f"questions for: {', '.join(thin)}. A null against these arms means the factor "
                     "was not reached, which is not the same finding as the factor not helping.")
    band = b["bare_band"]
    L += ["", "bare run-to-run band (max - min of run means): " + (", ".join(f"{k} {fmt(v)}" for k, v in band.items()) if band else "never-ran"), "",
          "| pair | win | loss | tie | could-not-judge | n | sign p | per-question w/l/t | per-question sign p |", "|---|---|---|---|---|---|---|---|---|"]
    for name, p in b["pairwise"].items():
        if p["verdict"] != "ran":
            L.append(f"| {name} | never-ran ({', '.join(p['missing'])} missing) | | | | | | | |")
            continue
        q = p["per_question"]
        L.append(f"| {name} | {p['win']} | {p['loss']} | {p['tie']} | {p['could-not-judge']} | {p['n']} | {fmt(p['sign_test_p'])} | "
                 f"{q['win']}/{q['loss']}/{q['tie']} | {fmt(q['sign_test_p'])} |")
    L += ["", "A pairwise win needs both presentation orders to agree. Runs at temperature 0 are near-replicates, so the "
          "per-question sign test (majority over runs) is the one whose n is honest.", ""]
    return "\n".join(L)


def run(args):
    bank = tomllib.loads(Path(args.bank).read_text(encoding="utf-8"))
    runs, synth_models, skipped = load_runs(args.runs)
    judge = Judge(http_transport(args.daemon, args.model, args.seed, args.max_tokens, args.timeout), args.out, args.workers,
                  salt=f"{args.model}|{args.seed}|")
    board = judge_runs(judge, bank, runs, args.out)
    resolved = resolve_alias(args.daemon, args.model)
    reported = board["judge"]["reported_models"]
    board.update({"schema": "ei7-essay-board/v1", "bank": bank["bank"].get("name"), "runs_dir": str(args.runs), "seed": args.seed,
                  "skipped_runs": skipped, "synth_models": sorted(synth_models),
                  "judge_model": {"requested": args.model, "resolved": resolved, "reported": reported},
                  "factor_reach": factor_reach(runs),
                  "kinship": kinship_line([resolved] if resolved else reported, sorted(synth_models))})
    Path(args.out, "board.json").write_text(json.dumps(board, indent=1))
    Path(args.out, "board.md").write_text(board_md(board))
    print(board_md(board))


# ---- self-test: a stub judge, planted cases, no network -----------------------

def self_test():
    S = ["Anna loves Bo.", "Bo sails away.", "Bo returns after ten years.", "Anna and Bo marry."]
    REF = " ".join(S)
    QS = {"q1": "What is the arc?", "q2": "What is the POSITIONBIAS conflict?", "q3": "What is the ending?", "q4": "What is the mood?"}
    bank = {"bank": {"name": "stub", "corpus": "raptor-stub"},
            "questions": [{"id": k, "question": v, "answer1": REF} for k, v in QS.items()]}
    GENERIC = "It is a story about life and love at sea."
    state = {"busy": 1, "prompts": []}

    def stub(prompt):
        if state["busy"]:                       # PLANT: the first call is a 503
            state["busy"] -= 1
            raise Busy("HTTP 503")
        state["prompts"].append(prompt)
        if prompt.startswith("Split"):
            d = {"points": S}
        elif "Which points" in prompt:
            d = {"relevant": ["P1", "P2"]}
        elif prompt.startswith("Below are numbered plot points of a novel and an answer"):
            ans = prompt.split("\nAnswer:\n", 1)[1].casefold()
            d = {f"P{i}": ("contradicted" if ("never: " + s).casefold() in ans else "correct" if s.casefold() in ans else "absent")
                 for i, s in enumerate(S, 1)}
        elif prompt.startswith("Grade"):
            n = 5 if S[0] in prompt.split("\nAnswer:\n", 1)[1] else 2
            d = dict.fromkeys(RUBRIC, n)
        else:
            a, b = prompt.split("\nAnswer A:\n", 1)[1].split("\n\nAnswer B:\n", 1)
            hits = lambda t: sum(s in t for s in S)  # noqa: E731
            d = {"winner": "A" if "POSITIONBIAS" in prompt or hits(a) > hits(b) else "B" if hits(b) > hits(a) else "tie"}
        return {"content": f"<think>{{\"decoy\": 1}}</think>Sure:\n```json\n{json.dumps(d)}\n```\nHope that helps.", "model": "stub-judge-1"}

    def row(qid, answer, intent="DeepQuery", error=None):
        r = {"question_id": qid, "question": QS[qid], "category": "k4_whole_story", "synth": {"answer": answer, "intent": intent}}
        return {**r, "error": error} if error else r

    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        def write(arm, n, rows):
            d = tmp / "runs" / arm / f"run-{n}"
            d.mkdir(parents=True)
            (d / "eval.json").write_text(json.dumps({"results": rows}))
            (d / "manifest.json").write_text(json.dumps({"verdict": None, "identity": {"synth_model": "Stub-35B"}}))
        for n in (1, 2):   # no `deep` directory at all: PLANT for never-ran
            write("full", n, [row("q1", REF + " [Source: raptor-stub]"), row("q2", f"{S[0]} never: {S[1]}"),
                              row("q3", S[3]), row("q4", REF)])
            write("bare", n, [row("q1", GENERIC, error="503 from daemon" if n == 2 else None), row("q2", S[0]),
                              row("q3", ""), row("q4", REF, intent="ExpressiveQuery")])
        runs, synth_models, _ = load_runs(tmp / "runs")
        sleeps = []
        judge = Judge(stub, tmp / "out", sleep=sleeps.append)
        b = judge_runs(judge, bank, runs, tmp / "out")
        j3_expected = [judge.calls - 1]             # every call again, minus the one planted 503
        cell = lambda q, arm, n=1: b["cells"].get(f"{q}--{arm}-r{n}")  # noqa: E731
        cases = [
            ("restating the summary outscores a generic answer on coverage",
             lambda: cell("q1", "full")["coverage"]["coverage_all"] == 1.0 and cell("q1", "bare")["coverage"]["coverage_all"] == 0.0
             and cell("q1", "full")["rubric"]["specificity"] > cell("q1", "bare")["rubric"]["specificity"]),
            ("a planted contradiction is counted",
             lambda: cell("q2", "full")["coverage"]["contradictions"] == 1 and b["arms"]["full"]["contradictions"] == 2
             and b["arms"]["bare"]["contradictions"] == 0),
            ("coverage_relevant uses only the question's relevant points",
             lambda: cell("q3", "full")["coverage"]["coverage_relevant"] == 0.0 and cell("q3", "full")["coverage"]["coverage_all"] == 0.25),
            ("pairwise with order disagreement is a tie; agreement in both orders is a win",
             lambda: b["pairwise"]["full_vs_bare"]["outcomes"]["q2/run-1"] == "tie" and b["pairwise"]["full_vs_bare"]["outcomes"]["q1/run-1"] == "win"),
            ("a missing arm reads never-ran, not zero",
             lambda: b["arms"]["deep"] == {"verdict": "never-ran", "runs": []} and b["pairwise"]["full_vs_deep"]["verdict"] == "never-ran"
             and b["arms"]["closed-book"]["verdict"] == "never-ran" and "| deep | never-ran |" in board_md({**b, "bank": "stub", "kinship": "",
                 "synth_models": [], "judge_model": {"requested": "x", "resolved": None, "reported": []}})),
            ("an empty answer scores 0, costs no judge call, and loses the pair",
             lambda: cell("q3", "bare")["status"] == "empty-answer" and cell("q3", "bare")["coverage"]["coverage_all"] == 0.0
             and cell("q3", "bare")["rubric"]["synthesis"] == 0 and b["pairwise"]["full_vs_bare"]["outcomes"]["q3/run-1"] == "win"
             and not any(p.endswith("Answer:\n") for p in state["prompts"])),
            ("a row carrying `error` is never-ran for that cell, not a zero",
             lambda: cell("q1", "bare", 2) is None and b["arms"]["bare"]["runs"][1]["never_ran"] == 1
             and "q1/run-2" not in b["pairwise"]["full_vs_bare"]["outcomes"]),
            ("a question routed to a non-retrieving handler in any arm leaves the board",
             lambda: list(b["route_exclusions"]) == ["q4"] and b["n_questions"] == 3 and cell("q4", "full") is None),
            ("no prompt names an arm, and source tags are scrubbed",
             lambda: b["arm_leaks"] == 0 and not any(re.search(r"(?i)raptor|\bbare\b|\bdeep\b|closed-book|\[Source", p) for p in state["prompts"])),
            ("a 503 is retried with backoff; think-blocks, fences and prose do not break extraction",
             lambda: sleeps == [BUSY_BACKOFF_S] and b["judge"]["unparsed"] == 0 and b["judge"]["reported_models"] == ["stub-judge-1"]),
            ("the sign test is exact and two-sided", lambda: sign_test(5, 0) == 0.0625 and sign_test(3, 3) == 1.0 and sign_test(0, 0) is None),
            ("a second pass reuses raw/ and makes no call",
             lambda: (j2 := Judge(lambda p: (_ for _ in ()).throw(AssertionError("network")), tmp / "out")) is not None
             and judge_runs(j2, bank, runs, tmp / "out")["arms"] == b["arms"] and j2.calls == 0),
            ("a different judge model or seed reuses nothing",
             lambda: (j3 := Judge(stub, tmp / "out", salt="other-model|7|")) is not None
             and judge_runs(j3, bank, runs, tmp / "out")["arms"] == b["arms"] and j3.cached == 0 and j3.calls == j3_expected[0]),
            ("kinship names same model, same family, and refuses to guess",
             lambda: "SAME MODEL" in kinship_line(["Qwen3-35B"], ["Qwen3-35B"]) and "same model family (qwen)" in kinship_line(["commonwealth/Qwen3.5-4B"], ["Qwen3.6-35B"])
             and "could-not-judge" in kinship_line([], ["Qwen3.6-35B"]) and "different" in kinship_line(["gemma-3"], ["Qwen3"])),
        ]
        failed = 0
        for name, fn in cases:
            try:
                ok = bool(fn())
            except Exception as e:  # a crashing case is a failing case, named
                ok, name = False, f"{name}  [{type(e).__name__}: {e}]"
            failed += not ok
            print(f"{'ok  ' if ok else 'FAIL'} {name}")
        print(f"self-test: {len(cases) - failed}/{len(cases)} passed")
        return 1 if failed else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--runs", help="runs dir holding <arm>/run-<N>/eval.json")
    ap.add_argument("--bank", help="essay bank TOML (make_essay_bank.py)")
    ap.add_argument("--daemon", help="OpenAI-compatible base URL, e.g. http://127.0.0.1:9841")
    ap.add_argument("--model", default="commonwealth/primary")
    ap.add_argument("--out", help="output dir: points.json relevance.json board.json board.md raw/")
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--max-tokens", type=int, default=4096)
    ap.add_argument("--timeout", type=float, default=600.0)
    ap.add_argument("--workers", type=int, default=1, help="concurrent judge calls (keep <= the daemon's slots)")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()
    if args.self_test:
        return self_test()
    missing = [f"--{k}" for k in ("runs", "bank", "daemon", "out") if not getattr(args, k)]
    if missing:
        ap.error(f"missing {', '.join(missing)}")
    return run(args)


if __name__ == "__main__":
    sys.exit(main())
