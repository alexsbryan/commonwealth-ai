# Backlog migration — the first pass, and what it says about the ruler

Generated 2026-08-09 by order `seat-backlog-protocol` (D3). Every number
below was read from the store at generation time.

**What happened.** Every live `kind=todo` note in `~/.sovereign/notes.db`
was superseded by a new note carrying the backlog header block, with a
pointer back to the original. Clean history: nothing was edited in place
and **nothing was retired by hand**. The originals are retired by the
supersede itself, each with `retired_by = "superseded by note <id>"`, and
their rows are kept for the gossip-propagated supersedes chain.

**Value and Cost are the seat's PROPOSAL, not a verdict.** They are a
first pass by one worker against the ruler in the doc header of
`scripts/co-backlog.py`. Edit them freely — the edits are the training
data for ruler v2. This report exists so that editing them takes one read
rather than 79.

## The numbers

| | |
|---|---|
| Notes migrated | **79** |
| Live backlog items now | **76** |
| Originals retired with a pointer | 79 / 79 |
| Original rows preserved (not tombstoned) | 79 / 79 |
| Originals with exactly one migrated successor | 79 / 79 |
| Vetted (pullable) | **55** |
| Unvetted (greyed, not pullable) | **24** |
| Chunk groups formed | 9 |
| Live todos NOT in the backlog | 3 (deliberate — see 'Two anchors') |

**Value distribution** (5 = moves A-D with a measurement attached; 1 = everything else):

| Value | Count | Share |
|---|---|---|
| 5 | 6 | 8% |
| 4 | 13 | 16% |
| 3 | 25 | 32% |
| 2 | 27 | 34% |
| 1 | 8 | 10% |

**Cost:** S=31, M=36, L=12  ·  **Axis:** A Grounded=7, B Responsive=3, C Well-cited=10, D One sweep=10, E Clean handoffs=49

## The finding the operator should read first

**49 of 79 items (62%) scored on axis E, and
35 of 79 (44%) scored Value 1-2.**
That is not a claim that the backlog is low-value work. It is a fact about
**ruler v1**: the five axes are answer-quality axes, so mesh plumbing,
install defects, daemon lifecycle, test flakes and build gates have no axis
that fits them and land at 1-2 by construction.

The sharpest case is `f81a59a5` (was `ddb7c7bc`): on every default fresh install
there is no
mesh-consent screen, no seed conversations, and the Wikipedia Core install
the user explicitly asked for never starts. Each link in that mechanism was
verified, not inferred. Ruler v1 scores it **2** ("protects the above"),
below three documentation items. The ruler was applied faithfully rather
than quietly bent, because a ruler bent per-item stops being a ruler; the
mis-fit is reported here instead.

**The decision this puts in front of the operator:** does ruler v2 need a
sixth axis for "the product works at all" (install, lifecycle, mesh
reachability), or is 1-2 the honest score for that work? v1 deliberately has
no age term either, so nothing here is ranked by how long it has waited.

## One thing the migration noticed in passing

Three of the 79 originals were ALREADY the target of a `supersedes` link
from a note of a different kind — `5780c214` from a `decision`, `de5c7ee4`
from a `decision`, `0a3f47d1` from an `invariant`, all written by other
sessions in early August. Those targets were nevertheless still LIVE when
this migration read the store, so a supersede link existed without the
target having been retired. It is recorded here as an observation, not
chased: it is outside this order and nothing was done about it.

## Vetting — 24 items name what they are missing

Vetted means the body carries a clean header **plus** a `Done-when:` and an
`Evidence:`. Only vetted items are pullable.

`Done-when` was **lifted, never invented**: an item got one only where the
original note states a completion condition. Where it does not, the item
migrated unvetted and says so in its own body. That was the instruction and
it is also the safer failure: an unvetted item is a true report, whereas a
fabricated done-when is a trap the next worker walks into.

Two sub-cohorts inside the unvetted set were deliberately left unpullable:

- **Apparently already shipped — `2c87d5c9` (was `05a0eeaf`), `445f2970` (was `0214b8af`), `8a4afab8` (was `0e4bab92`).**
  Two were verified resolved during this migration (`which sovereign` now
  resolves `~/.local/bin/sovereign`; the MCP tools are live in the session
  that read the store). They were NOT retired — pruning is an operator
  decision, and this pass supersedes rather than prunes.
