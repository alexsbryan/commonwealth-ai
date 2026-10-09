#!/usr/bin/env python3
"""The stage ladder: one table, the same three stages on every system, from one run's own outputs and logs.

    ladder.py [--ward RUN] [--uv RUN] [--gvc RUN] [--json]   # the three ladders as one table, then a Judgement line
    ladder.py --reproduce                                     # the baseline row, figure by figure; exit 1 on a mismatch

Every gold item (a Ward deal's current stage, a uv case state, a GVC event mention) stops at its first lost stage:

    READ   the claim exists on the right document
    PLACE  it sits on the record matched to its gold particular (one to one, maximising overlap, as CEAF-e aligns)
    FOLD   the record's state is right

Beside each stage, the run's model calls per document read, per question kind (QUESTION_KINDS). It is an
instrument, not a gate: nothing here decides a build. A system with no run reads never-ran, never zero.

The systems' own finer rungs (the baseline's) are kept beside the stages; both are computed from one set of
per-item facts (`Facts`), so the two views cannot disagree about an item. Defaults are the 2026-10-09 baseline
runs (~/.svrnmesh/bench-corpora/baseline-3sys-20261009); GVC has no end-to-end run there.
"""
import argparse, collections, dataclasses, enum, importlib.util, json, pathlib, re, sys

HERE = pathlib.Path(__file__).resolve().parent
BASELINE = pathlib.Path.home() / ".svrnmesh/bench-corpora/baseline-3sys-20261009"
sys.dont_write_bytecode = True


class Stage(enum.Enum):
    READ = "read"
    PLACE = "place"
    FOLD = "fold"


# The run's model-call phases (`/v1/chat/completions ok phase=...` in the debug log), as question kinds, each
# beside the stage its answer decides: Locate finds the lines of a claim kind; Choose answers a statement's field
# values, which the fold folds; RESOLVE's reads and forced choices decide which record a statement sits on.
# A phase not listed here is reported under its own name with no stage, never dropped.
QUESTION_KINDS = {
    "document_passes_locate": ("Locate", Stage.READ),
    "document_passes_choose": ("Choose", Stage.FOLD),
    "resolve_read": ("RESOLVE read", Stage.PLACE),
    "resolve_reason": ("RESOLVE reason", Stage.PLACE),
    "resolve_select": ("RESOLVE forced choice", Stage.PLACE),
}

# ---------------------------------------------------------------- the run's own logs (was trace_common.py)
LOCATED = re.compile(r'located document="?([^" ]+)"? line=(\d+) kind="([^"]+)" p=(\S+) dist=(.*?) text=')
READ_LINE = re.compile(r"chapter read chapter=(\S+) documents=(\d+) claims=(\d+) calls=(\d+)")
CALL = re.compile(r"/v1/chat/completions ok phase=(\S+) .*?elapsed_ms=(\d+)")


def trace(run):
    """({document: [(line, kind, {label: p})]}, cost) from run/job.debug.log, run/job.log, run/_tokens.json."""
    lines = collections.defaultdict(list)
    calls, ms = collections.Counter(), collections.Counter()
    with open(run / "job.debug.log", errors="replace") as log:
        for raw in log:
            m = LOCATED.search(raw)
            if m:
                dist = dict(x.rsplit(" ", 1) for x in m.group(5).split(", "))
                lines[m.group(1)].append((int(m.group(2)), m.group(3), {k: float(v) for k, v in dist.items()}))
                continue
            c = CALL.search(raw)
            if c:
                calls[c.group(1)] += 1
                ms[c.group(1)] += int(c.group(2))
    docs = sum(int(m.group(2)) for m in READ_LINE.finditer((run / "job.log").read_text(errors="replace")))
    tok = json.loads((run / "_tokens.json").read_text()) if (run / "_tokens.json").exists() else {}
    wall = (tok.get("updated_at_ms", 0) - tok.get("started_at_ms", 0)) / 1000 if tok else None
    cost = {"documents_read": docs, "phase1_wall_s": wall, "s_per_document": round(wall / docs, 2) if wall and docs else None,
            "calls_by_phase": dict(calls), "model_s_by_phase": {k: round(v / 1000, 1) for k, v in ms.items()},
            "calls_per_document": round(sum(calls.values()) / docs, 1) if docs else None}
    return lines, cost


def kind_label(lines, kind):
    """The Locate label that names `kind`, read off the trace: the argmax label of the lines located as it."""
    seen = collections.Counter(max(d, key=d.get) for v in lines.values() for _, k, d in v if k == kind)
    return seen.most_common(1)[0][0] if seen else None


def near(lines, documents, label):
    """How near Locate came on these documents: the best probability any of their lines gave `label`."""
    ps = [dist.get(label, 0.0) for doc in documents for _, _, dist in lines.get(doc, [])]
    return {"documents": len(documents), "lines": len(ps), "best_p": round(max(ps), 3) if ps else None}


