#!/usr/bin/env python3
"""Blind round 2's difference classes (order ontology-layer-3-any-author step 9), written AFTER reading the residual.

    r2_classes.py DIFFERENCES.jsonl OUT_CLASSES.json

Reads agreement.py's unclassed difference rows and the two runs behind them, finds the fact that explains each row,
and writes one rule per unit ({system, unit, class, why}) for agreement.py --classes. The fact is code, below; the
class each fact gets is the evaluator's reading (ONTOLOGY_METHOD §The example checks the work), one line per fact:

  gvc, GVC RESOLVE alone (units: gold event mentions; gold carries each mention's incident)
    unplaced, covered only by a claim about a non-event record (blind: a person)  -> example gap: gold granularity
    unplaced, covered by claims about several records                              -> method gap: one span, several records
    unplaced, covered by nothing                                                   -> method gap: reader recall
    joins, every joined mention in the unit's own gold incident                    -> example gap: gold granularity
    joins across gold incidents                                                    -> method gap: identity over-merges
    splits, or a regrouping                                                        -> method gap: identity
  ward (units: tune gold files in one deal)
    unplaced, the side's claims on the message sit on several deal records          -> method gap: one message, several records
    unplaced, no claim on the message at all                                        -> method gap: reader recall
    unplaced, the side's claims on it are all about a type gold does not score      -> example gap: gold scores deals only
  uv (units: tune gold documents)
    unplaced, the side states no case claim on the document                         -> method gap: reader recall
    unplaced, its case claims sit on several records                                -> method gap: one document, several records
    unplaced, its claims on it are all about a type gold does not score             -> example gap: gold scores cases only
    joins / splits / regrouping                                                     -> method gap: identity
A row no fact explains stays unclassed.
"""
import collections, json, pathlib, sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
sys.dont_write_bytecode = True
import ladder as L  # noqa: E402
import ladder_gvc as G  # noqa: E402
import ladder_ward as W  # noqa: E402

RUNS = HERE.parents[2] / "runs"
BLIND, OURS = RUNS / "blind-r2", RUNS / "c2-lines"
U = L.load_module("support_score", HERE.parent / "support/score.py")
INCIDENT = {m["mention_id"]: m.get("incident") for m in G.jsonl(G.CORPUS / "gold/mentions.jsonl")}  # gold's incident


def view(run):
    run = pathlib.Path(run)
    return L.atlas_view(run, L.run_atlas(run))["atoms"]


def gvc_cover(run):
    """mention -> (event records covering it, other-type records covering it), every record type aligned."""
    ms, bodies, keys = G.load_gold(G.GOLD, G.CORPUS)
    atoms = view(run)
    types = {a["data"]["id"]: L.records_of([a])[a["data"]["id"]]["record_type"] for a in atoms if a.get("atom_type") in L.RECORD_ATOMS}
    every = [{**a, "data": {**a["data"], L.RECORD_ATOMS[a["atom_type"]]: G.EVENT_TYPE}} if a.get("atom_type") in L.RECORD_ATOMS else a
             for a in atoms]
    st, _, _ = G.statements(every, bodies, keys)
    out = {}
    for mid, m in ms.items():
        ss = [s for s in st if s["doc"] == m["doc"] and s["start"] <= m["start"] and m["end"] <= s["end"]]
        ev = {s["record"] for s in ss if types[s["record"]] == G.EVENT_TYPE}
        out[mid] = (ev, {s["record"] for s in ss} - ev)
    return out, ms


def gvc_rule(row, cover, ms, mates):
    side = "blind" if row["kind"] == "blind_unplaced" else "ours" if row["kind"] == "ours_unplaced" else None
    if side:
        ev, other = cover[side][row["unit"]] if side in cover else (set(), set())
        if len(ev) > 1:
            return "method gap", "one statement span, several records: the mention cannot sit on one"
        if not ev and other:
            return "example gap", ("gold granularity: gold marks sub-event mentions (deaths, shots, wounds); this "
                                   "author states them about a person in the incident, a record gold does not score")
        if not ev:
            return "method gap", "reader recall: no claim covers the mention"
        return None
    if row["kind"] == "blind_joins":
        inc = INCIDENT.get(row["unit"])
        if inc and all(INCIDENT.get(u) == inc for u in mates[row["unit"]]):
            return "example gap", "gold granularity: one gold incident's sub-event chains on one incident record"
        return "method gap", "identity over-merges across gold incidents"
    return "method gap", f"identity: {row['kind'].replace('_', ' ')}"


