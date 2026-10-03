#!/usr/bin/env python3
"""T0 census: what the EXISTING atlas already holds of the dispute bank's gold. Model-free.

    python3 census.py        # -> census.json; prints recall per check

Reads bank.toml, the entry's chunks from ~/.svrnmesh/indexes/sep/chunks.lance and the atlas at
~/.svrnmesh/indexes/sep-kant-transcendental-idealism/atlas. Writes nothing else.

REFUSES (exit 2) when any witness quote is not in its chunk (whitespace-collapsed only), and
(exit 3) when a position's or holder's `match` list misses any of its own witness quotes: the
instrument is checked before any number is reported.

Per gold (position, holder) pair of every scored item, over the atlas atoms:
  claim_position      a Claim carries the position: its `match` hits the claim's content /
                      quotable_excerpt or the name (canonical + aliases) of its `attributed_to`
  claim_pair          such a Claim also names the holder (content or attributed_to name)
  claim_pair_grounded ... and its evidence section is a section of that pair's own witnesses
  attributed_right    such a Claim's `attributed_to` IS the holder
  argument_pair       an ArgumentReconstruction (name, premises, conclusion) names both
  holder_entity       an Entity for the holder exists at all
Per position: position_atom (atom kind `Position`; absent from every SEP atlas, see
inventory.json), concept_entity (an Entity named for the position; a proxy, named as one),
misattributed claims (attributed_to is a holder this item credits only to another position).
Per opposing pair of required positions: tension_edge (a Tension edge whose endpoint names or
sub_question match both), tension_edge_holders (endpoints are a holder of each side),
collapsed_entity (ONE entity whose names match both sides: the dispute merged at write time),
tension_candidate_direct (a candidate in tension_candidates.json whose two atoms, resolved by id,
carry P and Q). COULD-NOT-JUDGE only while no candidate id resolves: a candidates file older than
atoms.json (e.g. before the 2026-05-22 content-hash id migration, c9e6e4095) orphans every id until
a rebuild re-keys it. A positional recovery (claim-000N = the N-th Claim in atoms.json) is reported
beside it, labelled as the substitution it is, with its own consistency rate.
"""
import collections, json, pathlib, re, sys

import lance

from score import hits, load_bank, norm

HERE = pathlib.Path(__file__).resolve().parent
IDX = pathlib.Path.home() / ".svrnmesh/indexes"
OUT = HERE / "census.json"


def ws(s):
    return re.sub(r"\s+", " ", s).strip()


def load_chunks(url):
    ds = lance.dataset(str(IDX / "sep" / "chunks.lance"))
    rows = ds.to_table(columns=["id", "content"], filter=f"url = '{url}'").to_pylist()
    return {r["id"]: r["content"] for r in rows}


def section_map(chunks, corpus):
    """chunk id -> sec_NNNN, from each atlas section's first_line (no arithmetic assumed)."""
    chapters = json.loads((IDX / corpus / "chapters.json").read_text())["chapters"]
    ids = sorted(chunks)
    starts, i = [], 0
    for ch in chapters:
        fl = ws(ch["first_line"])[:60]
        j = next((k for k in range(i, len(ids)) if fl in ws(chunks[ids[k]])), None)
        if j is None:
            sys.exit(f"REFUSED: section {ch['id']} first_line not found in chunk text")
        starts.append((j, ch["id"]))
        i = j
    sec = {}
    for n, (j, sid) in enumerate(starts):
        end = starts[n + 1][0] if n + 1 < len(starts) else len(ids)
        for k in range(j, end):
            sec[ids[k]] = sid
    return sec


def all_witnesses(bank):
    for it in bank["questions"]:
        for w in it.get("hedges", []):
            yield it["id"], "hedge", None, w
        for p in it["positions"]:
            for w in p["witnesses"]:
                yield it["id"], "position", p, w
            for h in p.get("holders", []):
                for w in h["witnesses"]:
                    yield it["id"], "holder", h, w


def verify(bank, chunks, sec):
    missing, instrument = [], []
    for iid, kind, owner, w in all_witnesses(bank):
        c = chunks.get(w["chunk"])
        if c is None or ws(w["quote"]) not in ws(c):
            missing.append((iid, kind, w["chunk"], w["quote"][:80]))
        w["section"] = sec.get(w["chunk"])
    if missing:
        for m in missing:
            print("NOT IN CHUNK:", m)
        sys.exit(2)
    for it in bank["questions"]:
        for p in it["positions"]:
            for w in p["witnesses"]:
                if not hits(p["match"], norm(w["quote"])):
                    instrument.append((it["id"], p["id"], "position match misses its witness", w["chunk"]))
            for h in p.get("holders", []):
                for w in h["witnesses"]:
                    if not hits(h["match"], norm(w["quote"]), True):
                        instrument.append((it["id"], h["name"], "holder match misses its witness", w["chunk"]))
    if instrument:
        for m in instrument:
            print("INSTRUMENT:", m)
        sys.exit(3)


