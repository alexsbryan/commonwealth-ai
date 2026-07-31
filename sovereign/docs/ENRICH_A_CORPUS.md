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
→ tensions → gaps → configure → report. The LLM phases (`seed`,
`extract`, `name`, `configure`) need the daemon; the structural phases are
pure Rust and run offline. Any phase failure stops the flow with that
phase's exit code — nothing downstream runs on half-built input.

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
