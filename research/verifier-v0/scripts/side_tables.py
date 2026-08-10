#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Build the Stream B harvest side tables (VERIFIER_V0.md §3, M2_STREAM_B.md
"Volume run" step 2).

Two of the ten corruption kinds need a side table, and without one the
generator silently skips them:

  entity_swap            --entities   EntityCluster[]  {etype, surfaces}
  distractor_absorption  --distractors DistractorDoc[] {id, text}

Both are passed to `svrn bench verifier harvest`, which seals them into the
harvest artifact so the pure flywheel generator never touches an index.

WHERE THE ENTITIES COME FROM. Not `out/<corpus>.named-clusters.json` — that
is the literary_atlas *thematic* clustering (facets question/claim/
entity_state/...), prose labels with no surface forms. The real source is the
corpus's own atlas: `~/.svrnmesh/indexes/<corpus>/atlas/atoms.json` carries
`Entity` atoms with `canonical_name` + `entity_type`, which is exactly the
{surfaces, etype} pair `EntityCluster` wants.

THE ONE JUDGEMENT CALL. `entity_swap` builds an ungrounded case by replacing
a surface with a same-`etype` surface from a DIFFERENT cluster, and its label
is fixed by construction. So two atoms naming the SAME referent must land in
ONE cluster, or the generator can "corrupt" a claim into a still-true
statement and label it ungrounded. Saltgrass has exactly that pair —
"Doctor Fosk" and "Doctor Imbrey Fosk". The merge rule is token-set
containment within an etype (stopwords dropped); every merge is reported on
stderr so the table stays inspectable.

Usage:
  side_tables.py entities --corpus chaos-saltgrass --out data/stream_b/chaos-saltgrass/entities.json
  side_tables.py distractors --doc sovereign/bench/attached_doc/corpora/meridian_postmortem.txt \\
                             --out data/stream_b/distractors.json
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import random
import re
import sys
import unicodedata
from pathlib import Path

DEFAULT_INDEX_DIR = Path.home() / ".sovereign" / "indexes"

# Dropped before the same-referent containment test so that "the stranger"
# and "the great lady patroness" are not compared on "the".
STOPWORDS = {"the", "a", "an", "of", "from"}


# ─────────────────────────────── entities ────────────────────────────────


def name_tokens(name: str) -> frozenset[str]:
    """Lowercased alphanumeric tokens, stopwords dropped.

    Unicode is normalised first so "Mrs Verloc's mother" tokenises the same
    whether the apostrophe is U+2019 or U+0027.
    """
    norm = unicodedata.normalize("NFKD", name).lower()
    toks = re.findall(r"[a-z0-9]+", norm)
    return frozenset(t for t in toks if t not in STOPWORDS)


def load_entity_atoms(atoms_path: Path) -> list[tuple[str, str]]:
    """(entity_type, canonical_name) for every Entity atom, in file order."""
    with atoms_path.open() as fh:
        doc = json.load(fh)
    atoms = doc["atoms"] if isinstance(doc, dict) else doc
    out: list[tuple[str, str]] = []
    for atom in atoms:
        if atom.get("atom_type") != "Entity":
            continue
        data = atom.get("data") or {}
        name = (data.get("canonical_name") or "").strip()
        etype = (data.get("entity_type") or "").strip()
        if name and etype:
            out.append((etype, name))
    return out


def cluster_entities(pairs: list[tuple[str, str]]) -> tuple[list[dict], list[str]]:
    """Group same-referent names into one cluster per entity.

    Within an etype, name X joins name Y's cluster when X's token set is a
    subset of Y's ("Doctor Fosk" ⊆ "Doctor Imbrey Fosk"). Surfaces come out
    longest-first: the generator takes the FIRST surface it finds in the
    claim, so the most specific form must be tried before its own prefix.
    """
    by_type: dict[str, list[str]] = {}
    for etype, name in pairs:
        names = by_type.setdefault(etype, [])
        if name not in names:
            names.append(name)

    clusters: list[dict] = []
    merges: list[str] = []
    for etype in sorted(by_type):
        # Longest name first, so a shorter name merges INTO its superset.
        names = sorted(by_type[etype], key=lambda n: (-len(name_tokens(n)), n))
        groups: list[list[str]] = []
        group_tokens: list[frozenset[str]] = []
        for name in names:
            toks = name_tokens(name)
            if not toks:
                continue
            hit = None
            for gi, gtoks in enumerate(group_tokens):
                if toks < gtoks or toks == gtoks:
                    hit = gi
                    break
            if hit is None:
                groups.append([name])
                group_tokens.append(toks)
            else:
                groups[hit].append(name)
                merges.append(f"{etype}: '{name}' → cluster of '{groups[hit][0]}'")
        for group in groups:
            clusters.append(
                {
                    "etype": etype,
                    "surfaces": sorted(group, key=lambda s: (-len(s), s)),
                }
            )
    return clusters, merges


