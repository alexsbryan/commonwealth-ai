#!/usr/bin/env python3
"""Build the `sep-raptor-subset` fixture corpus for ei-7a's lanes.

WHY A SUBSET AND NOT `sep` (operator, 2026-09-04: "Why can't it just do a
subset of SEP as a test?"): `sep` and every `sep-<slug>` atlas is a declared
CONTROL that this campaign may not overwrite, and the ON arm of the lane has
to WRITE Summary atoms into the atlas the walk reads.

WHY SELF-HOSTED AND NOT `sep-raptor-subset-<slug>` PER ARTICLE: an atlas's
evidence site is derived from its id by `EvidenceSite::derive`, whose table is
`[("sep-", "sep")]`. An atlas named `sep-raptor-subset-abduction` would strip
the `sep-` prefix and declare its parent to be the CONTROL corpus — the walk
would fetch its evidence out of `sep` and the arms would not be separable. The
declaration path (`declared_or_derived`) exists as a type but nothing on disk
supplies it yet, so the shape that works TODAY is one self-hosted corpus with
one atlas. `candidate_atlas_ids("sep-raptor-subset", Some(title))` yields the
self candidate, which is what the writer then finds.

WHY THE `id` COLUMN IS COPIED VERBATIM: `CorpusIndex::get_chunks` filters
`id IN (...)` on the `id` COLUMN, not on a positional row number. Preserving
the original ids means every chunk reference an atlas already carries — and
every `evidence_chunk_ids` in `_raptor_checkpoint` — resolves unchanged. No
remap, and nothing silently pointing at the wrong passage.

NOTHING IS RE-EMBEDDED. Chunk vectors and summary vectors are copied as
stored, so the subset lives in the same vector space as its parent and a lane
delta cannot be an embedding difference.
"""
import json
import os
import re
import shutil
import sys
from pathlib import Path

import lance
import pyarrow as pa

INDEXES = Path(os.environ.get("SOVEREIGN_INDEXES", Path.home() / ".sovereign/indexes"))
SRC = INDEXES / "sep"
# TWO corpora, not one with a flag. The arms differ in whether the ATLAS
# carries `Summary` atoms, and there is no "un-write an atom" verb — so the
# OFF arm needs an atlas that never got them. Two ids let the runs interleave
# (§18.6 wants both directions), where a write-in-the-middle would force all
# the OFF runs to precede all the ON runs and confound order with arm.
#
# NEITHER id starts with `sep-`. `EvidenceSite::derive`'s table is
# `[("sep-", "sep")]`, so `sep-raptor-subset` would declare its parent to be
# the CONTROL corpus and fetch its evidence out of it. These derive
# self-hosted, which is what they are.
DSTS = [INDEXES / "raptor-subset-off", INDEXES / "raptor-subset-on"]
BENCH = Path(__file__).resolve().parents[2] / "sovereign/bench/sep"
# Articles beyond the bank's own, so retrieval has to discriminate rather than
# hit the only thing present. 3x the bank is enough to make a wrong article
# reachable and small enough to stay a fixture.
PADDING_FACTOR = 3


def bank_articles() -> set[str]:
    arts: set[str] = set()
    for f in sorted(BENCH.glob("*.toml")):
        t = f.read_text()
        arts |= set(re.findall(r"https://plato\.stanford\.edu/entries/([A-Za-z0-9\-_.]+)", t))
        for m in re.finditer(r"expected_sources\s*=\s*\[([^\]]*)\]", t):
            arts |= set(re.findall(r'"([^"]+)"', m.group(1)))
    return {a for a in arts if a}


def checkpoint_articles() -> set[str]:
    """Articles the surviving RAPTOR tree covers — ALWAYS kept.

    `_raptor_checkpoint` is the only place `evidence_chunk_ids` and
    `children_node_ids` still exist (`conv_raptor_nodes` is empty in both
    stores), so it is the ONLY real-data case for the writer's evidence and
    `Composes` paths. A subset that samples articles without pinning these
    drops the tree by luck of the draw — which is exactly what the first build
    did: 37 of 37 evidence-bearing nodes filtered out, and the projection
    reported `0 with evidence, 0 with children` while looking entirely healthy.
    Keeping them is not a special case for one article; it is the rule that the
    fixture must contain the evidence it exists to exercise.
    """
    ck = SRC / "_raptor_checkpoint"
    if not ck.is_dir():
        return set()
    ids = set()
    for f in ck.glob("level-*/cluster-*.json"):
        try:
            ids.add(json.loads(f.read_text())["node_id"])
        except Exception:
            continue
    if not ids:
        return set()
    rap = lance.dataset(str(SRC / "raptor_summaries.lance"))
    t = rap.to_table(columns=["node_id", "conv_uuid"]).to_pylist()
    arts = {r["conv_uuid"].rstrip("/").rsplit("/", 1)[-1] for r in t if r["node_id"] in ids}
    print(f"checkpoint pins {len(arts)} article(s) with a real tree: {sorted(arts)}")
    return arts


