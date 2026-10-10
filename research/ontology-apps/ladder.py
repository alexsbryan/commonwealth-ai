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
On a scaffold leg (a recorded run, then its `--asker replay` rerun over the same atlas and logs) every ladder reads
the recorded half, through one accessor (run_atlas, trace), and its `read` entry says which half it read.

Every ladder reads gold through our recipes' names. A run made with a blind author's recipe is read through the
evaluator's frozen map from its names onto ours (blind-author/<tag>-mapping.json; translate, atlas_view), and each
ladder reports its identity's per-unit `placements`, which blind-author/agreement.py compares between two runs.

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
# Checked against the frozen scaffold binary's own logs (runs/scaffold-3sys/gvc, 984427884, 2026-10-09): the passes
# reader's `located` DEBUG line, its `chapter read` INFO line, and inference_client's `ok` line. A pattern that
# matches nothing in a run makes its reading could-not-judge (None), never zero: `cost["patterns"]` says which.
LOCATED = re.compile(r'located document="?([^" ]+)"? line=(\d+) kind="([^"]+)" p=(\S+) dist=(.*?) text=')
READ_LINE = re.compile(r"chapter read chapter=(\S+) documents=(\d+) claims=(\d+) calls=(\d+)")
CALL = re.compile(r"/v1/chat/completions ok phase=(\S+) .*?elapsed_ms=(\d+)")

# A scaffold leg (runs/scaffold-3sys/job.sh) copies its atlas to recorded/ after the recorded run, then reruns the
# same steps with `--asker replay` into the same atlas dir and the same logs; run_step.py marks each step
# "=== step <name> start", and the replay's steps are named replay_*. The ladder reads the recorded half.
RECORDED = "recorded"
REPLAY_STEP = re.compile(r"=== step (replay_\S+) start")


def recorded_half(path):
    """(the lines of `path` before the first replay step's marker, that marker's step name or None)."""
    out = []
    with open(path, errors="replace") as f:
        for raw in f:
            m = REPLAY_STEP.search(raw)
            if m:
                return out, m.group(1)
            out.append(raw)
    return out, None


def run_atlas(run):
    """The one accessor for a run's atlas: {"dir", "half", "index"}, or why there is none (a str).
    recorded/ when the run kept one (a scaffold leg); else the run's one data/indexes/*/atlas, refused when the
    logs show a replay ran over it. `index` is the atlas's corpus index dir (data/indexes/<corpus>), when single."""
    run = pathlib.Path(run)
    found = sorted(run.glob("data/indexes/*/atlas/atoms.json"))
    index = found[0].parent.parent if len(found) == 1 else None
    rec = run / RECORDED
    if rec.is_dir():
        if not (rec / "atoms.json").exists():
            return f"{rec} holds no atoms.json: the recorded atlas was not kept, and the atlas dir may be the replay's"
        return {"dir": rec, "half": "recorded", "index": index}
    if len(found) != 1:
        return f"{run} holds {len(found)} atlases (data/indexes/*/atlas/atoms.json); want one"
    if (run / "job.log").exists() and recorded_half(run / "job.log")[1]:
        return f"{run}'s logs hold a replay step but no {RECORDED}/: its atlas dir is the replay's"
    return {"dir": found[0].parent, "half": "the run's one atlas (no replay)", "index": index}


def run_trace(run):
    """trace(run) when the run kept both logs, else ({}, None): the one way a ladder reads a run's logs."""
    run = pathlib.Path(run)
    if not ((run / "job.debug.log").exists() and (run / "job.log").exists()):
        return {}, None
    lines, cost = trace(run)
    vocab = run_vocabulary(run)
    if not isinstance(vocab, dict):
        return lines, cost
    # a blind run's Locate names its own claim kinds: read them in ours (its labels are letters, kept as they are;
    # where several blind kinds map onto one of ours, `near` reads the most common one's letter: a diagnostic only)
    name = lambda k: (vocab.get("kinds", {}).get(k) or {}).get("as", f"{BLIND_PREFIX}{k}")  # noqa: E731
    return {d: [(ln, name(k), dist) for ln, k, dist in v] for d, v in lines.items()}, cost


# A run that read a subset of its corpus names the sections it read in sections.ids beside its atlas (comma-separated,
# as runs/scaffold-3sys/job.sh writes it). Its population is the fold's gold items on those sections' documents.
SECTIONS = "sections.ids"