def resolve_atlas_paths(args: argparse.Namespace) -> list[Path]:
    """One corpus, an explicit atoms.json, or a glob over sibling shards.

    SEP is the reason `--corpus-glob` exists: its 187k chunks live in ONE
    aggregate `sep` index whose own atlas is empty, while the 1,770
    `sep-<slug>` shards are atlas-only (no chunks.lance) and each carry
    25-100 Entity atoms. So the chunks and the entities come from different
    places, and the entity table has to be pooled across the shards.
    """
    if args.atoms:
        return [Path(args.atoms)]
    if args.corpus_glob:
        hits = sorted(Path(args.index_dir).glob(f"{args.corpus_glob}/atlas/atoms.json"))
        if not hits:
            print(
                f"error: no atlas atoms matched "
                f"{args.index_dir}/{args.corpus_glob}/atlas/atoms.json",
                file=sys.stderr,
            )
            sys.exit(1)
        return hits
    return [Path(args.index_dir) / args.corpus / "atlas" / "atoms.json"]


def cmd_entities(args: argparse.Namespace) -> int:
    atlas_paths = resolve_atlas_paths(args)
    pairs: list[tuple[str, str]] = []
    empty = 0
    for p in atlas_paths:
        if not p.is_file():
            print(f"error: no atlas atoms at {p}", file=sys.stderr)
            print("  the corpus has no atlas — build one, or pass --atoms explicitly",
                  file=sys.stderr)
            return 1
        got = load_entity_atoms(p)
        if not got:
            empty += 1
        pairs.extend(got)
    atoms_path = atlas_paths[0] if len(atlas_paths) == 1 else f"{len(atlas_paths)} atlases"
    if empty:
        print(f"[entities] {empty}/{len(atlas_paths)} atlas file(s) carried no Entity atoms",
              file=sys.stderr)
    if not pairs:
        print(f"error: {atoms_path} carries zero Entity atoms", file=sys.stderr)
        return 1

    clusters, merges = cluster_entities(pairs)

    for line in merges:
        print(f"[entities] same-referent merge — {line}", file=sys.stderr)
    swappable = sum(1 for c in clusters if sum(1 for d in clusters if d["etype"] == c["etype"]) > 1)
    if swappable == 0:
        print(
            "warning: no etype has two or more clusters — entity_swap needs a "
            "same-type partner and will produce nothing from this table",
            file=sys.stderr,
        )

    write_json(Path(args.out), clusters)
    by_type: dict[str, int] = {}
    for c in clusters:
        by_type[c["etype"]] = by_type.get(c["etype"], 0) + 1
    print(
        json.dumps(
            {
                "source": str(atoms_path),
                "entity_atoms": len(pairs),
                "clusters": len(clusters),
                "merged": len(merges),
                "by_etype": dict(sorted(by_type.items())),
                "swappable_clusters": swappable,
                "out": str(args.out),
            }
        )
    )
    return 0


# ────────────────────────────── distractors ──────────────────────────────

RULE_RE = re.compile(r"^\s*[-=_]{6,}\s*$")
HEADING_RE = re.compile(r"^\s*\d+\.\s+[^a-z]+$")


def normalise_doc(raw: str) -> str:
    """Un-wrap hard line breaks and drop rules/headings.

    `distractor_absorption` lifts a whole sentence out of this text and makes
    it the claim, so the text must read the way `extract_claim_list` output
    reads: one line per sentence-bearing paragraph, no mid-sentence newlines,
    no ASCII rules glued onto the front of a lifted sentence. Sentences are
    split on `.!?` downstream, so anything without terminal punctuation on a
    line of its own would otherwise fuse into its neighbour.
    """
    paragraphs: list[str] = []
    current: list[str] = []
    for line in raw.splitlines():
        stripped = line.strip()
        if not stripped or RULE_RE.match(stripped) or HEADING_RE.match(stripped):
            if current:
                paragraphs.append(" ".join(current))
                current = []
            continue
        # An all-caps line carries no lowercase letters: a banner, not prose.
        if not any(ch.islower() for ch in stripped):
            if current:
                paragraphs.append(" ".join(current))
                current = []
            continue
        current.append(stripped)
    if current:
        paragraphs.append(" ".join(current))
    return "\n".join(re.sub(r"\s+", " ", p).strip() for p in paragraphs)


