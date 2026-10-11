#!/usr/bin/env python3
"""Candidate estimators for what each identity source is worth, scored offline on the labelled pair tables
(campaign ontology-layer E2b, order ontology-layer-6-estimator step 2; tables from pairs.py).

    estimators.py TABLES --reproduce      # E0 must refit every run's recorded estimate (the instrument, principle 7)
    estimators.py TABLES --score [--out DIR]   # every candidate on every run: DIR/gaps.json and DIR/gaps.md

PRE-REGISTERED CHOICE RULE (committed before any candidate was scored; the rule is `choose`):
  Candidates, simplest first:
    E0  today's EM (resolve_records/estimate.rs: Fellegi-Sunter weights, conditional independence, fitted by EM).
    E2  E0 with each declared document field's u (P(agree | two particulars)) held at its agreement rate over random
        cross-document pairs of the run's own documents, free of the proposer; a source whose values the run does not
        record keeps E0's u, and the table says so.
    E3  E0 with the text-reading sources (proposed_answer, model_choice, reasoned_choice) fitted as one categorical
        source over their joint say, so their dependence is not assumed away.
    E4  E2 and E3 together.
  A source is JUDGED on a run when gold labels at least 20 of the pairs it agreed on (fewer cannot resolve .1: the
  binomial CI90 at n=20 is wider than ±.15); every judged source is scored |estimated − labelled|, with the labelled
  precision's CI90 bootstrapped over documents (500 draws, seed 7, as score_resolve.py). A candidate PASSES a run
  when every judged source's gap is at most .1 (layer-estimator's target). The chosen estimator is the first in the
  order above that passes all eight runs. If none does, today's E0 stays and the result is the curve: per candidate,
  per run, the largest judged gap (the order's "not worth continuing if").
  E1 is not a candidate but a check of E2's premise: each document field's random-pair u beside its labelled u over
  the proposed pairs. Oracles O1 (u held at labelled u), O2 (prior held at the labelled base rate), O3 (both) locate
  the error; they read labels and can never be ported.

E7 PROBE (order ontology-layer-7-entities step 2; `--entities DIR` reads entities.py's feature rows), PRE-REGISTERED
before any score against gold:
  Feature sources, fixed before scoring. A pair's two sides share an entity of label X when a mention of label X
  normalises the same on both (entities.py). Labels used: Person, Organization, Location. Event is left out (it is the
  kind the recipes already read as `necessary:kind`, and the same word on both sides is what a lookalike is); Work
  fires on 5 to 18 documents a corpus and is left out. Scope `document` on every run; `lines` (the cited lines) only on
  GVC, where the reader's lines are verified against the raw body.
    E5   E0 plus one source `entity`: the sides share an entity of any of the three labels, document scope.
    E6   E0 plus three sources `entity:Person`, `entity:Organization`, `entity:Location`, document scope.
    E5L  E5 at lines scope (GVC runs only).
    E6L  E6 at lines scope (GVC runs only).
  Lookalikes: the labelled pairs a text-reading source (proposed_answer or model_choice) agreed on; the false ones are
  those gold calls different. On those agreed pairs, per run: P(same | feature agrees) against P(same | date agrees)
  and P(same | kind agrees) over the same pairs, and the feature's recall of the same pairs.
  The order's stop fires when, on any run where the feature and date (or kind) are each judged on >= 20 of those
  agreed pairs, the feature's P(same | agrees) is no better than the better of date's and kind's.
  Verdict for the operator, from the largest judged gap (E2b's rule, same JUDGED_MIN and TARGET):
    WORTH A STAGE    E5 or E6 (the simplest that does) is within .1 on every judged run.
    PARTIAL          not that, but on both GVC RESOLVE-alone runs the largest judged gap at least halves against E0,
                     and no run E0 had within .1 leaves it.
    NOT WORTH        otherwise, or the stop above fired.
  The lines-scope variants are reported beside, never chosen over a document-scope one that reaches the same verdict.

E7 STEP 2b (the seat, after the agreement verdict; `--differs DIR`), PRE-REGISTERED before scoring: do entities that
DIFFER on the cited lines separate the lookalikes? GVC only (verified lines), from the same cache, no new extraction.
  Feature `entity_differs:<label>` for Person and Location: both sides' cited lines name at least one mention of the
  label and the two sides' normalised sets do not intersect: it FIRES (evidence against identity: another victim,
  another city). Where both sides name the label and the sets intersect it agrees; where either side names none it
  is absent. `entity_differs` (the candidate) fires when either label's does, agrees when neither fires and some
  label has both sides, else absent.
  Lookalike false links: the labelled pairs a text-reading source agreed on that gold calls different.
  WORTH A STAGE when, on both GVC RESOLVE-alone runs (ours and blind), `entity_differs` fires on at most .10 of the
  gold-same pairs, at least .90 of the pairs it fires on are gold-different, and it flags at least .30 of the
  lookalike false links; judged only where it fires on >= 20 labelled pairs. NOT WORTH otherwise. Per-label rows
  and the GVC end-to-end runs are reported beside, not gated.
  Beside the verdict: E0 with `entity_differs` added as a source (fitted on agree and disagree; the decider would
  apply its disagreement weight only), its largest judged gap with and without the feature's own row; and on the
  two RESOLVE-alone runs the CoNLL (MUC, B3, CEAF-e, LEA) a decider reaches when every weighed link whose pair the
  feature fires on is vetoed (the statement opens its own record), from resolve/decisions.jsonl and clustering.json
  through `sovereign bench er-score`, with the vetoed links counted right and wrong under gold.
"""
import argparse, collections, json, math, pathlib, random, sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
sys.dont_write_bytecode = True
import ladder as L  # noqa: E402

