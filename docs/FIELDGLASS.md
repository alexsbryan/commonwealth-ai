# Fieldglass — see the architecture, judge it yourself

**Date:** 2026-08-06
**Status:** shipped (P1 + P2 agent heat / churn / delta). Render it: `svrn code fieldglass [corpus-id] --open`
**Companion:** `docs/COMAINTAINER.md` — the judgment-side complement. The
comaintainer reads landings; Fieldglass renders the field.

**One line.** A deterministic, self-contained page that projects the
codebase's structure and history into pictures where architectural
pathologies have unmistakable shapes — so an architect-level engineer
supervising a swarm of agents (or fifteen juniors) can judge health at a
glance, the way a veteran once judged a file "badly written" because at a
glance it looked too symmetrical.

## The user

The one senior engineer responsible for a codebase that grows faster than
anyone can read it. At ~12.7 commits/day of agent output, diff review does
not scale — and the failure mode isn't a bad commit, it's accretion: every
landing looks fine; the fortieth one makes a tollbooth, a stapled-together
trait, a copy-paste cradle. **Rot is invisible in diffs and visible in
fields.**

## The design law

**Render evidence, not verdicts.** A score ("SOLID: 7/10") is someone
presenting judgment — Goodhart-able, argument-from-authority, and it teaches
nothing. Fieldglass instead picks, for each pathology, a projection in which
the violation is a *structural property of the picture*: a block pattern, an
against-the-grain arrow, a bridge node, a cat's cradle. Shapes are evidence;
the architect's visual cortex stays the judge. Because the page never claims
green, it can never be falsely green (ARCH_PRINCIPLES §18, satisfied by
construction). It gates nothing, ever — gates stay code
(`cargo xtask quality`, the bench lanes).

The second law: **layout stability.** Gestalt perception is
delta-from-familiar — the morning glance works because yesterday's shape is
remembered. So crates sit in fixed (layer, name) order, files in path order,
via an order-preserving strip treemap that is a pure function of its inputs
(no force layout, no RNG, no clock in geometry). Growth reads as growth, not
as motion.

## How to read the page

**Start here — the attention queue.** The top of the page lists the evidence
sorted by magnitude: duplication clusters by redundant lines, bridge files by
caller split, offenders by size, hidden co-change by strength. This is
curation, not judgment — nothing is scored, it is ordered by size so a human
reads the biggest evidence first. Click a row to light its files up on the
field; click again to clear.

**The field is the one view (P4).** The treemap is the page; layer flow and
the trait matrices are drill-downs that open from evidence on it and close
away. With every lens off the field still paints the STRONGEST evidence:
any layer violation as a red arrow between crate regions (a magenta arrow =
forbidden edge), the largest clone families, the strongest ghost couplings,
a blue ▦ on every file defining a matrixed trait, and the red >1200-line
rings. The legend states exactly what the default hides; lenses reveal the
rest (all arcs, all ghosts, re-export-hidden deps, SRP communities — the
SRP lens colors only the 12 largest of ~92, because 92 at once is confetti).