def run_documents(run, atlas):
    """The one map from a run's sections to their documents: None when the run names no sections (its population is
    the whole fold), {"sections": n, "documents": {source_doc_id}} through the run's own index (chapters.json chunk
    ids -> chunks.lance source_doc_id), or why it cannot be mapped (a str: could-not-judge, never a smaller fold).
    Each system reads a source_doc_id as its gold document itself (uv: url -> id; ward: file; gvc: id or url)."""
    path = pathlib.Path(run) / SECTIONS
    if not path.exists():
        return None
    ids = [s.strip() for s in path.read_text().replace("\n", ",").split(",") if s.strip()]
    if not ids:
        return f"{path} names no section"
    if atlas["index"] is None or not (atlas["index"] / "chapters.json").exists():
        return f"{run} names its sections but holds no single index with a chapters.json to map them to documents"
    chapters = {c["id"]: c["chunk_ids"] for c in json.loads((atlas["index"] / "chapters.json").read_text())["chapters"]}
    absent = sorted(set(ids) - set(chapters))
    if absent:
        return f"{len(absent)} of {SECTIONS}'s sections are not in the run's chapters.json, e.g. {absent[0]}"
    import lance  # noqa: PLC0415  (only a run that names its sections needs it)
    rows = lance.dataset(str(atlas["index"] / "chunks.lance")).to_table(columns=["id", "source_doc_id"]).to_pylist()
    doc_of = {str(r["id"]): r["source_doc_id"] for r in rows}
    chunks = [str(c) for s in ids for c in chapters[s]]
    lost = [c for c in chunks if not doc_of.get(c)]
    if lost:
        return f"{len(lost)} chunks of the named sections have no source_doc_id in chunks.lance, e.g. chunk {lost[0]}"
    return {"sections": len(ids), "documents": {doc_of[c] for c in chunks}}


def scope(sliced, total, kept):
    """The `scope` every ladder reports: None for the whole fold, else the slice and the gold items it left out."""
    if sliced is None:
        return None
    return {"sections": sliced["sections"], "documents": len(sliced["documents"]), "items": kept,
            "items_left_out": total - kept}


def population(base, sliced):
    """The `population` every ladder names: the fold's gold items, and the restriction to the run's sections."""
    return base if sliced is None else f"{base} on the documents of the run's {sliced['sections']} sections ({SECTIONS})"


def could_not_judge(system, why):
    return {"system": system, "status": "could-not-judge", "reason": why}


def what_was_read(atlas, cost):
    """The `read` entry every ladder reports: which atlas and which half of the logs it measured."""
    return {"atlas": str(atlas["dir"]), "half": atlas["half"], "logs": cost["log_half"] if cost else "no logs kept"}


# The atom kinds a declared type's records are written as, each with the field naming its declared type: RESOLVE
# writes an entity type's records as Entity atoms and an event type's as Event atoms (4314b062f).
RECORD_ATOMS = {"Entity": "entity_type", "Event": "event_type"}


def records_of(atoms):
    """{atom id: its data, plus `record_type`} for every atom a declared type's records are written as."""
    return {a["data"]["id"]: {**a["data"], "record_type": a["data"].get(RECORD_ATOMS[a["atom_type"]])}
            for a in atoms if a.get("atom_type") in RECORD_ATOMS}


# ---------------------------------------------------------------- a run's vocabulary (blind authors)
# The ladders read gold through our recipes' names (types, claim kinds, attributes, values). A run made with a blind
# author's recipe is read through the evaluator's frozen map from that recipe's names onto ours
# (blind-author/<tag>-mapping.json, its "round" naming the round dir), here and nowhere else; our runs read through
# the identity. A blind run with no map is could-not-judge, never read under our names.
ROUND = re.compile(r"^blind-author-(\d{8})(?:-r(\d+))?$")
SYSTEMS = ("gvc", "ward", "uv")
MAPPINGS = HERE / "blind-author"
JOB_START = re.compile(r"job start: name=(\S+) corpus=(\S+) recipe=(\S+)")
BLIND_PREFIX, UNMAPPED = "blind:", "unmapped:"


def blind_of(recipe, home=None):
    """(round dir name, system) when `recipe` lies under ~/blind-author-<round>/<system>/, else None: the one decider."""
    home = pathlib.Path(home or pathlib.Path.home())
    try:
        rel = pathlib.Path(recipe).expanduser().resolve().relative_to(home.resolve())
    except ValueError:
        return None
    parts = rel.parts
    if len(parts) >= 3 and ROUND.match(parts[0]) and parts[1] in SYSTEMS:
        return parts[0], parts[1]
    return None


def run_recipe(run):
    """The recipe a run was made with: its job.log's `job start` line (a job leg), else resolve/summary.json's."""
    run = pathlib.Path(run)
    log = run / "job.log"
    if log.exists():
        with open(log, errors="replace") as f:
            m = next((m for line in f for m in [JOB_START.search(line)] if m), None)
        if m:
            return m.group(3)
    s = run / "resolve/summary.json"
    return json.loads(s.read_text()).get("recipe") if s.exists() else None