class Atlas:
    def __init__(self, corpus):
        d = IDX / corpus / "atlas"
        atoms = json.loads((d / "atoms.json").read_text())["atoms"]
        self.by = collections.defaultdict(list)
        for a in atoms:
            self.by[a["atom_type"]].append(a["data"])
        self.atom = {a["data"]["id"]: a["data"] for a in atoms}
        self.ids = set(self.atom)
        self.ent = {e["id"]: e for e in self.by["Entity"]}
        self.edges = json.loads((d / "edges.json").read_text())["edges"]
        self.cands = json.loads((d / "tension_candidates.json").read_text())["candidates"]
        self.kinds = sorted(self.by)

    def names(self, eid):
        e = self.ent.get(eid)
        return norm(" | ".join([e["canonical_name"]] + (e.get("aliases") or []))) if e else ""

    def claim_text(self, c):
        return norm(f'{c.get("content", "")} {c.get("quotable_excerpt", "")}')

    @staticmethod
    def sections(a):
        return {e.get("chunk_id") for e in a.get("evidence", [])}

    def positional(self, pid):
        kind = {"claim": "Claim", "entity": "Entity", "state": "State"}.get(pid.rsplit("-", 1)[0])
        n = pid.rsplit("-", 1)[1]
        if kind is None or not (len(n) == 4 and n.isdigit()):
            return None
        lst = self.by[kind]
        return lst[int(n) - 1] if 0 < int(n) <= len(lst) else None


def claim_carries(atlas, c, pats):
    return hits(pats, atlas.claim_text(c)) or hits(pats, atlas.names(c.get("attributed_to")))


def attribution_class(atlas, c, gold_pats):
    """Who a position-carrying claim is credited to: a gold holder, Kant, a concept/reading, another
    person, or nobody. Lexical carriers, so indicative only."""
    e = atlas.ent.get(c.get("attributed_to"))
    if not e:
        return "none"
    if gold_pats and hits(gold_pats, atlas.names(e["id"]), True):
        return "gold_holder"
    if re.search(r"\bKant\b", e["canonical_name"]):
        return "kant"
    return "person_other" if e.get("entity_type") == "person" else "concept_or_reading"


def snippet(atlas, c):
    return {"id": c["id"], "attributed_to": atlas.names(c.get("attributed_to")) or None,
            "sections": sorted(atlas.sections(c)), "content": c.get("content", "")[:200]}


