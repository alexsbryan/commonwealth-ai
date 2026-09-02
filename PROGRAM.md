# Where we are — ontology-v1

**The point:** a domain expert writes TOML, and their own nouns come out the other end.
No Rust. That is the whole objective, and the chain below is how we know it's true.

## The chain — 1 of 6 links proven end to end

```
recipe new --ontology numismatics   ~  works in tests
  → validate                        ✔  PROVEN — prints the derived facets
  → install → init                  ?  never run
  → build (ends with backfill)      ~  unit-proven; never run on a real corpus
  → chat enumerates coins           ✗  NEVER RUN — no model has touched this program
  → desktop shows the author's nouns ?  never run
```

**Nothing above has been run end to end, once.** 11574 passing tests prove the parts
fit together; they say nothing about whether the thing works. The single most
valuable next action is not a phase — it is running that chain and finding out.

## Phases

Landed: **P1** ontology block → policies · **P2 · P2b** parser, schema generator,
`CustomOntology`, projection · **P7** base-kind refs, eight templates, interview, card
In flight: **P0** crate split (daemon stops depending on the CLI crate)
Ahead: **P3** derivation, then **P4 · P5 · P6** · `--quick` bench once, at the very end

`318dd5281` · 11574 tests / 0 fail · arch-gate clean · 2026-09-02

## Watch

- The stop rule is **unmeasured** — and it is the link marked ✗ above.
- `resolution.rs` is at 5217 lines against a 5223 ceiling.
- P0.4b — no daemon route reaches `enrich_now` for an installed recipe corpus.

---

*Hand-maintained; update it in the merge commit when a link or a phase moves.
The chain is the point of this file: phase progress can look healthy while every
link stays unproven, which is the failure this board exists to make visible.*
