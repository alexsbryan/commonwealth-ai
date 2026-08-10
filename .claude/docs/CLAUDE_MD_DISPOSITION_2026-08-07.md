# Disposition ledger — CLAUDE.md consolidation (order claude-md-slim, 2026-08-07)

Every H2/H3 section of `.claude/CLAUDE.md` as of main tip `31435b6b`
(56,479 chars, ~14.5k tokens), accounted for below. Nothing leaves the
file without a row. Line numbers reference the pre-restructure file at
that commit.

**The razor (from the order):** would a spawned worker executing a work
order need this in-context to avoid breaking something (build gates,
tool discipline, coordination, the ten)? Keep in core. Main-session
lifecycle/ritual? Move to `.claude/docs/MAIN_SESSION_PROTOCOL.md`, with
a pointer where discovery matters.

**Dependency search (order Budget clause), run before any edit:**
`grep -rn "CLAUDE.md|CLAUDE_MD"` over `.claude/hooks/`, `scripts/`,
`.githooks/` returns exactly three hits, all prose comments, none
parsing content: `hooks/session-boot.sh:179`,
`scripts/sovereign-lint.sh:56`, `scripts/desktop-soak.py:183`. A second
pass for section-title strings ("architectural compass", "Which door to
open", "smell table", "Session start — do these") over the same set
plus `.claude/settings.json` returns zero hits. No hook or script
depends on CLAUDE.md's internal structure; restructuring is safe.

**Movement rule:** moved content relocates verbatim (assembled by line
range from the original, byte-for-byte). The only rewording anywhere is
pointer maintenance on cross-references whose target moved, each
ledgered in the "Pointer edits" table below.

## Dispositions

Destinations: **core** = `.claude/CLAUDE.md` (slim), **doc** =
`.claude/docs/MAIN_SESSION_PROTOCOL.md` (new).

| # | Section (old lines) | Disposition | Reason |
|---|---|---|---|
| 1 | Untitled preamble (1–7) | split: para 1 (l.1) kept; paras 2–3 (l.4–7) → doc §"Original preamble" | Para 1 is the persona and must load per-turn. Paras 2–3 are content-duplicated by kept core: empathy/glassbox = the ten #1/#3; SYSTEM_OVERVIEW upkeep + ARCH_PRINCIPLES-as-compass = System geography + the compass itself. Moved, not deleted, for the record. |
| 2 | ## The architectural compass (10–13) | kept, one pointer edit | The operating instruction that binds the ten/smells/door. Edit: "the four commitments they descend from" now cites `sovereign/ARCH_PRINCIPLES.md §0` since row 4 deletes the local copy. |
| 3 | ### The ten (14–30) | kept verbatim | Order-frozen: moves nowhere, changes not one word. |
| 4 | ### The four commitments (31–39) | **deleted** | Verbatim duplicate of `sovereign/ARCH_PRINCIPLES.md §0` (verified 2026-08-07: all four commitments present at ARCH_PRINCIPLES.md lines 56–76, same text). Deletion of duplicates of a named canonical doc is the one sanctioned delete. Compass intro (row 2) points there. |
| 5 | ### The smell table (40–60) | kept verbatim | Order-frozen. |
| 6 | ### Which door to open (61–87) | kept verbatim | Order-frozen. |
| 7 | ### System geography (88–98) | kept | Guards against reading 265KB narrative docs whole — read-budget breakage prevention every agent needs, worker or main. |
| 8 | ## Reporting to the operator (99–108) | moved → doc, same heading | Report-shaping protocol, main-session-facing; workers reach it via pointer. No build/tool breakage prevented by holding it in-context. |
| 9 | ## Code Intelligence (109–158) | split: kept l.109, 119–144 (CLI binary, debug-not-release, dispatcher exec map + table, dev-tools feature trap, MCP-preferred + `sovereign tools` usage + equivalence); moved l.111–117 → doc §"MCP surface" (tool inventory, dormant build-feedback, CLI-only list, wire-is-authoritative) and l.146–157 → same (behavioural properties, rename/alias table) | Kept paras each prevent a silent breakage (stale-sibling no-op rebuild, dev-tools downgrade, wrong profile). Moved paras are inventory/reference: the moved text itself says `tools/list` is the authoritative answer, aliases still work, and dormant watchers are restated in the kept Compilation section (l.440). |
| 10 | ### Session start (159–168) | moved → doc | Main-session boot ritual; a worker boots from an order, not this checklist. |
| 11 | ### Session splitting (169–233) | moved → doc | Main-session lifecycle, order-named (splitting, frames). |
| 12 | ### Precision tools (234–241) | kept | Order-mandated (precision-tool discipline). |
| 13 | ### Read budget (242–257) | split: rules kept (l.242–254); cache-audit para (l.256) → doc §"Read budget — cache-audit telemetry" | The three DO-NOTs + batching are order-mandated discipline. cache-audit is session telemetry, order-named as lifecycle. |
| 14 | ### Delegation (258–315) | moved → doc | Order-named (delegation authorization). Discovery matters — the failure mode is a silent default — so the core pointer states the standing authorization and cap explicitly. |
| 15 | ### When to use which tool (316–346) | kept, one pointer edit | Order-mandated (the MCP tool table). Edit: the bench row's `see "Measuring quality" below` retargeted to the doc (row 22). |
| 16 | ### Coordination — work atlas (347–380) | split: basics kept (l.347–378); privacy para (l.379) → doc §"Work atlas — privacy" | Order mandates coordination basics (read surface, query, declare, release). Privacy toggling is per-node ops config. |
| 17 | ### Mandatory pre-flight checks (381–391) | kept | "Hard to undo when skipped" — exactly the worker breakage class the razor keeps. |
| 18 | ### Writing notes (392–402) | moved → doc | Memory-writing protocol, order-named. Core pointer carries the two triggers that gate commits: note-at-decision-time and the DEFAULTS_LEDGER same-commit rule. |
| 19 | ### Session reflection (403–426) | moved → doc | Task-end ritual, order-named. Release-claims-at-end is also covered by the kept atlas basics ("When to release", l.375–377). |
| 20 | ### Drift tool feedback (427–437) | moved → doc | Order-named (drift-tool feedback). |
| 21 | ### Compilation and test feedback (438–479) | split: kept l.438–472 (watchers-off posture, gate commands, toolbox/host rule, no-bare-cargo, feature-unification + dev-release.sh, lint/test usage, exit-code rule, the three guards); moved l.474 (jobs throttling detail), l.476 (doctests-off detail), l.478 (watcher restore) → doc §"Gate details" | Kept = the order-mandated build/test gate rules. Moved paras are runtime-self-documenting (the `jobs:` line and the doctests banner announce themselves) or opt-in ops (watcher restore: "Nothing else in this file depends on that path"). |
| 22 | ### Measuring quality (480–515) | moved → doc, same heading | The trigger rule a worker must not miss lives in the kept DoD para (l.541: run `--quick`, gate on VERDICT, read lane KIND, `retrieval-prod` for retrieval). Lane taxonomy, baseline traps and the doc map are drill-down, and the kept tool table (rows at l.339–341) preserves discovery. |
| 23 | ### Definition of done (516–544) | split: kept l.516–525 (the gate: commands, exit-code rule, coverage, cost) + l.541 (bench trigger) + l.543 (contract census trigger); moved l.527–539 (scoping flags, sovereign-compute exception, adapter-log paths, runner-discrepancy note) → doc §"Definition of done — iteration detail" | Kept = order-mandated definition of done plus the two gate triggers. Moved = iteration conveniences: scoping flags are explicitly "not for the final gate", and the log-path/discrepancy notes duplicate promises the kept l.466 para already makes. |
| 24 | ### Index freshness (545–557) | moved → doc | Ops triage for a degraded daemon. Core pointer keeps the two act-now triggers: doctor-first, and never "fix" a knowledge-kind repo corpus. |
| 25 | ### When MCP tools add less value (558–561) | moved → doc | Greenfield nuance; no breakage prevented by holding it in-context. |

Tally: 25 sections — 9 kept whole, 5 kept-with-split (remainder moved),
10 moved whole, 1 deleted (cited duplicate). Every moved fragment lands
in `.claude/docs/MAIN_SESSION_PROTOCOL.md` verbatim.

## Pointer edits (the only non-verbatim touches)

| Where | Old | New |
|---|---|---|
| Core, compass intro (old l.12) | "the four commitments they descend from" | adds "(`sovereign/ARCH_PRINCIPLES.md §0`)" |
| Core, tool table bench row (old l.339) | `see "Measuring quality" below` | `see MAIN_SESSION_PROTOCOL.md §"Measuring quality"` |
| Core, DoD bench para (old l.541) | `(see "Measuring quality" above)` | `(see .claude/docs/MAIN_SESSION_PROTOCOL.md §"Measuring quality")` |
| Core, new "Moved protocol" section | — | new trigger → doc-section pointer table (new text, not a paraphrase of any moved body) |
| Doc, provenance header | — | new text: origin, order id, note that internal cross-references may point back at sections that stayed in core |

## Gate note (glassbox, committed with the plan)

The order's char gate is ≤22,000. Projected honest core is ~27–28k
chars (~7k tokens): the order's own mandated-minimum list (frozen
compass trio, build/test gate rules, precision + read-budget, atlas
basics, tool table, pointers) sums to ~21k by itself, and the razor
honestly keeps ~5–6k more of silent-breakage preventers (dispatcher
exec map, dev-tools trap, pre-flight checks, CLI binary/debug rules,
System geography). Under the order's seams the forbidden move is
paraphrase-to-shrink, and the stop condition (>10k tokens resisting)
is not met — so the restructure proceeds at the honest line and the
landing report names the resisting sections and the exact final
counts. Final measured counts land in an addendum to this ledger.