TEXT = ("proposed_answer", "model_choice", "reasoned_choice")
START_PRIOR, START_M, START_U, TOLERANCE, MAX_ITERATIONS, EDGE = 0.1, 0.9, 0.1, 1e-9, 500, 1e-6
JUDGED_MIN, REPS, SEED, TARGET = 20, 500, 7, 0.1
RUNS = ["c2-lines-gvc-resolve-alone", "blind-r2--gvc-resolve-alone", "c2-lines--ward-tune", "c2-lines--uv-third", "c2-lines--gvc",
        "blind-r2--ward-tune", "blind-r2--uv-third", "blind-r2--gvc"]
CANDIDATES = ("E0", "E2", "E3", "E4")
ORACLES = ("O1", "O2", "O3")
ENTITY_LABELS = ("Person", "Organization", "Location")
ENTITY_CANDIDATES = ("E5", "E6", "E5L", "E6L")
GVC_RUNS = [r for r in RUNS if "gvc" in r]


def clamp(p):
    return min(max(p, EDGE), 1 - EDGE)


# ---------------------------------------------------------------- the fit (estimate.rs in Python)
def view(comp, joint):
    """The comparison as the fit sees it: {source: True/False}, with the text sources as one tuple-valued source."""
    if not joint:
        return dict(comp)
    out = {k: v for k, v in comp.items() if k not in TEXT}
    said = tuple(comp.get(k) for k in TEXT)
    if any(v is not None for v in said):
        out["text"] = said
    return out


def identical(patterns):
    """Groups of binary sources whose say (agree, disagree, absent) is the same on every pattern; first names the group."""
    names = sorted({s for p in patterns for s, x in p if not isinstance(x, tuple)})
    say = lambda p, s: next((x for t, x in p if t == s), None)  # noqa: E731
    groups, taken = [], set()
    for i, a in enumerate(names):
        if a in taken:
            continue
        g = [a] + [b for b in names[i + 1:] if b not in taken and all(say(p, a) == say(p, b) for p in patterns)]
        if len(g) > 1:
            groups.append(g)
            taken.update(g)
    return groups


def fit(comps, joint=False, fixed_u=None, fixed_prior=None):
    """EM over the comparisons. Binary sources as estimate.rs fits them (posterior-mean rates, agreeing never evidence
    against, identical sources once, classes named by lean); a tuple-valued source as categorical with Dirichlet-1
    smoothing. `fixed_u`: {source: u} held through the M-step; `fixed_prior`: the prior held."""
    fixed_u = {s: clamp(v) for s, v in (fixed_u or {}).items()}  # a held rate is still an open-interval probability
    patterns = collections.Counter()
    for c in comps:
        v = view(c, joint)
        if v:
            patterns[tuple(sorted(v.items(), key=lambda kv: kv[0]))] += 1
    total = sum(patterns.values())
    groups = identical(list(patterns))
    drop = {m for g in groups for m in g[1:]}
    folded = collections.Counter()
    for p, n in patterns.items():
        folded[tuple(kv for kv in p if kv[0] not in drop)] += n
    binary = sorted({s for p in folded for s, x in p if not isinstance(x, tuple)})
    cat_states = collections.defaultdict(set)
    for p in folded:
        for s, x in p:
            if isinstance(x, tuple):
                cat_states[s].add(x)
    rates = {s: [START_M, fixed_u.get(s, START_U)] for s in binary}
    cat = {}
    for s, xs in cat_states.items():
        pm = {x: (0.1 + 0.4 * sum(1 for y in x if y)) for x in xs}
        pu = {x: (0.9 - 0.4 * sum(1 for y in x if y)) for x in xs}
        cat[s] = [{x: v / sum(pm.values()) for x, v in pm.items()}, {x: v / sum(pu.values()) for x, v in pu.items()}]
    prior = fixed_prior if fixed_prior is not None else START_PRIOR
    iterations = 0
    if total > 0:
        while iterations < MAX_ITERATIONS:
            iterations += 1
            seen = {s: [0.0, 0.0, 0.0, 0.0] for s in binary}
            cseen = {s: [collections.Counter(), collections.Counter(), 0.0, 0.0] for s in cat}
            matched = 0.0
            for p, n in folded.items():
                lm, lu = math.log(prior), math.log(1 - prior)
                for s, x in p:
                    if isinstance(x, tuple):
                        lm += math.log(cat[s][0][x]); lu += math.log(cat[s][1][x])
                    else:
                        m, u = rates[s]
                        lm += math.log(m if x else 1 - m); lu += math.log(u if x else 1 - u)
                g = 1 / (1 + math.exp(lu - lm))
                matched += n * g
                for s, x in p:
                    if isinstance(x, tuple):
                        cseen[s][0][x] += n * g; cseen[s][1][x] += n * (1 - g); cseen[s][2] += n * g; cseen[s][3] += n * (1 - g)
                    else:
                        e, a = seen[s], (1.0 if x else 0.0)
                        e[0] += n * g * a; e[1] += n * g; e[2] += n * (1 - g) * a; e[3] += n * (1 - g)
            moved = 0.0
            if fixed_prior is None:
                nxt = clamp((matched + 1) / (total + 2))
                moved = max(moved, abs(nxt - prior)); prior = nxt
            for s in binary:
                e = seen[s]
                m = clamp((e[0] + 1) / (e[1] + 2))
                u = fixed_u[s] if s in fixed_u else clamp((e[2] + 1) / (e[3] + 2))
                if m < u:
                    pooled = u if s in fixed_u else clamp((e[0] + e[2] + 1) / (e[1] + e[3] + 2))
                    m, u = pooled, pooled
                moved = max(moved, abs(m - rates[s][0]), abs(u - rates[s][1]))
                rates[s] = [m, u]
            for s in cat:
                k = len(cat_states[s])
                pm = {x: (cseen[s][0][x] + 1) / (cseen[s][2] + k) for x in cat_states[s]}
                pu = {x: (cseen[s][1][x] + 1) / (cseen[s][3] + k) for x in cat_states[s]}
                moved = max([moved] + [abs(pm[x] - cat[s][0][x]) for x in pm] + [abs(pu[x] - cat[s][1][x]) for x in pu])
                cat[s] = [pm, pu]
            if moved < TOLERANCE:
                break
        lean = sum(m - u for m, u in rates.values())
        if lean < 0:
            prior = 1 - prior
            rates = {s: [u, m] for s, (m, u) in rates.items()}
            cat = {s: [pu, pm] for s, (pm, pu) in cat.items()}
    else:
        prior = 0.5
    sources = {}
    for s, (m, u) in rates.items():
        group = next((g for g in groups if g[0] == s), [s])
        for member in group:
            sources[member] = {"m": m, "u": u, "precision": prior * m / (prior * m + (1 - prior) * u)}
    for s, (pm, pu) in cat.items():
        for i, name in enumerate(TEXT):
            m = sum(p for x, p in pm.items() if x[i] is True)
            u = sum(p for x, p in pu.items() if x[i] is True)
            if any(x[i] is not None for x in pm):
                sources[name] = {"m": m, "u": u, "precision": prior * m / (prior * m + (1 - prior) * u) if m + u else float("nan")}
        sources[s] = {"states": {str(x): [pm[x], pu[x]] for x in pm}}
    return {"pairs": total, "prior": prior, "iterations": iterations, "sources": sources}


