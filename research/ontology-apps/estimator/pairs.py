#!/usr/bin/env python3
"""The estimator's inputs for a RESOLVE run, labelled (campaign ontology-layer E2b, order ontology-layer-6-estimator
step 1): every (statement, alternative) pair RESOLVE weighed, each source's say on it, and whether gold puts the two
in one particular.

    pairs.py RUN [RUN ...] --out DIR        # DIR/<run>.jsonl, one row per pair; DIR/labelled.json, one block per run

A row: {"run", "type", "document", "statement", "alternative", "comparison": {source: agreed}, "same": true | false |
null, "p": the model's p for the alternative where it was shown one, "zone"}. `same` is null where gold labels either
side with no particular, or several (a Ward file in two deals), and where one side's particular is `none` (a document
gold places in no deal or case: a pair with a real one is different, a pair of two is not judged): counted, never
defaulted. A GVC line is labelled with the set of gold chains its mentions belong to: two lines with equal sets are one
particular, with disjoint sets two, and overlapping unequal sets are not judged.

The pairs are the run's own trace, `atlas/resolve: the sources weighed` (resolve_records/select.rs), read from the
recorded half of a leg that replayed (ladder.recorded_half). A statement's type is the run's resolve_decisions.jsonl's
(the trace does not name it); on a blind author's run the type is read in our name through the evaluator's frozen map
(ladder.run_vocabulary), and only the one type each system's identity is judged on is labelled.

One labeller per system, each the ladder's own reading of gold (one decider, principle 8):
  gvc   resolve-statements: a statement is a gold mention, gold.json names its chain (score_resolve.py's reading).
        atlas-resolve: a statement is the lines it cites (`doc@..#"lA-B"`); its label is the one gold chain whose
        mentions those lines cover (ladder_gvc.items' cover), null when none or several.
  ward  a statement's document is a message; the message's file sits in exactly one tune gold deal (ladder_ward.identity),
        or in none (`none:<file>`, its own particular, as er_score reads a no-deal file), null when in several or unknown.
  uv    a statement's document is an issue, comment or event url; raw/documents.jsonl names its id, gold/cases.json its
        tune case (support/score.py load_gold), `none:<id>` for a known no-case document, null when ambiguous or outside.

labelled.json, per run: the estimate RESOLVE recorded (summary.json on resolve-statements; the type's last `agreement
weights fitted` line on atlas-resolve) beside each source's precision under gold over the pairs it agreed on, and the
rates EM is meant to find: P(agree | same), P(agree | different), the base rate. Checked against score_resolve.py
--trace on the two resolve-statements runs (`--check`).
"""
import argparse, collections, json, pathlib, re, sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
sys.dont_write_bytecode = True
import ladder as L  # noqa: E402

WEIGHED = re.compile(r'atlas/resolve: the sources weighed document="?([^" ]+)"? statement=(.+?) alternatives=(\[.*?\]) '
                     r'comparisons=(\[.*\]) zone=(\S+)')
FITTED = re.compile(r'atlas/resolve estimate: agreement weights fitted pairs=(\d+) prior=([\d.e+-]+) iterations=(\d+) '
                    r'sources=\{(.*)\}')
SOURCE = re.compile(r'"([^"]+)": SourceWeight \{ m: ([\d.e+-]+), u: ([\d.e+-]+), agree: ([\d.e+-]+|-?inf|NaN), '
                    r'disagree: ([\d.e+-]+|-?inf|NaN), precision: ([\d.e+-]+), spoke: (\d+)')
STATEMENT = re.compile(r'^(.*)@(\d+)\.\.(\d+)#"l(\d+)-(\d+)"$')
# The system each corpus is an instrument for, and the one record type its identity is judged on (our names).
SYSTEMS = {"crm-ward": ("ward", "deal"), "uv-support": ("uv", "case"), "cdcr-gvc": ("gvc", "happening"), "gvc": ("gvc", "happening")}


def run_name(run):
    run = pathlib.Path(run).resolve()
    parts = run.parts
    i = parts.index("runs") if "runs" in parts else len(parts) - 1
    return "--".join(parts[i + 1:])


# ---------------------------------------------------------------- labellers: statement id -> gold particular or None
def label_gvc_mentions(gold_path):
    chain = json.loads(pathlib.Path(gold_path).read_text())
    return lambda sid: chain.get(sid)


def label_gvc_lines():
    G = L.load_module("ladder_gvc", HERE.parent / "ladder_gvc.py")
    mentions, bodies, keys = G.load_gold(G.GOLD, G.CORPUS)
    lines = {d: G.line_spans(b) for d, b in bodies.items()}
    by_doc = collections.defaultdict(list)
    for m in mentions.values():
        by_doc[m["doc"]].append(m)
    cache = {}

    def label(sid):
        if sid in cache:
            return cache[sid]
        m = STATEMENT.match(sid)
        doc = keys.get(m.group(1)) if m else None
        out = None
        if doc is not None:
            ls = lines[doc]
            a, b = int(m.group(4)) - 1, int(m.group(5)) - 1
            if 0 <= a <= b < len(ls):
                s, e = ls[a][0], ls[b][1]
                chains = frozenset(x["chain"] for x in by_doc[doc] if s <= x["start"] and x["end"] <= e)
                out = chains or None
        cache[sid] = out
        return out

    return label


