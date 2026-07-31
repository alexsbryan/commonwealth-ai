# Govern a corpus: surface tensions, resolve them into common law

A house charter says quiet hours start at 11 PM; a meeting note from
February says weeknights start at 10. Both are really in your documents.
Which one is the rule *now*?

`svrn govern` answers that class of question. It treats the claims in a
corpus as rules, surfaces the places where two rules can't both hold
(**tensions**), lets you adjudicate them, and then answers questions from
whatever survived — citing current law and excluding what you superseded.
Every decision is an append-only log entry, never an edit: history is
preserved, any act can be reverted, and "current law" is always derived
from the log rather than stored. That's the common-law conceit, and it's
literal.

**You need:** [a running daemon](../../docs/START_THE_DAEMON.md) (for
`ask`), [an installed corpus](./KNOWLEDGE_BASES.md), and
[an enriched atlas over it](./ENRICH_A_CORPUS.md) — rules and tensions
are read from the atlas enrichment built there.

## 1 — Establish the governed baseline

```sh
svrn govern seed <corpus>
```

Every rule-shaped claim the atlas extracted becomes a governed rule.
Idempotent — re-running it asserts nothing twice.

## 2 — See what's in tension

```sh
svrn govern tensions <corpus>
```

Open tensions, ranked, glassbox: both rules' text, the model's reading of
each, its reasoning for flagging the pair, and a copy-pasteable resolve
command per conflict:

```
  conflict edge-00001  (confidence 0.85)
    A · sec_charter_ii  [claim-179b...]
        model read it as: Quiet hours begin at 11 PM every night.
    B · sec_2026_02_10  [claim-7d24...]
        model read it as: Quiet hours begin at 10 PM on weeknights.
    → resolve: svrn govern resolve <corpus> edge-00001 --keep <rule-id>
```

`--format json` emits the raw tension records instead.

## 3 — Adjudicate

Two dispositions, deliberately different:

```sh
svrn govern resolve <corpus> <tension-id> --keep <rule-id> [--rationale "<why>"]
svrn govern accept  <corpus> <tension-id> --rationale "<why>"
```

`resolve` picks a winner: the other rule is **superseded** — still in the
history, no longer law. `accept` records the tension as
known-and-tolerated: both rules stay in force, with your rationale on the
record. `accept` requires the rationale; a tolerated contradiction with
no stated reason is just a contradiction.

## 4 — Ask what the law is

```sh
svrn govern ask <corpus> "how many nights can a guest stay?"
```

A grounded, cite-or-abstain answer over the **active** rule set: evidence
from superseded rules is dropped from retrieval, the answer cites the
rules it stands on, and when a cited rule superseded another the answer
says so. If current law doesn't cover the question, it abstains rather
than guessing.

## Where it lives

One append-only file:
`~/.svrnmesh/indexes/<corpus>/atlas/governance_oplog.jsonl`. The atlas
records what your documents say; the oplog records what you decided about
it. Deleting the oplog un-governs the corpus (your enrichment is
untouched); the full audit trail is the file itself.

One current rough edge: `svrn govern <verb> --help` prints the parent
help rather than per-verb detail — the usage lines above are the real
shapes.
