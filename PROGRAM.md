# Where we are — ontology-v1

**The point:** a domain expert writes TOML, and their own nouns come out the other end.
No Rust. That is the whole objective, and the chain below is how we know it's true.

## The chain — 4½ of 6 links proven end to end

```
recipe new --ontology numismatics   ✔  PROVEN — scaffolds, ids filled
  → validate                        ✔  PROVEN — prints the derived facets
  → install → init                  ✔  PROVEN — by PATH; ontology reaches the config
  → build (ends with backfill)      ✔  PROVEN — 9/9 steps, exit 0, 35/35 backfilled
  → chat enumerates coins           ~  HALF — names 4 of 7; retrieval-shaped, not
                                       ontology-shaped. This is P5's number.
  → desktop shows the author's nouns ?  never run — needs P6
```

**Proven on `wessex-hoard`, 2026-09-02, against a live 35B model:**

```
declared: coin sceatta ruler mint attribution
atoms: 169
  coin 9 · sceatta 4 · ruler 3 · mint 4 · attribution 0
PASS: 20 of 169 atom(s) carry a type the author declared.
```

"Eoforwic mint" is typed `mint`. "Aldfrith of Northumbria" is typed `ruler`.
`atlas/ontology.json` records the five declared types beside the atoms.
Re-run it any time: `scripts/setup-numismatics-corpus.sh` (`--assert-only` for
the payload check alone, ~1s against the built atlas).

## What running it found — three breaks in the seams between commands

None were visible to 11,574 passing tests, because unit tests test commands
and every command worked. All three are fixed.

1. `corpus install my-coins.toml` refused the path `recipe validate my-coins.toml`
   had just accepted. Two consecutive lines of shipped documentation did not compose.
2. A relative `[acquire] path` resolved against the DAEMON's working directory,
   failing 20s later in a log the author never sees.
3. `enrich build` stopped at step 1 of 9 because five sections fell under phase 1's
   own 40-word floor — the pipeline treating its own decision as a fatal failure.
   Twenty lines below, the same predicate meant "continue".

The CLI-contract journey that should have caught #1 installed the id right after
validating the path, so it composed where the docs did not. It now installs the
file it just validated.

## Phases

Landed: **P0** crate split · **P1** ontology block → policies · **P2 · P2b** parser,
schema generator, `CustomOntology`, projection · **P7** base-kind refs, eight
templates, interview, card
In flight: **P3** resolution + identity · **P4** tension + change axes · **P5** declared
types reach answers
Ahead: **P6** desktop · `--quick` bench once, at the very end

## Watch

- `attribution: 0`. All 48 Claim atoms serialize without `claim_kind`, `subject` or
  `attributes` — the fields exist since P2 and `resolution.rs:1442` writes `None`
  into every one. A declared CLAIM type reaches the config and the sidecar and
  not one atom. **P3 item 2.**
- Three Tension edges, all false positives pairing claims about DIFFERENT coins;
  the one planted disagreement missed. `between = ["attribution"]` restricts
  nothing while no claim carries a kind. **P4, blocked behind P3 item 2.**
- `scope: ClaimScope::Fictional` is hard-coded at `resolution.rs:1442` — "the
  literary default" — so every claim in a numismatics catalogue is labelled
  fiction. Not changed: it is a semantic default, it touches SEP/Wikipedia/Enron
  (I5), and no reader of claim `.scope` was found to measure against.
- `resolution.rs` is at 5217 lines against a 5223 ceiling.
- P0.4b — no daemon route reaches `enrich_now` for an installed recipe corpus.

---

*Hand-maintained; update it in the merge commit when a link or a phase moves.
The chain is the point of this file: phase progress can look healthy while every
link stays unproven, which is the failure this board exists to make visible.*