def census_item(it, atlas):
    req = [p for p in it["positions"] if p.get("required", True)]
    claims, args = atlas.by["Claim"], atlas.by["ArgumentReconstruction"]
    pairs, pos_rows = [], []
    for p in req:
        carriers = [c for c in claims if claim_carries(atlas, c, p["match"])]
        gold = [pat for h in p.get("holders", []) for pat in h["match"]]
        concept = [e["canonical_name"] for e in atlas.by["Entity"] if hits(p["match"], atlas.names(e["id"]))]
        pos_rows.append({"position": p["id"], "claim_position": bool(carriers), "claims_carrying": len(carriers),
                         "carrier_attribution": dict(collections.Counter(
                             attribution_class(atlas, c, gold) for c in carriers)),
                         "position_atom": len(atlas.by.get("Position", [])) and sum(
                             hits(p["match"], norm(json.dumps(x))) for x in atlas.by["Position"]),
                         "concept_entity_proxy": concept})
        for h in p.get("holders", []):
            wsec = {w["section"] for w in p["witnesses"] + h["witnesses"]}
            pair = [c for c in carriers if hits(h["match"], atlas.claim_text(c), True)
                    or hits(h["match"], atlas.names(c.get("attributed_to")), True)]
            right = [c for c in carriers if hits(h["match"], atlas.names(c.get("attributed_to")), True)]
            grounded = [c for c in pair if atlas.sections(c) & wsec]
            arg = [a for a in args if hits(p["match"], norm(json.dumps([a["name"], a["premises"], a["conclusion"]])))
                   and hits(h["match"], norm(json.dumps([a["name"], a["premises"], a["conclusion"]])), True)]
            pairs.append({
                "position": p["id"], "holder": h["name"], "witness_sections": sorted(wsec),
                "claim_pair": bool(pair), "claim_pair_grounded": bool(grounded),
                "attributed_right": bool(right), "argument_pair": bool(arg),
                "attributed_right_to": sorted({atlas.ent[c["attributed_to"]].get("entity_type") for c in right}),
                "holder_entity": any(hits(h["match"], atlas.names(e["id"]), True) for e in atlas.by["Entity"]),
                "evidence": {"pair": [snippet(atlas, c) for c in pair][:6],
                             "attributed_right": [c["id"] for c in right],
                             "argument": [a["id"] for a in arg]},
            })
    opp = []
    for i, p in enumerate(req):
        for q in req[i + 1:]:
            ph = [pat for h in p.get("holders", []) for pat in h["match"]]
            qh = [pat for h in q.get("holders", []) for pat in h["match"]]
            te, teh = [], []
            for e in atlas.edges:
                if e.get("edge_type") != "Tension":
                    continue
                s, t, sq = atlas.names(e["source"]), atlas.names(e["target"]), norm(e.get("sub_question", ""))
                if (hits(p["match"], s) and hits(q["match"], t)) or (hits(q["match"], s) and hits(p["match"], t)) \
                        or (hits(p["match"], sq) and hits(q["match"], sq)):
                    te.append(e["id"])
                if (hits(ph, s, True) and hits(qh, t, True)) or (hits(qh, s, True) and hits(ph, t, True)):
                    teh.append(e["id"])
            collapsed = [atlas.names(e["id"]) for e in atlas.by["Entity"]
                         if hits(p["match"], atlas.names(e["id"])) and hits(q["match"], atlas.names(e["id"]))]
            resolved = [c for c in atlas.cands if c["source_atom"] in atlas.ids and c["target_atom"] in atlas.ids]
            direct = [c["id"] for c in resolved
                      if (claim_carries(atlas, atlas.atom[c["source_atom"]], p["match"])
                          and claim_carries(atlas, atlas.atom[c["target_atom"]], q["match"]))
                      or (claim_carries(atlas, atlas.atom[c["source_atom"]], q["match"])
                          and claim_carries(atlas, atlas.atom[c["target_atom"]], p["match"]))]
            pos_hit = []
            for c in atlas.cands:
                a, b = atlas.positional(c["source_atom"]), atlas.positional(c["target_atom"])
                if not a or not b or "content" not in a or "content" not in b:
                    continue
                if (claim_carries(atlas, a, p["match"]) and claim_carries(atlas, b, q["match"])) or \
                        (claim_carries(atlas, a, q["match"]) and claim_carries(atlas, b, p["match"])):
                    pos_hit.append(c["id"])
            opp.append({"pair": [p["id"], q["id"]], "tension_edge": te, "tension_edge_holders": teh,
                        "collapsed_entity": collapsed,
                        "tension_candidate_direct": direct[:20] if resolved else "could-not-judge",
                        "tension_candidate_direct_count": len(direct),
                        "tension_candidate_positional_SUBSTITUTION": pos_hit[:20],
                        "tension_candidate_positional_count": len(pos_hit)})
    return {"id": it["id"], "positions": pos_rows, "pairs": pairs, "opposing": opp}


def positional_consistency(atlas):
    """Of claim-claim entity_overlap candidates, how many share an attribution under the
    positional mapping (what entity_overlap means). The recovery's own validity, not a result."""
    ok = n = 0
    for c in atlas.cands:
        a, b = atlas.positional(c["source_atom"]), atlas.positional(c["target_atom"])
        if c.get("discovery") != "entity_overlap" or not a or not b or "content" not in b:
            continue
        n += 1
        ok += bool(a.get("attributed_to")) and a.get("attributed_to") == b.get("attributed_to")
    return ok, n


def load_audit():
    path = HERE / "audit.toml"
    if not path.exists():
        return {"meta": {}, "rows": []}
    import tomllib
    with open(path, "rb") as f:
        return tomllib.load(f)


def join_audit(items, audit):
    """Per pair: read_true / read_via_reading / read_false / unread from audit.toml rows. A lexical
    attributed_right claim with no row is `unread` and never counted true."""
    rows = audit["rows"]
    for it in items:
        for r in it["pairs"]:
            key = (it["id"], r["position"], r["holder"])
            verdicts = {}
            for c in r["evidence"]["attributed_right"]:
                row = next((x for x in rows if x["kind"] == "attributed" and (x["item"], x["position"], x["holder"]) == key
                            and x["claim"] == c), None)
                verdicts[c] = row["verdict"] if row else "unread"
            for x in rows:
                if x["kind"] == "missed" and (x["item"], x["position"], x["holder"]) == key:
                    verdicts[x["claim"]] = "missed-" + x["verdict"]
            r["read"] = verdicts
            vals = set(verdicts.values())
            r["read_attributed_right"] = bool(vals & {"true", "missed-true"})
            r["read_via_reading_only"] = not r["read_attributed_right"] and "via-reading" in vals
            r["unread"] = "unread" in vals
        for o in it["opposing"]:
            o["tension_edge_read"] = {e: next((x["verdict"] for x in rows if x["kind"] == "tension" and x["item"] == it["id"]
                                               and x["edge"] == e and sorted(x["pair"]) == sorted(o["pair"])), "unread")
                                      for e in o["tension_edge"]}