def label_ward():
    S = L.load_module("ward_score", HERE.parent / "ward/score.py")
    W = L.load_module("ladder_ward", HERE.parent / "ladder_ward.py")
    g = S.load_gold(S.WARD / "gold")
    path_of = {}
    for e in json.loads((S.WARD / "manifest.json").read_text()):
        for mid in e.get("message_ids") or []:
            path_of[W.doc_id(mid)] = e["path"]
    deals = collections.defaultdict(set)
    for d in g["deals"]:
        for f in d["files"]:
            deals[f].add(d["id"])
    tune = {f for f in g["files"] if f.split("/", 1)[0] in W.TUNE}

    def label(sid):
        path = path_of.get(sid.split("@", 1)[0])
        if path is None or path not in tune:
            return None
        ds = deals.get(path, set())
        if len(ds) == 1:
            return next(iter(ds))
        return f"none:{path}" if not ds else None

    return label


def label_uv():
    U = L.load_module("support_score", HERE.parent / "support/score.py")
    gold, ambiguous, none, _ = U.load_gold("tune")
    url_id = {d["url"]: str(d["id"]) for d in map(json.loads, filter(str.strip, (U.ROOT / "raw/documents.jsonl").read_text().splitlines()))}

    def label(sid):
        i = url_id.get(sid.split("@", 1)[0])
        if i is None or i in ambiguous:
            return None
        if i in gold:
            return gold[i]
        return f"none:{i}" if i in none else None

    return label


def same_particular(a, b):
    """Gold's verdict on a pair from its two labels: None when either is unlabelled; a `none:` document against a
    particular is different, against another `none:` not judged; chain sets (GVC lines) equal is one, disjoint two."""
    if a is None or b is None:
        return None
    if isinstance(a, frozenset) or isinstance(b, frozenset):
        if a == b:
            return True
        return False if not (a & b) else None
    a_none, b_none = str(a).startswith("none:"), str(b).startswith("none:")
    if a_none and b_none:
        return None
    return a == b


# ---------------------------------------------------------------- one run
def decisions_of(run, atlas):
    """statement -> (type, {alternative: p}, p_none) from the run's decisions (resolve/decisions.jsonl or the atlas's
    resolve_decisions.jsonl)."""
    run = pathlib.Path(run)
    path = run / "resolve/decisions.jsonl"
    if not path.exists():
        path = atlas["dir"] / "resolve_decisions.jsonl"
    out = {}
    for line in path.read_text().splitlines():
        if not line.strip():
            continue
        d = json.loads(line)
        for o in d["outcomes"]:
            ch = o.get("choice") or {}
            out[o["statement"]] = (d.get("type"), dict(map(tuple, ch.get("candidates", []))), ch.get("none"))
    return out


def fitted(line):
    m = FITTED.search(line)
    srcs = {s[0]: {"m": float(s[1]), "u": float(s[2]), "precision": float(s[5]), "spoke": int(s[6])} for s in SOURCE.findall(m.group(4))}
    return {"pairs": int(m.group(1)), "prior": float(m.group(2)), "iterations": int(m.group(3)), "sources": srcs}


def read_run(run):
    """(rows, recorded estimate per type, meta) for one run."""
    run = pathlib.Path(run)
    alone = (run / "resolve/summary.json").exists()
    if alone:
        summary = json.loads((run / "resolve/summary.json").read_text())
        system, judged = "gvc", summary["type_name"]
        vocab, label = None, label_gvc_mentions(L.BASELINE / "gvc/statements/gold.json")
        atlas = None
        type_of = lambda t: t  # noqa: E731
    else:
        atlas = L.run_atlas(run)
        if isinstance(atlas, str):
            sys.exit(f"{run}: {atlas}")
        system, judged = SYSTEMS[atlas["index"].name]
        vocab = L.run_vocabulary(run)
        if isinstance(vocab, str):
            sys.exit(f"{run}: {vocab}")
        types = (vocab or {}).get("types", {})
        type_of = (lambda t: t) if vocab is None else (lambda t: types.get(t, f"{L.BLIND_PREFIX}{t}"))
        label = {"gvc": label_gvc_lines, "ward": label_ward, "uv": label_uv}[system]()
    decided = decisions_of(run, atlas)
    lines, stop = L.recorded_half(run / "job.debug.log")
    rows, estimates, current = [], {}, None
    counts = collections.Counter()
    for raw in lines:
        if "agreement weights fitted" in raw:
            fit = fitted(raw)
            # The fit of no pairs is a Resolver's default (Estimate::none), logged as a type begins: no estimate.
            if current is not None and fit["pairs"] > 0:
                estimates[current] = fit
            continue
        m = WEIGHED.search(raw)
        if not m:
            continue
        doc, sid = m.group(1), m.group(2)
        t, p_of, p_none = decided.get(sid, (None, {}, None))
        if sid not in decided:
            counts["statements_not_in_decisions"] += 1
        if alone:
            t = judged  # resolve-statements runs one type; its decisions name none
        t = type_of(t) if t is not None else None
        current = t
        alts, comps = json.loads(m.group(3)), json.loads(m.group(4))
        g_s = label(sid) if t == judged else None
        for alt, comp in zip(alts, comps):
            if not comp:
                counts["pairs_no_source_spoke"] += 1
                continue
            g_a = label(alt) if t == judged else None
            same = same_particular(g_s, g_a)
            rows.append({"run": run_name(run), "type": t, "document": doc, "statement": sid, "alternative": alt,
                         "comparison": comp, "same": same, "p": p_of.get(alt), "p_none": p_none if alt in p_of else None,
                         "zone": m.group(5)})
    if alone:
        estimates = {judged: summary["estimate"]}
    meta = {"run": str(run), "name": run_name(run), "system": system, "judged_type": judged, "path": "resolve-statements" if alone else "atlas-resolve",
            "author": "blind" if vocab else "ours", "log_half": f"recorded (stopped at {stop})" if stop else "whole log",
            "types": dict(collections.Counter(r["type"] for r in rows)), "counts": dict(counts)}
    return rows, estimates, meta


