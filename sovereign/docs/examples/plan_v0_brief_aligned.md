# Claude Code plugin — v0 plan (aligned rewrite, 2026-05-08)

> Retroactive rewrite of the original v0 plan, restructured around
> the four alignment questions. Substance preserved; structure
> hardened. Companion case study at
> `docs/PLAN_ALIGNMENT.md` in the sovereign repo.

## Context

The design doc proposes a thin Claude Code plugin that surfaces the
sovereign daemon's two-stream atlas (narrative + structural) to a
developer session: a working-set brief at session start, mid-session
atlas-query tools, and end-of-session reflection capture.

**v0 is the smallest slice that proves the loop on this repo and
closes the feedback cycle on day one.** Three exploration findings
reframe scope substantially:

1. **The hook seam already exists.** `.claude/settings.json` already
   wires `UserPromptSubmit → inject-notes.sh`. We don't need a TS
   plugin file for v0 — we replace the hook.
2. **Auth is loopback-trust by design.** No OIDC needed; v0 is
   purely loopback.
3. **The CLI binary is self-contained.** Only the brief endpoint
   would be net-new daemon work; the other 5 design endpoints are
   v1+. And the CLI alone is enough — the hook can shell out to it.
   *(See "Could this be done with less?" — this is where v0 is
   smaller than the design doc implies.)*

User decisions on scope: **v0 = brief + reflection capture.
Replace `inject-notes.sh`** (one canonical injection).

## What this extends

| Existing | Reuse for |
|---|---|
| `corpus-engine::git_archaeology::batch_harvest_all_commits` | "Recent activity" section |
| `corpus-engine::git_archaeology::source_to_repo_relative` | Lifting chunk file_paths into repo-relative form |
| `corpus-engine::enrichment::atlas::read_atlas_atoms` | Loading narrative + structural atoms |
| `corpus-engine::NoteStore::read_notes_scoped` | "Stated about this area" — replaces what inject-notes.sh does |
| `sovereign-tools::knowledge_view::tokens::estimate_tokens` | Token budgeting |
| `sovereign-tools::knowledge_view::digest::format_landscape` | Section + per-bullet budget pattern (fork shape) |
| `sovereign-mesh::commit_harvest::read_commits_between` | Per-branch commit walk |
| `.claude/hooks/inject-notes.sh` | Reference shape for `inject-brief.sh` |
| Existing `.claude/settings.json` `UserPromptSubmit` hook wiring | Where the brief gets injected |

Critical paths to read before writing:
- `.claude/hooks/inject-notes.sh` — the seam we're replacing.
- `crates/sovereign-tools/src/knowledge_view/digest.rs:38-125` — fork target.
- `corpus-engine/src/git_archaeology.rs` — pattern for the new module's tests + git subprocess idiom.

## What this removes

- **Unwiring `inject-notes.sh`** from `.claude/settings.json`'s
  `UserPromptSubmit` block. The script file itself is **kept** as
  historical reference and as a fallback during the brief's
  bake-in period; it's the wiring that goes.

Explicitly NOT removed:
- The `read_notes` MCP tool — still used by other surfaces and by
  the brief's own notes-section query.
- `KnowledgeViewManager` and its digest renderer — the brief
  forks the *pattern*, not the code. Both keep their existing
  callers.

## Restraint patterns

Principles + inquiries that govern the touched files:

- **§3.1 File-size ceilings** — both new files target <500 lines.
  `brief.rs` aims for ~400 (renderer + 4 sections + tests);
  `working_set.rs` aims for ~250 (3 strategies + tests).
