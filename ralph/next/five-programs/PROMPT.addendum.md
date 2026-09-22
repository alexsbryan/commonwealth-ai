<!-- five-programs' differences from ralph/PROMPT.base.md. The campaign is
     docs/FIVE_PROGRAMS.md; the row catalog is
     docs/FIVE_PROGRAMS_DECISIONS.tsv (79 rows); the six decisions are §12 —
     every queue row carries its decision number, so the worker never designs. -->
<!-- section: vars -->
prefix = fp
<!-- section: intro -->
# ralph — the {{queue}} queue, one unit per session

You are a worker executing ONE unit of the `{{queue}}` campaign
(docs/FIVE_PROGRAMS.md; the row catalog is
docs/FIVE_PROGRAMS_DECISIONS.tsv — find your row by its source→target pair).
Every design decision already lives in FIVE_PROGRAMS §12 (six decisions, all
taken — cite the decision number in your commit body) and §11 (the seam and
refusal history: a probe marked REFUSED there is dead — do not re-probe it).
A fresh session starts every iteration: this file, `{{state}}` and the repo
are your whole memory. Any other STATE.md under `ralph/` is ANOTHER queue —
never open it, never mark it. When a row and the tree disagree, you stop (§6);
you never improvise around it.

<!-- section: facts after=intro -->
## 0. Standing facts (do not re-derive)

- Branch `cut`, tag `pre-cut` = 6bda3417a. NOTHING IS EVER PUSHED — push is
  the operator's call.
- `cargo xtask boundary-gate` (from `corpus-engine/`) is the only scoreboard:
  EXIT=1 with `N violation(s)`. The raw count goes in EVERY commit body.
  `layer-gate` stays ✓.
- The atlas carve (Phase A) has LANDED: the `corpus-engine-atlas-reader` leaf
  holds the whole resolved-atlas READ surface; corpus-engine keeps writers,
  backfill, freshness deciders, the class-composite opener, the chapters
  manifest and `read_section_rows`. Consumers' paths are NOT yet repointed —
  deliberately: path repoints alone close ZERO gate edges (the 8 consumer
  crates keep non-atlas residue), so each crate's repoint is folded into the
  row that closes ITS last corpus-engine use (fp-14). Do not repoint early.
- ~331 old consumer enrichment refs already resolve into
  `understanding_vocab` (atoms/edges/stable_key/read fns/ATLAS_DIRNAME/
  skeleton) — repointing those is a path rewrite, part of fp-14's folds.
- Builds on this host MUST go through the toolbox:
  `toolbox run -c sovereign-vulkan bash -lc '...'` (native host builds die on
  llama-cpp-sys-4). Compile is the gate: scoped
  `./scripts/sovereign-lint.sh --human` per step, `--full` when a unit
  touches a shared crate. `sovereign-test.sh` only when a row touches test
  wiring.
- A `[[package_leaf]]` must NOT also be a package member — check which list a
  crate line lives in (layer vs package) in `quality/ARCH_LAYERS.toml`.
  `sovereign-contracts` (layer 0) may not name understanding-vocab or
  corpus-index (layer 1): layer-gate blocks it.
- After a crate fork expect ~2 lint rounds (`sovereign_daemon::` → `crate::`;
  sibling paths → `super::`). A moved module keeps every pub item reachable
  at its historical path (a re-export, never a twin — ARCH §10.6). Child
  `mod` decls under a `#[path = "..."]`-loaded file need explicit `#[path]`
  themselves (the tests/main/gossip_integration precedent).
- Every file the arch-gate flags is SPLIT, never re-pinned; never
  `--update-baseline` on a dirty tree.

<!-- section: prefixes -->
| prefix | what you do |
|---|---|
| `fp-` | build the unit (§3) |
| `HUMAN-fp-` | never do it and never mark it: write `{{control_dir}}/NEEDS_HUMAN.md` (§6) from the row, then stop |

<!-- section: checks-queue-1 -->
| BOUNDARY | `scripts/ralph-check.sh boundary` — the burn-down count (EXIT=1 is its honest state; the COUNT is what your commit body quotes) | prints `N violation(s)` |
| LAYER | `scripts/ralph-check.sh layer` (builtin) | exit=0 |
| COMPILE | `scripts/ralph-check.sh compile` — the toolbox-wrapped scoped compile; on this host it is the only honest LINT for anything that reaches llama-cpp-sys-4 | exit=0 |
| ARCH | `scripts/ralph-check.sh arch` (builtin; rides on LINT in the base) | exit=0 |
<!-- section: hard-rules-scope -->
- Never edit `sovereign/ARCH_PRINCIPLES.md`, `AGENTS.md`, `.claude/`, or
  `scripts/ralph*`. Never stop or restart the DEPLOYED daemon (the one
  `svrn daemon status` names). Never touch `ralph/STOP`,
  `ralph/NEEDS_HUMAN.md`, or another queue's directory under `ralph/next/`.
- **Behaviour-preserving or reported.** This campaign re-cuts the repo; a
  route, tool or read that worked must still work. When a row dials a
  capability away from the daemon, the stub REPORTS ABSENCE (§12 decision 2,
  principle 6) — never a silent fallback, never a 404 where a named absence
  belongs. A row that would change the TEXT of an answer or a default
  without its decision saying so is §6.
- A `[[package_leaf]]` admission must be one the queue row names exactly; the
  leaf test is: no fs, no store, workspace deps ⊆ the leaf's allow-list.