def slug(path: Path) -> str:
    stem = re.sub(r"[^a-z0-9]+", "-", path.stem.lower()).strip("-")
    return stem or "distractor"


def docs_from_corpus(index_dir: str, corpus: str, n: int, seed: int):
    """Whole documents out of an installed corpus, for same-genre distractors.

    The corruption this feeds — distractor_absorption — models "adjacent-doc
    fact absorbed as if grounded". A CROSS-GENRE distractor makes that trivial:
    lifting "04:31 — Full recovery declared." into a Saltgrass context teaches
    a verifier to spot incident-report vocabulary, not to track support. Drawing
    the distractor from the SAME corpus keeps register and genre constant so the
    only thing distinguishing it is whether the window actually supports it,
    which is the mechanism we want learned.

    Overlap with a harvested window is safe by construction: the generator
    skips any sentence that `value_present`s in the evidence window.
    """
    import lance

    ds = lance.dataset(os.path.join(index_dir, corpus, "chunks.lance"))
    tbl = ds.to_table(columns=["id", "title", "content"]).to_pylist()
    by_title = {}
    for row in tbl:
        title = row.get("title") or ""
        if title:
            by_title.setdefault(title, []).append((row["id"], row["content"]))
    titles = sorted(by_title)
    if not titles:
        sys.exit(f"error: corpus {corpus} has no titled chunks to build distractors from")
    random.Random(seed).shuffle(titles)
    out = []
    for title in titles[:n]:
        chunks = [c for _, c in sorted(by_title[title])]
        text = re.sub(r"\s+", " ", " ".join(chunks)).strip()
        if len(text) < 400:
            continue
        out.append({"id": re.sub(r"[^a-z0-9]+", "-", title.lower()).strip("-")[:60], "text": text})
    return out


def cmd_distractors(args: argparse.Namespace) -> int:
    docs = []
    if args.from_corpus:
        docs.extend(docs_from_corpus(args.index_dir, args.from_corpus, args.n, args.seed))
    for spec in args.doc:
        path = Path(spec)
        if not path.is_file():
            print(f"error: no such document: {path}", file=sys.stderr)
            return 1
        text = normalise_doc(path.read_text())
        if not text.strip():
            print(f"error: {path} normalised to empty text", file=sys.stderr)
            return 1
        docs.append({"id": slug(path), "text": text})

    if not docs:
        print("error: no distractor documents produced (--doc and/or --from-corpus)",
              file=sys.stderr)
        return 1
    ids = [d["id"] for d in docs]
    if len(set(ids)) != len(ids):
        print(f"error: duplicate distractor ids: {ids}", file=sys.stderr)
        return 1

    write_json(Path(args.out), docs)
    # The generator's own sentence filter, mirrored so the report states the
    # usable pool rather than a raw sentence count.
    report = []
    for d in docs:
        sents = [s.strip() for s in re.split(r"[.!?]", d["text"])]
        usable = [s for s in sents if 30 <= len(s) <= 240]
        report.append({"id": d["id"], "chars": len(d["text"]), "usable_sentences": len(usable)})
    print(json.dumps({"docs": report, "out": str(args.out)}))
    return 0


# ─────────────────────────────── patch ───────────────────────────────────
#
# Why a post-harvest patch instead of `harvest --entities`: `entity_swap`
# (adversarial.rs:545) scans EVERY cluster and EVERY surface on every attempt,
# so a table's cost is paid hundreds of thousands of times. SEP's pooled table
# is 49,522 clusters — passing it at harvest time would add billions of string
# searches to the export. Almost all of it is dead weight: a cluster only does
# work if one of its surfaces actually appears in a harvested claim. That is
# unknowable until the claims exist, which is exactly why this runs after.
#
# `entities` and `distractors` are plain fields on the HarvestFile, so filling
# them in afterwards produces a byte-identical artifact to one harvested with
# the tables — the generator cannot tell the difference.


def ngram_sets(texts, max_n: int):
    """Word n-grams (n = 1..max_n) over `texts`, lowercased.

    Mirrors `find_word_ci`'s boundary rule by construction: a surface matches
    only if its token sequence appears as whole words, never inside one.
    """
    sets = [set() for _ in range(max_n + 1)]
    for t in texts:
        toks = WORD_RE_LOWER.findall(t.lower())
        for n in range(1, max_n + 1):
            s = sets[n]
            for i in range(len(toks) - n + 1):
                s.add(tuple(toks[i : i + n]))
    return sets


