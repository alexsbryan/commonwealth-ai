#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""R1 -- retrieval coverage `r` over the AA-Omniscience bank.

Measures ONE quantity, pre-registered in PREREG.md before any judgment ran:
the fraction of items whose gold answer is actually present in what retrieval
returns. It bounds every grounded arm --

    OI_taxed = 100*(r*(2p - 0.9) - 0.1)

-- so the target of 10 is unreachable below r = 0.182 at ANY answer precision.
Half an hour here prices a 7.5-hour run.

Five phases, separately resumable, because they fail differently:

  retrieve   `chat inspect --format json` -- /v1/embeddings ONLY, never the
             chat slot, so it cannot be OOM-killed mid-generation the way the
             2026-08-25 probe was. It CAN still be refused: a loaded daemon
             stops accepting connections without dying, so this phase retries
             with backoff and trips a circuit breaker rather than converting
             one outage into a failed run.
  control    THE INSTRUMENT GATE (PREREG §instrument validation, ARCH §18.4).
             Judges each (question, gold) against a DIFFERENT item's context.
             A judge answering from its own weights rather than the passage
             says COVERED here; a fit judge says NOT COVERED. Bar >= 0.90 and
             below it NO r is reported -- not a low r, none.
  poscontrol THE OTHER HALF, added 2026-08-26 after the n=6 smoke. The
             shuffled control alone cannot separate "the corpus holds nothing"
             from "the judge always answers B" -- both score r = 0.0 and both
             pass it. Here the fact is PLANTED mid-passage and the judge must
             find it. Bar >= 0.90 COVERED.
  grade      The real judgments. k=10 first, k=5 only where k=10 said COVERED:
             the top-5 is a subset of the top-10, so coverage is monotone in k
             and a NOT_COVERED at k=10 settles k=5 for free.
  summarize  Refuses to emit r unless BOTH controls passed AND coverage is 1.0.

The OPERATIVE cut is one corpus, not the unscoped fan-out the pre-registration
named. This host carries ~40 dev-fixture corpora, and the smoke found a Law
question about OSHA cadmium rulemaking retrieving Rust unit tests from this very
repo. The unscoped number is measured in its own run and reported beside it --
declared, not dropped.

`--inspect-scope` and `--scope` are TWO knobs and both are needed. The first
asks `chat inspect` for a corpus; the second selects within what came back.
Post-filtering an unscoped result does NOT give you the scoped cut: the
unscoped call returns 38-40 corpora and wikipedia was absent from 59 of 60 of
them, so the filter yielded an EMPTY passage -- which grades NOT_COVERED for
free and reads as a corpus that holds nothing. That is what produced a false
`r = 0.0` and a false KILL verdict on 2026-08-26.

