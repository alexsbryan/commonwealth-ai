#!/usr/bin/env python3
"""Pairwise essay judge that holds the WHOLE NOVEL in context. Validated or silent.

    book_judge.py --runs raptor-proof/runs-essay/<slug> --bank essay/bank-<slug>.toml \
        --book books/<slug>.txt --out <dir> --dry-run            # price it; sends nothing
    book_judge.py ... --endpoint https://host --model <id> --api-style anthropic|openai \
        --key-env MY_KEY_VAR --only-runs 1 \
        --agreement blind/human-verdicts*.json --min-agreement 0.75   # calibrate on run 1
    book_judge.py --self-test                                         # no network; planted cases

Why: essay_judge.py graded against a 225-word plot summary and failed a face
check (a specific, accurate answer scored 1 of 5; 33 of 36 pairs tied). Here the
reference is the book itself. Pairwise only, both presentation orders, and a win
needs both orders to agree (`essay_judge.pair_outcome`).

A JUDGE NOBODY VALIDATED ISSUES NO VERDICT. `board.json` / `board.md` are written
only when `--agreement` was given, agreement with the human packet
(blind/score_packet.py -> human-verdicts.json) was computable, and it met
`--min-agreement` (and `--min-kappa` when given). Otherwise the run writes
`agreement.json` (when there is one), exits 3, and says why.

Reused from essay_judge.py, not rewritten: run loading, `[Source: ...]` scrubbing
and the arm-leak count, route exclusion, both-orders outcome, the sign test, JSON
extraction, and the `Judge` class (raw records keyed by a hash of the prompt, so a
killed window resumes for free; one nudge on an unparseable reply; busy backoff;
`--workers`). The raw key carries NO run number: temperature-0 replicates that
produced byte-identical answers are one call, not three.

Four verdicts per cell: byte-identical answers are a tie by construction and cost
no call (and are left OUT of agreement: trivial matches would flatter the judge);
an unparseable reply, a refusal or a cut reply is could-not-judge; a missing row
is never-ran; both excluded and counted.

Prompt caching: the book is the FIRST bytes of every request and byte-identical
across calls (the system block: book, then the fixed rubric); only the user turn
varies. `--api-style anthropic` marks the block `cache_control`; OpenAI-style
endpoints cache a shared prefix on their own. The first call runs alone so the
rest can read what it wrote. `raw/usage.jsonl` records each call's token usage,
which is the only proof the cache hit.

The API key is read from the env var NAMED by `--key-env`. Neither the key nor
the value of `--key-env` is ever printed or written (someone will paste the key
there by mistake one day).
"""
import argparse, hashlib, json, os, sys, tempfile, threading, tomllib
import urllib.error, urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from essay_judge import (ARMS, BUSY_CODES, PAIRS, Busy, Judge, load_runs, pair_outcome,  # noqa: E402
                         route_exclusions, scrub, sign_test)

OUTCOMES = ("win", "loss", "tie")
RUBRIC_KEYS = ("accuracy_A", "accuracy_B")

BOOK_HEAD = "Below, between <book> tags, is the complete text of a novel. It is the only reference: judge against it, not against memory.\n\n<book>\n"
BOOK_TAIL = """
</book>

You will be given an exam question about this novel and two answers, A and B. Decide which answer is more accurate to the book and better answers the question.
Judge in this order: (1) accuracy to the book above: wrong events, wrong names, invented plot and misattributed quotes count against an answer; (2) completeness for what the question asks; (3) synthesis across the whole book rather than one scene. Ignore length, formatting and writing style. A footer in which an answer lists its own unverified statements is part of the answer.
List each factual error you can check against the book. Say "tie" only when neither answer is better on those three grounds.
Return only JSON:
{"winner": "A" or "B" or "tie", "accuracy_A": 1-5, "accuracy_B": 1-5, "errors_A": ["..."], "errors_B": ["..."], "reason": "one or two sentences"}"""
P_USER = """Question: {question}

Answer A:
{a}

Answer B:
{b}"""


def system_text(book):
    """The cached prefix. Book first, fixed rubric after it; nothing per-call may enter here."""
    return BOOK_HEAD + book + BOOK_TAIL