def main() -> int:
    if not SRC.is_dir():
        print(f"error: {SRC} not found", file=sys.stderr)
        return 1
    for d in DSTS:
        if d.exists():
            print(f"error: {d} already exists — remove it deliberately, never silently",
                  file=sys.stderr)
            return 1

    wanted = bank_articles() | checkpoint_articles()
    chunks = lance.dataset(str(SRC / "chunks.lance"))
    titles_tbl = chunks.to_table(columns=["title"])
    all_titles = sorted(set(t for t in titles_tbl.column("title").to_pylist() if t))
    present = sorted(wanted & set(all_titles))
    missing = sorted(wanted - set(all_titles))
    padding_pool = [t for t in all_titles if t not in wanted]
    # Deterministic padding: every Nth title, so a rebuild picks the same set.
    step = max(1, len(padding_pool) // max(1, len(present) * PADDING_FACTOR))
    padding = padding_pool[::step][: len(present) * PADDING_FACTOR]
    keep = set(present) | set(padding)

    print(f"bank articles: {len(wanted)}  present in sep: {len(present)}  missing: {len(missing)}")
    if missing:
        # NAMED, never defaulted: a bank article absent from the corpus is a
        # question the subset cannot answer, and the lane has to know.
        print(f"  MISSING (the subset cannot answer these): {missing}")
    print(f"padding articles: {len(padding)}  total kept: {len(keep)}")

    DST = DSTS[0]
    DST.mkdir(parents=True)
    # 1. chunks, ids preserved
    # One filter shape for every size: a pushed-down `title = '...' or ...`
    # string degrades badly past a few hundred terms, and an arrow `is_in`
    # mask is exact at any size. One decider, no size-dependent branch.
    full = chunks.to_table()
    mask = pa.compute.is_in(full.column("title"), value_set=pa.array(sorted(keep)))
    sub = full.filter(mask)
    print(f"chunks: {sub.num_rows} of {chunks.count_rows()}")
    lance.write_dataset(sub, str(DST / "chunks.lance"), mode="create")

    # 2. corpus meta — copied, so the embed model + dim are the parent's
    meta = json.loads((SRC / "_corpus_meta.json").read_text())
    meta["corpus_id"] = DST.name
    for k in ("name", "display_name", "title"):
        if k in meta and isinstance(meta[k], str):
            meta[k] = "SEP RAPTOR subset (ei-7a fixture)"
    (DST / "_corpus_meta.json").write_text(json.dumps(meta, indent=2))

    # 3. the summary rows for the kept articles, vectors verbatim
    rap = lance.dataset(str(SRC / "raptor_summaries.lance"))
    rt = rap.to_table()
    # `conv_uuid` is the entry URL; its last path segment is the article
    # title — the same derivation `corpus_engine::raptor_article_title` makes
    # on the Rust side.
    convs = rt.column("conv_uuid").to_pylist()
    keep_ix = [i for i, u in enumerate(convs) if u.rstrip("/").rsplit("/", 1)[-1] in keep]
    rsub = rt.take(keep_ix)
    print(f"summary rows: {rsub.num_rows} of {rt.num_rows}")
    lance.write_dataset(rsub, str(DST / "raptor_summaries.lance"), mode="create")
    shutil.copy(SRC / "raptor_summaries.meta.json", DST / "raptor_summaries.meta.json")
    # 4. the tree, such as it is — copied whole; the writer reports how many
    #    of the kept nodes it actually covers.
    if (SRC / "_raptor_checkpoint").is_dir():
        shutil.copytree(SRC / "_raptor_checkpoint", DST / "_raptor_checkpoint")

    # 5. an EMPTY atlas for the writer to append into. Empty and not a copy of
    #    the per-article atlases: those carry Entity atoms whose chunk ids
    #    point into `sep`, and merging 71 of them would make the ON/OFF arms
    #    differ in more than one thing.
    atlas = DST / "atlas"
    atlas.mkdir()
    (atlas / "atoms.json").write_text(json.dumps({"schema_version": "2.5", "atoms": []}, indent=2))
    (atlas / "edges.json").write_text(json.dumps({"schema_version": "2.0", "edges": []}, indent=2))

    # The ON corpus is a byte copy of the OFF one, so the two arms differ in
    # exactly one thing: whether `enrich summary-atoms` has run over it.
    on = DSTS[1]
    shutil.copytree(DST, on)
    m = json.loads((on / "_corpus_meta.json").read_text())
    m["corpus_id"] = on.name
    (on / "_corpus_meta.json").write_text(json.dumps(m, indent=2))

    print(f"\nbuilt {DST} and {on} (identical)")
    print(f"next: svrn enrich summary-atoms {on.name}   # ON arm only")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
