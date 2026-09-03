# Enrich a corpus into an atlas

Search finds passages; an atlas knows what's *in* them. Enrichment reads
an installed corpus and builds a structured graph over it — the entities
and what's claimed about them, how they relate, how they change across the
text, and where two claims are in tension. Once it's built, `enrich query`
answers questions from that structure, with citations back into the
corpus.

**You need:** [a running daemon](../../docs/START_THE_DAEMON.md) and
[an installed corpus](./KNOWLEDGE_BASES.md). The LLM phases run on your
resident models — nothing leaves your machine.

## 1 — Point enrichment at the corpus

```sh
svrn enrich init <corpus> --from-corpus <corpus> --pipeline <pipeline>
```

`--pipeline` matters: `init`'s default is a legacy pipeline, and the
atlas build below refuses anything that isn't an atlas pipeline. Name one
explicitly.

## 2 — Build

```sh
svrn enrich build <corpus> --full
```

One shot, every phase in order: seed → extract → cluster → name → resolve
→ tensions → gaps → configure → report → backfill. The LLM phases (`seed`,
`extract`, `name`, `configure`) need the daemon; the structural phases are
pure Rust and run offline. Any phase failure stops the flow with that
phase's exit code — nothing downstream runs on half-built input.

The last phase, `backfill`, embeds the resolved atoms into
`atlas/atoms_ann.lance` through the daemon's embed model — the table the
daemon seeds atlas grounding from — so a freshly built corpus grounds
without a further command. It needs the daemon's embed slot, which the
build probes before the first phase runs (a build that cannot embed fails
there, not thirty minutes later). `--skip backfill` builds without
grounding; `svrn atlas backfill-ann <corpus>` seeds it afterwards. A
re-run skips `backfill` when the table is newer than `atoms.json` and
rebuilds it when a resolve has made it stale.

## 3 — Check what you have

```sh
svrn enrich status <corpus>
```

A per-phase freshness table — `✓ fresh`, `⚠ stale`, or `· never run` —
so "did the build actually land?" is a read, not a guess. The structural
view of the result is `svrn atlas status <corpus>`: atom counts and
readiness for the same corpus.

## 4 — Ask it something

```sh
svrn enrich query <corpus> "Who is Alyosha?"
```

The answer is an assembled brief from the atlas, not a chat response:
where the entity first appears, its relations, the claims attributed to
it, its trajectory through the text — each line marked with how it was
derived. Depth-limited traversal flags (`--depth N`, `--json`) and the
other query shapes are in the
[command reference](./CLI_REFERENCE.md#svrn-enrich).

## Declare your own nouns

Everything above extracts into six generic kinds — entity, event, state,
relation, claim, question. If your field has its own vocabulary, you can
declare it in the recipe and get *that* back instead. A numismatist's
recipe says:

```toml
[enrichment.ontology]
version = 1

[[enrichment.ontology.types]]
name = "coin"
kind = "entity"
attributes = [
  { name = "mint",  type = "ref",  of = "mint" },
  { name = "metal", type = "text", values = ["gold", "silver", "billon"] },
]

[[enrichment.ontology.types]]
name = "sceatta"
kind = "entity"
specializes = "coin"
```

`svrn recipe new --ontology <name>` scaffolds a complete recipe from one
of ten worked declarations (`--ontology list` names them), and
`svrn recipe validate` prints what the declaration *derives* — the clock,
the identity criterion per type, the question shapes the corpus will
answer — so you can see the inference before you build on it.

Then the chain is the same as above, with one difference:

```sh
svrn corpus install my-coins.toml --wait     # by PATH — it's your file
svrn enrich init my-coins --from-corpus my-coins
svrn enrich build my-coins --full
```

No `--pipeline`. The declaration *is* the pipeline selection: `init`
resolves the custom atlas from the `[enrichment.ontology]` block.

Two reads tell you whether your nouns arrived:

```sh
svrn enrich schema-report my-coins
svrn enrich atlas-query my-coins "Which coins are in this catalogue?" --json
```

`schema-report` is the coverage table — how many atoms landed under each
type you declared, and which declared types got nothing. `atlas-query`
is the answer itself: over a declared atlas the question classifies as an
enumeration of *your* type, and each atom in the JSON carries its
`entity_type` — `coin`, `sceatta` — not a generic kind. A subtype counts
toward its parent, so asking for `coin` returns the sceattas too.

This sequence is the `ontology-author` journey in the CLI contract
(`svrn contract map`), so it is exercised rather than only described.

## Where it lives

Phase caches and run outputs under `~/.svrnmesh/enrichment/<corpus>/`;
the resolved atlas — atoms, edges, trajectories, configurations — under
`~/.svrnmesh/indexes/<corpus>/atlas/`. Deleting those directories undoes
enrichment; the corpus itself is untouched.

## What's built on top of it

- [Governance](./GOVERN_A_CORPUS.md) turns the atlas's claims and
  tensions into an adjudicable rule set — surface conflicts, resolve
  them, ask what current law says.
- Chat retrieval consults the atlas when one is present, so grounded
  answers get its structure for free.

The full architecture — atom and edge types, the deterministic resolver,
cross-corpus bridges — is engineering material:
[`corpus-engine/ENRICHMENT.md`](../../corpus-engine/ENRICHMENT.md). This
page is deliberately only the journey.