# ---------------------------------------------------------------- random cross-document pairs (E1/E2's u)
def collision(values):
    """P(two documents drawn at random, from different documents, agree on the value): the proposer-free u."""
    vals = [v for v in values if v is not None]
    n = len(vals)
    if n < 2:
        return None
    c = collections.Counter(vals)
    return sum(k * (k - 1) for k in c.values()) / (n * (n - 1))


def stamps_of(name, rows):
    """{source: {document: value}} for the declared document fields of a run's judged-type documents, from the
    layer's own stamps (the claims' document_* attributes on atlas-resolve; records.json and the corpus on
    resolve-statements). A field with no recorded values is absent, so E2 keeps E0's u for it, named."""
    docs = {r["document"] for r in rows} | {r["alternative"].split("@", 1)[0].split("/", 1)[0] for r in rows}
    out = collections.defaultdict(dict)
    run = pathlib.Path("runs") / name.replace("--", "/")
    if (run / "resolve/records.json").exists():
        corpus = pathlib.Path.home() / ".svrnmesh/bench-corpora/gvc/raw/documents.jsonl"
        for d in map(json.loads, filter(str.strip, corpus.read_text().splitlines())):
            if d["id"] in docs:
                out["document_date"][d["id"]] = d.get("created_at")
        kinds = {}
        for rec in json.loads((run / "resolve/records.json").read_text()):
            k = (rec.get("fields") or {}).get("kind")
            if k:
                kinds[rec["id"]] = tuple(sorted(k))
        if kinds:
            out["necessary:kind"] = kinds
        return out
    atlas = L.run_atlas(run)
    atoms = json.loads((atlas["dir"] / "atoms.json").read_text())["atoms"]
    for a in atoms:
        if a.get("atom_type") != "Claim":
            continue
        attrs = (a.get("data") or {}).get("attributes") or {}
        key = None
        for e in (a["data"].get("evidence") or []):
            if e.get("source_doc_id") in docs:
                key = e["source_doc_id"]
        if key is None:
            continue
        for s in ("document_date", "document_thread"):
            if attrs.get(s) is not None:
                out[s][key] = str(attrs[s])
    return out


def random_pair_u(name, rows):
    stamps = stamps_of(name, rows)
    return {s: collision(list(v.values())) for s, v in stamps.items() if collision(list(v.values())) is not None}