WORD_RE_LOWER = re.compile(r"[a-z0-9]+")
MAX_SURFACE_TOKENS = 6


def cmd_patch(args: argparse.Namespace) -> int:
    hp = Path(args.harvest)
    with hp.open() as fh:
        harvest = json.load(fh)
    claims = [it["claim"] for it in harvest.get("items", [])]
    if not claims:
        print(f"error: {hp} has no items", file=sys.stderr)
        return 1

    result = {"harvest": str(hp), "claims": len(claims)}

    if args.entities:
        with open(args.entities) as fh:
            pool = json.load(fh)
        grams = ngram_sets(claims, MAX_SURFACE_TOKENS)

        def occurs(surface: str) -> bool:
            toks = tuple(WORD_RE_LOWER.findall(surface.lower()))
            return bool(toks) and len(toks) <= MAX_SURFACE_TOKENS and toks in grams[len(toks)]

        matched, absent = [], []
        for c in pool:
            (matched if any(occurs(s) for s in c["surfaces"]) else absent).append(c)

        # Partners are the INJECTION targets: same etype, and guaranteed absent
        # from every claim (they matched nothing), which is half the site
        # condition already. The generator still re-checks against the window.
        rng = random.Random(args.seed)
        by_etype = {}
        for c in absent:
            by_etype.setdefault(c["etype"], []).append(c)
        partners = []
        for etype in sorted({c["etype"] for c in matched}):
            cands = by_etype.get(etype, [])
            rng.shuffle(cands)
            partners.extend(cands[: args.partners_per_etype])

        table = matched + partners
        harvest["entities"] = table
        result["entities"] = {
            "pool": len(pool),
            "matched_in_claims": len(matched),
            "partners_added": len(partners),
            "table": len(table),
            "by_etype": dict(sorted(collections.Counter(c["etype"] for c in table).items())),
        }
        if not matched:
            print("warning: no pooled entity surface occurs in any claim — "
                  "entity_swap will still produce nothing", file=sys.stderr)

    if args.distractors:
        with open(args.distractors) as fh:
            harvest["distractors"] = json.load(fh)
        result["distractors"] = len(harvest["distractors"])

    with hp.open("w") as fh:
        json.dump(harvest, fh, indent=2, ensure_ascii=False)
        fh.write("\n")
    print(json.dumps(result))
    return 0


# ──────────────────────────────── shared ─────────────────────────────────


def write_json(out: Path, payload) -> None:
    if out.parent and str(out.parent):
        os.makedirs(out.parent, exist_ok=True)
    with out.open("w") as fh:
        json.dump(payload, fh, indent=2, ensure_ascii=False)
        fh.write("\n")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)

    e = sub.add_parser("entities", help="atlas atoms.json → EntityCluster[]")
    e.add_argument("--corpus", help="installed corpus id, e.g. chaos-saltgrass")
    e.add_argument("--corpus-glob", help="pool every matching shard's atlas, e.g. 'sep-*'")
    e.add_argument("--atoms", help="explicit path to atoms.json (overrides --corpus)")
    e.add_argument("--index-dir", default=str(DEFAULT_INDEX_DIR))
    e.add_argument("--out", required=True)
    e.set_defaults(fn=cmd_entities)

    d = sub.add_parser("distractors", help="plain-text document(s) → DistractorDoc[]")
    d.add_argument("--doc", action="append", default=[], help="repeatable")
    d.add_argument("--from-corpus", help="pull whole documents out of an installed corpus's chunks")
    d.add_argument("--n", type=int, default=8, help="--from-corpus: how many documents")
    d.add_argument("--index-dir", default=str(DEFAULT_INDEX_DIR))
    d.add_argument("--seed", type=int, default=17)
    d.add_argument("--out", required=True)
    d.set_defaults(fn=cmd_distractors)

    p = sub.add_parser("patch", help="fill a harvest artifact's side tables in place")
    p.add_argument("--harvest", required=True, help="claims.json to patch")
    p.add_argument("--entities", help="pooled EntityCluster[] to FILTER against the claims")
    p.add_argument("--distractors", help="DistractorDoc[] to inject verbatim")
    p.add_argument("--partners-per-etype", type=int, default=40)
    p.add_argument("--seed", type=int, default=17)
    p.set_defaults(fn=cmd_patch)

    args = ap.parse_args()
    if args.cmd == "entities" and not (args.corpus or args.corpus_glob or args.atoms):
        ap.error("entities needs one of --corpus, --corpus-glob or --atoms")
    return args.fn(args)


if __name__ == "__main__":
    raise SystemExit(main())