def rate(rows, key):
    n = len(rows)
    k = sum(1 for r in rows if r[key] is True or (isinstance(r[key], list) and r[key]))
    return {"hit": k, "of": n, "recall": round(k / n, 3) if n else None}


def main():
    bank = load_bank()
    meta = bank["meta"]
    chunks = load_chunks(meta["url"])
    sec = section_map(chunks, meta["atlas_corpus"])
    verify(bank, chunks, sec)
    atlas = Atlas(meta["atlas_corpus"])
    scored = [it for it in bank["questions"] if not it.get("neutral")]
    items = [census_item(it, atlas) for it in scored]
    pairs = [r for it in items for r in it["pairs"]]
    poss = [r for it in items for r in it["positions"]]
    opps = [r for it in items for r in it["opposing"]]
    audit = load_audit()
    join_audit(items, audit)
    pc_ok, pc_n = positional_consistency(atlas)
    summary = {
        "atlas": meta["atlas_corpus"], "atom_kinds": atlas.kinds,
        "counts": {k: len(v) for k, v in atlas.by.items()},
        "tension_edges": sum(e.get("edge_type") == "Tension" for e in atlas.edges),
        "tension_candidates": len(atlas.cands),
        "tension_candidates_resolvable": sum(c["source_atom"] in atlas.ids and c["target_atom"] in atlas.ids
                                             for c in atlas.cands),
        "positional_recovery_consistency": {"shared_attribution": pc_ok, "of": pc_n},
        "bank": {"items": len(bank["questions"]), "scored": len(scored), "neutral": len(bank["questions"]) - len(scored),
                 "positions": len(poss), "pairs": len(pairs), "opposing_pairs": len(opps),
                 "witnesses": sum(1 for _ in all_witnesses(bank))},
        "pair_recall_lexical": {k: rate(pairs, k) for k in ("claim_pair", "claim_pair_grounded",
                                                             "attributed_right", "argument_pair", "holder_entity")},
        "pair_recall_read": {
            "reader": audit["meta"].get("reader"),
            "attributed_right": rate(pairs, "read_attributed_right"),
            "via_reading_entity_only": rate(pairs, "read_via_reading_only"),
            "pairs_with_unread_hits": rate(pairs, "unread"),
        },
        "item_level_read": {
            "items_every_required_position_held_by_a_right_holder": sum(
                all(any(r["read_attributed_right"] for r in it["pairs"] if r["position"] == pos["position"])
                    for pos in it["positions"]) for it in items),
            "of": len(items),
            "per_item": {it["id"]: f'{sum(any(r["read_attributed_right"] for r in it["pairs"] if r["position"] == pos["position"]) for pos in it["positions"])}/{len(it["positions"])}'
                         for it in items},
        },
        "position_recall": {
            "claim_position_lexical": rate(poss, "claim_position"),
            "carrier_attribution_lexical": dict(sum((collections.Counter(r["carrier_attribution"]) for r in poss),
                                                    collections.Counter())),
            "position_atom": {"hit": sum(bool(r["position_atom"]) for r in poss), "of": len(poss),
                              "note": "no Position atom kind in this atlas"},
            "concept_entity_proxy": rate([{**r, "x": bool(r["concept_entity_proxy"])} for r in poss], "x"),
            "misattribution": "WITHDRAWN: a lexical check (claim carries P, attributed_to holds only Q) flagged "
                              "7 claims on the first run; all 7 were the holder's own claims against P on reading",
        },
        "opposing_recall": {
            "tension_edge": rate(opps, "tension_edge"),
            "tension_edge_read_true": rate([{**r, "x": "true" in r["tension_edge_read"].values()} for r in opps], "x"),
            "tension_edge_holders": rate(opps, "tension_edge_holders"),
            "collapsed_entity": rate(opps, "collapsed_entity"),
            "tension_candidate_direct": rate(opps, "tension_candidate_direct")
            if any(r["tension_candidate_direct"] != "could-not-judge" for r in opps)
            else "could-not-judge (0 candidate ids resolve in atoms.json)",
            "tension_candidate_positional_SUBSTITUTION": rate(
                [{**r, "x": r["tension_candidate_positional_count"] > 0} for r in opps], "x"),
        },
    }
    OUT.write_text(json.dumps({"summary": summary, "items": items}, indent=1, ensure_ascii=False) + "\n")
    print(json.dumps(summary, indent=1, ensure_ascii=False))


if __name__ == "__main__":
    main()
