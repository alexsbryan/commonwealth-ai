# ralph — the five-programs campaign, one unit per session

You are a worker executing ONE unit of the `five-programs` campaign
(docs/FIVE_PROGRAMS.md; the row catalog is
docs/FIVE_PROGRAMS_DECISIONS.tsv). A fresh session starts every iteration:
this file, `ralph/next/five-programs/STATE.md` and the repo are your whole
memory. Do exactly what your unit's row says. Do not design anything — every
design decision already lives in FIVE_PROGRAMS §12 and the files the row
points at. When a row and the tree disagree, you stop (§6); you never
improvise around it.

The campaign's one rule is also yours: **strictly necessary.** Change nothing
the row does not name — no renames, no nearby cleanup, no new abstraction, no
comment beyond the one the row asks for (ARCH principle 2).

## The loop — the operator's cadence, verbatim (2026-09-23)

```
until boundary-gate exits 0 {
    run the gate;                    # toolbox + cargo lock; group BY TARGET
    identify gaps;                   # first dep-ready row; scout before cutting
    close gaps;                      # cutters never cargo/git; you hold the compile
    if (closures_since_last_full_check % 5 == 0) {
        run sovereign-test.sh AND sovereign-lint.sh --full;   # both green
    } else {
        all-targets build only;      # full resolved feature set
    }
}
```

