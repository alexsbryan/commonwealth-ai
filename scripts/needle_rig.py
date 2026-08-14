#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""needle_rig — the heterogeneous corpus-selection instrument.

ORDER `mesh-scale-t2-needle-rig`. Spec: MESH_SCALE_100_USERS_1000_CORPORA.md §8.6.

WHY THIS EXISTS
---------------
The Tier-0/Tier-1 1000-corpus rig (`scripts/probe-b-index-residency.sh`,
reused by `scripts/probe-t1-*.sh`) is `cp -r` clones of ONE tiny index with a
rewritten `corpus_id`. That rig proves a BOUND — how many corpora an expansion
fan-out opens, how per-turn wall scales — because every corpus costs the same.
It cannot detect a WRONG PICK: every clone holds identical text, so every
corpus scores identically against any query and "top-8" is tie-arbitrary.
Tuning a selection index against it would be tuning against an instrument that
cannot see the answer it is meant to grade (doc §8.4, "Rig caveat").

This module builds the missing half: k SMALL, GENUINELY DISTINCT corpora, each
carrying EXACTLY ONE invented, mechanically verifiable fact, plus the eval bank
that asks for that fact and the manifest that says which corpus should have
answered. Scattered among the clone stubs, the needles are signal and the
clones are bulk.

THE FACTS ARE INVENTED
----------------------
Every proper noun, place, code and date is synthesised from syllable pools with
a seeded RNG. Nothing here is a real-world fact, so a model cannot answer
parametrically: a hit means RETRIEVAL found the document, not that the weights
remembered the world. (Order seam 1; `feedback_no_teaching_to_test` — the
questions describe SHAPES, and no question repeats the bank's own vocabulary
back at the scorer.)

DETERMINISM
-----------
Per-corpus RNG is seeded from `f"{seed}:{index}"`, NOT drawn from one global
stream. Consequence: `--count 5` and `--count 100` produce byte-identical first
five corpora. That is what makes the order's "measure ingest at k=5, then
extrapolate before committing to k=100" honest — the k=5 probe measures the
same five corpora the k=100 rig will contain. (Order seam 2.)

TWO SUBCOMMANDS, ONE SCHEMA
---------------------------
`generate` writes the manifest; `score` reads it. They live in one file so the
rig schema has ONE decider (ARCH §10.6) — a needle whose question the scorer
cannot find is a parse error here, not a silently-missing row.

    needle_rig.py generate --out DIR [--count K] [--seed S]
    needle_rig.py score --eval-json RUN.json --manifest DIR/manifest.json

SCORING IS MECHANICAL — NO JUDGE
--------------------------------
`svrn eval run --prod-pipeline --format json` emits, per question, the ORDERED
evidence pool the production KnowledgeQuery pipeline handed synthesis
(`eval_cmd/runner.rs:1155 run_question_prod` → `EvalResult.retrieved`, plus
`corpora_hit`). Three questions are answered by string comparison alone:

  needle-corpus hit  expected corpus id ∈ `corpora_hit`
  needle-chunk hit   some retrieved chunk has that corpus id AND the needle
                     document's title
  rank               1-based position of that chunk in `retrieved`