- **Corrected by a newer note — `7bc29d90` (was `06e0ee5d`), `18d4b13a` (was `8de0d918`).**
  Each is chunked with the note that corrects it, so the operator can retire
  the stale framing in one gesture.

## The pre-2026-07 cohort — 19 items, prunable in one gesture

Requested by the seat. These predate 2026-07 (oldest 2026-04-21):

| Item (on the page) | was | Date | V/Cost | ROI | Vetted | Objective |
|---|---|---|---|---|---|---|
| `5dec9fd8` | `f152dfe7` | 2026-04-21 | 3E/L | 1.00 | yes | work-queue follow-ups from the pull-queue E2E |
| `8a4afab8` | `0e4bab92` | 2026-05-01 | 1E/S | 1.00 | no | Claude Code surfaces the sovereign MCP tools |
| `445f2970` | `0214b8af` | 2026-05-01 | 1E/S | 1.00 | no | a `sovereign` shim resolves on PATH |
| `0077c70d` | `b73c8df9` | 2026-05-01 | 3C/L | 1.00 | yes | glass-box voice — Wave 2 R3 and Wave 3 R5 |
| `11eb2a94` | `35bc5d4a` | 2026-05-01 | 3E/M | 1.50 | yes | voice eval Tier-B has a live runner |
| `a100420f` | `b6a24ba0` | 2026-05-11 | 2E/M | 1.00 | yes | atos_cmd/run.rs is split by runner stage |
| `1ae7c7ec` | `c28e82a9` | 2026-05-12 | 3C/S | 3.00 | yes | the drift report shows what is grounded, not only what is broken |
| `8ee01308` | `30782768` | 2026-05-12 | 3E/S | 3.00 | yes | drift_findings kind=path means what it says |
| `11fcb375` | `e6e46586` | 2026-05-12 | 4C/S | 4.00 | yes | engineering_atlas code_anchors are anchors, not prose |
| `c6576020` | `e3b4e481` | 2026-05-24 | 4C/L | 1.33 | yes | the vault typed-atom pass restores the dropped obsidian axes |
| `7bec9625` | `f804fc62` | 2026-05-25 | 3E/M | 1.50 | yes | one select_route for both mesh routing bodies |
| `25d3c94a` | `4f78f9f1` | 2026-05-25 | 3C/S | 3.00 | yes | dedup keeps multi-chunk depth on wiki |
| `810a2f91` | `44096b0d` | 2026-05-25 | 2C/S | 2.00 | yes | atlas_weight is validated cross-corpus, not SEP-only |
| `5f3fbbe0` | `7dafcd6e` | 2026-05-25 | 1E/M | 0.50 | no | RAPTOR atlas progress is granular |
| `0bf324f6` | `9897df42` | 2026-05-25 | 5C/L | 1.67 | yes | T5 anti-canonical synthesis quality |
| `0455c7a1` | `e4c974b7` | 2026-05-26 | 3D/S | 3.00 | yes | the compaction-pressure sensor sees the whole prompt |
| `48cc8c49` | `3d2fcb0f` | 2026-06-02 | 2E/L | 0.67 | yes | tech-debt refactor program — PR4-7 |
| `7933c04a` | `9c213529` | 2026-06-10 | 1E/L | 0.33 | no | transport migration — phone first, then mesh per-class |
| `193ed98f` | `3a098a87` | 2026-06-11 | 4A/M | 2.00 | yes | personal-scope retrieval includes watched-folder corpora |

Their value distribution is V5=1, V4=3, V3=8, V2=3, V1=4; 4 are unvetted. Age is NOT a
value signal in ruler v1, so nothing here was marked down for being old —
several are cheap and well-cited, including the current top of the heap.

## Two anchors, and why 3 live todos are not on the page

The comaintainer skill now distinguishes them: `comaintainer-seat` is the
seat's own business, what the NEXT SEAT picks up; `backlog` is work a WORKER
could be handed with an order. Three items are genuinely the former (two
disk-reclaim chores and the seat handoff), so they kept the seat anchor and
do not render on the heap. The page footer counts them, so the absence is
visible rather than silent.

## Chunk groups

Formed only where the notes point at each other. A group's ROI is summed
value over summed session-chunks, so a pair of cheap items is worth more
together than either alone only when both are genuinely one sitting.