def run_vocabulary(run, home=None, mappings=MAPPINGS):
    """None (our recipe: the identity), {"round", "system", **map} for a blind run, or why not (a str)."""
    recipe = run_recipe(run)
    found = blind_of(recipe, home) if recipe else None
    if found is None:
        return None
    rnd, system = found
    maps = [p for p in sorted(pathlib.Path(mappings).glob("*-mapping.json"))
            if json.loads(p.read_text()).get("round") == rnd]
    if len(maps) != 1:
        return f"{run} ran blind recipe {recipe}; {len(maps)} of {mappings}/*-mapping.json name round {rnd}, want one"
    block = json.loads(maps[0].read_text())["systems"].get(system)
    if block is None:
        return f"{maps[0]} maps no {system}"
    return {"round": rnd, "system": system, "file": str(maps[0]), **block}


def _value(v, table, counts):
    if table is None:
        return v
    if isinstance(v, list):
        return [_value(x, table, counts) for x in v]
    if v is None:
        return v
    to = table.get(str(v))
    if to is None:
        counts["values_unmapped"] += 1
        return f"{UNMAPPED}{v}"
    return to


def _attrs(attrs, renames, values, prefix, counts):
    out = {}
    for k, v in (attrs or {}).items():
        out[renames.get(k, k)] = _value(v, values.get(f"{prefix}.{k}"), counts)
    return out


def translate(atoms, decisions, vocab):
    """(atoms, decisions, counts) renamed into our names by a run's vocabulary; the identity when vocab is None.
    Types and claim kinds the map does not name keep their name prefixed `blind:`; an attribute it does not name is
    kept as is (a document stamp, a ref); a mapped attribute's value it does not name becomes `unmapped:<value>`."""
    if vocab is None:
        return atoms, list(decisions), None
    counts = collections.Counter()
    types, kinds = vocab.get("types", {}), vocab.get("kinds", {})
    tattrs, values = vocab.get("attributes", {}), vocab.get("values", {})
    tname = lambda t: types.get(t, f"{BLIND_PREFIX}{t}") if t else t  # noqa: E731
    out = []
    for a in atoms:
        d = dict(a.get("data") or {})
        if a.get("atom_type") in RECORD_ATOMS:
            field = RECORD_ATOMS[a["atom_type"]]
            t = d.get(field)
            d["attributes"] = _attrs(d.get("attributes"), tattrs.get(t, {}), values, t, counts)
            d[field] = tname(t)
            counts["records_mapped" if t in types else "records_prefixed"] += 1
        elif a.get("atom_type") == "Claim":
            k = d.get("claim_kind")
            rule = kinds.get(k)
            if rule is None:
                d["claim_kind"] = f"{BLIND_PREFIX}{k}"
                counts["claims_prefixed"] += 1
            else:
                vals = {f"{k}.{a_}": t for a_, t in (rule.get("values") or {}).items()}
                d["attributes"] = {**_attrs(d.get("attributes"), rule.get("attributes") or {}, vals, k, counts),
                                   **(rule.get("set") or {})}
                d["claim_kind"] = rule["as"]
                counts["claims_mapped"] += 1
        out.append({**a, "data": d})
    decs = []
    for x in decisions:
        x = dict(x)
        t, attr = x.get("type"), x.get("attribute")
        if attr is not None:
            x["values"] = _value(x.get("values"), values.get(f"{t}.{attr}"), counts)
            x["attribute"] = tattrs.get(t, {}).get(attr, attr)
        x["type"] = tname(t)
        decs.append(x)
    return out, decs, dict(counts)


def atlas_view(run, atlas, atoms_path=None):
    """The one way a ladder loads a run's atlas: {"atoms", "decisions", "vocabulary"} in our names (translate), or
    why not (a str). `atoms_path` overrides the atlas's atoms.json (decisions are then the atlas's, if any)."""
    vocab = run_vocabulary(run)
    if isinstance(vocab, str):
        return vocab
    p = pathlib.Path(atoms_path) if atoms_path else atlas["dir"] / "atoms.json"
    atoms = json.loads(p.read_text())["atoms"]
    dp = atlas["dir"] / "derived_decisions.jsonl"
    decisions = [json.loads(l) for l in dp.read_text().splitlines() if l.strip()] if dp.exists() else []
    atoms, decisions, counts = translate(atoms, decisions, vocab)
    named = "identity (our recipe)" if vocab is None else f"{vocab['file']} ({vocab['round']}/{vocab['system']})"
    return {"atoms": atoms, "decisions": decisions, "unjudged": (vocab or {}).get("unjudged", {}),
            "vocabulary": {"map": named, "translated": counts}}


