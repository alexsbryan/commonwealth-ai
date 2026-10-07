#!/usr/bin/env python3
"""Oracle ablation of the deal's unfolding, tune fold only: which step loses the most.

    ablate.py --run <chain dir with R/ S/ C/> --out <dir>

A deal unfolds into five steps (ONTOLOGY_PRIMITIVES.md §8): an act per message, the production fact it
is about, the party it is with, the grouping of acts into transactions, the fold of a transaction's acts
into its stage. Steps 1-3 are read straight off stage.py's mentions against gold. Steps 4-5 are measured
by feeding gold into compose.py's INPUT and re-running it unchanged, one oracle at a time, so the grouping
code measured is the one that produced the tune numbers (one decider). Only labelled members (deals.py's
labeller) take an oracle value; every arm is scored by deals.analyse on the tune fold.

An arm is clean only if its compose run asked the model nothing: a replaced counterparty name stays on a
shadow member that cites no file, so the identity pass sees the same names and replays from cache.
"""
import argparse, collections, copy, json, pathlib, re, subprocess, sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import deals as D  # noqa: E402
import score as S  # noqa: E402

FOLD = D.FOLDS["tune"]
FACT = {"delivery_point": "equal", "commodity": "equal", "deal_ref": "equal", "kind": "equal", "period": "overlap"}


def facts(at):
    """A mention's production-fact fields, normalised for comparison; absent fields are left out."""
    out = {}
    for k, how in FACT.items():
        if how == "overlap":
            lo, hi = at.get("period_start"), at.get("period_end")
            if lo or hi:
                out[k] = (lo or hi, hi or lo)
        elif at.get(k):
            out[k] = S.fold(at[k])
    return out


def agree(k, a, b):
    if FACT[k] == "overlap":
        return max(a[0], b[0]) <= min(a[1], b[1])
    return a == b


def step_measures(g, members, label, ent, resolves):
    """Steps 1-3 off the mentions themselves."""
    deal = {d["id"]: d for d in g["deals"]}
    trans = {d["id"] for d in g["deals"] if d.get("kind", "transaction") == "transaction" and d["counterparty"].split(":", 1)[0] in FOLD}
    lab = {c["id"]: label(c) for c in members}
    # 1 acts: each gold stage update on a tune transaction deal
    acts = collections.Counter()
    for s in [s for s in g["stage_updates"] if s["deal"] in trans]:
        on = [c for c in members if s["file"] in c["_files"]]
        mine = [c for c in on if lab[c["id"]] == s["deal"]]
        acts["gold stage updates"] += 1
        acts["a mention on its message"] += bool(on)
        acts["a mention labelled to its deal"] += bool(mine)
        acts["... whose event-fold stage is gold's"] += any((c["attributes"] or {}).get("stage") == s["stage"] for c in mine)
        acts["... whose first-order stage is gold's"] += any(c.get("stage_first_order") == s["stage"] for c in mine)
    # 2 production facts: labelled pairs, same deal vs different deals with one counterparty
    pairs = {"same deal": collections.Counter(), "other deal, same party": collections.Counter()}
    lm = [c for c in members if lab[c["id"]]]
    for i, x in enumerate(lm):
        for y in lm[i + 1:]:
            dx, dy = deal[lab[x["id"]]], deal[lab[y["id"]]]
            cls = "same deal" if dx["id"] == dy["id"] else "other deal, same party" if dx["counterparty"] == dy["counterparty"] else None
            if not cls:
                continue
            fx, fy = facts(x["attributes"] or {}), facts(y["attributes"] or {})
            both = [k for k in FACT if k in fx and k in fy]
            p = pairs[cls]
            p["pairs"] += 1
            p["some field both carry"] += bool(both)
            p["a field conflicts (cannot-link)"] += any(not agree(k, fx[k], fy[k]) for k in both)
            for k in both:
                p[f"{k}: both carry"] += 1
                p[f"{k}: conflict"] += not agree(k, fx[k], fy[k])
    # 3 party: the mention's own counterparty against gold's
    party = collections.Counter()
    for c in lm:
        cp = deal[lab[c["id"]]]["counterparty"]
        party["labelled mentions"] += 1
        v = (c["attributes"] or {}).get("counterparty")
        party["names no counterparty"] += not v
        party["names gold's counterparty"] += bool(v) and resolves({"attributes": {"counterparty": v}}, cp)
    return {"1 acts": dict(acts), "2 production facts": {k: dict(v) for k, v in pairs.items()}, "3 party": dict(party)}


