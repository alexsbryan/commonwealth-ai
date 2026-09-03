# Where we are — ontology-v1

**The point:** a domain expert writes TOML, and their own nouns come out the other end.
No Rust. That is the whole objective, and the chain below is how we know it's true.

## The chain — all 6 links proven end to end

```
recipe new --ontology numismatics   ✔  PROVEN — scaffolds, ids filled
  → validate                        ✔  PROVEN — prints the derived facets
  → install → init                  ✔  PROVEN — by PATH; ontology reaches the config
  → build (ends with backfill)      ✔  PROVEN — 8/8 steps, exit 0, backfill 32/32
  → chat enumerates coins           ✔  PROVEN — 19 coin-family atoms, all 7 catalogued
  → desktop shows the author's nouns ✔  PROVEN — real-mode Playwright, 2 passed
```

**Proven on `wessex-hoard`, against a live 35B model. The payload, scored
against `sovereign-recipes/wessex-hoard/truth.json` (`scripts/setup-numismatics-corpus.sh
--assert-only`, ~1s):**

```
atlas: 159 atoms, fingerprint sha256:22c90ef2…

  catalogue_ref           7 / 7   ok
  coin family            19 / 7   ok      coin + sceatta
  mint                    3 / 3   ok
  ruler                   4 / 4   ok
  attribution            49 / 7   ok
  grade values            1 / 1   ok      3 of 4 declared — never extracted: die-link
```

Recall bars, not counts: the corpus holds articles about coins outside the
catalogue, so an atlas legitimately over-produces and the question is whether
every catalogued thing LANDED. Every one did. `catalogue_ref` 7/7 is the
identity arc's own bar — the declared key reaches every catalogued coin.

`ruler` reads as a **State on a person atom**, not a `ruler` entity — a part
played is not an essence (§7.5), and the four named rulers are all there.
Enumeration returns all seven catalogue coins including the three that lost the
chunk top-k to Wikipedia; enumeration has no top-k. `scope` is `universal` on
all 49 claims (was `fictional` — the literary default leaking into a
numismatics catalogue).

Until 2026-09-03 the bar here was "at least one atom carries a declared type",
which failed on 1 of the 176 shapes an atlas can take. The script reports four
verdicts now (PASS / FAIL / COULD-NOT-JUDGE / NEVER-RAN), and a stale atlas or
one built from a different declaration no longer prints PASS.

## Closed — where each is recorded

Every one of these had a section here restating a commit body that had already
been harvested verbatim into a note. Two copies of a finding is one more than
the finding needs, and this board's job is the chain, not the archive
(AGENTS.md, "Ship code, not prose"). The record, in order:

| Closed | What it was | Where it is written |
|---|---|---|
| Link 6, the desktop | a corpus declaring `coin` opened as a list of CONVERSATIONS; a declared ontology now outranks the conv listing | note `5611e8ee` |
| The attributes gap | declared attributes reached 0 of 32 entities; the cause was neither the prompt's prose nor the family validation | note `cda80196` |
| The subject link | `attribution` declares `subject = "coin"` and 36 of 43 claims carried none — the neutral prompt names `attributed_to` and never `subject` | note `cda80196` |
| The slots, required | the `attributes` bag is `required`, and `subject` is asked for BEFORE it, because under llguidance property order IS generation order | note `5c06bc92` (invariant), commit `bd1d11e9` |
| The three seams | typed claim subjects, the identity merge veto, the shed retry — a true extraction turned into a wrong atlas in three places downstream of both schema and prompt | note `53e036a4`, commit `0d1ca609e` |
| Four breaks in the seams | `corpus install` refusing a validated path; a relative acquire path against the daemon's cwd; phase 1's own word floor fatal; `--wait` green for an ingest that never ran | note `5611e8ee`; three more from the four-branch merge in note `e5036d81` |
| The reuse debt | four deciders collapsed to one each; then one ontology instead of six, a chain proof with four verdicts, `ontology-author` in the contract, glob-discovered templates, one absent-marker decider, `extract --dry-run` | notes `020972b0`, `d61eb8d4` |

## Phases

Landed: **P0** crate split · **P1** ontology block → policies · **P2 · P2b** parser,
schema generator, `CustomOntology`, projection · **P3** resolution + identity ·
**P4** tension + change axes · **P5** declared types reach answers · **P7** base-kind
refs, ten templates, interview, card
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
- Claim attributes stay below the pre-registered bar (`grade` 14, `proposed_date`
  11 of 49 on the seam-fixed rebuild). The probes say the remainder is claims that
  propose no date; the fills that exist are true to the text.
- `claims_missing_subject` is 9 of 49 — HIGHER than the 2 the mis-typed resolver
  reported, and that is the point: a subject that is not of the declared type is
  now an honest absence rather than a link to the wrong atom.
- The Aldfrith dispute (Halstead 685-690 against Ferreira 695-704) still does not
  pair. On this extraction Phase 1 did not give Halstead's claim a subject at all,
  so it is upstream of the seams now fixed. The Series R dispute DOES pair.
- Coin `weight` is invented under the full-key worked example: 0.32 / 0.72 g for a
  coin whose section states no weight, in every probe variant. The parser catches a
  zero, not a plausible number. Order item 5 (trim the example to fidelity) is open.
- `catalogue_ref` reaches all seven catalogued coins in the built atlas (7/7,
  2026-09-03) — the standing probe result that it was omitted at extraction
  (3 of 3 probes, and identity-first bag order did not change it) no longer
  describes the shipped result. An eighth ref, the literal string `above`,
  is a resolution artefact worth a look; it fails no bar.
- `die-link` is declared in `grades` and never extracted: 3 of the 4 declared
  values land, on 49 attribution claims of which 35 carry no grade at all.
  Reported by the payload check, deliberately not gated — which of an
  author's enum values an extraction reaches is a quality measure, not a
  chain break, and moving a bar to hide it would be the wrong repair.
- Four files accepted over their size baseline at the wave merge — `SYSTEM_OVERVIEW`
  §10.1h names them and schedules the splits.
- `bench enron` B³ has never been run for P3, before or after.

---

*Hand-maintained; update it in the merge commit when a link or a phase moves.
The chain is the point of this file: phase progress can look healthy while every
link stays unproven, which is the failure this board exists to make visible.*