# ---------------------------------------------------------------- labels and scoring
def labelled(rows):
    """Per source: labelled precision over agreed pairs, its CI90 over documents, agreed count, labelled m and u."""
    by_doc = collections.defaultdict(list)
    for r in rows:
        if r["same"] is not None:
            by_doc[r["document"]].append(r)
    docs = list(by_doc.values())

    def tally(sample):
        t = collections.defaultdict(lambda: [[0, 0], [0, 0]])
        for rs in sample:
            for r in rs:
                for s, a in r["comparison"].items():
                    t[s][a][r["same"]] += 1
        return t

    point = tally(docs)
    rng = random.Random(SEED)
    boots = [tally([rng.choice(docs) for _ in docs]) for _ in range(REPS)]
    out = {}
    for s, t in point.items():
        agreed = t[1][0] + t[1][1]
        same_n, diff_n = t[0][1] + t[1][1], t[0][0] + t[1][0]
        b = sorted(x[s][1][1] / (x[s][1][0] + x[s][1][1]) for x in boots if s in x and (x[s][1][0] + x[s][1][1]))
        out[s] = {"labelled": t[1][1] / agreed if agreed else None, "agreed": agreed,
                  "ci90": [b[int(.05 * (len(b) - 1))], b[int(.95 * (len(b) - 1))]] if b else None,
                  "labelled_m": t[1][1] / same_n if same_n else None, "labelled_u": t[1][0] / diff_n if diff_n else None}
    n = sum(len(d) for d in docs)
    base = sum(r["same"] for d in docs for r in d) / n if n else None
    return out, base


def with_entities(rows, feats, kind):
    """The comparisons with the feature sources E5/E6 (document scope) or E5L/E6L (lines scope) added; a pair whose
    feature is absent (no text for a side, no cited lines) carries no entity source, as a silent source would."""
    scope = "lines" if kind.endswith("L") else "document"
    out = []
    for r, f in zip(rows, feats):
        c = dict(r["comparison"])
        block = (f or {}).get(scope)
        if block:
            shared = {lab: bool(block.get(lab, {}).get("shared")) for lab in ENTITY_LABELS}
            if kind.startswith("E5"):
                c["entity"] = any(shared.values())
            else:
                for lab, v in shared.items():
                    c[f"entity:{lab}"] = v
        out.append(c)
    return out


def lookalikes(rows, feats, scope):
    """Per text source, over its agreed labelled pairs: the feature's P(same | agrees) and recall of the same pairs
    beside date's and kind's on the same pairs, and how many false links the feature leaves (disagrees on)."""
    out = {}
    for src in ("proposed_answer", "model_choice"):
        agreed = [(r, f) for r, f in zip(rows, feats) if r["same"] is not None and r["comparison"].get(src) is True]
        if not agreed:
            continue
        same = [x for x in agreed if x[0]["same"]]
        diff = [x for x in agreed if not x[0]["same"]]
        ent = lambda f: bool((f or {}).get(scope)) and any((f[scope].get(lab) or {}).get("shared") for lab in ENTITY_LABELS)  # noqa: E731
        block = {"agreed": len(agreed), "same": len(same), "different": len(diff), "text_precision": round(len(same) / len(agreed), 3)}
        for name, says in (("entity", ent), ("document_date", lambda f, r=None: None), ("necessary:kind", None)):
            if name == "entity":
                a_same = sum(ent(f) for _, f in same); a_diff = sum(ent(f) for _, f in diff)
            else:
                a_same = sum(1 for r, _ in same if r["comparison"].get(name) is True)
                a_diff = sum(1 for r, _ in diff if r["comparison"].get(name) is True)
            spoke = sum(1 for r, _ in agreed if name == "entity" or name in r["comparison"])
            n = a_same + a_diff
            block[name] = {"agrees_on": n, "precision": round(a_same / n, 3) if n else None, "recall_same": round(a_same / len(same), 3) if same else None,
                           "false_links_left": len(diff) - a_diff, "judged": n >= JUDGED_MIN and spoke > 0}
        out[src] = block
    return out


def candidate(kind, comps, lab, base, rand_u):
    if kind == "E0":
        return fit(comps)
    if kind == "E2":
        return fit(comps, fixed_u=rand_u)
    if kind == "E3":
        return fit(comps, joint=True)
    if kind == "E4":
        return fit(comps, joint=True, fixed_u=rand_u)
    if kind == "O1":
        return fit(comps, fixed_u={s: v["labelled_u"] for s, v in lab.items() if v["labelled_u"] is not None})
    if kind == "O2":
        return fit(comps, fixed_prior=base)
    if kind == "O3":
        return fit(comps, fixed_prior=base, fixed_u={s: v["labelled_u"] for s, v in lab.items() if v["labelled_u"] is not None})
    raise ValueError(kind)


def score(est, lab):
    """Per source: estimated, labelled, gap, judged; and the run's largest judged gap (None when nothing is judged)."""
    rows, largest = {}, None
    for s, v in sorted(lab.items()):
        e = (est["sources"].get(s) or {}).get("precision")
        judged = v["agreed"] >= JUDGED_MIN and v["labelled"] is not None and e is not None
        gap = abs(e - v["labelled"]) if judged else None
        rows[s] = {"estimated": None if e is None else round(e, 3), "labelled": None if v["labelled"] is None else round(v["labelled"], 3),
                   "agreed": v["agreed"], "ci90": None if not v["ci90"] else [round(x, 3) for x in v["ci90"]],
                   "judged": judged, "gap": None if gap is None else round(gap, 3)}
        if gap is not None and (largest is None or gap > largest[0]):
            largest = (round(gap, 3), s)
    return rows, largest