A fourth, `fact_present`, checks the needle CODE is in the retrieved snippet.
That one is not a quality metric — it is the INSTRUMENT CHECK (ARCH §18.4).
A chunk-hit whose snippet lacks the planted code means the rig mis-ingested,
and the run is `could-not-judge`, not a low score.
"""

from __future__ import annotations

import argparse
import json
import os
import random
import re
import sys
from pathlib import Path

# ── Invented vocabulary ──────────────────────────────────────────────────────
# Syllable pools, not word lists: the product is pronounceable but absent from
# any corpus a model was trained on. Kept deliberately small — the RNG's job is
# to combine them, and a large pool would only hide collisions.

_ONSET = "b br c ch cl d dr f fl g gl h j k kh l m n p pr qu r s sh sk sl sn st t th tr v w y z".split()
_NUCLEUS = "a ae ai e ea ee i ia o oa oo ou u ue y".split()
_CODA = "ck ct ft gh ld lk ll lm ln lt mb mp nd ng nk nt pt rd rf rk rl rm rn rt sk sp st th".split()

# 20 domains. Each is (field, artifact, role, instrument, [three filler topics]).
# Heterogeneity is the whole point: if every needle corpus were the same shape,
# their centroids would collide and the rig would grade selection as impossible
# for a reason that is the rig's fault, not the system's.
_DOMAINS = [
    ("tidal hydrology", "gauge log", "tide warden", "stilling-well float gauge",
     ["channel dredging schedule", "silt accumulation survey", "spring-tide staffing roster"]),
    ("seed vault curation", "accession record", "vault curator", "germination cabinet",
     ["cold-room humidity policy", "accession numbering scheme", "viability retest interval"]),
    ("textile mill metrology", "loom certificate", "shift metrologist", "warp-tension dynamometer",
     ["shuttle replacement ledger", "dye-lot tracking rules", "night-shift handover form"]),
    ("mycology survey", "plot voucher", "field mycologist", "spore-print reference plate",
     ["transect layout guide", "substrate classification key", "wet-season sampling window"]),
    ("radio propagation", "beacon sheet", "propagation officer", "ionosonde sweep receiver",
     ["antenna mast inspection", "callsign allocation table", "solar-flux logging habit"]),
    ("glacier monitoring", "ablation stake report", "ice observer", "sonic ranging mast",
     ["crevasse hazard mapping", "stake redrill procedure", "meltwater channel notes"]),
    ("apiary inspection", "hive audit", "apiary inspector", "brood-frame counting board",
     ["forage radius mapping", "swarm-capture protocol", "winter feed accounting"]),
    ("kiln ceramics", "firing docket", "kiln master", "cone-pack pyrometer",
     ["clay body recipe index", "glaze crazing incidents", "wood-fuel moisture log"]),
    ("harbour dredging", "spoil manifest", "dredge supervisor", "cutter-suction draft meter",
     ["berth depth allowances", "spoil-ground rotation", "barge turnaround timings"]),
    ("orchard grafting", "rootstock ledger", "orchard steward", "cambium alignment jig",
     ["scion storage practice", "graft union failure notes", "pruning cycle calendar"]),
    ("cave hydrogeology", "dye-trace record", "cave hydrologist", "fluorometric sampler",
     ["sump diving safety rules", "conduit mapping method", "recharge event triggers"]),
    ("lighthouse optics", "lamp service card", "optics keeper", "rotating Fresnel carriage",
     ["mercury bath maintenance", "character timing checks", "fog signal duty roster"]),
    ("peat coring", "core log", "coring technician", "piston corer barrel",
     ["core storage temperatures", "compaction correction rules", "transect spacing policy"]),
    ("vineyard phenology", "veraison sheet", "vineyard scout", "canopy porosity meter",
     ["bud-break scoring method", "row orientation trials", "harvest-window heuristics"]),
    ("bridge strain telemetry", "strain sweep", "structures engineer", "vibrating-wire gauge array",
     ["expansion joint inspection", "traffic loading assumptions", "cable tension retest rules"]),
    ("saltern harvesting", "pond yield note", "saltern foreman", "brine densitometer",
     ["evaporation pond rotation", "rake schedule constraints", "crystal grade sorting"]),
    ("bat acoustic survey", "roost transect", "acoustic surveyor", "heterodyne detector rig",
     ["emergence count protocol", "roost disturbance limits", "call classification caveats"]),
    ("foundry sand testing", "mould batch card", "sand technician", "compactability rammer",
     ["binder mix proportions", "shakeout dust controls", "reclaim sand blending"]),
    ("weir fish counting", "passage tally", "counting warden", "resistivity counter tunnel",
     ["trap-and-truck fallback", "debris rack clearing", "smolt run timing notes"]),
    ("herbarium conservation", "sheet treatment log", "conservation botanist", "anoxic freezer chamber",
     ["mounting adhesive trials", "pest incursion response", "loan handling conditions"]),
]

_PLACE_TAIL = ["Hollow", "Reach", "Bight", "Fen", "Scar", "Moor", "Spit", "Combe",
               "Drift", "Haugh", "Rill", "Knap", "Slade", "Wold", "Carr", "Nook",
               "Brae", "Cleuch", "Garth", "Holm"]
_ORG_TAIL = ["Trust", "Consortium", "Board", "Institute", "Cooperative", "Authority",
             "Foundation", "Syndicate", "Bureau", "Assembly"]


def _syllable(rng: random.Random) -> str:
    s = rng.choice(_ONSET) + rng.choice(_NUCLEUS)
    if rng.random() < 0.45:
        s += rng.choice(_CODA)
    return s


def _word(rng: random.Random, syllables: int) -> str:
    return "".join(_syllable(rng) for _ in range(syllables)).capitalize()


def _slug(text: str) -> str:
    return re.sub(r"-+", "-", re.sub(r"[^a-z0-9]+", "-", text.lower())).strip("-")


def normalize_title(text: str) -> str:
    """Mirror of `corpus_engine::filters::normalize_title` for scoring.

    The eval scorer that ships (`eval_cmd/score.rs:47`) normalises expected
    sources through that function before comparing. We do NOT reimplement its
    fuzzy behaviour — the rig's titles are filename stems we chose, so
    lowercase + non-alphanumeric collapse is exact for this input set, and any
    divergence would show up as a chunk-hit rate of zero rather than a subtle
    skew. Named here so the next reader knows it is a deliberate narrow mirror,
    not an accidental second implementation of a shared scorer (ARCH §10.6).
    """
    return _slug(text)


# ── Generation ───────────────────────────────────────────────────────────────

class Needle:
    """One corpus's planted fact plus everything derived from it."""

    def __init__(self, index: int, seed: str):
        rng = random.Random(f"{seed}:{index}")
        self.index = index
        domain, artifact, role, instrument, fillers = _DOMAINS[index % len(_DOMAINS)]
        self.domain = domain
        self.artifact = artifact
        self.role = role
        self.instrument = instrument
        self.fillers = fillers

        self.place = f"{_word(rng, 2)} {rng.choice(_PLACE_TAIL)}"
        self.org = f"{_word(rng, 2)} {rng.choice(_ORG_TAIL)}"
        self.person = f"{_word(rng, 2)} {_word(rng, 2)}"
        # The three-part needle. Each part is independently checkable and none
        # of them exists outside this corpus.
        self.code = "{}{}-{}".format(
            rng.choice("BCDFGHJKLMNPQRSTVWXZ"),
            rng.choice("BCDFGHJKLMNPQRSTVWXZ"),
            rng.randrange(1000, 10000),
        )
        self.date = "{:04d}-{:02d}-{:02d}".format(
            rng.randrange(1961, 2011), rng.randrange(1, 13), rng.randrange(1, 29)
        )
        self.reading = "{}.{} {}".format(
            rng.randrange(2, 99), rng.randrange(10, 100),
            rng.choice(["mm", "kPa", "µS/cm", "dB", "g/L", "N", "lux", "ppm"]),
        )

        self.corpus_id = "needle-{:03d}-{}".format(index, _slug(self.place))
        self.needle_stem = "{}-{}-{}".format(
            _slug(self.artifact), _slug(self.place), self.date.replace("-", "")
        )
        self.question_id = "needle_{:03d}".format(index)

    # The question names the PLACE, the ORG and the INSTRUMENT — enough
    # distinctive vocabulary that a corpus selector could route it — and asks
    # for the code and the person, neither of which it contains.
    @property
    def question(self) -> str:
        return (
            f"In the {self.org}'s {self.artifact} for the {self.instrument} at "
            f"{self.place}, what {self.domain} reference code was logged, and which "
            f"{self.role} signed the entry?"
        )

    @property
    def expected_facts(self) -> list[str]:
        return [self.code, self.person, self.date]

    def needle_document(self) -> str:
        return (
            f"# {self.artifact.title()} — {self.place}\n\n"
            f"Issued by the {self.org} for the {self.instrument} installed at "
            f"{self.place}.\n\n"
            f"On {self.date} the {self.instrument} at {self.place} was serviced and "
            f"re-certified. The {self.domain} reference code logged against this "
            f"entry is {self.code}. The entry was signed by {self.person}, "
            f"{self.role} for the {self.org}.\n\n"
            f"Recorded value at certification: {self.reading}. This "
            f"{self.artifact} supersedes all prior entries for {self.place} and is "
            f"the sole authority for the {self.code} reference.\n"
        )

    def filler_documents(self) -> list[tuple[str, str]]:
        """Three docs sharing the corpus's vocabulary but not its needle.

        They exist so the corpus has a real centroid rather than a single
        document's embedding, and so a chunk-level hit inside the right corpus
        is not automatic — the needle has to beat its own neighbours too.
        """
        out = []
        for topic in self.fillers:
            stem = "{}-{}".format(_slug(topic), _slug(self.place))
            body = (
                f"# {topic.title()} — {self.org}\n\n"
                f"Standing guidance for {self.domain} work at {self.place}, issued by "
                f"the {self.org}.\n\n"
                f"This note covers {topic} as it applies to the {self.instrument} and "
                f"to any comparable equipment held at {self.place}. It sets out the "
                f"working expectations for the {self.role} on duty and the "
                f"circumstances in which a separate {self.artifact} must be raised.\n\n"
                f"Nothing in this note carries a certification reference; reference "
                f"codes are issued only on a {self.artifact}. Queries about {topic} at "
                f"{self.place} go to the {self.org}.\n"
            )
            out.append((stem, body))
        return out