def valid_reply(d):
    return (str(d.get("winner", "")).strip() in ("A", "B", "tie")
            and all(isinstance(d.get(k), (int, float)) and not isinstance(d.get(k), bool) and 1 <= d[k] <= 5 for k in RUBRIC_KEYS)
            and all(isinstance(d.get(k), list) for k in ("errors_A", "errors_B")) and isinstance(d.get("reason"), str))


# ---- transports -------------------------------------------------------------

def read_key(key_env):
    """The key, or a refusal that echoes neither the key nor what was passed as --key-env."""
    key = os.environ.get(key_env or "", "").strip()
    if not key:
        raise SystemExit("book_judge: refused: the environment variable named by --key-env is unset or empty (its name is not echoed here)")
    return key


def endpoint_url(endpoint, api_style):
    leaf = "/v1/messages" if api_style == "anthropic" else "/v1/chat/completions"
    e = endpoint.rstrip("/")
    return e if e.endswith(leaf.rsplit("/", 1)[1]) else e + (leaf[3:] if e.endswith("/v1") else leaf)


def request_body(api_style, model, system, user, max_tokens, temperature, cache_ttl):
    """(body, headers-without-auth). Sampling params are omitted unless asked for: current hosted models reject them."""
    if api_style == "anthropic":
        cc = {"type": "ephemeral"} if cache_ttl == "5m" else {"type": "ephemeral", "ttl": cache_ttl}
        body = {"model": model, "max_tokens": max_tokens,
                "system": [{"type": "text", "text": system, "cache_control": cc}],
                "messages": [{"role": "user", "content": user}]}
        headers = {"content-type": "application/json", "anthropic-version": "2023-06-01"}
    else:
        body = {"model": model, "max_tokens": max_tokens, "stream": False,
                "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}]}
        headers = {"Content-Type": "application/json"}
    if temperature is not None:
        body["temperature"] = temperature
    return body, headers


def parse_response(api_style, doc):
    """{content, model, usage, stop}. A refusal or a cut reply yields empty content -> could-not-judge, never a guess."""
    if api_style == "anthropic":
        stop = doc.get("stop_reason")
        text = "".join(b.get("text", "") for b in doc.get("content") or [] if b.get("type") == "text")
        ok = stop in ("end_turn", "stop_sequence")
    else:
        ch = (doc.get("choices") or [{}])[0]
        stop, text = ch.get("finish_reason"), (ch.get("message") or {}).get("content") or ""
        ok = stop in ("stop", None)
    return {"content": text if ok else "", "model": doc.get("model"), "usage": doc.get("usage"), "stop": stop}


def http_transport(args, system, key, usage_log):
    url = endpoint_url(args.endpoint, args.api_style)
    lock = threading.Lock()

    def send(user):
        body, headers = request_body(args.api_style, args.model, system, user, args.max_tokens, args.temperature, args.cache_ttl)
        headers["x-api-key" if args.api_style == "anthropic" else "Authorization"] = key if args.api_style == "anthropic" else f"Bearer {key}"
        req = urllib.request.Request(url, data=json.dumps(body).encode(), headers=headers)
        try:
            with urllib.request.urlopen(req, timeout=args.timeout) as resp:
                doc = json.loads(resp.read().decode())
        except urllib.error.HTTPError as e:
            if e.code in BUSY_CODES + (529,):
                raise Busy(f"HTTP {e.code}") from None
            detail = e.read().decode(errors="replace")[:600]   # the server's words; request headers are never included
            raise SystemExit(f"book_judge: HTTP {e.code} from the endpoint: {detail}") from None
        except (urllib.error.URLError, TimeoutError, ConnectionError) as e:
            raise Busy(type(e).__name__) from None
        got = parse_response(args.api_style, doc)
        with lock, open(usage_log, "a") as f:
            f.write(json.dumps({"user_sha": hashlib.sha256(user.encode()).hexdigest()[:10], "model": got["model"],
                                "stop": got["stop"], "usage": got["usage"]}) + "\n")
        return got
    return send


# ---- planning, judging, tallying ---------------------------------------------

def parse_pairs(spec):
    pairs = tuple(tuple(p.split(":")) for p in spec.split(",")) if spec else PAIRS
    bad = [p for p in pairs if len(p) != 2 or not set(p) <= set(ARMS)]
    if bad:
        raise SystemExit(f"book_judge: --pairs wants arm:arm from {ARMS}, got {bad}")
    return pairs


def plan(bank, runs, pairs, only_runs=None):
    """Cells and the judge jobs they need. One decider for both --dry-run and the real run."""
    questions = {q["id"]: q["question"] for q in bank["questions"]}
    corpus = bank["bank"].get("corpus", "")
    excluded = route_exclusions(runs)
    cells, jobs, seen, leaks, cut, never = [], [], set(), 0, 0, []
    for x, y in pairs:
        for n in sorted(set(runs.get(x, {})) & set(runs.get(y, {}))):
            if only_runs and n not in only_runs:
                continue
            for qid, question in questions.items():
                if qid in excluded:
                    continue
                if qid not in runs[x][n] or qid not in runs[y][n]:
                    never.append(f"{x}_vs_{y}/{qid}/run-{n}")
                    continue
                (ax, cx, lx), (ay, cy, ly) = (scrub((runs[a][n][qid].get("synth") or {}).get("answer"), corpus) for a in (x, y))
                leaks, cut = leaks + lx + ly, cut + cx + cy
                cell = {"pair": f"{x}_vs_{y}", "qid": qid, "run": n, "fixed": None, "keys": None}
                if ax == ay:
                    cell["fixed"] = "identical"
                elif not ax or not ay:
                    cell["fixed"] = "win" if ax else "loss"       # an empty answer loses to any answer
                else:
                    cell["keys"] = {}
                    for tag, a, b in (("ab", ax, ay), ("ba", ay, ax)):
                        prompt = P_USER.format(question=question, a=a, b=b)
                        key = f"{qid}--{x}-vs-{y}-{tag}"
                        ident = (key, prompt)
                        cell["keys"][tag] = ident
                        if ident not in seen:
                            seen.add(ident)
                            jobs.append(("pairwise", key, prompt, valid_reply))
                cells.append(cell)
    return {"questions": questions, "excluded": excluded, "cells": cells, "jobs": jobs,
            "arm_leaks": leaks, "answers_cut": cut, "never_ran": never}


def tally_outcomes(outcomes):
    """outcomes: {"<qid>/run-<n>": outcome}. Per-cell tally plus the per-question majority, each with its sign test."""
    t = {o: sum(v == o for v in outcomes.values()) for o in OUTCOMES + ("could-not-judge",)}
    by_q = {}
    for k, v in outcomes.items():
        by_q.setdefault(k.split("/")[0], []).append(v)
    sign = {q: (v.count("win") > v.count("loss")) - (v.count("win") < v.count("loss")) for q, v in by_q.items()}
    qw, ql = sum(s > 0 for s in sign.values()), sum(s < 0 for s in sign.values())
    return {**t, "n": len(outcomes), "sign_test_p": sign_test(t["win"], t["loss"]),
            "per_question": {"win": qw, "loss": ql, "tie": len(sign) - qw - ql, "n": len(sign), "sign_test_p": sign_test(qw, ql)}}


def judge_cells(judge, p):
    """Run the plan. The first call goes alone: a cache entry is readable only once its writer has answered."""
    jobs = p["jobs"]
    replies = ([judge.ask(*jobs[0])] + judge.ask_many(jobs[1:])) if jobs else []
    by_ident = {(j[1], j[2]): r for j, r in zip(jobs, replies)}
    out = {}
    for c in p["cells"]:
        if c["fixed"]:
            outcome, acc = ("tie" if c["fixed"] == "identical" else c["fixed"]), None
        else:
            ab, ba = by_ident[c["keys"]["ab"]], by_ident[c["keys"]["ba"]]
            outcome = pair_outcome(ab, ba)
            acc = None if ab is None or ba is None else {"x": (ab["accuracy_A"] + ba["accuracy_B"]) / 2, "y": (ab["accuracy_B"] + ba["accuracy_A"]) / 2}
        out.setdefault(c["pair"], {})[f"{c['qid']}/run-{c['run']}"] = {"outcome": outcome, "identical": c["fixed"] == "identical", "accuracy": acc}
    return out


# ---- agreement with the human packet ------------------------------------------

def cohen_kappa(pairs_):
    """Cohen's kappa over (human, judge) labels. None when chance agreement is 1: nothing to measure against."""
    n = len(pairs_)
    if not n:
        return None
    po = sum(h == j for h, j in pairs_) / n
    pe = sum((sum(h == c for h, _ in pairs_) / n) * (sum(j == c for _, j in pairs_) / n) for c in OUTCOMES)
    return None if pe >= 1.0 else (po - pe) / (1 - pe)


def agreement(judged, human_files):
    """Compare on (pair, qid, run). Identical pairs and could-not-judge cells are left out, and counted."""
    items, skipped = [], {"identical": 0, "judge_could_not_judge": 0, "not_judged_here": 0}
    for f in human_files:
        h = json.loads(Path(f).read_text())
        for qid, v in h["verdicts"].items():
            mine = judged.get(h["pair"], {}).get(f"{qid}/run-{h['run']}")
            why = ("identical" if v.get("identical") else "not_judged_here" if mine is None
                   else "judge_could_not_judge" if mine["outcome"] == "could-not-judge" else None)
            if why:
                skipped[why] += 1
            else:
                items.append({"pair": h["pair"], "qid": qid, "run": h["run"], "human": v["outcome"], "judge": mine["outcome"]})
    per_q = {}
    for it in items:
        m = per_q.setdefault(it["qid"], [0, 0])
        m[0] += it["human"] == it["judge"]
        m[1] += 1
    n = len(items)
    return {"n": n, "match_rate": (sum(i["human"] == i["judge"] for i in items) / n) if n else None,
            "kappa": cohen_kappa([(i["human"], i["judge"]) for i in items]),
            "per_question": {q: f"{a}/{b}" for q, (a, b) in per_q.items()}, "skipped": skipped, "items": items}


def gate(agr, min_agreement, min_kappa):
    """None when the judge may issue a board; otherwise the reason it may not."""
    if agr is None:
        return "no --agreement file was given: this judge has not been compared with a human reading"
    if min_agreement is None:
        return "--agreement was given without --min-agreement: no bar was set before the data"
    if not agr["n"]:
        return "agreement could not be computed: no cell was judged by both the human and this run"
    if agr["match_rate"] < min_agreement:
        return f"agreement {agr['match_rate']:.3f} over {agr['n']} pairs is below --min-agreement {min_agreement}"
    if min_kappa is not None and (agr["kappa"] is None or agr["kappa"] < min_kappa):
        return f"kappa {agr['kappa']} is below --min-kappa {min_kappa} (None = one label only, nothing to measure)"
    return None


def fmt(x):
    return "-" if x is None else f"{x:.3f}" if isinstance(x, float) else str(x)


def board_md(b):
    a = b["agreement"]
    L = [f"# Book-judge board: {b['bank']}", "", f"judge: `{b['model']}` ({b['api_style']}), whole novel in context ({b['book_chars']} chars, sha256 {b['book_sha256'][:12]})",
         f"validated against the human packet: match {fmt(a['match_rate'])} over {a['n']} pairs, kappa {fmt(a['kappa'])} (bar: {b['min_agreement']})",
         f"route exclusions {len(b['route_exclusions'])}; arm-revealing tokens surviving the scrub {b['arm_leaks']}; never-ran cells {len(b['never_ran'])}", "",
         "| pair | win | loss | tie | could-not-judge | n | identical (tie, no call) | sign p | per-question w/l/t | per-question sign p | accuracy x | accuracy y |", "|---|---|---|---|---|---|---|---|---|---|---|---|"]
    for name, p in b["pairwise"].items():
        q = p["per_question"]
        L.append(f"| {name} | {p['win']} | {p['loss']} | {p['tie']} | {p['could-not-judge']} | {p['n']} | {p['identical']} | {fmt(p['sign_test_p'])} | "
                 f"{q['win']}/{q['loss']}/{q['tie']} | {fmt(q['sign_test_p'])} | {fmt(p['accuracy_x'])} | {fmt(p['accuracy_y'])} |")
    return "\n".join(L + ["", "A win needs both presentation orders to agree. x is the first arm of the pair.", ""])


def build_board(judged):
    out = {}
    for name, cells in judged.items():
        accs = [c["accuracy"] for c in cells.values() if c["accuracy"]]
        out[name] = {**tally_outcomes({k: c["outcome"] for k, c in cells.items()}), "identical": sum(c["identical"] for c in cells.values()),
                     "accuracy_x": (sum(a["x"] for a in accs) / len(accs)) if accs else None,
                     "accuracy_y": (sum(a["y"] for a in accs) / len(accs)) if accs else None,
                     "outcomes": {k: c["outcome"] for k, c in cells.items()}}
    return out


def dry_run(p, system, max_tokens):
    calls, est = len(p["jobs"]), lambda chars: chars // 4  # noqa: E731
    prefix, suffix = est(len(system)), sum(est(len(j[2])) for j in p["jobs"])
    fixed = {}
    for c in p["cells"]:
        fixed[c["fixed"] or "judged"] = fixed.get(c["fixed"] or "judged", 0) + 1
    return {"cells": len(p["cells"]), "cells_by_kind": fixed, "calls": calls, "prefix_tokens_per_call": prefix,
            "suffix_tokens_total": suffix, "input_tokens_uncached": calls * prefix + suffix,
            "input_tokens_if_prefix_cached": {"written_once": prefix, "read_from_cache": max(calls - 1, 0) * prefix, "uncached": suffix},
            "max_output_tokens": calls * max_tokens, "estimate": "chars/4; a nudge retry on an unparseable reply adds one call each",
            "route_exclusions": p["excluded"], "never_ran": p["never_ran"], "arm_leaks": p["arm_leaks"]}


def run(args):
    bank = tomllib.loads(Path(args.bank).read_text(encoding="utf-8"))
    book = Path(args.book).read_bytes().decode("utf-8")           # bytes in, bytes out: no newline translation
    system = system_text(book)
    runs, _, skipped = load_runs(args.runs)
    only = {int(n) for n in args.only_runs.split(",")} if args.only_runs else None
    p = plan(bank, runs, parse_pairs(args.pairs), only)
    if args.dry_run:
        print(json.dumps(dry_run(p, system, args.max_tokens), indent=1))
        return 0
    missing = [f"--{k.replace('_', '-')}" for k in ("endpoint", "model", "key_env", "out") if not getattr(args, k)]
    if missing:
        raise SystemExit(f"book_judge: missing {', '.join(missing)}")
    key = read_key(args.key_env)
    out = Path(args.out)
    (out / "raw").mkdir(parents=True, exist_ok=True)
    book_sha = hashlib.sha256(book.encode()).hexdigest()
    judge = Judge(http_transport(args, system, key, out / "raw" / "usage.jsonl"), out, args.workers,
                  salt=f"{args.model}|{args.api_style}|{hashlib.sha256(system.encode()).hexdigest()}|")
    return finish(args, bank, p, judge_cells(judge, p), judge, out, book, book_sha, skipped)


def finish(args, bank, p, judged, judge, out, book, book_sha, skipped=()):
    agr = agreement(judged, args.agreement) if args.agreement else None
    if agr is not None:
        (out / "agreement.json").write_text(json.dumps({**agr, "min_agreement": args.min_agreement, "min_kappa": args.min_kappa}, indent=1))
        print(f"book_judge: agreement with the human packet: match {fmt(agr['match_rate'])} over {agr['n']} pairs, kappa {fmt(agr['kappa'])}; "
              f"per question {agr['per_question']}; skipped {agr['skipped']}", file=sys.stderr)
    print(f"book_judge: {judge.calls} calls sent, {judge.cached} raw records reused, {judge.unparsed} unparsed", file=sys.stderr)
    refusal = gate(agr, args.min_agreement, args.min_kappa)
    if refusal:
        for f in ("board.json", "board.md"):                      # a stale board from a looser bar does not outlive this verdict
            (out / f).unlink(missing_ok=True)
        print(f"book_judge: NO BOARD WRITTEN: {refusal}", file=sys.stderr)
        return 3
    board = {"schema": "ei7-book-judge-board/v1", "bank": bank["bank"].get("name"), "model": args.model, "api_style": args.api_style,
             "book_chars": len(book), "book_sha256": book_sha, "agreement": {k: v for k, v in agr.items() if k != "items"},
             "min_agreement": args.min_agreement, "min_kappa": args.min_kappa, "route_exclusions": p["excluded"],
             "arm_leaks": p["arm_leaks"], "answers_cut": p["answers_cut"], "never_ran": p["never_ran"], "skipped_runs": list(skipped),
             "pairwise": build_board(judged), "judge": {"calls": judge.calls, "reused": judge.cached, "unparsed": judge.unparsed,
                                                       "reported_models": sorted(judge.models)}}
    (out / "board.json").write_text(json.dumps(board, indent=1))
    (out / "board.md").write_text(board_md(board))
    print(board_md(board))
    return 0


# ---- self-test: a stub judge, planted cases, no network -----------------------

def self_test():
    import contextlib, io, re
    BOOK = "Anna loves Bo. Bo sails away. Bo returns after ten years. Anna and Bo marry.\n"
    FACTS = BOOK.strip().split(". ")
    QS = {"q1": "What is the arc?", "q2": "What is the POSITIONBIAS conflict?", "q3": "What is the ending?",
          "q4": "What is the mood?", "q5": "What is the GARBLE theme?"}
    bank = {"bank": {"name": "stub", "corpus": "raptor-stub"}, "questions": [{"id": k, "question": v} for k, v in QS.items()]}
    SECRET = "sk-planted-secret-0123456789"
    sent = []

    def stub(user):
        sent.append(user)
        a, b = user.split("\nAnswer A:\n", 1)[1].split("\n\nAnswer B:\n", 1)
        hits = lambda t: sum(f.rstrip(".") in t for f in FACTS)  # noqa: E731
        if "GARBLE" in user:
            return {"content": "I would rather not say.", "model": "stub-1"}
        w = "A" if "POSITIONBIAS" in user or hits(a) > hits(b) else "B" if hits(b) > hits(a) else "tie"
        d = {"winner": w, "accuracy_A": 1 + hits(a), "accuracy_B": 1 + hits(b), "errors_A": [], "errors_B": [], "reason": "stub"}
        return {"content": f"```json\n{json.dumps(d)}\n```", "model": "stub-1"}

    def row(qid, answer):
        return {"question_id": qid, "question": QS[qid], "synth": {"answer": answer, "intent": "DeepQuery"}}

    def ns(**kw):
        return argparse.Namespace(**{"agreement": None, "min_agreement": None, "min_kappa": None, "model": "stub", "api_style": "anthropic", **kw})

    def human(tmp, name, verdicts, pair="full_vs_bare"):
        f = tmp / name
        f.write_text(json.dumps({"pair": pair, "run": 1, "verdicts": {q: {"outcome": o, "identical": o == "identical-tie"} for q, o in verdicts.items()}}))
        return str(f)

    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        for arm, answers in (("full", {"q1": BOOK + " [Source: raptor-stub]", "q2": FACTS[0], "q3": BOOK, "q4": "Same text.", "q5": BOOK}),
                             ("bare", {"q1": "A story of the sea.", "q2": FACTS[1], "q3": "", "q4": "Same text.", "q5": "A story."})):
            for n in (1, 2):
                d = tmp / "runs" / arm / f"run-{n}"
                d.mkdir(parents=True)
                (d / "eval.json").write_text(json.dumps({"results": [row(q, a) for q, a in answers.items()]}))
        runs, _, _ = load_runs(tmp / "runs")
        p = plan(bank, runs, (("full", "bare"),))
        out = tmp / "out"
        out.mkdir()
        judge = Judge(stub, out, salt="stub|")
        judged = judge_cells(judge, p)
        o = {k: v["outcome"] for k, v in judged["full_vs_bare"].items()}
        first_calls = judge.calls

        def finish_with(h, **kw):
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                return finish(ns(agreement=[h] if h else None, **kw), bank, p, judged, judge, out, BOOK, "0" * 64)

        good = human(tmp, "h-good.json", {"q1": "win", "q2": "tie", "q3": "win", "q4": "identical-tie"})
        bad = human(tmp, "h-bad.json", {"q1": "loss", "q2": "win", "q3": "win"})

        def key_refusal():
            os.environ.pop("BOOK_JUDGE_SELFTEST_UNSET", None)
            msgs = []
            for name in ("BOOK_JUDGE_SELFTEST_UNSET", SECRET):      # the second is the paste-the-key-here mistake
                try:
                    read_key(name)
                    return False
                except SystemExit as e:
                    msgs.append(str(e))
            return all("refused" in m and SECRET not in m and "BOOK_JUDGE_SELFTEST_UNSET" not in m for m in msgs)

        def prefix_is_stable():
            bodies = [request_body(style, "m", system_text(BOOK), u, 100, None, "5m")[0] for style in ("anthropic", "openai") for u in sent[:3]]
            firsts = [(b["system"][0]["text"] if "system" in b else b["messages"][0]["content"]) for b in bodies]
            return (len(set(firsts)) == 1 and firsts[0].startswith(BOOK_HEAD + BOOK) and bodies[0]["system"][0]["cache_control"] == {"type": "ephemeral"}
                    and "temperature" not in bodies[0] and not any("<book>" in u for u in sent))

        def dry_run_is_silent():
            before = len(sent)
            d = dry_run(plan(bank, runs, (("full", "bare"),)), system_text(BOOK), 100)
            return len(sent) == before and d["calls"] == 6 and d["cells"] == 10 and d["cells_by_kind"] == {"judged": 6, "win": 2, "identical": 2}

        cases = [
            ("agreement in both orders is a win; order disagreement is a tie", lambda: o["q1/run-1"] == "win" and o["q2/run-1"] == "tie"),
            ("an empty answer loses without a call; identical answers tie without a call",
             lambda: o["q3/run-1"] == "win" and o["q4/run-1"] == "tie" and judged["full_vs_bare"]["q4/run-1"]["identical"]
             and not any("Same text." in u or u.endswith("Answer B:\n") for u in sent)),
            ("an unparseable reply is could-not-judge, not a tie", lambda: o["q5/run-1"] == "could-not-judge" and judge.unparsed == 2),
            ("byte-identical prompts across runs are one call (3 judged questions x 2 orders, +2 nudges)", lambda: first_calls == 8 and len(p["jobs"]) == 6),
            ("no board without an --agreement file", lambda: finish_with(None) == 3 and not (out / "board.json").exists()),
            ("no board when --agreement has no bar", lambda: finish_with(good) == 3 and not (out / "board.json").exists()),
            ("agreement below the threshold refuses, and says so in agreement.json",
             lambda: finish_with(bad, min_agreement=0.75) == 3 and not (out / "board.json").exists()
             and abs(json.loads((out / "agreement.json").read_text())["match_rate"] - 1 / 3) < 1e-9),
            ("agreement at the threshold writes the board; identical pairs are left out of agreement",
             lambda: finish_with(good, min_agreement=0.75) == 0 and (b := json.loads((out / "board.json").read_text()))["agreement"]["n"] == 3
             and b["agreement"]["skipped"]["identical"] == 1 and b["pairwise"]["full_vs_bare"]["win"] == 4 and b["pairwise"]["full_vs_bare"]["identical"] == 2),
            ("a later failing run removes the earlier board", lambda: finish_with(bad, min_agreement=0.75) == 3 and not (out / "board.md").exists()),
            ("a kappa bar refuses when only one label was seen",
             lambda: finish_with(human(tmp, "h-one.json", {"q1": "win", "q3": "win"}), min_agreement=0.5, min_kappa=0.2) == 3),
            ("kappa is exact on a known table",
             lambda: abs(cohen_kappa([("win", "win")] * 2 + [("tie", "tie")] * 2 + [("win", "tie"), ("tie", "win")]) - 1 / 3) < 1e-9
             and cohen_kappa([("win", "win")] * 3) is None and cohen_kappa([]) is None),
            ("a missing key env refuses without echoing the key or the variable's name", key_refusal),
            ("a second pass reuses raw/ and sends nothing",
             lambda: (j2 := Judge(lambda u: (_ for _ in ()).throw(AssertionError("network")), out, salt="stub|")) is not None
             and judge_cells(j2, p) == judged and j2.calls == 0 and j2.cached == 6),
            ("a different model reuses nothing", lambda: (j3 := Judge(stub, out, salt="other|")) is not None and judge_cells(j3, p) == judged and j3.cached == 0),
            ("the book is first and byte-identical in every request; only the user turn varies; no sampling params by default", prefix_is_stable),
            ("no prompt names an arm, and source tags are scrubbed",
             lambda: p["arm_leaks"] == 0 and not any(re.search(r"(?i)raptor|\bbare\b|\bdeep\b|\bfull\b|\[Source", u) for u in sent)),
            ("--dry-run counts calls and sends nothing", dry_run_is_silent),
            ("a refusal or a cut reply parses to empty content",
             lambda: parse_response("anthropic", {"stop_reason": "refusal", "content": [{"type": "text", "text": "{}"}]})["content"] == ""
             and parse_response("openai", {"choices": [{"finish_reason": "length", "message": {"content": "{"}}]})["content"] == ""
             and parse_response("anthropic", {"stop_reason": "end_turn", "content": [{"type": "thinking", "thinking": "x"}, {"type": "text", "text": "ok"}]})["content"] == "ok"),
            ("endpoint URLs resolve for both styles",
             lambda: endpoint_url("https://h", "anthropic") == "https://h/v1/messages" and endpoint_url("https://h/v1/", "openai") == "https://h/v1/chat/completions"
             and endpoint_url("https://h/v1/messages", "anthropic") == "https://h/v1/messages"),
            ("the key is in no file this run wrote", lambda: not any(SECRET in f.read_text() for f in out.rglob("*") if f.is_file())),
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
    ap.add_argument("--bank", help="essay bank TOML")
    ap.add_argument("--book", help="the novel, plain text; sent whole, byte-identical, first")
    ap.add_argument("--out", help="output dir: raw/ agreement.json and, only when validated, board.json board.md")
    ap.add_argument("--endpoint", help="API base URL")
    ap.add_argument("--model")
    ap.add_argument("--api-style", choices=("openai", "anthropic"), default="anthropic")
    ap.add_argument("--key-env", help="NAME of the env var holding the API key (never the key itself)")
    ap.add_argument("--pairs", help="arm:arm[,arm:arm]; default full:bare,full:deep")
    ap.add_argument("--only-runs", help="e.g. 1 to judge only the run the human packet was drawn from")
    ap.add_argument("--max-tokens", type=int, default=8000)
    ap.add_argument("--temperature", type=float, default=None, help="omitted from the request unless given")
    ap.add_argument("--cache-ttl", choices=("5m", "1h"), default="5m", help="anthropic style; 1h costs a 2x write")
    ap.add_argument("--timeout", type=float, default=900.0)
    ap.add_argument("--workers", type=int, default=1)
    ap.add_argument("--agreement", nargs="+", help="human-verdicts.json file(s) from blind/score_packet.py")
    ap.add_argument("--min-agreement", type=float, help="match rate the judge must reach before it may write a board")
    ap.add_argument("--min-kappa", type=float, help="optional second bar: Cohen's kappa")
    ap.add_argument("--dry-run", action="store_true", help="print the call count and a chars/4 token estimate; send nothing")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()
    if args.self_test:
        return self_test()
    missing = [f"--{k}" for k in ("runs", "bank", "book") if not getattr(args, k)]
    if missing:
        ap.error(f"missing {', '.join(missing)}")
    return run(args)


if __name__ == "__main__":
    sys.exit(main())