def choose(results):
    """The pre-registered rule: the first candidate, simplest first, that passes every run; else None (keep E0)."""
    for c in CANDIDATES:
        if all(results[c][r]["passes"] for r in RUNS):
            return c
    return None


def load(tables, name):
    rows = [json.loads(l) for l in (tables / f"{name}.jsonl").read_text().splitlines() if l.strip()]
    judged = json.loads((tables / "labelled.json").read_text())[name]["judged_type"]
    return [r for r in rows if r["type"] == judged]


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("tables", type=pathlib.Path)
    ap.add_argument("--reproduce", action="store_true")
    ap.add_argument("--score", action="store_true")
    ap.add_argument("--out", type=pathlib.Path, default=HERE)
    ap.add_argument("--entities", type=pathlib.Path, help="entities.py's feature dir: score the E7 probe (gaps-e7.md/json)")
    ap.add_argument("--differs", type=pathlib.Path, help="entities.py's feature dir: score E7 step 2b, entities that differ (gaps-e7b.md/json)")
    a = ap.parse_args()
    recorded = json.loads((a.tables / "labelled.json").read_text())
    if a.differs:
        differs_probe(a.tables, a.differs, a.out)
        return
    if a.entities:
        entities_probe(a.tables, a.entities, a.out)
        return
    if a.reproduce:
        worst = 0.0
        for name in RUNS:
            rows = load(a.tables, name)
            est = fit([r["comparison"] for r in rows])
            rec = recorded[name]
            diffs = []
            for s, v in rec["sources"].items():
                if v["estimated"] is None:
                    continue
                d = abs(est["sources"][s]["precision"] - v["estimated"])
                diffs.append(f"{s} {est['sources'][s]['precision']:.3f}/{v['estimated']:.3f}")
                worst = max(worst, d)
            print(f"{name:30s} pairs {est['pairs']}/{rec['pairs']} prior {est['prior']:.4f}/{rec['prior_recorded']:.4f} " + " ".join(diffs))
        print(f"largest difference from the recorded estimate: {worst:.4f}")
        sys.exit(0 if worst < 1e-3 else 1)
    if a.score:
        results = {c: {} for c in CANDIDATES + ORACLES}
        premise = {}
        for name in RUNS:
            rows = load(a.tables, name)
            comps = [r["comparison"] for r in rows]
            lab, base = labelled(rows)
            rand_u = random_pair_u(name, rows)
            premise[name] = {s: {"random_pair_u": round(u, 4), "labelled_u": None if lab.get(s, {}).get("labelled_u") is None else round(lab[s]["labelled_u"], 4)}
                             for s, u in sorted(rand_u.items())}
            for c in CANDIDATES + ORACLES:
                est = candidate(c, comps, lab, base, rand_u)
                srcs, largest = score(est, lab)
                results[c][name] = {"prior": round(est["prior"], 4), "base_rate_labelled": None if base is None else round(base, 4),
                                    "sources": srcs, "largest_gap": largest,
                                    "passes": largest is None or largest[0] <= TARGET,
                                    "judged_sources": [s for s, v in srcs.items() if v["judged"]]}
        chosen = choose(results)
        out = {"rule": "first of " + ", ".join(CANDIDATES) + f" whose every judged source (>= {JUDGED_MIN} agreed labelled pairs) is within {TARGET} on all {len(RUNS)} runs",
               "chosen": chosen, "E1_premise": premise, "results": results}
        a.out.mkdir(parents=True, exist_ok=True)
        (a.out / "gaps.json").write_text(json.dumps(out, indent=1) + "\n")
        lines = ["# Estimator candidates: largest judged gap per run (|estimated − labelled| on sources with >= 20 agreed labelled pairs)", "",
                 "| run | " + " | ".join(CANDIDATES + ORACLES) + " |", "|---|" + "---|" * len(CANDIDATES + ORACLES)]
        for name in RUNS:
            cells = []
            for c in CANDIDATES + ORACLES:
                lg = results[c][name]["largest_gap"]
                cells.append("could-not-judge" if lg is None else f"{lg[0]:.3f} ({lg[1]})")
            lines.append(f"| {name} | " + " | ".join(cells) + " |")
        lines += ["", f"Chosen under the pre-registered rule: {chosen or 'none (E0 stays)'}", "",
                  "## E1: is a document field's random-pair u the u the weighed pairs have?", "", "| run | source | random-pair u | labelled u (proposed pairs) |", "|---|---|---|---|"]
        for name in RUNS:
            for s, v in premise[name].items():
                lines.append(f"| {name} | {s} | {v['random_pair_u']} | {v['labelled_u']} |")
        lines += ["", "## Per source", ""]
        for name in RUNS:
            lines.append(f"### {name} (labelled base rate {results['E0'][name]['base_rate_labelled']})")
            lines.append("")
            lines.append("| source | agreed | labelled [CI90] | " + " | ".join(CANDIDATES + ORACLES) + " |")
            lines.append("|---|---|---|" + "---|" * len(CANDIDATES + ORACLES))
            for s, v in results["E0"][name]["sources"].items():
                ci = f"{v['labelled']} {v['ci90']}" if v["labelled"] is not None else "none agreed"
                row = [f"{results[c][name]['sources'][s]['estimated']}" + ("" if results[c][name]["sources"][s]["judged"] else " (unjudged)") for c in CANDIDATES + ORACLES]
                lines.append(f"| {s} | {v['agreed']} | {ci} | " + " | ".join(row) + " |")
            lines.append("")
        (a.out / "gaps.md").write_text("\n".join(lines) + "\n")
        print("\n".join(lines[:4 + len(RUNS) + 2]))