def calls_by_question(cost):
    """{stage or "unstaged": {question kind: {calls, per_document}}} from trace()'s cost."""
    docs = cost.get("documents_read") or 0
    out = {s.value: {} for s in Stage}
    for phase, n in sorted((cost.get("calls_by_phase") or {}).items()):
        kind, stage = QUESTION_KINDS.get(phase, (phase, None))
        out.setdefault(stage.value if stage else "unstaged", {})[kind] = {
            "calls": n, "per_document": round(n / docs, 2) if docs else None}
    return out


# ---------------------------------------------------------------- matching (was measure_uv.py's assign)
def assign(weight):
    """{row: col} maximising the total of `weight` ({(row, col): w > 0}), one to one (Hungarian, padded square)."""
    rows, cols = sorted({r for r, _ in weight}), sorted({c for _, c in weight})
    n = max(len(rows), len(cols))
    if not n:
        return {}
    big = max(weight.values())
    cost = [[big - weight.get((rows[i], cols[j]), 0) if i < len(rows) and j < len(cols) else big
             for j in range(n)] for i in range(n)]
    inf = float("inf")
    u, v, p, way = [0] * (n + 1), [0] * (n + 1), [0] * (n + 1), [0] * (n + 1)
    for i in range(1, n + 1):
        p[0], j0 = i, 0
        minv, used = [inf] * (n + 1), [False] * (n + 1)
        while True:
            used[j0] = True
            i0, delta, j1 = p[j0], inf, 0
            for j in range(1, n + 1):
                if not used[j]:
                    cur = cost[i0 - 1][j - 1] - u[i0] - v[j]
                    if cur < minv[j]:
                        minv[j], way[j] = cur, j0
                    if minv[j] < delta:
                        delta, j1 = minv[j], j
            for j in range(n + 1):
                if used[j]:
                    u[p[j]] += delta
                    v[j] -= delta
                else:
                    minv[j] -= delta
            j0 = j1
            if p[j0] == 0:
                break
        while j0:
            j1 = way[j0]
            p[j0] = p[j1]
            j0 = j1
    out = {}
    for j in range(1, n + 1):
        i = p[j]
        if i and i <= len(rows) and j <= len(cols) and weight.get((rows[i - 1], cols[j - 1]), 0) > 0:
            out[rows[i - 1]] = cols[j - 1]
    return out


# ---------------------------------------------------------------- the one decider for an item's stage
@dataclasses.dataclass(frozen=True)
class Facts:
    """What a run did with one gold item. None is could-not-judge, never False."""
    read: bool | None
    placed: bool | None
    folded: bool | None

    def stage(self):
        """The first lost stage, "hit", or ("could_not_judge", stage) where a fact is absent."""
        for stage, ok in ((Stage.READ, self.read), (Stage.PLACE, self.placed), (Stage.FOLD, self.folded)):
            if ok is None:
                return ("could_not_judge", stage)
            if not ok:
                return stage
        return "hit"

    def name(self):
        s = self.stage()
        return s if s == "hit" else (f"could_not_judge:{s[1].value}" if isinstance(s, tuple) else s.value)


def summarize(facts, cost):
    """The ladder: per stage, the items lost there and the model calls beside it."""
    lost, unjudged, hit = collections.Counter(), collections.Counter(), 0
    for f in facts:
        s = f.stage()
        if s == "hit":
            hit += 1
        elif isinstance(s, tuple):
            unjudged[s[1].value] += 1
        else:
            lost[s.value] += 1
    calls = calls_by_question(cost) if cost else None
    return {"n": len(facts), "hit": hit,
            "stages": {s.value: {"lost": lost[s.value], "could_not_judge": unjudged[s.value],
                                 "calls": calls.get(s.value, {}) if calls else None} for s in Stage},
            "unstaged_calls": calls.get("unstaged") if calls else None,
            "documents_read": cost.get("documents_read") if cost else None}


def load_module(name, path):
    """A scorer by path: ward/score.py and support/score.py share a file name."""
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def never_ran(system, why):
    return {"system": system, "status": "never-ran", "reason": why}


# ---------------------------------------------------------------- the baseline row, as the campaign recorded it
# campaign.md "LIVE FRONTIER" 0, 2026-10-09. --reproduce checks the moved instrument against these, exactly.
BASELINE_ROW = {
    "uv": {"identity.b_cubed": 0.838, "identity.ceaf_e": 0.752, "identity.lea": 0.8,
           "rungs.hit": 79, "rungs.no_case_state_claim": 232, "rungs.document_not_read": 25,
           "rungs.wrong_state": 33, "rungs.record_not_matched": 17, "ladder.n": 386},
    "ward": {"bars.deals.hit": 9, "bars.deals.n": 39, "bars.stage_served.hit": 2, "bars.stage_served.n": 47,
             "identity.b_cubed": 0.875, "identity.ceaf_e": 0.792, "identity.lea": 0.556, "identity.scored": 18},
}