def labelled_block(rows, estimate, judged):
    """Per source: the recorded estimate against gold over the judged type's pairs."""
    rs = [r for r in rows if r["type"] == judged]
    lab = [r for r in rs if r["same"] is not None]
    n_same = sum(r["same"] for r in lab)
    tally = collections.defaultdict(lambda: [[0, 0], [0, 0]])  # source -> [agree][same]
    for r in lab:
        for s, a in r["comparison"].items():
            tally[s][a][r["same"]] += 1
    out = {}
    for s, t in sorted(tally.items()):
        agreed = t[1][0] + t[1][1]
        same_n, diff_n = t[0][1] + t[1][1], t[0][0] + t[1][0]
        e = (estimate.get("sources") or {}).get(s, {})
        p = t[1][1] / agreed if agreed else None
        out[s] = {"estimated": round(e["precision"], 3) if e else None, "labelled": round(p, 3) if p is not None else None,
                  "agreed_pairs": agreed, "gap": round(abs(e["precision"] - p), 3) if e and p is not None else None,
                  "labelled_m": round(t[1][1] / same_n, 4) if same_n else None, "labelled_u": round(t[1][0] / diff_n, 4) if diff_n else None,
                  "estimated_m": round(e["m"], 4) if e else None, "estimated_u": round(e["u"], 4) if e else None,
                  "spoke_labelled": same_n + diff_n}
    gaps = [(v["gap"], s) for s, v in out.items() if v["gap"] is not None]
    return {"pairs": len(rs), "labelled_pairs": len(lab), "unlabelled_pairs": len(rs) - len(lab),
            "base_rate_labelled": round(n_same / len(lab), 4) if lab else None, "prior_recorded": round(estimate.get("prior", float("nan")), 4),
            "sources": out, "largest_gap": max(gaps) if gaps else None}


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("runs", nargs="+", type=pathlib.Path)
    ap.add_argument("--out", type=pathlib.Path, required=True)
    ap.add_argument("--check", action="store_true", help="on a resolve-statements run, compare with its score_resolve.json estimator block")
    a = ap.parse_args()
    a.out.mkdir(parents=True, exist_ok=True)
    blocks = {}
    for run in a.runs:
        rows, estimates, meta = read_run(run)
        with open(a.out / f"{meta['name']}.jsonl", "w") as f:
            for r in rows:
                f.write(json.dumps(r) + "\n")
        est = estimates.get(meta["judged_type"]) or {}
        block = labelled_block(rows, est, meta["judged_type"])
        blocks[meta["name"]] = {**meta, **block}
        print(f"{meta['name']}: {meta['path']} {meta['author']} {meta['system']} type={meta['judged_type']} pairs={block['pairs']} "
              f"labelled={block['labelled_pairs']} base={block['base_rate_labelled']} prior={block['prior_recorded']} largest_gap={block['largest_gap']}")
        for s, v in block["sources"].items():
            print(f"   {s:18s} est {v['estimated']}  lab {v['labelled']}  gap {v['gap']}  (agreed {v['agreed_pairs']}; m {v['estimated_m']} vs {v['labelled_m']}; u {v['estimated_u']} vs {v['labelled_u']})")
        if a.check and (run / "score_resolve.json").exists():
            ref = json.loads((run / "score_resolve.json").read_text())["estimator"]["sources"]
            for s, v in ref.items():
                mine = block["sources"].get(s, {})
                ok = mine.get("labelled") == v["labelled"] and mine.get("agreed_pairs") == v["agreed_pairs"] and mine.get("estimated") == v["estimated"]
                print(f"   check {s}: {'same' if ok else 'DIFFERS'} (score_resolve labelled {v['labelled']} n {v['agreed_pairs']} est {v['estimated']})")
                if not ok:
                    sys.exit(2)
    (a.out / "labelled.json").write_text(json.dumps(blocks, indent=1) + "\n")


if __name__ == "__main__":
    main()