def entities_probe(tables, feature_dir, out_dir):
    results, look, stop = {}, {}, []
    for name in RUNS:
        rows = load(tables, name)
        feats = [json.loads(l) for l in (feature_dir / f"{name}.jsonl").read_text().splitlines() if l.strip()]
        assert len(feats) == len(rows) and all(f["statement"] == r["statement"] and f["alternative"] == r["alternative"] for f, r in zip(feats, rows))
        lab, base = labelled(rows)
        results[name] = {}
        e0 = fit([r["comparison"] for r in rows])
        results[name]["E0"] = score(e0, lab)
        for kind in ENTITY_CANDIDATES:
            if kind.endswith("L") and name not in GVC_RUNS:
                continue
            comps = with_entities(rows, feats, kind)
            est = fit(comps)
            lab_e, _ = labelled([{**r, "comparison": c} for r, c in zip(rows, comps)])
            results[name][kind] = score(est, lab_e)
        look[name] = {"document": lookalikes(rows, feats, "document")}
        if name in GVC_RUNS:
            look[name]["lines"] = lookalikes(rows, feats, "lines")
        for src, b in look[name]["document"].items():
            e, d, k = b["entity"], b["document_date"], b["necessary:kind"]
            rivals = [x["precision"] for x in (d, k) if x["judged"] and x["precision"] is not None]
            if e["judged"] and rivals and e["precision"] <= max(rivals):
                stop.append(f"{name}/{src}: entity {e['precision']} <= {max(rivals)}")

    def largest(name, kind):
        return results[name].get(kind, (None, None))[1]

    def passes(name, kind):
        lg = largest(name, kind)
        return lg is None or lg[0] <= TARGET

    verdict, chosen = "NOT WORTH", None
    for kind in ("E5", "E6"):
        if all(passes(n, kind) for n in RUNS):
            verdict, chosen = "WORTH A STAGE", kind
            break
    if verdict == "NOT WORTH" and not stop:
        for kind in ("E5", "E6"):
            halves = all(largest(n, "E0") and largest(n, kind) and largest(n, kind)[0] <= largest(n, "E0")[0] / 2 for n in RUNS[:2])
            keeps = all(passes(n, kind) for n in RUNS if passes(n, "E0"))
            if halves and keeps:
                verdict, chosen = "PARTIAL", kind
                break
    if stop:
        verdict = "NOT WORTH (stop fired)"
    kinds = ("E0",) + ENTITY_CANDIDATES
    lines = ["# E7 probe: largest judged gap per run, today's EM with an entity source added", "",
             "| run | " + " | ".join(kinds) + " |", "|---|" + "---|" * len(kinds)]
    for name in RUNS:
        cells = []
        for k in kinds:
            if k not in results[name]:
                cells.append("n/a"); continue
            lg = results[name][k][1]
            cells.append("could-not-judge" if lg is None else f"{lg[0]:.3f} ({lg[1]})")
        lines.append(f"| {name} | " + " | ".join(cells) + " |")
    lines += ["", f"Verdict under the pre-registered rule: {verdict}" + (f" ({chosen})" if chosen else ""),
              "Stop clause: " + ("; ".join(stop) if stop else "did not fire"), "",
              "## Lookalikes: over each text source's agreed labelled pairs, who separates gold-different from gold-same", "",
              "| run | scope | text source | agreed (same/diff) | text P | entity P / recall / false left | date P / recall | kind P / recall |", "|---|---|---|---|---|---|---|---|"]
    for name in RUNS:
        for scope, blocks in look[name].items():
            for src, b in blocks.items():
                f = lambda x: f"{x['precision']} / {x['recall_same']}" + ("" if x["judged"] else " (unjudged)")  # noqa: E731
                lines.append(f"| {name} | {scope} | {src} | {b['agreed']} ({b['same']}/{b['different']}) | {b['text_precision']} | "
                             f"{f(b['entity'])} / {b['entity']['false_links_left']} left of {b['different']} | {f(b['document_date'])} | {f(b['necessary:kind'])} |")
    lines += ["", "## Per source", ""]
    for name in RUNS:
        present = [k for k in kinds if k in results[name]]
        lines.append(f"### {name}"); lines.append("")
        lines.append("| source | " + " | ".join(f"{k} est (lab, agreed)" for k in present) + " |"); lines.append("|---|" + "---|" * len(present))
        srcs = sorted({s for k in present for s in results[name][k][0]})
        for s_ in srcs:
            cells = []
            for k in present:
                v = results[name][k][0].get(s_)
                cells.append("-" if v is None else f"{v['estimated']} ({v['labelled']}, {v['agreed']}){'' if v['judged'] else ' unjudged'}")
            lines.append(f"| {s_} | " + " | ".join(cells) + " |")
        lines.append("")
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "gaps-e7.md").write_text("\n".join(lines) + "\n")
    (out_dir / "gaps-e7.json").write_text(json.dumps({"verdict": verdict, "chosen": chosen, "stop": stop, "lookalikes": look,
                                                      "results": {n: {k: {"sources": v[0], "largest_gap": v[1]} for k, v in r.items()} for n, r in results.items()}}, indent=1) + "\n")
    print("\n".join(lines[:4 + len(RUNS) + 3]))


