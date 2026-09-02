# Where we are — ontology-v1

**The point:** a domain expert writes TOML, and their own nouns come out the other end.
No Rust. That is the whole objective, and the chain below is how we know it's true.

## The chain — all 6 links proven end to end

```
recipe new --ontology numismatics   ✔  PROVEN — scaffolds, ids filled
  → validate                        ✔  PROVEN — prints the derived facets
  → install → init                  ✔  PROVEN — by PATH; ontology reaches the config
  → build (ends with backfill)      ✔  PROVEN — 8/8 steps, exit 0, backfill 32/32
  → chat enumerates coins           ✔  PROVEN — 13 coin atoms, all 7 catalogue coins
  → desktop shows the author's nouns ✔  PROVEN — real-mode Playwright, 2 passed
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
leaking into a numismatics catalogue). 33 of 49 claims carried an attribute — though only 10 of those were a DECLARED
one, the other 28 being the reserved `grade` enum (see the section below).
`ruler` reads as a **State on a person atom**, not a `ruler` entity — a part played
is not an essence (§7.5). Enumeration returns all seven catalogue coins including
the three that lost the chunk top-k to Wikipedia; enumeration has no top-k.

Re-run any time: `scripts/setup-numismatics-corpus.sh` (`--assert-only` for the
payload check alone, ~1s against the built atlas).

## Link 6 — the desktop, closed 2026-09-02

A corpus that declared `coin` was opening as a list of CONVERSATIONS. A folder
of markdown gets conversation skeletons from the importer whatever else it is,
`resolveCorpusKind` asked the conv listing first, and nothing had ever opened
Explore in a real-mode test — so the program's own thesis was failing on its
last screen, silently. A declared ontology now outranks the conv listing.

Found by RUNNING the spec. It had been written, committed and never executed;
both its tests failed on first execution. (On this host the whole Playwright
command must run inside the `sovereign-vulkan` toolbox — its `global-setup`
shells out to `cargo build` with no prefix and dies on llama-cpp before a
single test starts.)

    numismatics.real.spec.ts   2 passed
    governance.real.spec.ts    1 passed — unchanged

Three of the order's assumptions were wrong and are worth keeping written down:
item 1 was not finished (`subject` and `ref` attributes never reached
`referenced_atoms`, so both rendered as raw ids); Position/Opposition/Asset
never threw a `body-render-error` — the TS union had 8 variants against the
backend's 11, so they rendered BLANK, and the order's acceptance test would
have passed on an untouched tree; and the pill badge disagreed with the list it
opened, because the filter was specified as "no roll-up, ask for the family by
name" and then given a single `Option<String>` that could not express a family.
`AtomFilter.subtypes` is a list now, the pill names every descendant, and
`--subtype=coin` returns 13 where `--subtype=coin,sceatta` returns 15.

## The attributes gap — closed 2026-09-02

**Declared attributes never reached an ENTITY: 0 of 32.** They do now. The cause
was neither the prompt's prose nor the family validation, and the trace that
settles it says so in one line:

```
ontology parse: declared attributes atom=entity subject=Series Y sceattas
                declared=7 offered=0 kept=0