| ROI | Items | What they share |
|---|---|---|
| 3.00 | `8ee01308`, `1ae7c7ec` | the drift tool tells the truth about paths and about what is grounded |
| 2.50 | `810a2f91`, `25d3c94a` | wiki-side retrieval dedup and atlas weighting |
| 2.00 | `66fa2adc`, `a2af2ff8`, `d4cbf14f`, `b183445c`, `1939bab8` | the full-suite test gate: four sightings of the same flake family plus one with a found root cause |
| 1.80 | `67beb617`, `0ca0a90e`, `7bec9625` | one routing surface keeps getting fixes the other does not; `f804fc62`'s select_route extraction is the structural fix |
| 1.50 | `2c87d5c9`, `b1996440` | the two halves of the host disk reclaim (seat-anchored) |
| 1.50 | `7bc29d90`, `b9488aa2` | the pipeline pod phantom-cost note and its correction |
| 1.50 | `d96b0387`, `ebc2a1f1` | the same in-flight-counter question from two sides |
| 1.20 | `11eb2a94`, `0077c70d` | voice eval Tier-B and the glass-box voice waves that need it |
| 0.67 | `74baebc2`, `18d4b13a` | the GTT ratchet finding and the note it corrects |

## What `--pull` would hand you today

Top of the heap by ROI among pullable items. Three tie at 4.00.

**One honest wrinkle in that tie-break.** A migrated item is a NEW note, so
its `created_at` is 2026-08-09, not the original's date. Ruler v1 has no age
term, so age changes no score — but it is the third tie-break after ROI and
value, which means ties are now broken by MIGRATION ORDER rather than by how
long an item has waited. That is why `de5c7ee4` heads the 4.00 tie ahead of
`e6e46586`, which is three months older. The original dates are preserved in
this report's tables and in each item's verbatim body; if the operator wants
age to matter, that is a ruler v2 decision, not a rendering fix.

| ROI | Item (on the page) | was | V/Cost | Axis | Objective |
|---|---|---|---|---|---|
| 4.00 | `34c3658d` | `de5c7ee4` | 4/S | A Grounded | a hop-exhausted refusal names its real cause |
| 4.00 | `47d06c81` | `9a8e7c1a` | 4/S | A Grounded | `svrn code watch` never writes zero-vector embeddings |
| 4.00 | `11fcb375` | `e6e46586` | 4/S | C Well-cited | engineering_atlas code_anchors are anchors, not prose |
| 3.00 | `67beb617` | `18eee3fb` | 3/S | C Well-cited | every routing refusal emits an outcome record to join to |
| 3.00 | `9b463ef6` | `ccb15537` | 3/S | C Well-cited | GLiNER2's threshold is calibrated for our label set |
| 3.00 | `4105dedb` | `97433ad7` | 3/S | E Clean handoffs | `mesh plan` accepts a mesh-resolved model reference |
| 3.00 | `e4da9bc1` | `421416c7` | 3/S | C Well-cited | interop docs report the live surface, not a stale allowlist |
| 3.00 | `0455c7a1` | `e4c974b7` | 3/S | D One sweep | the compaction-pressure sensor sees the whole prompt |
| 3.00 | `25d3c94a` | `4f78f9f1` | 3/S | C Well-cited | dedup keeps multi-chunk depth on wiki |
| 3.00 | `8ee01308` | `30782768` | 3/S | E Clean handoffs | drift_findings kind=path means what it says |

Note what does NOT appear: the highest-ROI items in the store are not
necessarily here, because unvetted items are excluded however attractive
their numbers. That exclusion is the point of vetting.

## Full table — all 79 items