DIFFER_LABELS = ("Person", "Location")
DIFFER_RULE = {"max_fire_on_same": 0.10, "min_different_when_fired": 0.90, "min_lookalikes_flagged": 0.30}
RESOLVE_ALONE = {"c2-lines-gvc-resolve-alone": "runs/c2-lines-gvc-resolve-alone", "blind-r2--gvc-resolve-alone": "runs/blind-r2/gvc-resolve-alone"}
GOLD_GVC = L.BASELINE / "gvc/statements/gold.json"


def differs(f, label):
    """True (agrees: both name the label, sets intersect), False (fires: both name it, disjoint), None (absent)."""
    block = (f or {}).get("lines")
    if not block:
        return None
    if label == "either":
        says = [differs(f, lab) for lab in DIFFER_LABELS]
        if any(x is False for x in says):
            return False
        return True if any(x is True for x in says) else None
    b = block.get(label)
    if not b or not b["statement"] or not b["alternative"]:
        return None
    return bool(b["shared"])


def differs_rates(rows, feats, label):
    lab = [(r, f) for r, f in zip(rows, feats) if r["same"] is not None]
    same = [x for x in lab if x[0]["same"]]
    diff = [x for x in lab if not x[0]["same"]]
    fired_same = sum(differs(f, label) is False for _, f in same)
    fired_diff = sum(differs(f, label) is False for _, f in diff)
    look = [(r, f) for r, f in diff if any(r["comparison"].get(t) is True for t in ("proposed_answer", "model_choice"))]
    look_same = [(r, f) for r, f in same if any(r["comparison"].get(t) is True for t in ("proposed_answer", "model_choice"))]
    fired = fired_same + fired_diff
    out = {"labelled": len(lab), "same": len(same), "different": len(diff), "fires": fired,
           "fire_on_same": round(fired_same / len(same), 3) if same else None,
           "different_when_fired": round(fired_diff / fired, 3) if fired else None,
           "lookalikes": len(look), "lookalikes_flagged": sum(differs(f, label) is False for _, f in look),
           "lookalikes_flagged_share": round(sum(differs(f, label) is False for _, f in look) / len(look), 3) if look else None,
           "true_text_links": len(look_same), "true_text_links_flagged": sum(differs(f, label) is False for _, f in look_same),
           "judged": fired >= JUDGED_MIN}
    out["meets"] = (out["judged"] and out["fire_on_same"] is not None and out["fire_on_same"] <= DIFFER_RULE["max_fire_on_same"]
                    and out["different_when_fired"] >= DIFFER_RULE["min_different_when_fired"]
                    and out["lookalikes_flagged_share"] is not None and out["lookalikes_flagged_share"] >= DIFFER_RULE["min_lookalikes_flagged"])
    return out


def veto_simulation(name, rows, feats):
    """The RESOLVE-alone run's clustering with every weighed link the feature fires on undone, scored by er-score."""
    import subprocess, tempfile  # noqa: PLC0415
    run = pathlib.Path(RESOLVE_ALONE[name])
    gold = json.loads(GOLD_GVC.read_text())
    fires = {(r["statement"], r["alternative"]) for r, f in zip(rows, feats) if differs(f, "either") is False}
    clustering = json.loads((run / "resolve/clustering.json").read_text())
    vetoed, right, wrong, unscorable, links = [], 0, 0, 0, 0
    for line in (run / "resolve/decisions.jsonl").read_text().splitlines():
        for o in json.loads(line)["outcomes"]:
            d = o["outcome"].get("decided") or {}
            if d.get("decision") != "weighed" or d.get("record") == o["statement"]:
                continue
            links += 1
            if (o["statement"], d["record"]) in fires:
                vetoed.append(o["statement"])
                g_s, g_r = gold.get(o["statement"]), gold.get(d["record"])
                if g_s is None or g_r is None:
                    unscorable += 1
                elif g_s == g_r:
                    wrong += 1
                else:
                    right += 1
    after = dict(clustering)
    for s_ in vetoed:
        after[s_] = s_

    def er(c):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as t:
            json.dump(c, t)
        p = subprocess.run([str(pathlib.Path.home() / ".local/bin/sovereign"), "bench", "er-score", t.name, str(GOLD_GVC)], capture_output=True, text=True)
        if p.returncode != 0:
            sys.exit(f"er-score exited {p.returncode}: {p.stderr.strip()}")
        e = json.loads(p.stdout)
        return {"conll": round(e["conll_f1"], 3), "muc": round(e["muc"]["f1"], 3), "b3": round(e["b_cubed"]["f1"], 3),
                "ceaf_e": round(e["ceaf_e"]["f1"], 3), "lea": round(e["lea"]["f1"], 3)}

    return {"weighed_links": links, "vetoed": len(vetoed), "vetoed_right": right, "vetoed_wrong": wrong, "vetoed_unscorable": unscorable,
            "before": er(clustering), "after": er(after)}