- The counter lives in STATE.md's header ("closures since last full
  check"). After YOUR row closes net-decreasing: increment it; if it
  reaches 5, run the full test + lint pair, repair anything red, then reset
  it to 0. A rolled-back or delta-0 row does NOT count (fp-29's refusal is
  the precedent — net +1 as spec'd means refused, not counted).
- A red test at a check point is repaired IN THE SAME STEP, not queued:
  classify drift-vs-regression FIRST (read what the census pins, find the
  commit that moved the code), then fix in the correct direction. A
  census's failure message is an argument, not an order — fp-53 refused
  chunk_provenance's own "delete from MANUFACTURED" advice because the
  producer was alive and the scan was broken. Faking a test, weakening a
  census, or fixing the test instead of the tree is a fake zero and halts
  for the operator.
- Build-only between checks still means the FULL all-targets build with the
  repo's resolved feature contract — through the scripts, never bare cargo.

## Standing rules — the whole loop obeys these

- **One cargo worker.** Every cargo call goes through
  `scripts/with-cargo-lock.sh` inside `toolbox run -c sovereign-vulkan`.
  Cutters never run cargo or git; the session holds the single compile and
  every commit.
- **Net-decreasing only.** A move that adds a red edge elsewhere is rolled
  back and recorded, not kept.
- **No fake zeros.** Never promote a program-owned store to a leaf; never
  re-home a crate to hide an edge (§11's daemon correction). A
  `[[package_leaf]]` admission is an OPERATOR decision requiring all three:
  the named refusal of each existing home, the two programs that share the
  vocabulary, and the leaf count shown in the burn-down. The N+1 ladder is
  FIVE_PROGRAMS §12 decision 3a — first match wins.
- **All tests green is standing** (operator, 2026-09-23: "doesn't matter
  who caused them"). Repairing them is loop work, not a detour.
- **Commit as you go; NOTHING IS EVER PUSHED** — push is the operator's.
- **DONE** is written only when the gate exits 0 AND §11's three finish
  conditions hold. Never for anything less.

## 0. Standing facts (do not re-derive)

- Branch `cut`, tag `pre-cut` = 6bda3417a. NOTHING IS EVER PUSHED — push is
  the operator's call.
- `cargo xtask boundary-gate` (run from `corpus-engine/`) is the only
  scoreboard: EXIT=1 with `N violation(s)`. The raw count goes in EVERY
  commit body. `layer-gate` must stay ✓.
- The atlas carve (Phase A) has LANDED: the
  `corpus-engine-atlas-reader` leaf holds the whole resolved-atlas READ
  surface (projection, citation, context_filter, section_cache,
  evidence_site, axis_catalog, ann_store port, store format+read half,
  summary derivation+cache key, question_kind+linalg, ground cluster,
  context read side, provider trait + atom-class graph provider, inventory,
  raptor_read, raw readers). corpus-engine keeps writers, backfill,
  freshness deciders, the class-composite opener, the chapters manifest and
  `read_section_rows`. Consumers' paths are NOT yet repointed — that is
  deliberate: path repoints alone close ZERO gate edges (the 8 consumer
  crates keep non-atlas residue), so each crate's repoint is folded into the
  row that closes ITS last corpus-engine use. Do not repoint early.
- 331 of the ~920 old consumer enrichment refs already resolve into
  `understanding_vocab` (atoms/edges/stable_key/read fns/ATLAS_DIRNAME/
  skeleton) — repointing those is a path rewrite, part of the same
  fold-into-closure rows.
- Builds on this host MUST go through the toolbox:
  `toolbox run -c sovereign-vulkan bash -lc '...'` (native host builds die
  on llama-cpp-sys-4). Compile is the gate: scoped
  `./scripts/sovereign-lint.sh --human` per step, `--full` before you
  declare a unit done. The test/lint cadence is the LOOP's, not per-step —
  see "The loop" above: every 5th net-decreasing closure runs
  `sovereign-test.sh` + `sovereign-lint.sh --full`, both green, and the
  all-tests-green invariant is standing.
- A `[[package_leaf]]` must NOT also be a package member — check which list
  a crate line lives in (layer vs package) in quality/ARCH_LAYERS.toml.
- `sovereign-contracts` (layer 0) may not name understanding-vocab or
  corpus-index (layer 1): layer-gate blocks it.
- After a crate fork expect ~2 lint rounds (`sovereign_daemon::`→`crate::`;
  sibling paths→`super::`). After moving a module into the leaf, keep every
  pub item reachable at its historical path (a re-export, never a twin —
  ARCH §10.6). Test-module child decls under a `#[path = "..."]`-loaded file
  need explicit `#[path]` themselves (the gossip_integration precedent).
- Every file the arch-gate flags is SPLIT, never re-pinned; never
  `--update-baseline` on a dirty tree.

## 1. Pick your unit

1. Open `ralph/next/five-programs/STATE.md`. If a row is `[~]`, that is your
   unit — a previous session was killed mid-row. Continue it.
2. Otherwise your unit is the FIRST `[ ]` row whose `depends` are all `[x]`.
3. If the loop's note names your unit (`Your unit: <id>`), open only that row.
4. If the loop tells you the tree holds uncommitted work, it belongs to the
   `[~]` unit: read `git status` + `git diff`, keep what is right, continue.

## 2. Reading a row

`- [ ] <id> — depends [<ids>] — <VERB> <what> — read: <pointers> — check: <checks>`

- **read:** the only files you read besides the ones you edit. `TSV` means
  docs/FIVE_PROGRAMS_DECISIONS.tsv (the 79-row catalog; find your row by its
  source→target pair). `§12` is docs/FIVE_PROGRAMS.md §12 (the six decisions
  — cite the decision number in your commit body). `§11` is the seam/probe
  history: a probe marked REFUSED there is dead, do not re-probe it.
- **check:** what proves the unit done — usually
  `toolbox run -c sovereign-vulkan bash -lc 'cd corpus-engine && cargo
  xtask boundary-gate'` reporting a LOWER count, plus scoped lint exit 0.

## 3. Doing the unit

- One dimension per move (ARCH 2). Commit when the unit's check passes —
  commit message: what landed + WHY one line + the gate's raw count. Docs
  that became true land in the same commit (§11 lines, SYSTEM_OVERVIEW rows).
- Cutter discipline does not apply (you are one agent), but the compile
  discipline does: scoped lint after every file move, never leave the tree
  red at the end of the session — if you must stop, `[~]` the row and leave
  a NOTE in the row text saying exactly what state the tree is in.
- When a row closes an edge, the consumer's Cargo.toml line disappears in
  the same commit; if the consumer still has corpus-engine uses, the row is
  not done — say so in the row and leave it `[~]`.

## 4. Refusing

Cutters refuse rather than force; so do you. A row whose premise the tree
contradicts, whose decision turns out to be missing, or whose check cannot
be run honestly → write the refusal into the row text (one line: what you
found, file:line), mark the row BLOCKED, and stop this session with a
commit if anything landed. Refusals are copied into FIVE_PROGRAMS §11 by the
director, not by you.

## 5. Done

When the gate reports EXIT=0 and every row is `[x]` or BLOCKED-with-reason:
write `ralph/DONE` (one line: the final count and the finish conditions from
FIVE_PROGRAMS §11) and stop. Never write ralph/DONE for anything less.
