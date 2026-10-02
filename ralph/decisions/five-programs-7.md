<!-- ledger -->

**five-programs-7 · 2026-09-23 · fp-44 (second halt) · director** — this commit
- Needed: the rescoped fp-44 mandates constructing `RingRail`, which exists only in `commonwealth-rail` (the journal half; rail-core performs no I/O by its own Cargo.toml oath), and `[[forbid]] commonwealth-rails -> commonwealth-*` (quality/ARCH_LAYERS.toml:680) excepts only {core, transport, media, discovery} — the worker's rules (PROMPT.md:217-218) forbid it to widen an except list the row does not name, so it halted with the census and no code touched.
- Chose: the package's option 1, re-mint sub-path (the fp-40/fp-22 pattern) — rewrite fp-44 to NAME that forbid row and mandate the except widening as STEP 0, landing in fp-44's own commit together with the doors and a refreshed reason text. The director does not edit ARCH_LAYERS.toml ahead of the code: an except entry with no consumer edge is dead config, and both campaign precedents landed their seam exceptions with the rows that needed them.
- Because: the false-premise clause is the director's (the row's mandate collided with a manifest row it failed to name — the same class as the 2026-09-22 lesson), and the operator reserve ("adding an `[[exception]]` row") covers the grandfathered-violation burn-down list, not the design-intent `except` on a `[[forbid]]` row, which queue rows have already widened twice (fp-40 at cd0ee7834's parent; fp-22's exceptions at ARCH_LAYERS.toml:704). Once the row names the forbid row, the widening is a queue row explicitly naming its manifest growth — the charter's own grammar for what stays in-lane.

<!-- appendix -->

## five-programs-7 · 2026-09-23 — fp-44 names the rails→commonwealth-* forbid row; except widening rides fp-44's commit (option 1, re-mint)

<details><summary>reasoning, evidence, package</summary>

Census reproduced line by line before deciding (package §(b)):
- `grep -rn "pub struct RingRail" commonwealth/crates/*/src/*.rs` → commonwealth-rail/src/lib.rs:105, only definition. rail-core's Cargo.toml: "this crate performs NO I/O ... the caller that read them off a disk is `commonwealth-rail`" — a rail-core-only append door cannot durably assign seq/signature/timestamp/id, which is the wire contract this row pins (TSV behaviour_delta = none).
- quality/ARCH_LAYERS.toml:680-686: from = "commonwealth-rails", to = "commonwealth-*", except = [core, transport, media, discovery]. `commonwealth-rail` absent; the glob also catches a direct `commonwealth-rail-core` dep. forbidden_by (quality/arch-layers/src/lib.rs:270-276) checks forbids first and outranks every allowance — no relief short of the except list.
- Closure cost, reproduced by the director: `comm -13` over the two `cargo tree --edges normal` closures → exactly commonwealth-rail, commonwealth-rail-core, oplog join cw-rails' closure. One manifest line; rail re-exports the fold wholesale (lib.rs:29), so no door code names rail-core directly. (Absolute closure counts differ between the worker's run and the director's host run; the delta is the load-bearing number and reproduces exactly.)
- Precedent: PROMPT.md:217-218 ("never ... widen an `except` list unless the row names that exact row") + fp-40 ("the [[forbid]] now carries its seam exception") + fp-22's exceptions standing in the TOML at :704 with the row id in the reason text.

Refused alternatives (package §(c), with the worker's reasoning accepted):
- Doors over rail-core only: breaks the pinned wire contract and leaves fp-54's client dialing a different surface than the daemon's doors.
- Serve-half inside commonwealth-rail (axum feature): still needs the same widening, and breaks that crate's recorded "DELIBERATE: four dependencies, and the shape is the point" (Cargo.toml:7).

Falsified if: fp-44's worker finds the doors need MORE than {commonwealth-rail, commonwealth-rail-core, oplog} from the commonwealth-* side (the reason text's closure delta would be wrong — STOP and repackage); or layer-gate reds on an edge the widened except does not cover (same).

</details>