def differs_probe(tables, feature_dir, out_dir):
    rates, fits, vetoes = {}, {}, {}
    for name in GVC_RUNS:
        rows = load(tables, name)
        feats = [json.loads(l) for l in (feature_dir / f"{name}.jsonl").read_text().splitlines() if l.strip()]
        assert len(feats) == len(rows) and all(f["statement"] == r["statement"] and f["alternative"] == r["alternative"] for f, r in zip(feats, rows))
        rates[name] = {lab: differs_rates(rows, feats, lab) for lab in ("either",) + DIFFER_LABELS}
        lab, _ = labelled(rows)
        e0 = score(fit([r["comparison"] for r in rows]), lab)
        comps = []
        for r, f in zip(rows, feats):
            c = dict(r["comparison"])
            v = differs(f, "either")
            if v is not None:
                c["entity_differs"] = v
            comps.append(c)
        lab_e, _ = labelled([{**r, "comparison": c} for r, c in zip(rows, comps)])
        srcs, largest = score(fit(comps), lab_e)
        without = [(v["gap"], s_) for s_, v in srcs.items() if v["gap"] is not None and s_ != "entity_differs"]
        fits[name] = {"E0": e0[1], "E0+differs": largest, "E0+differs_without_feature_row": max(without) if without else None, "sources": srcs}
        if name in RESOLVE_ALONE:
            vetoes[name] = veto_simulation(name, rows, feats)
    verdict = "WORTH A STAGE" if all(rates[n]["either"]["meets"] for n in RESOLVE_ALONE) else "NOT WORTH"
    lines = ["# E7 step 2b: entities that DIFFER on the cited lines (GVC, verified lines; Person and Location)", "",
             "| run | feature | fires | fire on gold-same | different when fired | lookalikes flagged | true text links flagged | judged | meets rule |", "|---|---|---|---|---|---|---|---|---|"]
    for name in GVC_RUNS:
        for lab, v in rates[name].items():
            lines.append(f"| {name} | {lab} | {v['fires']} of {v['labelled']} | {v['fire_on_same']} | {v['different_when_fired']} | "
                         f"{v['lookalikes_flagged']} of {v['lookalikes']} ({v['lookalikes_flagged_share']}) | {v['true_text_links_flagged']} of {v['true_text_links']} | "
                         f"{'yes' if v['judged'] else 'no'} | {'yes' if v['meets'] else 'no'} |")
    lines += ["", f"Verdict under the pre-registered rule (both RESOLVE-alone runs: fire on gold-same <= {DIFFER_RULE['max_fire_on_same']}, "
              f"different when fired >= {DIFFER_RULE['min_different_when_fired']}, lookalikes flagged >= {DIFFER_RULE['min_lookalikes_flagged']}): {verdict}", "",
              "## Today's EM with `entity_differs` added (largest judged gap)", "", "| run | E0 | E0+differs | E0+differs, feature's own row aside | entity_differs est (lab, agreed) |", "|---|---|---|---|---|"]
    for name in GVC_RUNS:
        f_ = fits[name]
        g = lambda x: "could-not-judge" if x is None else f"{x[0]:.3f} ({x[1]})"  # noqa: E731
        v = f_["sources"].get("entity_differs")
        lines.append(f"| {name} | {g(f_['E0'])} | {g(f_['E0+differs'])} | {g(f_['E0+differs_without_feature_row'])} | "
                     f"{'-' if v is None else f'{v[chr(101)+chr(115)+chr(116)+chr(105)+chr(109)+chr(97)+chr(116)+chr(101)+chr(100)]} ({v[chr(108)+chr(97)+chr(98)+chr(101)+chr(108)+chr(108)+chr(101)+chr(100)]}, {v[chr(97)+chr(103)+chr(114)+chr(101)+chr(101)+chr(100)]})'} |")
    lines += ["", "## RESOLVE alone: every weighed link the feature fires on vetoed", "", "| run | weighed links | vetoed (right / wrong / unscorable) | CoNLL before -> after | MUC | B3 | CEAF-e | LEA |", "|---|---|---|---|---|---|---|---|"]
    for name, v in vetoes.items():
        b, a_ = v["before"], v["after"]
        lines.append(f"| {name} | {v['weighed_links']} | {v['vetoed']} ({v['vetoed_right']} / {v['vetoed_wrong']} / {v['vetoed_unscorable']}) | {b['conll']} -> {a_['conll']} | "
                     f"{b['muc']} -> {a_['muc']} | {b['b3']} -> {a_['b3']} | {b['ceaf_e']} -> {a_['ceaf_e']} | {b['lea']} -> {a_['lea']} |")
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "gaps-e7b.md").write_text("\n".join(lines) + "\n")
    (out_dir / "gaps-e7b.json").write_text(json.dumps({"rule": DIFFER_RULE, "verdict": verdict, "rates": rates, "fits": fits, "vetoes": vetoes}, indent=1) + "\n")
    print("\n".join(lines))


if __name__ == "__main__":
    main()