def cmd_generate(args: argparse.Namespace) -> int:
    if args.count > len(_DOMAINS) * 50:
        print(f"needle_rig: --count {args.count} exceeds the vocabulary's "
              f"collision-free range", file=sys.stderr)
        return 2
    out = Path(args.out)
    docs_root = out / "docs"
    docs_root.mkdir(parents=True, exist_ok=True)

    needles = [Needle(i, args.seed) for i in range(args.count)]

    # Collision guard. Two corpora sharing a code or a title would make a
    # "wrong corpus answered correctly" indistinguishable from a right answer,
    # which is exactly the defect this rig exists to remove. Fail loudly.
    for field in ("code", "corpus_id", "needle_stem", "person"):
        seen: dict[str, int] = {}
        for n in needles:
            v = getattr(n, field)
            if v in seen:
                print(f"needle_rig: FATAL seed collision on {field}={v!r} "
                      f"(corpora {seen[v]} and {n.index}) — pick another --seed",
                      file=sys.stderr)
                return 1
            seen[v] = n.index

    for n in needles:
        d = docs_root / n.corpus_id
        d.mkdir(parents=True, exist_ok=True)
        (d / f"{n.needle_stem}.md").write_text(n.needle_document(), encoding="utf-8")
        for stem, body in n.filler_documents():
            (d / f"{stem}.md").write_text(body, encoding="utf-8")

    # No absolute paths in here. The manifest is the A/B identity of a rig —
    # two rigs minted from the same seed and count MUST compare byte-identical
    # so a later run can prove it graded the same corpora. An `--out`-derived
    # path would make that comparison fail for a reason that has nothing to do
    # with the rig's content (caught by the `cmp` check during build-out).
    manifest = {
        "schema": "needle-rig/v1",
        "seed": args.seed,
        "count": args.count,
        "needles": [
            {
                "index": n.index,
                "corpus_id": n.corpus_id,
                "question_id": n.question_id,
                "needle_title": n.needle_stem,
                "code": n.code,
                "person": n.person,
                "date": n.date,
                "question": n.question,
            }
            for n in needles
        ],
    }
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")

    # The eval bank. `[bank] corpus` is only consulted by `--isolate` (which
    # this rig must NEVER use — isolating to one corpus is precisely the
    # selection decision under test); it is recorded for provenance.
    lines = [
        "# GENERATED by scripts/needle_rig.py — do not hand-edit.",
        f"# seed={args.seed} count={args.count}",
        "#",
        "# Every fact below is invented. Run WITHOUT --isolate: the whole point is",
        "# that retrieval must choose the right corpus out of the full installed set.",
        "",
        "[bank]",
        'name = "needle-rig-v1"',
        f'corpus = "{needles[0].corpus_id}"' if needles else 'corpus = ""',
        'description = """',
        f"{args.count} planted-needle questions, one per distinct tiny corpus.",
        "Scored mechanically by scripts/needle_rig.py score against the run's",
        "ordered evidence pool — corpus hit, chunk hit, rank. No judge.",
        '"""',
        "",
    ]
    for n in needles:
        lines += [
            "[[questions]]",
            f'id = "{n.question_id}"',
            'category = "needle"',
            'question = """',
            n.question,
            '"""',
            "expected_facts = [",
        ]
        lines += [f'    "{f}",' for f in n.expected_facts]
        lines += [
            "]",
            "expected_sources = [",
            f'    "{n.needle_stem}",',
            "]",
            f'notes = "corpus={n.corpus_id}"',
            "",
        ]
    (out / "bank.toml").write_text("\n".join(lines), encoding="utf-8")

    print(f"needle_rig: generated {args.count} corpora under {docs_root}")
    print(f"needle_rig: bank     {out / 'bank.toml'}")
    print(f"needle_rig: manifest {out / 'manifest.json'}")
    return 0