The instrument this REPLACES is a string match on the gold answer. It scores
topical adjacency as coverage -- see PREREG and COVERAGE_GRADER_TEMPLATE
example 3. `gold_string_present` is still recorded per item, never as a grade,
so the size of that trap can be reported either way.
"""
import argparse, json, os, pathlib, shutil, subprocess, sys, time
from collections import Counter, defaultdict

from prompts import COVERAGE_GRADER_TEMPLATE
from run import chat, load_bank

COVER = {"A": "COVERED", "B": "NOT_COVERED"}
CONTROL_BAR = 0.90          # PREREG: >= 90% NOT COVERED on shuffled context
FLOOR_R = 0.2 / 1.1         # 0.1818 -- OI 10 at p = 1.0
GO_R = 0.30                 # PREREG: target needs only p >= 0.78


def resolve_cli(explicit):
    """`svrn` is the prod symlink, `sovereign` the local-only legacy name, and
    not every host has both (AGENTS.md). Try the documented name first."""
    if explicit:
        return explicit
    for name in ("svrn", "sovereign"):
        if shutil.which(name):
            return name
    sys.exit("no `svrn` or `sovereign` on PATH -- pass --cli")


def retrieve(cli, question, limit, timeout, corpus=None, backoff=(5, 15, 45)):
    """Retrieval stage with NO model call. Returns the parsed corpora list.

    RETRIES, because the daemon goes UNREACHABLE without dying. Measured
    2026-08-26: pid 16031 was up 2h42m and answering before and after, but
    refused connections for ~30s while 8 rustc processes and a 39 GB-RSS
    daemon fought over a host at 11% free. Without a retry that blip cost 45
    of 60 items in 90 seconds -- each one "failed" permanently and instantly.
    A transient refusal is not an answer about the corpus.
    """
    last = None
    for wait in (*backoff, None):
        out = subprocess.run(
            [cli, "chat", "inspect", "--format", "json", "--limit", str(limit)]
            + (["--corpus", corpus] if corpus else []) + [question],
            capture_output=True, text=True, timeout=timeout,
            env={**os.environ, "SVRNMESH_NO_STALE_WARN": "1"})
        if out.returncode == 0:
            return json.loads(out.stdout)["corpora"]
        last = "chat inspect exit %d: %s" % (out.returncode, out.stderr.strip()[-300:])
        if wait is None:
            break
        log("    transient: %s -- retrying in %ds" % (last[:110], wait))
        time.sleep(wait)
    raise RuntimeError(last)


def top_chunks(corpora, k, corpus_id=None):
    """Global top-k by score across corpora (or within one).

    ASSUMPTION, named in PREREG's confound table: the answer path assembles
    its context from the best-scoring chunks across corpora, and scores from
    one embed model are comparable between them. `--limit` is DISPLAY depth,
    not assembly depth, so this is an upper bound on what the answerer saw.
    """
    chunks = [c for corp in corpora
              if corpus_id is None or corp["corpus_id"] == corpus_id
              for c in corp["chunks"]]
    chunks.sort(key=lambda c: -float(c["score"]))
    return chunks[:k]


def passage(chunks, chunk_chars):
    return "\n\n".join("[%s] %s" % (c.get("title", "?"), c["content"][:chunk_chars])
                       for c in chunks)


def judge_coverage(question, gold, text, args):
    prompt = COVERAGE_GRADER_TEMPLATE.format(question=question, gold=gold, passage=text)
    for _ in range(2):
        txt, _meta = chat(args.base_url, args.judge_model,
                          [{"role": "user", "content": prompt}],
                          args.judge_max_tokens, 0.0, args.timeout, args.seed)
        for ch in txt.strip().upper():
            if ch in COVER:
                return COVER[ch], txt
    return None, txt  # unparseable after a retry == could-not-judge, never a grade


def log(msg):
    print("%7.1fs  %s" % (time.time() - T0, msg), flush=True)


def load_jsonl(path):
    if not path.exists():
        return {}
    return {json.loads(l)["question_id"]: json.loads(l)
            for l in path.open() if l.strip()}


# ---------------------------------------------------------------- phases

def phase_retrieve(items, out, args):
    path = out / "contexts.jsonl"
    done = load_jsonl(path) if args.resume else {}
    log("retrieve: %d items, %d already done" % (len(items), len(done)))
    errors = consecutive = written = 0
    with path.open("a" if done else "w") as fh:
        for n, it in enumerate(items, 1):
            qid = int(it["question_id"])
            if qid in done:
                continue
            try:
                corpora = retrieve(args.cli, it["question"], args.limit, args.timeout,
                                   corpus=args.inspect_scope)
                consecutive = 0
            except Exception as exc:                       # noqa: BLE001
                errors += 1
                consecutive += 1
                log("  qid=%d RETRIEVAL ERROR %s" % (qid, str(exc)[:160]))
                # CIRCUIT BREAKER. An endpoint that has refused three items in
                # a row after all its retries is down, not flaky, and grinding
                # through the remaining bank at 2s an item converts one outage
                # into a whole failed run. Stop and keep what is on disk --
                # `--resume` picks the rest up once the host is quiet.
                if consecutive >= args.max_consecutive_errors:
                    log("  CIRCUIT BREAKER: %d consecutive failures -- stopping the phase "
                        "with %d contexts banked (%d this pass). Re-run with --resume." %
                        (consecutive, len(done) + written, written))
                    break
                continue
            row = {"question_id": qid, "question": it["question"],
                   "gold": it["answer"], "domain": it["domain"], "corpora": corpora}
            fh.write(json.dumps(row) + "\n")
            fh.flush()
            written += 1
            if n % 5 == 0 or n == len(items):
                log("  retrieve %d/%d  errors=%d" % (n, len(items), errors))
    return errors


def phase_control(out, args):
    """Shuffled-context negative control -- the gate on the instrument."""
    ctx = load_jsonl(out / "contexts.jsonl")
    ids = sorted(ctx)
    if len(ids) < 4:
        sys.exit("control: need at least 4 contexts, have %d" % len(ids))
    pairs = [(ids[i], ids[(i + len(ids) // 2) % len(ids)]) for i in range(0, len(ids), 2)]
    log("control: %d shuffled pairs (bar: >=%.0f%% NOT_COVERED)" % (len(pairs), CONTROL_BAR * 100))
    rows = []
    with (out / "control.jsonl").open("w") as fh:
        for n, (qid, other) in enumerate(pairs, 1):
            a, b = ctx[qid], ctx[other]
            verdict, raw = judge_coverage(
                a["question"], a["gold"],
                passage(top_chunks(b["corpora"], args.k_ceiling), args.chunk_chars), args)
            row = {"question_id": qid, "context_from": other, "verdict": verdict, "raw": raw[:80]}
            rows.append(row)
            fh.write(json.dumps(row) + "\n")
            fh.flush()
            if n % 10 == 0:
                log("  control %d/%d" % (n, len(pairs)))
    graded = [r for r in rows if r["verdict"]]
    nc = sum(1 for r in graded if r["verdict"] == "NOT_COVERED")
    rate = nc / len(graded) if graded else 0.0
    verdict = "PASS" if rate >= CONTROL_BAR and len(graded) == len(rows) else "FAIL"
    log("control: NOT_COVERED %d/%d = %.3f -> %s" % (nc, len(graded), rate, verdict))
    (out / "control_summary.json").write_text(json.dumps(
        {"n_pairs": len(rows), "n_graded": len(graded), "not_covered": nc,
         "not_covered_rate": rate, "bar": CONTROL_BAR, "verdict": verdict}, indent=2))
    return verdict == "PASS"


def phase_poscontrol(out, args):
    """Positive control -- the OTHER half of instrument validation.

    AMENDMENT 2026-08-26, added after the n=6 smoke and BEFORE any real r.
    The shuffled control only catches a judge answering from its own weights
    (false COVERED). A judge that simply always answers B passes it perfectly
    and reports r = 0.0, which is indistinguishable from a corpus that holds
    nothing. This phase closes that: the fact is INSERTED into the item's own
    retrieved passage, at the midpoint rather than the front so recency cannot
    carry it, and the judge must find it. Bar >= 0.90 COVERED.
    """
    ctx = load_jsonl(out / "contexts.jsonl")
    ids = sorted(ctx)
    log("poscontrol: %d items (bar: >=%.0f%% COVERED)" % (len(ids), CONTROL_BAR * 100))
    rows = []
    with (out / "poscontrol.jsonl").open("w") as fh:
        for n, qid in enumerate(ids, 1):
            c = ctx[qid]
            # NO FALLBACK. It must plant into the SAME passage grading reads,
            # or it validates the judge on context the real run never sees --
            # which is exactly how the 2026-08-26 empty-passage run passed
            # both controls while grading nothing.
            chunks = top_chunks(c["corpora"], args.k_ceiling, corpus_id=args.scope)
            if not chunks:
                row = {"question_id": qid, "verdict": None, "raw": "", "no_context": True}
                rows.append(row); fh.write(json.dumps(row) + "\n"); fh.flush()
                continue
            half = len(chunks) // 2
            planted = "[planted] %s The answer is %s." % (c["question"], c["gold"])
            text = "\n\n".join([passage(chunks[:half], args.chunk_chars), planted,
                                passage(chunks[half:], args.chunk_chars)])
            verdict, raw = judge_coverage(c["question"], c["gold"], text, args)
            row = {"question_id": qid, "verdict": verdict, "raw": raw[:80]}
            rows.append(row)
            fh.write(json.dumps(row) + "\n")
            fh.flush()
            if n % 10 == 0:
                log("  poscontrol %d/%d" % (n, len(ids)))
    graded = [r for r in rows if r["verdict"]]
    cov = sum(1 for r in graded if r["verdict"] == "COVERED")
    rate = cov / len(graded) if graded else 0.0
    verdict = "PASS" if rate >= CONTROL_BAR and len(graded) == len(rows) else "FAIL"
    log("poscontrol: COVERED %d/%d = %.3f -> %s" % (cov, len(graded), rate, verdict))
    (out / "poscontrol_summary.json").write_text(json.dumps(
        {"n": len(rows), "n_graded": len(graded), "covered": cov,
         "covered_rate": rate, "bar": CONTROL_BAR, "verdict": verdict}, indent=2))
    return verdict == "PASS"


def phase_grade(out, args):
    ctx = load_jsonl(out / "contexts.jsonl")
    path = out / "grades.jsonl"
    done = load_jsonl(path) if args.resume else {}
    log("grade: %d contexts, %d already graded (scope=%s)" % (len(ctx), len(done), args.scope))
    ko, kc = "k%d" % args.k_operative, "k%d" % args.k_ceiling
    with path.open("a" if done else "w") as fh:
        for n, qid in enumerate(sorted(ctx), 1):
            if qid in done:
                continue
            c = ctx[qid]
            row = {"question_id": qid, "domain": c["domain"], "gold": c["gold"]}
            wide = top_chunks(c["corpora"], args.k_ceiling, corpus_id=args.scope)
            # AN EMPTY PASSAGE IS ABSENCE, NOT EVIDENCE (§18.3). Grading one
            # returns NOT_COVERED for free and reads as "the corpus does not
            # hold it". On 2026-08-26 that produced r = 0.0 on 59/60 items --
            # `chat inspect` had returned 38-40 corpora and wikipedia was not
            # among them, so the scoped cut was empty and nothing said so.
            # NO_CONTEXT is its own outcome; it drops coverage below 1.0 and
            # summarize then refuses to report r at all.
            if not wide:
                row[kc] = row[ko] = None
                row["no_context"] = True
                row["raw"] = ""
                fh.write(json.dumps(row) + "\n")
                fh.flush()
                continue
            row[kc], row["raw"] = judge_coverage(
                c["question"], c["gold"], passage(wide, args.chunk_chars), args)
            # Monotone in k: the top-5 is a subset of the top-10, so a
            # NOT_COVERED up there settles the narrow cut without a call.
            if row[kc] == "COVERED":
                row[ko], _ = judge_coverage(
                    c["question"], c["gold"],
                    passage(top_chunks(c["corpora"], args.k_operative, corpus_id=args.scope),
                            args.chunk_chars), args)
            else:
                row[ko] = row[kc]
            # The REGISTERED unscoped reading, kept for the record even though
            # this host's ~40 dev-fixture corpora contaminate it (see PREREG
            # amendment). Reported, never operative.
            allc = top_chunks(c["corpora"], args.k_ceiling)
            row["unscoped_" + kc] = judge_coverage(
                c["question"], c["gold"], passage(allc, args.chunk_chars), args)[0]
            # NOT a grade -- recorded so the grep trap can be sized (PREREG).
            row["gold_string_present"] = c["gold"].strip().lower() in passage(
                wide, args.chunk_chars).lower()
            fh.write(json.dumps(row) + "\n")
            fh.flush()
            if n % 5 == 0 or n == len(ctx):
                log("  grade %d/%d" % (n, len(ctx)))


def phase_summarize(out, args):
    rows = list(load_jsonl(out / "grades.jsonl").values())
    read = lambda name: json.loads((out / name).read_text()) \
        if (out / name).exists() else {"verdict": "NEVER_RAN"}
    ctl, pos = read("control_summary.json"), read("poscontrol_summary.json")
    ko, kc = "k%d" % args.k_operative, "k%d" % args.k_ceiling
    graded = [r for r in rows if r[ko] and r[kc]]
    coverage = len(graded) / len(rows) if rows else 0.0

    s = {"n_items": len(rows), "n_graded": len(graded), "coverage": coverage,
         "no_context": sum(1 for r in rows if r.get("no_context")),
         "scope": args.scope, "control_shuffled": ctl, "control_positive": pos,
         "judge_model": args.judge_model, "judge_is_official": False,
         "k_operative": args.k_operative, "k_ceiling": args.k_ceiling,
         "floor_r": round(FLOOR_R, 4), "go_r": GO_R}

    # §18.3: absence is reported, never defaulted. No r unless it is earned --
    # and BOTH controls must pass. The shuffled one alone cannot tell a corpus
    # that holds nothing from a judge that always answers B.
    failed = [n for n, c in (("shuffled", ctl), ("positive", pos)) if c["verdict"] != "PASS"]
    if failed or coverage < 1.0 or not graded:
        s["r"] = None
        s["verdict"] = "COULD_NOT_JUDGE"
        s["why"] = ("instrument control(s) %s did not pass" % ", ".join(failed)) if failed \
            else "coverage %.3f < 1.0" % coverage
    else:
        r_op = sum(1 for r in graded if r[ko] == "COVERED") / len(graded)
        r_ceil = sum(1 for r in graded if r[kc] == "COVERED") / len(graded)
        uns = [r for r in graded if r.get("unscoped_" + kc)]
        s["r"] = r_op
        s["r_operative_%s_%s" % (args.scope or "all", ko)] = r_op
        s["r_ceiling_%s_%s" % (args.scope or "all", kc)] = r_ceil
        s["r_registered_unscoped_%s" % kc] = (
            sum(1 for r in uns if r["unscoped_" + kc] == "COVERED") / len(uns)) if uns else None
        per = defaultdict(lambda: [0, 0])
        for r in graded:
            per[r["domain"]][0] += 1
            per[r["domain"]][1] += r[ko] == "COVERED"
        s["r_by_domain"] = {d: {"n": n, "covered": c, "r": c / n} for d, (n, c) in sorted(per.items())}
        # Reported regardless (PREREG): the size of the grep trap.
        trap = [r for r in graded if r["gold_string_present"] and r[kc] == "NOT_COVERED"]
        s["gold_string_present"] = sum(1 for r in graded if r["gold_string_present"])
        s["gold_string_present_but_not_covered"] = len(trap)
        s["verdict"] = ("GO" if r_op >= GO_R else
                        "AMBIGUOUS" if r_op >= FLOOR_R else
                        "KILL_RETRIEVAL_TO_10")
        s["oi_ceiling_at_p1"] = 100 * (r_op * 1.1 - 0.1)

    (out / "summary.json").write_text(json.dumps(s, indent=2))
    print(json.dumps(s, indent=2))
    return s


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--judge-model", required=True, help="model id as /v1/models reports it")
    p.add_argument("--base-url", default="http://localhost:9741/v1")
    p.add_argument("--cli", default=None, help="svrn|sovereign (default: whichever is on PATH)")
    p.add_argument("--phase", default="all",
                   choices=["all", "retrieve", "control", "poscontrol", "grade", "summarize"])
    p.add_argument("--out", default=None)
    p.add_argument("--n", type=int, default=60, help="stratified subsample size")
    p.add_argument("--limit", type=int, default=10, help="chat inspect per-corpus display depth")
    p.add_argument("--k-operative", type=int, default=5)
    p.add_argument("--k-ceiling", type=int, default=10)
    p.add_argument("--inspect-scope", default=None,
                   help="pass --corpus <id> to chat inspect. REQUIRED to measure one "
                        "corpus: the unscoped call returns 38-40 corpora and wikipedia "
                        "is usually NOT among them, so post-filtering yields an empty "
                        "passage, not a low score (measured 2026-08-26, 59/60 items)")
    p.add_argument("--scope", default="wikipedia",
                   help="corpus_id for the OPERATIVE r; '' = unscoped. Default wikipedia: "
                        "this host carries ~40 dev-fixture corpora that contaminate the "
                        "unscoped cut (PREREG amendment 2026-08-26)")
    p.add_argument("--chunk-chars", type=int, default=1200)
    p.add_argument("--max-consecutive-errors", type=int, default=3,
                   help="circuit breaker: stop the retrieve phase after this many "
                        "back-to-back failures rather than burning the bank")
    p.add_argument("--judge-max-tokens", type=int, default=8)
    p.add_argument("--seed", type=int, default=1729)
    p.add_argument("--timeout", type=float, default=300.0)
    p.add_argument("--resume", action="store_true")
    args = p.parse_args()
    args.cli = resolve_cli(args.cli)
    args.scope = args.scope or None

    out = pathlib.Path(args.out or (pathlib.Path(__file__).parent / "runs" /
                                    ("coverage-n%d--%s" % (args.n, args.judge_model.replace("/", "_")))))
    out.mkdir(parents=True, exist_ok=True)
    items = load_bank(args.n)
    log("R1 coverage: n=%d cli=%s judge=%s out=%s" % (len(items), args.cli, args.judge_model, out))

    if args.phase in ("all", "retrieve"):
        phase_retrieve(items, out, args)
    if args.phase in ("all", "control"):
        if not phase_control(out, args) and args.phase == "all":
            log("SHUFFLED CONTROL FAILED -- continuing, but summarize will refuse to report r")
    if args.phase in ("all", "poscontrol"):
        if not phase_poscontrol(out, args) and args.phase == "all":
            log("POSITIVE CONTROL FAILED -- continuing, but summarize will refuse to report r")
    if args.phase in ("all", "grade"):
        phase_grade(out, args)
    if args.phase in ("all", "summarize"):
        s = phase_summarize(out, args)
        return 0 if s.get("r") is not None else 4
    return 0


T0 = time.time()
if __name__ == "__main__":
    sys.exit(main())