def trace(run):
    """({document: [(line, kind, {label: p})]}, cost) from the recorded half of run/job.debug.log and run/job.log,
    and run/_tokens.json. `cost["log_half"]` names where the reading stopped; `cost["patterns"]` counts each pattern's
    matches, and a reading whose pattern matched nothing is None."""
    lines = collections.defaultdict(list)
    calls, ms = collections.Counter(), collections.Counter()
    debug, replay_d = recorded_half(run / "job.debug.log")
    for raw in debug:
        m = LOCATED.search(raw)
        if m:
            dist = dict(x.rsplit(" ", 1) for x in m.group(5).split(", "))
            lines[m.group(1)].append((int(m.group(2)), m.group(3), {k: float(v) for k, v in dist.items()}))
            continue
        c = CALL.search(raw)
        if c:
            calls[c.group(1)] += 1
            ms[c.group(1)] += int(c.group(2))
    job, replay_j = recorded_half(run / "job.log")
    reads = [m for raw in job for m in [READ_LINE.search(raw)] if m]
    patterns = {"LOCATED": sum(len(v) for v in lines.values()), "READ_LINE": len(reads), "CALL": sum(calls.values())}
    docs = sum(int(m.group(2)) for m in reads) if reads else None
    tok = json.loads((run / "_tokens.json").read_text()) if (run / "_tokens.json").exists() else {}
    wall = (tok.get("updated_at_ms", 0) - tok.get("started_at_ms", 0)) / 1000 if tok else None
    stop = replay_d or replay_j
    cost = {"documents_read": docs, "phase1_wall_s": wall, "s_per_document": round(wall / docs, 2) if wall and docs else None,
            "calls_by_phase": dict(calls) if calls else None,
            "model_s_by_phase": {k: round(v / 1000, 1) for k, v in ms.items()} if calls else None,
            "calls_per_document": round(sum(calls.values()) / docs, 1) if docs and calls else None,
            "patterns": patterns,
            "could_not_judge": [k for k, n in patterns.items() if not n],
            "log_half": f"recorded: stopped at step {stop}'s start marker" if stop else "the whole log (no replay step)"}
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
    """{stage or "unstaged": {question kind: {calls, per_document}}} from trace()'s cost; None when no call line
    matched (could-not-judge, never zero calls)."""
    if cost.get("calls_by_phase") is None:
        return None
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
    unseen = "could_not_judge" if cost and calls is None else None  # logs kept, but no call line matched
    return {"n": len(facts), "hit": hit,
            "stages": {s.value: {"lost": lost[s.value], "could_not_judge": unjudged[s.value],
                                 "calls": calls.get(s.value, {}) if calls else unseen} for s in Stage},
            "unstaged_calls": calls.get("unstaged") if calls else unseen,
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
    if isinstance(calls, str):
        return calls
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
        if r.get("fold_checks"):
            out.append(f"      a hit: {r['fold_checks']}")
        ss = r.get("served_state")
        if ss:
            out.append("      served state: " + (f"{ss['served_right']} of {ss['cases']} cases right"
                                                 if ss["status"] == "judged" else f"could not judge: {ss['reason']}"))
        out.append(f"      read: atlas {r['read']['half']}; logs {r['read']['logs']}")
        cost = r.get("cost") or {}
        if cost.get("calls_per_document") is not None:
            docs, model_s = cost.get("documents_read") or 0, sum((cost.get("model_s_by_phase") or {}).values())
            out.append(f"      cost: {cost['calls_per_document']} model calls/doc, "
                       f"{round(model_s / docs, 2) if docs else 'could not judge'} model s/doc over every phase, "
                       f"{cost.get('s_per_document')} s/doc extract wall")
        sc = r.get("scope")
        if sc:
            out.append(f"      scope: {sc['sections']} sections, {sc['documents']} documents; "
                       f"{sc['items_left_out']} gold items lie outside them, left out")
        for s in Stage:
            st = lad["stages"][s.value]
            cnj = f" (+{st['could_not_judge']} could not judge)" if st["could_not_judge"] else ""
            out.append(f"      {s.name:5}  lost {st['lost']:4}{cnj:26}  calls/doc: {fmt_calls(st['calls'])}")
        if lad.get("unstaged_calls"):
            out.append(f"      unstaged calls/doc: {fmt_calls(lad['unstaged_calls'])}")
        idn = r.get("identity")
        if idn:
            out.append(f"      identity  conditional on the {idn['scored']} units placed (coverage {idn['coverage']}): "
                       f"B3 {idn['b_cubed']}  CEAF-e {idn['ceaf_e']}  LEA {idn['lea']}; recovery B3 "
                       f"{idn.get('recovery_b_cubed', 'not reported')} (missing and extra placements count)")
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
    # absolute once, here: ward/score.py's section_files joins a run's index under ~/.svrnmesh/indexes
    opt = lambda s: None if s in (None, "none") else pathlib.Path(s).expanduser().resolve()  # noqa: E731
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