# ── Scoring ──────────────────────────────────────────────────────────────────

_RANK_BUCKETS = [(1, 1), (2, 3), (4, 8), (9, 20), (21, 10**9)]


def cmd_score(args: argparse.Namespace) -> int:
    manifest = json.loads(Path(args.manifest).read_text(encoding="utf-8"))
    run = json.loads(Path(args.eval_json).read_text(encoding="utf-8"))
    by_qid = {n["question_id"]: n for n in manifest["needles"]}
    results = run.get("results", [])
    if not results:
        print("NEEDLE_RIG COULD-NOT-JUDGE reason=no_results_in_eval_json")
        return 4

    rows = []
    unmatched = []
    for r in results:
        qid = r.get("question_id", "")
        n = by_qid.get(qid)
        if n is None:
            unmatched.append(qid)
            continue
        want_corpus = n["corpus_id"]
        want_title = normalize_title(n["needle_title"])
        retrieved = r.get("retrieved", [])
        corpus_hit = want_corpus in (r.get("corpora_hit") or [])
        rank = None
        fact_present = None
        for i, c in enumerate(retrieved, start=1):
            if c.get("corpus_id") != want_corpus:
                continue
            if normalize_title(c.get("title") or "") != want_title:
                continue
            rank = i
            fact_present = n["code"] in (c.get("snippet") or "")
            break
        rows.append({
            "question_id": qid,
            "corpus": want_corpus,
            "corpus_hit": corpus_hit,
            "chunk_hit": rank is not None,
            "rank": rank,
            "fact_present": fact_present,
            "pool": len(retrieved),
            "embed_ms": r.get("embed_ms", 0),
            "search_ms": r.get("search_ms", 0),
        })

    if unmatched:
        # §18.3: never silently substitute. An eval run whose questions do not
        # line up with the manifest is a different run, not a partial one.
        print(f"NEEDLE_RIG COULD-NOT-JUDGE reason=unmatched_question_ids "
              f"n={len(unmatched)} first={unmatched[0]}")
        return 4

    n_q = len(rows)
    corpus_hits = sum(1 for r in rows if r["corpus_hit"])
    chunk_hits = sum(1 for r in rows if r["chunk_hit"])
    ranks = sorted(r["rank"] for r in rows if r["rank"] is not None)

    # INSTRUMENT CHECK BEFORE RESULT (ARCH §18.4). A chunk we identified by
    # corpus+title whose text does not contain the planted code means the rig
    # ingested something other than what it generated. That invalidates the
    # measurement; it does not lower it.
    bad_fact = [r["question_id"] for r in rows if r["chunk_hit"] and not r["fact_present"]]
    if bad_fact:
        print(f"NEEDLE_RIG COULD-NOT-JUDGE reason=needle_chunk_missing_planted_code "
              f"n={len(bad_fact)} first={bad_fact[0]}")
        print("  the rig's own document text is not what reached the index — "
              "regenerate and re-ingest before reading any rate below")
        return 4

    def pct(x: int) -> str:
        return f"{100.0 * x / n_q:.1f}%"

    # `hit_at_10` is the production-budget reading: `eval run`'s own default
    # keeps 10 chunks, and a synthesis prompt of that order is the realistic
    # evidence budget. Reported ALONGSIDE the uncensored chunk-hit rate, never
    # instead of it — the gap between the two is exactly "retrieved but ranked
    # too low", which is a different defect from "never selected".
    hits_at_10 = sum(1 for r in rows if r["rank"] is not None and r["rank"] <= 10)

    print(f"NEEDLE_RIG questions={n_q} "
          f"corpus_hit={corpus_hits}/{n_q} ({pct(corpus_hits)}) "
          f"chunk_hit={chunk_hits}/{n_q} ({pct(chunk_hits)}) "
          f"hit_at_10={hits_at_10}/{n_q} ({pct(hits_at_10)})")
    if ranks:
        mid = ranks[len(ranks) // 2]
        print(f"NEEDLE_RIG rank_of_hits n={len(ranks)} best={ranks[0]} "
              f"median={mid} worst={ranks[-1]}")
    else:
        print("NEEDLE_RIG rank_of_hits n=0 — no needle chunk reached any evidence pool")
    for lo, hi in _RANK_BUCKETS:
        c = sum(1 for r in ranks if lo <= r <= hi)
        label = f"{lo}" if lo == hi else (f"{lo}+" if hi > 10**8 else f"{lo}-{hi}")
        print(f"NEEDLE_RIG rank_bucket {label:>5} {c:>4} ({pct(c)})")
    miss = n_q - len(ranks)
    print(f"NEEDLE_RIG rank_bucket  miss {miss:>4} ({pct(miss)})")

    pools = sorted(r["pool"] for r in rows)
    embed = sorted(r["embed_ms"] for r in rows)
    search = sorted(r["search_ms"] for r in rows)
    print(f"NEEDLE_RIG pool_size median={pools[len(pools)//2]} "
          f"min={pools[0]} max={pools[-1]}")
    print(f"NEEDLE_RIG per_turn_ms embed_median={embed[len(embed)//2]} "
          f"search_median={search[len(search)//2]} "
          f"search_min={search[0]} search_max={search[-1]}")

    if args.detail:
        for r in rows:
            print(f"NEEDLE_RIG_ROW {r['question_id']} corpus_hit={int(r['corpus_hit'])} "
                  f"chunk_hit={int(r['chunk_hit'])} rank={r['rank']} "
                  f"pool={r['pool']} search_ms={r['search_ms']}")
    if args.out:
        Path(args.out).write_text(json.dumps({
            "questions": n_q,
            "corpus_hit": corpus_hits,
            "chunk_hit": chunk_hits,
            "hit_at_10": hits_at_10,
            "ranks": ranks,
            "rows": rows,
        }, indent=2), encoding="utf-8")
    return 0


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(prog="needle_rig", description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)

    g = sub.add_parser("generate", help="write k needle corpora + bank + manifest")
    g.add_argument("--out", required=True)
    g.add_argument("--count", type=int, default=100)
    g.add_argument("--seed", default="mesh-scale-t2")
    g.set_defaults(fn=cmd_generate)

    s = sub.add_parser("score", help="score an `eval run --prod-pipeline --format json` run")
    s.add_argument("--eval-json", required=True)
    s.add_argument("--manifest", required=True)
    s.add_argument("--detail", action="store_true")
    s.add_argument("--out")
    s.set_defaults(fn=cmd_score)

    a = p.parse_args(argv)
    return a.fn(a)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