- **§3.2 Single concern per file** — `working_set` is *only*
  detection; `brief` is *only* rendering. Token estimation stays
  in `knowledge_view::tokens` (don't fork it; reuse).
- **§5.3 Don't widen single-method traits** — we don't introduce
  any new trait. `assemble_brief` is a free function.
- **§10.2 Touch one dimension at a time** — this PR adds the
  brief assembler. No collateral refactors of `knowledge_view`
  or `git_archaeology`.
- **§14.2 Notes at the moment of decision** — the brief itself
  surfaces decision/invariant notes; the reflection capture
  writes them. The principle is being operationalized, not just
  honored.
- **archaeology-eval baseline** — currently 8/8 inquiries
  passing. The PR must not regress this. (Pre-push ratchet
  enforces.)

## Could this be done with less?

Yes — three minimizations identified at plan time:

1. **Drop the daemon HTTP endpoint.** The original design called
   for `POST /v1/brief/working_set`. The CLI binary
   (`sovereign code brief`) is self-contained: it opens the
   NoteStore, walks git, reads the atlas. The hook can shell out
   directly. **Saves: 1 new module (`brief_http.rs`, ~150 lines),
   1 daemon `install_*_router` method, NoteStore wiring through
   the daemon's state.** Re-add in v0.5 if a real cross-process
   use case emerges.

2. **Defer the inquiry coverage.** The original plan included
   `inquiries/principle_brief_witness.toml` as v0 deliverable. It's
   useful but not on the critical path — the eval framework is
   already a regression gate via the baseline. *Keep* if it's a
   30-minute add; defer if it grows.

3. **Skip "Explicit gaps" section in v0 brief.** The plan listed
   5 sections; cross-corpus-edges-based gaps require an
   atlas-cross-corpus pass that most repos won't have run. Ship
   4 sections; add gaps only when the cross-corpus pass is
   routine.

Minimum viable v0: working_set module + brief module + CLI +
UserPromptSubmit hook + Stop hook + snapshot fixture. **Estimate
after minimization: ~1.5 weeks (was ~2 weeks).** Implementation
result: shipped in one session.

---

# Implementation

## Architecture

### Module layout

**New files:**

- `crates/sovereign-tools/src/code/working_set.rs` (~280 lines)
  - `Strategy { BranchDiff, RecentCommits, Explicit }` enum
  - `detect_working_set(repo_root, strategy) -> Vec<PathBuf>`
  - Default: BranchDiff vs `main`/`master` (auto-resolved via
    `git symbolic-ref refs/remotes/origin/HEAD`).

- `crates/sovereign-tools/src/code/brief.rs` (~430 lines)
  - `assemble_brief(BriefInputs, &NoteStore) -> Result<String>`
  - Sections (per-section AND per-bullet budget-checked):
    1. Working set
    2. Stated about this area (notes)
    3. Structurally observed (atoms via archaeology sidecar)
    4. Recent activity (commits in last 7 days)
  - Token estimation via `knowledge_view::tokens::estimate_tokens`.

- `.claude/hooks/inject-brief.sh` (~50 lines)
  - Discovers repo root, runs `sovereign code brief`, fails
    silently. Honors `SOVEREIGN_NO_BRIEF=1`.

- `.claude/hooks/capture-reflection.sh` (~40 lines)
  - Stop event. Auto-captures session metadata (branch + diff +
    recent commits) via `sovereign code reflect`. Non-interactive
    (no TTY required). Honors `SOVEREIGN_NO_REFLECTION=1`.

**Modified files:**

- `crates/sovereign-tools/src/code/mod.rs` — `pub mod brief; pub mod working_set;`
- `crates/sovereign-cli/src/code_cmd.rs` — new `brief` subcommand.
- `.claude/settings.json` — replace UserPromptSubmit wiring,
  add Stop hook.

## Verification

End-to-end gates, in execution order:

1. **Unit tests** — both new modules ship with scripted git
   fixtures. `working_set::detect_working_set` per strategy;
   `brief::assemble_brief` with synthetic atoms + notes.
2. **CLI smoke** — `sovereign code brief --strategy branch` from
   inside this repo outputs a coherent markdown brief.
3. **Hook smoke** — flip settings.json, fire `inject-brief.sh`,
   confirm output appears under expected sections.
4. **Reflection smoke** — `sovereign code reflect` writes a
   reflection note via NoteStore::write_reflection_scoped;
   confirm it surfaces in the next brief.
5. **Per-prompt latency** — < 500ms p99 across 10 fires.
6. **Snapshot fixtures** (added during build) — 5 scenarios
   under `tests/snapshots/` regression-tested via
   `cargo test brief_fixtures`.

## Open implementation questions (decide during build)

- **Default branch detection edge cases** — repos without
  `origin/HEAD` set; fall back to `main`, `master`, then error.
- **Reflection prompt UX** — non-interactive auto-capture is the
  v0 shape; explicit-prompt mode (slash command) deferred to
  later if the auto-capture proves too coarse.

## Out of scope — flag explicitly

- Daemon HTTP endpoint — see "Could this be done with less?" §1.
- TS plugin file — v1.
- Mid-session atlas-query tools (lineage, related sessions,
  invariants check, NL atom search) — v1.
- OIDC / multi-tenant auth — v2.
- Cross-repo working sets — v1.5.
- Brief caching layer — v0.5 if latency proves painful.
- Person-Knowledge Locus brief section — v2 (depends on v2
  archaeology atom-type design).