def registry_ids(g, C_ent):
    """Gold company -> the composed registry's id for it (by domain, else folded name)."""
    out = {}
    for c in g["companies"]:
        doms = {S.fold(d) for d in c.get("domains") or []}
        # a keyed id (company:<domain>) before a name-minted one (company:name:...)
        hit = sorted((e["id"] for e in C_ent.values() if e.get("entity_type") == "company" and e["id"].startswith("company:")
                      and (doms & {S.fold(d) for d in S.attr_list(e, "domain")} or S.fold(e.get("canonical_name")) == S.fold(c.get("name")))),
                     key=lambda i: (i.startswith("company:name:"), i))
        if hit:
            out[c["id"]] = hit[0]
    return out


def arm(name, base_atoms, g, label, reg, out, oracles, facets_text):
    """Write compose.py's input with the named oracles applied to labelled tune members, run it, score it."""
    atoms = copy.deepcopy(base_atoms)
    secfiles = S.section_files("crm-ward-acts")
    _, claims = S.load_atlas(atoms, secfiles)  # adds _files to the same dicts
    deal = {d["id"]: d for d in g["deals"]}
    gold_stage = {(s["file"], s["deal"]): s["stage"] for s in g["stage_updates"]}
    shadows, n = [], collections.Counter()
    for c in claims:
        if c.get("claim_kind") != "deal_mention" or not any(f.split("/", 1)[0] in FOLD for f in c["_files"]):
            continue
        d = deal.get(label(c) or "")
        if not d:
            continue
        at = c["attributes"]
        if "party" in oracles:
            rid = reg.get(d["counterparty"])
            if rid and rid.startswith("company:name:"):
                # an id the identity pass mints from a name: written into a member it reads as a new raw
                # name, changes the pass's prompt, and the arm stops replaying from cache
                n["party: gold company only name-keyed, left as read"] += 1
            elif rid:
                for slot in ("counterparty", "via"):  # both feed the identity pass's names
                    if at.get(slot) and at[slot] != rid:
                        shadows.append((slot, at[slot]))
                at["counterparty"], at["via"] = rid, None
                n["party set"] += 1
            else:
                n["party: gold company not in the registry"] += 1
        if "kind" in oracles:
            at["kind"] = d.get("kind", "transaction"); n["kind set"] += 1
        if "group" in oracles:
            at["deal_ref"] = d["id"]; n["deal_ref set"] += 1
        if "stage" in oracles:
            st = [gold_stage[(f, d["id"])] for f in sorted(c["_files"]) if (f, d["id"]) in gold_stage]
            if st:
                at["stage"] = st[0]; n["stage set"] += 1
    for x in atoms:  # load_atlas tags entities and claims alike
        for k in [k for k in x["data"] if k.startswith("_")]:
            del x["data"][k]
    for i, (slot, v) in enumerate(shadows):
        atoms.append({"atom_type": "Claim", "data": {"id": f"shadow:{i}", "claim_kind": "deal_mention",
                                                     "attributes": {slot: v}, "evidence": [], "anchor": ""}})
    d_ = out / name
    d_.mkdir(parents=True, exist_ok=True)
    (d_ / "in.json").write_text(json.dumps({"atoms": atoms}))
    (d_ / "facets.toml").write_text(facets_text)
    r = subprocess.run([sys.executable, str(HERE / "compose.py"), "--atoms", str(d_ / "in.json"), "--facets", str(d_ / "facets.toml"),
                        "--out", str(d_ / "C")], capture_output=True, text=True)
    if r.returncode:
        raise SystemExit(f"{name}: compose.py failed\n{r.stderr[-2000:]}")
    rep = json.loads((d_ / "C/compose_report.json").read_text())
    asked = {k: v for k, v in rep.items() if "asked" in k and v}
    ent, cl = S.load_atlas(json.loads((d_ / "C/atoms.json").read_text())["atoms"], secfiles)
    res = D.analyse(g, ent, cl, FOLD, {"deal_mention"})
    res.pop("per_deal")
    return {"oracles": sorted(oracles), "set": dict(n), "clean": not asked, "asked": asked,
            "deals, optimal assignment": optimal(g, ent), **res}