```

`offered=0` — the model emitted no `attributes` object at all, so the parser was
never the loss. The **neutral Phase-1 prompt carries a worked JSON example, and
that example shows a `coin` entity with no attributes**. The declared block asked
in prose; the example showed otherwise; the model followed the example. Phase 1
has no grammar to fall back on — the response schema is advisory (models emit
`"1.29 g"` where it says `number`), so the prompt is the whole lever.

The neutral example cannot gain the key: it is shared with every undeclared
corpus. So the declared block — which exists only when types are declared — shows
the sketch whole, in the author's own keys, and names the behaviour it has to beat.

| | before | after |
|---|---|---|
| coin metal | 0/14 | 6/13 |
| coin weight | 0/14 | 5/13 |
| coin catalogue_ref | 0/14 | 4/13 |
| coin ruler · mint · denomination · struck | 0/14 each | 4-5/13 each |
| attribution proposed_date | 10/49 | 14/43 |

Fourteen `attribute:zero:` gap signatures, gone. Both blocked behaviours:

- `Aggregate{coin, metal}` tallied `(unset): 13`; it now reads
  **`13 coin by metal — (unset): 7, gold: 2, silver: 4`**, and enumeration renders
  each coin with its mint, weight, metal and catalogue reference inline.
- `enrich reconcile` still merges **0**, and that is now the correct answer rather
  than a blocked one: the five coins carrying a reference carry five *different*
  references, so there is nothing to collapse. The declared-external-key path had
  never been watched to fire, so it has a test that does (§18.1) — "Coenwulf
  mancus" and "the gold mancus of Coenwulf" share no name token and no origin, and
  collapse on the reference alone.

## The subject link — closed 2026-09-02, at a price

Same root cause, next slot over. `attribution` declares `subject = "coin"` and
36 of 43 claims carried none, because the neutral prompt's claim field list names
`attributed_to` and never `subject`. The build filled `attributed_to` on all 49
claims and `subject` on 1 — a natural control cleaner than any A/B.

    claims of a subject-declaring type with no subject:  36/43  ->  1/37

The payoff is Phase 6, not the count. Subjects let the tension pass compare
claims about the SAME coin:

| | candidates | judged tensions |
|---|---|---|
| before | 155 | 1 |
| after | 34 | 2 |

It now finds both designed disagreements, including *"Do the Series Y sceattas of
Aldfrith date to 695-704 or to 685-690?"* — the Halstead/Ferreira dispute this
corpus was built around. That is the axis the board recorded as **could-not-judge**
below, for exactly this reason.

**The cost, open and unresolved.** Claim ATTRIBUTES collapsed in the same move:
`proposed_date` 14/43 to 1/37, `grade` 28 to 1. Four rebuilds — attributes
mid-object and last, bare shapes and descriptive ones — all land within noise of
zero, while the same example's `subject` fills 36 of 37. The model spends its
per-claim effort on the slot the example makes obligatory. The claim's `content`
still carries the date in prose; the queryable facet is what is lost. Shipped
because the is-about link is what makes a declared claim type mean anything, and
because the tension result is the capability the recipe was written for — but it
is a trade, not a win, and a narrower second pass over claims is the obvious
thing to price next.

Two things the fix had to earn rather than assume. A filled attribute must be
TRUE: the build was writing `weight=0` and `mint="Unknown in text"` where the text
states nothing, which reads downstream as a measurement. A declared `unit` now
refuses a zero in the parser (a count with no unit keeps its zero), and no
placeholder survives in the rebuilt atlas. And the fill rate is measurable at all
only because `enrich schema-report` gained a tenth dimension this session — the
type-count dimension reported `coin` as fully covered while it carried nothing,
which is how the gap survived four merged phases.

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
Landed: **P6** desktop — pills by declared subtype, attribute rows, the
`about` link, bodies for Position/Opposition/Asset, the build report card
Ahead: `--quick` bench once, at the very end · `bench enron` B³ for P3

## Watch

- Declared attributes reach roughly a third to a half of the atoms that could
  carry them (`coin metal 6/13`), not all of them. The zeros are gone and the
  remainder is elicitation, now measured every build by the tenth dimension.
- `enrich reconcile` is NOT a build step — the coverage report says "merges: not run"
  and names the command. P3's order sequenced it before backfill; it isn't there.
- P4's tension axes now have their input: `subject` reaches 36 of 37 claims and
  Phase 6 finds 2 tensions from 34 candidates (was 1 from 155). Whether `between`
  ENFORCES rather than reports is still P4's own question.
- Claim attributes are the price of that: `grade` 28 -> 1, `proposed_date` 14 -> 1.
  A narrower second pass over claims is the untried option.
- Four files accepted over their size baseline at the wave merge — `SYSTEM_OVERVIEW`
  §10.1h names them and schedules the splits.
- `bench enron` B³ has never been run for P3, before or after.

---

*Hand-maintained; update it in the merge commit when a link or a phase moves.
The chain is the point of this file: phase progress can look healthy while every
link stays unproven, which is the failure this board exists to make visible.*