**Drill-throughs.** Click a violation arrow → the layer-flow drill-down.
Click a ▦ → that trait's seriated matrix. Hover a matrix cell → the caller
files that bind that crate to that method; click the cell → the field scopes
and zooms to those call sites. Esc (or the scope pill's [clear]) returns to
the whole field.

## How to read each panel

| Panel | The question your eye answers | Healthy | Diseased |
|---|---|---|---|
| **Treemap base** | Where is the mass and the oversize? | calm small leaves | red-ringed >1200-line offenders (§3.1) clustering somewhere |
| **Layer flow** (DIP; drill-down — violations paint on the field) | Does dependency flow respect `quality/ARCH_LAYERS.toml`? | laminar downward flow, no arrows on the field | red upward arrows; magenta forbidden edges; dashed amber re-export-hidden coupling no Cargo.toml admits to |
| **Trait matrices** (ISP; drill-down — ▦ marks on the field) | Is this one interface or two stapled together? | dense caller×method matrix | **block-diagonal** — the blocks ARE the sub-traits waiting to be split. Rows/cols are seriated so blocks become contiguous; a cell click scopes the field to its call sites |
| **Co-change communities** (SRP) | Does this file change for more than one reason? | monochrome leaf inside one community | an orange-ringed **bridge** whose callers split across communities that never otherwise touch |
| **Duplication arcs** | Did anyone copy instead of reuse? | sparse short arcs | a cat's cradle across the map (the characteristic agent-swarm pathology — agents regenerate instead of finding the helper) |
| **Ghost edges** | Do things move together that claim independence? | dashed co-change edges coincide with structure | dashed edges crossing empty space between "unrelated" modules |
| **Agent read/write heat** | Where do the agents live — and where do they struggle? | read-heat coincides with write-heat | read-hot + edit-cold = the **comprehension tax**: load-bearing but confusing (the queue ranks these by tokens-per-edit) |
| **Churn (90d)** | Does a feature land by adding, or by re-editing the same files? | growth at the periphery | **tollbooths** — files riding a large share of all commits |
| **Since last render** | What moved since the last glance? | small deltas | new >1200-line offenders, large growths (diffed against the JSON sidecar; a missing previous render says "first render", never "no change") |
| **Honesty footer** | What can this picture not see? | — | (always read it: unattributed refs, windows, render caps, dark panels) |

**Defined but dormant — the LSP "missing pins" panel.** Impls that betray
their trait (`unimplemented!()`/`todo!()`/panicking trait methods) would
render as plugs with missing pins. Cut from this workspace's page on
measurement (2026-08-06): zero such sites exist outside test code — a
codified norm forbids stubs (`corpus-engine/src/enrichment/domain_registry.rs`).
Other repos pointing Fieldglass at themselves may want it; the projection is
specified here so it isn't reinvented.

## What feeds each panel (all existing subsystems — readers, not writers)

- Structure: `build_arch_report` (the same builder `svrn code arch-report`
  and the MCP tool use) — computed fresh in-process, so a stale render state
  cannot exist.
- Layer verdicts: `arch_layers::parse`/`evaluate` — the same
  parser/evaluator as the xtask layer gate. One decider (§10.6).
- Duplication: `sovereign_tools::code::dry_report` — the shipped clone
  detector (exact tier from SCIP+source; near tier cosine ≥0.95 over the
  code-chunk embeddings).
- Co-change: `git_archaeology::batch_harvest_all_commits` +
  `compute_co_evolution` (548-day window; jaccard ≥0.5, ≥5 joint commits),
  union-found into communities.
- Trait matrices: the SCIP `refs` table, keyed on the qualified-name
  descriptor grammar (`…/Trait#method().`). Deliberately NOT keyed on the
  DB's `kind`/`ref_kind` columns — both are junk (rust-analyzer barely
  populates SCIP Kind; the exporter hardcodes `ref_kind='direct'`).

Output: `~/.sovereign/arch/<corpus>/fieldglass.html` + a `.json` sidecar
(the future delta layer diffs against it). Flags: `--out`, `--json`,
`--open`, `--root`, `--no-git` (skip SRP/ghosts), `--no-dup` (skip the
slowest stage — the near-clone pass is O(n²) over ~27k symbol embeddings:
~4 min idle, ~9 min observed while competing with a resident model for
cores; it announces itself and prints progress every 10% so silence is
never ambiguous).

## Input freshness — how the picture stays honest during normal operations

The page is only as good as its inputs, and the inputs sit on three
different cadences:

| Input | Feeds | Maintained by | Failure mode |
|---|---|---|---|
| SCIP graph (`scip_graph.db`) | treemap fan-in, layer flow, ISP matrices, bridge scores | the daemon Reindexer — **only if the project is registered** (`svrn project list`; `svrn doctor`'s `watcher_freshness` check; manual nudge `svrn project refresh`) | index silently lags HEAD; structure panels describe an old commit |
| Chunk embeddings (`chunks.lance`) | duplication NEAR tier only | `svrn code index` (manual / incremental) | near-clone arcs describe a days-old codebase while the exact tier is fresh — the two tiers skew apart |
| git + working tree | SRP communities, ghost edges, churn/tollbooths, treemap mass, exact clones | nothing to maintain — read live at render time | none |
| Session transcripts (`~/.claude/projects/<dir>/*.jsonl`) | agent read/write heat, comprehension tax | Claude Code writes them; parsed by `sovereign cache-audit --by-file` (the one transcript decider, shelled) | transcripts pruned/moved → panel dark, said in the footer; only file-path tool calls count, so hook-injected context never pollutes the map |

Fieldglass does not try to fix these cadences; it makes the lag
**impossible to miss** (§18.4 — validate the instrument): the honesty footer
states the SCIP-indexed commit, how many commits HEAD is ahead, and the
embedding-index age; the header grows a red `STALE INPUTS` badge when
structure lags HEAD at all or the embeddings are >7 days old; the same
warnings print to the terminal and to `tracing`. An unknown lag renders as
"unknown", never as fresh. The first live render demonstrated why this
exists: the SCIP index was hours old but 4 commits behind HEAD, and the
embedding index was 12.8 days old — both previously invisible.

Operationally: keep the repo registered so the Reindexer owns SCIP
freshness, and refresh embeddings when the badge says so. `svrn posture` is
the aggregate staleness table across all quality subsystems.

## The ritual

One glance a morning, thirty seconds. The `/fieldglass` skill
(`.claude/skills/fieldglass/SKILL.md`) is the invocation: it runs the render,
opens the page, and relays only what moved since the last glance — read from
the JSON sidecar's `delta`, `honesty`, and `attention` fields. Deltas in the
relay, shapes on the page. The render replaces its own baseline (the delta is
computed against the sidecar it overwrites), so the ritual is one render per
glance, not render-until-satisfied. Scheduled runs stay out until a week of
reviewed renders earns them (recorded house decision, same as fleet-report).

## What it is not

No scores. No gates. No daemon surface, no MCP tool, no cron (skill-first;
a scheduled render only after reviewed renders earn it). No LLM calls and
no network anywhere in the render path — the page opens identically on an
air-gapped machine.

## Next phase

### P4 — one canvas, drill-throughs (SHIPPED 2026-08-06)

Shipped same day it was funded; the design below is now how the page reads
(see "How to read the page"). The funding test stands open until measured
from use: time-to-first-dragon on a glance drops. An inversion, not a
feature: the treemap
becomes THE view and every SOLID projection renders as an overlay or a
drill-down from evidence on the field — DIP violations as red
against-the-grain arrows between crate regions, an ISP block-diagonal
as a split-glyph on the defining trait's leaf (click → the seriated
matrix, with cell → call-site drill-through), SRP bridges / DRY arcs /
tollbooth glow already live there. The default view paints only the
top-K strongest shapes (the Start-here queue's magnitude ranking, K≈7);
lens toggles show everything; the footer states what is hidden.
Thresholds gate clutter, never verdicts. Net-simplification: the
always-rendered panel stack is DELETED in favor of drill-throughs.
Funding test: time-to-first-dragon on a glance drops — the operator
names it from the field without scrolling.

### The manual flow is the contract (until proven, no machinery)

Everything below stays documentation until the ritual — one script, run
by hand — proves the page earns automation. The manual flow is hardened
accordingly: every optional input degrades to a stated dark panel plus a
footer note (no transcripts → agent heat dark; no embeddings → NEAR tier
dark; no git → walk fallback, stated); the only hard failure is a
missing SCIP graph, and the error names the repair; a degraded render
(`--no-*`) NEVER replaces the delta baseline — full renders own it.

### Ingest completeness — what the page can and cannot see today

Audited 2026-08-06:

| Layer | Coverage today | Gap |
|---|---|---|
| Structure (SCIP) | maintained: 30s git-poll, failures loud on 4 surfaces | ~22% refs unattributed (stated); ISP top-12 (stated) |
| Git (SRP 548d, churn 90d) | full history, harvested fresh each render | none — windows stated |
| NEAR duplication | `chunks.lance` embeddings | **manual-only** — no watcher kind exists for it; drifts silently between `svrn code index` runs (age stated on page) |
| Agent heat | this machine's Claude Code transcripts for this repo | **~30-day retention cliff** (harness prunes transcripts — "all sessions" is a rolling window); peer nodes not ingested; other harnesses not ingested |

### Documented, unfunded — each waits on the ritual proving value

- **Replay** — activations over a time window: bucket the git harvest
  (already run every render) into weekly frames; a scrubber animates
  activation tint over the STABLE layout, so a thickening tollbooth or a
  swelling crate is visible as motion. Zero new ingest machinery — pure
  render-side. Today the only temporal elements are the 90d churn glow
  and the since-last-render delta; replay is designed, not built.
  Funding test: within a week of glances it surfaces one trend the
  operator didn't know.
- **Heat-rollup persistence** — append-only per-render snapshot of the
  by-file rollup in `~/.svrnmesh` (fleet-report's md+json precedent),
  converting the 30-day transcript cliff into cumulative history. Also
  the substrate replay's attention lens would need. First in line if
  funded: every day unbuilt is a day of attention history aging off the
  cliff.
- **Chunk-index auto-refresh** — piggyback the existing trigger: after a
  successful SCIP rebuild, kick a chunk reindex when the embedding index
  is older than N days. One decider, no new watcher kind.
- **Friction traces** — edit → test-fail → edit loops per file from
  transcripts (whack-a-mole zones).
- **Fleet scope** — heat from peer nodes' transcripts via the mesh, not
  just this workstation's.

## Success, honestly stated

Leads, to be measured from use: the first render surfacing a nameable
finding the operator didn't already know (the funding test — the first
render of this repo surfaced a 139-line near-clone family between
`corpus-engine/src/extractors/` and `sovereign-tools/src/corpus/`, a
25-method × 11-crate `InferenceProvider` matrix, and a 35-method `Pipeline`
trait with two consumers); "where are the dragons in subsystem Y" as a
thirty-second glance instead of a day of archaeology; duplicate-work caught
at the arc instead of the post-mortem.
