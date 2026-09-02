# Where we are — ontology-v1

**The point:** a domain expert writes TOML, and their own nouns come out the other end.
No Rust. That is the whole objective, and the chain below is how we know it's true.

## The chain — 5 of 6 links proven end to end

```
recipe new --ontology numismatics   ✔  PROVEN — scaffolds, ids filled
  → validate                        ✔  PROVEN — prints the derived facets
  → install → init                  ✔  PROVEN — by PATH; ontology reaches the config
  → build (ends with backfill)      ✔  PROVEN — 8/8 steps, exit 0, backfill 32/32
  → chat enumerates coins           ✔  PROVEN — 13 coin atoms, all 7 catalogue coins
  → desktop shows the author's nouns ?  never run — needs P6
```

**Proven on `wessex-hoard`, 2026-09-02, against a live 35B model:**

```
declared: coin sceatta ruler mint attribution
atoms: 176
  coin 10 · sceatta 4 · ruler 7 · mint 3 · attribution 49
roles carried as State (role_of): ruler
PASS: 73 of 176 atom(s) carry a type the author declared.
```

`scope` is `universal` on all 49 claims (was `fictional` — the literary default
leaking into a numismatics catalogue). 33 of 49 claims carry declared attributes.
`ruler` reads as a **State on a person atom**, not a `ruler` entity — a part played
is not an essence (§7.5). Enumeration returns all seven catalogue coins including
the three that lost the chunk top-k to Wikipedia; enumeration has no top-k.

Re-run any time: `scripts/setup-numismatics-corpus.sh` (`--assert-only` for the
payload check alone, ~1s against the built atlas).

## The one gap left in the chain

**Declared attributes never reach an ENTITY.** 0 of 32 entity atoms carry one,
while 33 of 49 claims do. Two consequences, both measured:

- `Aggregate{coin, metal}` classifies correctly and tallies `(unset): 13` — "and
  what metal is each" cannot be answered.
- `enrich reconcile` reads the declaration correctly (`identity criteria: 2
  external, 0 descriptive` — `catalogue_ref` on `coin`, inherited by `sceatta`)
  and merges nothing, because no atom carries the key. "Coenwulf mancus" and
  "gold mancus" stay two atoms.

This is upstream of P3 and P5 — the Phase-1 prompt or the family validation, which
is P2's ground. It is the next thing worth fixing.

## What running the chain found — four breaks, none visible to 11,657 tests

Unit tests test commands; every command worked. All four sat in the seams between
them, and all four are fixed.

1. `corpus install my-coins.toml` refused the path `recipe validate` had just accepted.
2. A relative `[acquire] path` resolved against the DAEMON's working directory.
3. `enrich build` stopped at step 1 of 9 because five sections fell under phase 1's
   own 40-word floor — the pipeline treating its own decision as fatal.
4. **`corpus install --wait` printed `✓ installed (ready after 0s)` for an ingest
   that never ran**, because the first readiness poll found the previous index.
   The other three failed loudly; this one succeeded in green.

The merge of four parallel branches found three more that no single branch could
see — see note `e5036d81`.

## Phases

Landed: **P0** crate split · **P1** ontology block → policies · **P2 · P2b** parser,
schema generator, `CustomOntology`, projection · **P3** resolution + identity ·
**P4** tension + change axes · **P5** declared types reach answers · **P7** base-kind
refs, eight templates, interview, card
Ahead: **P6** desktop · `--quick` bench once, at the very end

## Watch

- Declared attributes on entities: 0 of 32. The gap above. **P2's ground.**
- `enrich reconcile` is NOT a build step — the coverage report says "merges: not run"
  and names the command. P3's order sequenced it before backfill; it isn't there.
- P4's tension axes are **could-not-judge**, not passing: `between` reports rather
  than enforces while claims carry a kind but rarely a `subject` (1 of 49).
- Four files accepted over their size baseline at the wave merge — `SYSTEM_OVERVIEW`
  §10.1h names them and schedules the splits.
- `bench enron` B³ has never been run for P3, before or after.

---

*Hand-maintained; update it in the merge commit when a link or a phase moves.
The chain is the point of this file: phase progress can look healthy while every
link stays unproven, which is the failure this board exists to make visible.*
