#!/usr/bin/env python3
"""Inventory of philosophy atlases that could carry a dispute (feature-fidelity, SEP dispute half).

    python3 inventory.py            # -> inventory.json, prints the table

Model-free; reads only ~/.svrnmesh. For every atlas under indexes/ it records atom and edge kinds
from atlas/_summary.json, and for SEP and dispute-bearing atlases also: the pipeline (from
~/.svrnmesh/enrichment/<corpus>/config.json when present), whether the corpus has Position /
Opposition atoms, Tension edges, and whether tension_candidates.json ids resolve against atoms.json
(they do not when the candidates predate the 2026-05-22 content-hash id migration, c9e6e4095).

The per-entry `sep-<slug>` atlases were built by `sovereign enrich sep-ingest` + `enrich build`
(sovereign/bench/sep_atlas/run_batch.sh:99-100), which writes pipeline `philosophy_atlas`
(sovereign/crates/sovereign-pipeline/src/enrich_cmd/sep_ingest.rs:203). Most have no
config.json on disk, so their pipeline is reported as `philosophy_atlas (sep-ingest, no config)`,
named as the inference it is.
"""
import collections, glob, json, os, pathlib

HOME = pathlib.Path.home() / ".svrnmesh"
IDX = HOME / "indexes"
ENR = HOME / "enrichment"
OUT = pathlib.Path(__file__).resolve().parent / "inventory.json"
SHOW = {"sep-kant", "sep-span", "sep-kant-transcendental-idealism", "sep-compatibilism", "sep-freewill",
        "sep-incompatibilism-theories", "sep-african-sage", "sep-abner-burgos"}


def load(p):
    try:
        with open(p) as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def pipeline_of(corpus):
    cfg = load(ENR / corpus / "config.json")
    if cfg and cfg.get("pipeline_id"):
        return cfg["pipeline_id"], "config.json"
    if corpus.startswith("sep-"):
        return "philosophy_atlas", "inferred: enrich sep-ingest (sep_ingest.rs:203); no config.json"
    return None, "no config.json"


def candidate_resolution(atlas):
    """(candidates, resolvable) — resolvable = both endpoint ids exist in atoms.json."""
    cands = load(atlas / "tension_candidates.json")
    if not cands:
        return None, None
    cands = cands.get("candidates", [])
    atoms = load(atlas / "atoms.json")
    ids = {a["data"]["id"] for a in (atoms or {}).get("atoms", [])}
    ok = sum(c.get("source_atom") in ids and c.get("target_atom") in ids for c in cands)
    return len(cands), ok


def main():
    rows, totals = [], collections.Counter()
    for s in sorted(glob.glob(str(IDX / "*" / "atlas" / "_summary.json"))):
        corpus = pathlib.Path(s).parent.parent.name
        summ = load(s) or {}
        ac, ec = summ.get("atom_counts", {}), summ.get("edge_counts", {})
        is_sep = corpus == "sep" or corpus.startswith("sep-")
        carries = any(k in ac for k in ("Position", "Opposition")) or "Tension" in ec or "OpposesIn" in ec
        if not (is_sep or carries):
            continue
        atlas = pathlib.Path(s).parent
        pipe, how = pipeline_of(corpus)
        row = {"corpus": corpus, "sep": is_sep, "pipeline": pipe, "pipeline_source": how,
               "atom_counts": ac, "edge_counts": ec,
               "ontology": (atlas / "ontology.json").exists()}
        n, ok = candidate_resolution(atlas)
        row["tension_candidates"], row["tension_candidates_resolvable"] = n, ok
        rows.append(row)
        if is_sep:
            totals["sep_atlases"] += 1
            for k, v in ac.items():
                totals[f"atoms.{k}"] += v
            for k, v in ec.items():
                totals[f"edges.{k}"] += v
            totals["with_tension_edges"] += "Tension" in ec
            totals["with_argument_reconstruction"] += "ArgumentReconstruction" in ac
            totals["with_position_atoms"] += "Position" in ac
            totals["pipeline_from_config"] += how == "config.json"
            if n:
                totals["candidates_nonempty"] += 1
                totals["candidates_all_resolve"] += ok == n
                totals["candidates_none_resolve"] += ok == 0
    # The file keeps totals over all SEP atlases and the rows a reader needs: every non-SEP
    # dispute-bearing atlas, the named SEP entries, and any SEP atlas whose candidates resolve.
    keep = [r for r in rows if not r["sep"] or r["corpus"] in SHOW
            or (r.get("tension_candidates") and r["tension_candidates_resolvable"] == r["tension_candidates"])]
    out = {"totals_sep": dict(totals), "atlases": keep}
    OUT.write_text(json.dumps(out, indent=1, ensure_ascii=False) + "\n")
    print(json.dumps(dict(totals), indent=1))
    for r in rows:
        if not r["sep"] or r["corpus"] in SHOW:
            print(f'{r["corpus"]:38} {str(r["pipeline"]):20} pos={r["atom_counts"].get("Position", 0):3} '
                  f'claims={r["atom_counts"].get("Claim", 0):4} args={r["atom_counts"].get("ArgumentReconstruction", 0):3} '
                  f'tension_edges={r["edge_counts"].get("Tension", 0):3} '
                  f'cands={r.get("tension_candidates")} resolvable={r.get("tension_candidates_resolvable")}')


if __name__ == "__main__":
    main()