## Addendum — final measured counts (post-restructure)

- Before: `.claude/CLAUDE.md` 56,479 bytes (`wc -c`, the order's
  instrument) = 56,051 unicode chars; ~14.5k tokens per the order.
- After: core 28,907 bytes = 28,651 unicode chars (~7.2k tokens at
  4 chars/token), a 49% reduction on either instrument;
  `.claude/docs/MAIN_SESSION_PROTOCOL.md` 29,116 bytes = 28,896 chars.
- Verification: a line-coverage check against main-tip `31435b6b`
  reports zero non-blank original lines absent from core+doc, outside
  the ledgered delete (old l.31–39) and the three ledgered pointer
  edits (old l.12, l.339, l.541). All 13 §-references in the core's
  pointer table resolve to headings in the doc; all file paths
  referenced by core and doc exist on disk.
- The 22,000-char gate is not met. Sections resisting relocation
  beyond the order's own mandated minimum (~21k chars by itself):
  dispatcher exec map + dev-tools trap + debug-not-release + CLI
  binary (~3.5k — each prevents a silent build breakage), mandatory
  pre-flight checks (~1.6k — "hard to undo when skipped"), System
  geography (~0.8k — guards against 265KB narrative reads), persona
  para + compass intro (~1.1k — per-turn identity and the operating
  instruction for the frozen trio). Forcing these out by paraphrase
  is off-order; forcing them out by pointer would relocate exactly
  the silent-failure guards whose failure mode is not knowing to
  open the door.