| Item (on the page) | was | Date | V | Axis | Cost | ROI | Vetted | Anchor | Objective |
|---|---|---|---|---|---|---|---|---|---|
| `74c96fbe` | `78e51bad` | 2026-08-07 | 4 | A | S | 4.00 | no | backlog | wikipedia-article on-demand ingest must not die at enrichment |
| `47d06c81` | `9a8e7c1a` | 2026-07-25 | 4 | A | S | 4.00 | yes | backlog | `svrn code watch` never writes zero-vector embeddings |
| `34c3658d` | `de5c7ee4` | 2026-08-06 | 4 | A | S | 4.00 | yes | backlog | a hop-exhausted refusal names its real cause |
| `11fcb375` | `e6e46586` | 2026-05-12 | 4 | C | S | 4.00 | yes | backlog | engineering_atlas code_anchors are anchors, not prose |
| `67beb617` | `18eee3fb` | 2026-08-06 | 3 | C | S | 3.00 | yes | backlog | every routing refusal emits an outcome record to join to |
| `8ee01308` | `30782768` | 2026-05-12 | 3 | E | S | 3.00 | yes | backlog | drift_findings kind=path means what it says |
| `e4da9bc1` | `421416c7` | 2026-07-28 | 3 | C | S | 3.00 | yes | backlog | interop docs report the live surface, not a stale allowlist |
| `25d3c94a` | `4f78f9f1` | 2026-05-25 | 3 | C | S | 3.00 | yes | backlog | dedup keeps multi-chunk depth on wiki |
| `4105dedb` | `97433ad7` | 2026-07-30 | 3 | E | S | 3.00 | yes | backlog | `mesh plan` accepts a mesh-resolved model reference |
| `1ae7c7ec` | `c28e82a9` | 2026-05-12 | 3 | C | S | 3.00 | yes | backlog | the drift report shows what is grounded, not only what is broken |
| `9b463ef6` | `ccb15537` | 2026-08-03 | 3 | C | S | 3.00 | yes | backlog | GLiNER2's threshold is calibrated for our label set |
| `0455c7a1` | `e4c974b7` | 2026-05-26 | 3 | D | S | 3.00 | yes | backlog | the compaction-pressure sensor sees the whole prompt |
| `5a1a407e` | `654ae45a` | 2026-08-03 | 5 | D | M | 2.50 | yes | backlog | settle whether GLiNER earns its 15m17s on the vault path |
| `a08674ff` | `7767dc20` | 2026-08-06 | 5 | D | M | 2.50 | yes | backlog | chat streaming reaches the 13x prefix-cache win |
| `89787676` | `ccc99257` | 2026-08-05 | 5 | B | M | 2.50 | yes | backlog | the routing bench baselines describe today's model |
| `78c4318f` | `fe12232b` | 2026-08-06 | 5 | D | M | 2.50 | yes | backlog | cold start is priced honestly in mesh scheduling |
| `66fa2adc` | `246dc797` | 2026-08-07 | 2 | E | S | 2.00 | no | backlog | the definition-of-done gate is trustworthy under load |
| `cdaec073` | `30c83eea` | 2026-08-07 | 2 | E | S | 2.00 | no | backlog | the lineage-boot-hook suite is green or honestly scoped |
| `43404c70` | `35a805e7` | 2026-08-07 | 2 | E | S | 2.00 | yes | backlog | work-atlas claims are honest after a daemon restart |
| `b1996440` | `3697c52d` | 2026-08-08 | 2 | E | S | 2.00 | yes | seat | seat operations — host disk headroom |
| `81c15959` | `39d4f9a8` | 2026-07-29 | 2 | E | S | 2.00 | yes | backlog | the distributed 122B load reaches the peer's daemon HTTP surface |
| `193ed98f` | `3a098a87` | 2026-06-11 | 4 | A | M | 2.00 | yes | backlog | personal-scope retrieval includes watched-folder corpora |
| `810a2f91` | `44096b0d` | 2026-05-25 | 2 | C | S | 2.00 | yes | backlog | atlas_weight is validated cross-corpus, not SEP-only |
| `bfbf7752` | `5ee71c03` | 2026-08-06 | 2 | E | S | 2.00 | yes | backlog | cache-audit comprehension tax excludes vendored paths |
| `a2af2ff8` | `62715047` | 2026-07-27 | 2 | E | S | 2.00 | yes | backlog | the definition-of-done gate is trustworthy under load |
| `056faa52` | `6ea820ff` | 2026-07-26 | 4 | D | M | 2.00 | yes | backlog | cross-network distributed inference: worker identity survives bridge re-dials |
| `d4cbf14f` | `76a30db5` | 2026-07-31 | 2 | E | S | 2.00 | no | backlog | the definition-of-done gate is trustworthy under load |
| `0614ea77` | `84332ba0` | 2026-08-07 | 4 | B | M | 2.00 | no | backlog | mesh routing: a shed peer plus a shed local must not fail the caller |
| `fca40c74` | `91025679` | 2026-07-30 | 4 | A | M | 2.00 | yes | backlog | SEP RAPTOR rows are not orphaned while still serving retrieval |
| `bd1726eb` | `c0bda2ab` | 2026-08-09 | 4 | A | M | 2.00 | yes | backlog | native-grounding H0 — make the value unit measurable |
| `54072b2d` | `c5678d34` | 2026-07-28 | 4 | A | M | 2.00 | yes | backlog | named-model routing reaches the distributed primary |
| `fb338414` | `d2fc7549` | 2026-07-24 | 2 | E | S | 2.00 | yes | backlog | frame provenance survives the Stop-hook distill |
| `b183445c` | `dd337207` | 2026-07-27 | 2 | E | S | 2.00 | yes | backlog | the definition-of-done gate is trustworthy under load |
| `caaf0d1b` | `ebe06232` | 2026-08-07 | 2 | E | S | 2.00 | yes | backlog | the quality gate cannot pass without judging anything |
| `b9488aa2` | `ecc59649` | 2026-08-05 | 2 | E | S | 2.00 | yes | backlog | the pipeline pod ledger does not report phantom cost |
| `1939bab8` | `f60ff04e` | 2026-08-03 | 2 | E | S | 2.00 | no | backlog | the definition-of-done gate is trustworthy under load |
| `c38b6bd1` | `594d3c67` | 2026-07-13 | 5 | D | L | 1.67 | yes | backlog | per-turn TTFT is owned by retrieval fan-out, not prefill |
| `0bf324f6` | `9897df42` | 2026-05-25 | 5 | C | L | 1.67 | yes | backlog | T5 anti-canonical synthesis quality |
| `441deef1` | `0a3f47d1` | 2026-08-06 | 3 | B | M | 1.50 | no | backlog | a named request survives its sole holder disappearing |
| `51001dfe` | `188f06d0` | 2026-08-06 | 3 | E | M | 1.50 | yes | backlog | the SCIP index keeps pace with commit velocity |
| `d1e1ac42` | `30f49807` | 2026-08-06 | 3 | E | M | 1.50 | yes | backlog | mesh reports which models a peer actually holds |
| `11eb2a94` | `35bc5d4a` | 2026-05-01 | 3 | E | M | 1.50 | yes | backlog | voice eval Tier-B has a live runner |
| `536eb92e` | `5a56e565` | 2026-08-08 | 3 | E | M | 1.50 | no | backlog | enrichment visibility — EnrichmentChecker sees every skip |
| `d96b0387` | `9e149ab7` | 2026-07-26 | 3 | E | M | 1.50 | yes | backlog | settle whether the gossiped in-flight count sees inbound peer load |
| `ebc2a1f1` | `a24cfdb5` | 2026-07-26 | 3 | E | M | 1.50 | yes | backlog | settle whether the gossiped in-flight count sees inbound peer load |
| `4a4e63b8` | `b680cf6b` | 2026-07-28 | 3 | D | M | 1.50 | yes | backlog | the compute-child boundary tax is measured, not inferred |
| `95df22e2` | `b87bb8ea` | 2026-08-02 | 3 | D | M | 1.50 | yes | backlog | mesh warm-cache can start before the cluster fits |
| `e28f38bd` | `bef03728` | 2026-08-06 | 3 | E | M | 1.50 | yes | backlog | the queue-shed 503 carries Retry-After to the client |
| `0ca0a90e` | `d45489a3` | 2026-07-29 | 3 | E | M | 1.50 | no | backlog | streaming and non-streaming agree about slot availability |
| `4a9ece15` | `d903dec7` | 2026-07-07 | 3 | E | M | 1.50 | no | backlog | SOLVE's pin-then-green path works on a no-tests repo |
| `c47f7998` | `f29260b5` | 2026-08-04 | 3 | E | M | 1.50 | yes | backlog | one surface answers 'is this peer usable as a worker?' |
| `7bec9625` | `f804fc62` | 2026-05-25 | 3 | E | M | 1.50 | yes | backlog | one select_route for both mesh routing bodies |
| `2936225f` | `16fc9204` | 2026-08-03 | 4 | D | L | 1.33 | yes | backlog | the four knobs — no second user can run a distributed load |
| `e7286d88` | `b234de5e` | 2026-07-28 | 4 | D | L | 1.33 | yes | backlog | the distributed primary runs inside a sovereign-compute child |
| `c6576020` | `e3b4e481` | 2026-05-24 | 4 | C | L | 1.33 | yes | backlog | the vault typed-atom pass restores the dropped obsidian axes |
| `445f2970` | `0214b8af` | 2026-05-01 | 1 | E | S | 1.00 | no | backlog | a `sovereign` shim resolves on PATH |
| `2c87d5c9` | `05a0eeaf` | 2026-08-08 | 1 | E | S | 1.00 | no | seat | seat operations — host disk headroom |
| `7bc29d90` | `06e0ee5d` | 2026-08-05 | 1 | E | S | 1.00 | no | backlog | the pipeline pod ledger does not report phantom cost |
| `8a4afab8` | `0e4bab92` | 2026-05-01 | 1 | E | S | 1.00 | no | backlog | Claude Code surfaces the sovereign MCP tools |
| `afab7993` | `2f174e79` | 2026-08-08 | 2 | E | M | 1.00 | no | backlog | CLI-contract nightly lane back to green |
| `911e149a` | `5056dbed` | 2026-08-05 | 2 | E | M | 1.00 | no | backlog | cargo xtask quality is green or honestly baselined |
| `74baebc2` | `5780c214` | 2026-08-03 | 3 | E | L | 1.00 | no | backlog | verifier-v0: a full epoch survives on the Halo |
| `d31fa46c` | `6d059d47` | 2026-08-07 | 2 | E | M | 1.00 | no | backlog | the SCIP graph survives a daemon restart mid-rebuild |
| `dae53c2d` | `7a6f4df2` | 2026-07-28 | 2 | E | M | 1.00 | yes | backlog | desktop real-mode e2e has a mid-size fixture |
| `0aa22ac6` | `96b259f4` | 2026-07-30 | 2 | E | M | 1.00 | no | backlog | the distributed child stops flapping |
| `9b1f940a` | `b1c9df45` | 2026-07-28 | 2 | E | M | 1.00 | yes | backlog | desktop coverage is measured source-first, not spec-first |
| `a100420f` | `b6a24ba0` | 2026-05-11 | 2 | E | M | 1.00 | yes | backlog | atos_cmd/run.rs is split by runner stage |
| `0077c70d` | `b73c8df9` | 2026-05-01 | 3 | C | L | 1.00 | yes | backlog | glass-box voice — Wave 2 R3 and Wave 3 R5 |
| `8332594a` | `c7f7bb64` | 2026-07-30 | 2 | E | M | 1.00 | no | backlog | the daemon's RSS growth is explained |
| `0794df8c` | `ca8523f2` | 2026-07-24 | 2 | E | M | 1.00 | yes | backlog | peer continuity posture rides the mesh transport |
| `ebbce0c1` | `d2af7720` | 2026-08-04 | 2 | E | M | 1.00 | yes | backlog | conversation retrieval is gated by the comprehensive bench |
| `f39c032e` | `d7404ac7` | 2026-08-06 | 1 | E | S | 1.00 | no | seat | seat handoff — open business for the next seat |
| `f81a59a5` | `ddb7c7bc` | 2026-07-27 | 2 | E | M | 1.00 | yes | backlog | a default fresh install completes its setup tail |
| `5dec9fd8` | `f152dfe7` | 2026-04-21 | 3 | E | L | 1.00 | yes | backlog | work-queue follow-ups from the pull-queue E2E |
| `48cc8c49` | `3d2fcb0f` | 2026-06-02 | 2 | E | L | 0.67 | yes | backlog | tech-debt refactor program — PR4-7 |
| `70705031` | `ec73ffb4` | 2026-08-04 | 2 | E | L | 0.67 | yes | backlog | on-prem pilot: the clean-VM rehearsal |
| `5f3fbbe0` | `7dafcd6e` | 2026-05-25 | 1 | E | M | 0.50 | no | backlog | RAPTOR atlas progress is granular |
| `18d4b13a` | `8de0d918` | 2026-08-03 | 1 | E | L | 0.33 | no | backlog | verifier-v0: a full epoch survives on the Halo |
| `7933c04a` | `9c213529` | 2026-06-10 | 1 | E | L | 0.33 | no | backlog | transport migration — phone first, then mesh per-class |

## How to audit or redo this

Each migrated note carries its original **verbatim** below a
`--- the original note, verbatim ---` marker, so no scoring decision hides
what it was scored from. To re-score an item, edit its `Value:`/`Cost:`
lines; to vet one, add a `Done-when:` and an `Evidence:`. To see the result:

```
./scripts/co-backlog.py --open     # the ranked heap
./scripts/co-backlog.py --pull     # the top chunk as an order draft
```