def doc_claims(run, doc_of_claim):
    """unit -> [(claim kind, record type, subject)] for a run's claims, keyed by doc_of_claim(claim)."""
    atoms = view(run)
    rtype = {k: r["record_type"] for k, r in L.records_of(atoms).items()}
    out = collections.defaultdict(list)
    for a in atoms:
        c = a.get("data") or {}
        if a.get("atom_type") != "Claim":
            continue
        for u in doc_of_claim(c):
            out[u].append((c.get("claim_kind"), rtype.get(c.get("subject")), c.get("subject")))
    return out


def side_rule(claims, record_type, scored_type):
    if not claims:
        return "method gap", "reader recall: no claim on the document"
    on = {s for k, t, s in claims if t == record_type}
    if len(on) > 1:
        return "method gap", f"one document, claims on {len(on)} {record_type} records: unplaced, never voted"
    if not on:
        return "example gap", f"the author's claims on it are about types gold does not score (gold scores {scored_type} only)"
    return None


def main():
    rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
    rules = []
    by = collections.defaultdict(list)
    for r in rows:
        by[r["system"]].append(r)
    if by["gvc"] or by["gvc-resolve-alone"]:
        cover = {}
        cover["blind"], ms = gvc_cover(BLIND / "gvc")
        cover["ours"], _ = gvc_cover(OURS / "gvc")
    for system in ("gvc", "gvc-resolve-alone"):
        if not by[system]:
            continue
        if system == "gvc":
            src = {u: r for u, r in G.measure(BLIND / "gvc")["placements"]["items"].items()}
            pb = {u: rec for u, (_, rec) in src.items() if rec}
        else:
            pb = json.loads((BLIND / "gvc-resolve-alone/resolve/clustering.json").read_text())
            cover_used = {"blind": {u: ({pb[u]} if u in pb else set(), set()) for u in ms}}
        grp = collections.defaultdict(set)
        for u, rec in pb.items():
            grp[rec].add(u)
        mates = {u: grp[pb[u]] for u in pb}
        for r in by[system]:
            got = gvc_rule(r, cover if system == "gvc" else cover_used, ms, mates)
            if got:
                rules.append({"system": system, "unit": r["unit"], "class": got[0], "why": got[1]})
    paths = collections.defaultdict(set)
    for e in json.loads((W.S.WARD / "manifest.json").read_text()):
        for m in e.get("message_ids") or []:
            paths[m.strip("<>").lower()].add(e["path"])
    ward_doc = lambda c: paths.get(str((c.get("attributes") or {}).get("document_id") or "").strip("<>").lower(), ())  # noqa: E731
    uv_doc = lambda c: [str(d)] if (d := (c.get("attributes") or {}).get("document_id")) is not None else []  # noqa: E731
    for system, leg, doc, rtype, scored in (("ward", "ward-tune", ward_doc, "deal", "deals"),
                                            ("uv", "uv-third", uv_doc, "case", "cases")):
        if not by[system]:
            continue
        claims = {"blind": doc_claims(BLIND / leg, doc), "ours": doc_claims(OURS / leg, doc)}
        if system == "uv":  # membership is read off the state claims only (support/score.py atlas_pred)
            claims = {k: {u: [x for x in v if x[0] == "case_state"] or [("other", None, None)] * bool(v) for u, v in d.items()}
                      for k, d in claims.items()}
        for r in by[system]:
            side = "blind" if r["kind"] == "blind_unplaced" else "ours" if r["kind"] == "ours_unplaced" else None
            got = side_rule(claims[side].get(r["unit"], []), rtype, scored) if side else \
                ("method gap", f"identity: {r['kind'].replace('_', ' ')}")
            if got:
                rules.append({"system": system, "unit": r["unit"], "class": got[0], "why": got[1]})
    pathlib.Path(sys.argv[2]).write_text(json.dumps({"about": __doc__.split("\n\n")[0], "rules": rules}, indent=0) + "\n")
    for (s, c, w), n in collections.Counter((x["system"], x["class"], x["why"]) for x in rules).most_common():
        print(f"{s:18} {c:12} {n:4}  {w}")


if __name__ == "__main__":
    main()