def optimal(g, ent):
    """The bar's own edges (an atom cites a gold deal's message and its party resolves), matched one-to-one
    by maximum bipartite matching instead of the bar's first-come greedy: what greedy order costs."""
    resolves = D.resolver(g, ent)
    gd = [d for d in g["deals"] if d.get("kind", "transaction") == "transaction" and d["counterparty"].split(":", 1)[0] in FOLD]
    deals = [e for e in ent.values() if e.get("entity_type") == "deal"]
    adj = {d["id"]: [e["id"] for e in deals if e["_files"] & d["files"] and resolves(e, d["counterparty"])] for d in gd}
    owner = {}

    def take(d, seen):
        for e in adj[d]:
            if e not in seen:
                seen.add(e)
                if e not in owner or take(owner[e], seen):
                    owner[e] = d
                    return True
        return False
    hit = sum(take(d["id"], set()) for d in gd)
    return {"hit": hit, "n": len(gd), "recall": round(hit / len(gd), 3) if gd else None}


def edit(text, old, new):
    assert text.count(old) == 1, old
    return text.replace(old, new)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--run", type=pathlib.Path, required=True)
    ap.add_argument("--out", type=pathlib.Path, required=True)
    ap.add_argument("--facets", type=pathlib.Path, default=HERE / "compose.toml", help="the composer under test (e.g. method = \"ledger\")")
    ap.add_argument("--only", default="", help="comma-separated arm-name prefixes to run (default: all)")
    a = ap.parse_args()
    g = S.load_gold(S.WARD / "gold")
    label = D.labeller(g, FOLD)
    secfiles = S.section_files("crm-ward-acts")
    base = json.loads((a.run / "S/atoms.json").read_text())["atoms"]
    C_ent, _ = S.load_atlas(json.loads((a.run / "C/atoms.json").read_text())["atoms"], secfiles)
    resolves = D.resolver(g, C_ent)
    _, sc = S.load_atlas(copy.deepcopy(base), secfiles)
    members = [c for c in sc if c.get("claim_kind") == "deal_mention" and any(f.split("/", 1)[0] in FOLD for f in c["_files"])]
    report = {"steps": step_measures(g, members, label, C_ent, resolves)}
    reg = registry_ids(g, C_ent)
    facets = a.facets.read_text()
    member_first = edit(facets, 'block_from = ["agent", "document", "member"]', 'block_from = ["member", "document"]')
    # the anchor as a cluster-level cannot-link (a join), not only a pairwise one
    anchor_join = lambda t: edit(t, 'distinct = { period = "overlap" }', 'distinct = { period = "overlap", deal_ref = "equal" }')  # noqa: E731
    arms = [
        ("A0 baseline", set(), facets),
        ("A1 party", {"party"}, member_first),
        ("A2 kind", {"kind"}, facets),
        ("A3 party+kind", {"party", "kind"}, member_first),
        ("A4 grouping, anchor pairwise", {"party", "kind", "group"}, member_first),
        ("A4 grouping, anchor joined", {"party", "kind", "group"}, anchor_join(member_first)),
        ("A5 grouping+stage", {"party", "kind", "group", "stage"}, anchor_join(member_first)),
        # no oracle: the one structural change the arms above isolate
        ("V1 anchor joined", set(), anchor_join(facets)),
        # step 3 as first proposed: the read production facts as joins (cannot-link on conflict)
        ("V2 facts joined", set(), edit(anchor_join(facets), 'deal_ref = "equal" }', 'deal_ref = "equal", delivery_point = "equal" }')),
        ("V3 facts joined, kind split", set(), edit(anchor_join(facets), 'deal_ref = "equal" }', 'deal_ref = "equal", delivery_point = "equal", kind = "equal" }')),
    ]
    report["arms"] = {}
    only = [x.strip() for x in a.only.split(",") if x.strip()]
    for name, oracles, text in [x for x in arms if not only or any(x[0].startswith(o) for o in only)]:
        key = re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")
        r = arm(key, base, g, label, reg, a.out, oracles, text)
        report["arms"][name] = r
        print(f"{name:32} clean={r['clean']!s:5} deals {r['deals_recall']}  outcomes {r['outcomes']}\n"
              f"{'':32} grouping {r['grouping']}  entities {r['entities']}  stage {r['stage']}  current {r['current_stage']}  atoms {r['deal_atoms']}")
    (a.out / "ablation.json").write_text(json.dumps(report, indent=1) + "\n")
    print(json.dumps(report["steps"], indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