def pick(row, dotted):
    for k in dotted.split("."):
        row = row[k]
    return row


def reproduce():
    import ladder_uv, ladder_ward  # noqa: E401, PLC0415
    rows, ok = [], True
    for system, mod, run in (("uv", ladder_uv, BASELINE / "uv"), ("ward", ladder_ward, BASELINE / "ward")):
        got = mod.measure(run)
        for k, want in BASELINE_ROW[system].items():
            have = pick(got, k)
            same = have == want
            ok &= same
            rows.append((system, k, want, have, "ok" if same else "MISMATCH"))
    w = max(len(r[1]) for r in rows)
    for r in rows:
        print(f"{r[0]:5} {r[1]:{w}}  baseline {r[2]!s:>6}  ladder {r[3]!s:>6}  {r[4]}")
    return ok


# ---------------------------------------------------------------- the table
def fmt_calls(calls):
    return ", ".join(f"{k} {v['per_document']}" for k, v in calls.items()) if calls else "-"


def table(results):
    out = []
    for r in results:
        if r["status"] != "judged":
            out.append(f"{r['system']:5} {r['status']}: {r['reason']}")
            continue
        lad = r["ladder"]
        out.append(f"{r['system']:5} {r['population']}: n {lad['n']}, hit {lad['hit']}, "
                   f"{lad['documents_read']} documents read; run {r['run']}")
        for s in Stage:
            st = lad["stages"][s.value]
            cnj = f" (+{st['could_not_judge']} could not judge)" if st["could_not_judge"] else ""
            out.append(f"      {s.name:5}  lost {st['lost']:4}{cnj:26}  calls/doc: {fmt_calls(st['calls'])}")
        if lad.get("unstaged_calls"):
            out.append(f"      unstaged calls/doc: {fmt_calls(lad['unstaged_calls'])}")
        idn = r.get("identity")
        if idn:
            out.append(f"      identity  B3 {idn['b_cubed']}  CEAF-e {idn['ceaf_e']}  LEA {idn['lea']}  "
                       f"over {idn['scored']} scored, coverage {idn['coverage']}")
        out.append(f"      rungs  {json.dumps(r['rungs'], sort_keys=True)}")
    return "\n".join(out)


def verdict(results):
    by = collections.Counter(r["status"] for r in results)
    parts = "; ".join(f"{r['system']} {r['status']}" + (f" (n {r['ladder']['n']})" if r["status"] == "judged"
                                                          else f": {r['reason']}") for r in results)
    if by["failed"]:
        return "failed", parts
    if by["judged"] == len(results):
        return "passed", parts
    if by["could-not-judge"]:
        return "could-not-judge", parts
    return "never-ran", parts


def run_system(system, path):
    import ladder_gvc, ladder_uv, ladder_ward  # noqa: E401, PLC0415
    mod = {"ward": ladder_ward, "uv": ladder_uv, "gvc": ladder_gvc}[system]
    if path is None:
        return never_ran(system, "no run given" if system != "gvc" else
                         "no end-to-end GVC run exists yet (the baseline's gvc/ is RESOLVE over gold mentions)")
    if not path.exists():
        return never_ran(system, f"{path} does not exist")
    try:
        return mod.measure(path)
    except SystemExit as e:  # a scorer that refused (er-score, a missing file it names)
        return {"system": system, "status": "failed", "reason": str(e)}


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    opt = lambda s: None if s in (None, "none") else pathlib.Path(s).expanduser()  # noqa: E731
    ap.add_argument("--ward", default=str(BASELINE / "ward"))
    ap.add_argument("--uv", default=str(BASELINE / "uv"))
    ap.add_argument("--gvc", default=None, help="an end-to-end GVC run directory (none exists yet)")
    ap.add_argument("--json", action="store_true", help="every ladder in full, as JSON, before the Judgement line")
    ap.add_argument("--reproduce", action="store_true")
    a = ap.parse_args()
    sys.path.insert(0, str(HERE))
    if a.reproduce:
        sys.exit(0 if reproduce() else 1)
    results = [run_system(s, opt(getattr(a, s))) for s in ("ward", "uv", "gvc")]
    print(json.dumps(results, indent=1, default=sorted) if a.json else table(results))
    sys.path.insert(0, str(HERE.parents[1] / "scripts/lib"))
    from judgement import emit  # noqa: PLC0415
    emit("stage-ladders", *verdict(results))


if __name__ == "__main__":
    main()
