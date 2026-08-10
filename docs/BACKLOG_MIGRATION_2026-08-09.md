# Backlog migration — the first pass, and the ruler it rewrote

Generated 2026-08-09 by order `seat-backlog-protocol` (D3, plus the
addendum directives ee29b86d and 341884f5). Every number below was read
from the store at generation time.

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
| Live backlog items now | **78** (76 migrated + 2 banked live) |
| Originals retired with a pointer | 79 / 79 |
| Original rows preserved (not tombstoned) | 79 / 79 |
| Originals resolving to exactly one live item | 79 / 79 |
| Vetted (pullable) | **54** |
| Unvetted (greyed, not pullable) | **25** |
| Chunk groups formed | 9 |
| Approach derived from the note | 70 / 79 |
| Approach honestly unknown | 9 / 79 |
| Live todos NOT in the backlog | 3 (deliberate — see 'Two anchors') |

**Value distribution under ruler v2** (5 = moves A-D or F with a
measurement attached; 1 = everything else):

| Value | Count | Share |
|---|---|---|
| 5 | 7 | 9% |
| 4 | 16 | 20% |
| 3 | 25 | 32% |
| 2 | 23 | 29% |
| 1 | 8 | 10% |

**Cost:** S=31, M=36, L=12  ·  **Axis:** A Grounded=7, B Responsive=3, C Well-cited=10, D One sweep=7, E Clean handoffs=44

## The finding, and what the operator did about it

The first pass ran under **ruler v1** and came back skewed: 49 of 79 items
(62%) scored on axis E and 35 of 79 (44%) scored Value 1-2. That was never a
claim that the backlog is low-value work — it was a fact about the RULER.
v1's five axes are all answer-quality axes, so mesh plumbing, install
defects, daemon lifecycle, test flakes and build gates had no axis that fit
and landed at 1-2 by construction.

The sharpest case: on every default fresh install there is no mesh-consent
screen, no seed conversations, and the Wikipedia Core install the user
explicitly asked for never starts — each link verified, not inferred — and
v1 scored it **2**, below three documentation items. The ruler was applied
faithfully rather than bent per-item, because a ruler bent per-item stops
being a ruler, and the mis-fit was reported instead.

**The operator's answer (directive ee29b86d): ruler v2 adds axis F.**

> F. Viable — "a new user can reach the value proposition at all": install,
> onboarding, and lifecycle defects that gate everything else.

F scores exactly as A-D do. The boundary that keeps it honest is written
into the ruler: F is "the user cannot get there", not "this is important
infrastructure" — test flakes and build gates stay at 2 on axis E, because
they protect delivery rather than gate a user. Eight items moved:

| Item (on the page) | was | v1 score | v2 score | Why F |
|---|---|---|---|---|
| `b439e47f` | `ddb7c7bc` | V2 E Clean handoffs / M | **V5 F Viable / M** | the exemplar: on EVERY default fresh install the user gets no consent screen, no seed conversations, and not the corpus they asked for. The user does not reach the value proposition at all. Incidence is 100% and each link of the mechanism was verified, so it scores 5, not 4. |
| `218255c3` | `ec73ffb4` | V2 E Clean handoffs / L | **V4 F Viable / L** | the on-prem install path has never been executed end to end, so the pilot user's first hour is unverified and 'under an hour' is an estimate presented as a measurement. Falsifiable, not yet measured -> 4. |
| `8a5a3c80` | `f29260b5` | V3 E Clean handoffs / M | **V4 F Viable / M** | explicitly an onboarding defect: a new operator sees green on both boxes and reasonably concludes they can run a distributed model, when they cannot. Three failure modes observed live but no rate measured -> 4. |
| `24827f08` | `16fc9204` | V4 D One sweep / L | **V4 F Viable / L** | the note's own words are 'why no second user can run a distributed load'. That is axis F exactly; it was scored D only because v1 had no axis for it. |
| `d27f152c` | `6ea820ff` | V4 D One sweep / M | **V4 F Viable / M** | cross-network distributed inference cannot start AT ALL — the peer never holds Probationary long enough to reach Eligible. A hard gate, not a latency cost, so F rather than D. |
| `539f2fbc` | `b234de5e` | V4 D One sweep / L | **V4 F Viable / L** | a dead RPC worker mid-decode kills the WHOLE daemon. Daemon death is the lifecycle defect that gates everything downstream of it. |
| `5e06bd6e` | `c7f7bb64` | V2 E Clean handoffs / M | **V4 F Viable / M** | the daemon self-terminated twice at ~32GB RSS. Lifecycle death gates every axis. Magnitude is measured but the cause is not, and the fix is not yet falsifiable, so 4 rather than 5. Still UNVETTED and unpullable. |
| `2244b0bf` | `39d4f9a8` | V2 E Clean handoffs / S | **V3 F Viable / S** | the peer's daemon HTTP surface is refused, so the distributed load cannot warm. A real reachability gate, but one narrow instance of the class 16fc9204 and 6ea820ff cover, so 3 rather than 4. |

Axis distribution after v2: A Grounded=7, B Responsive=3, C Well-cited=10, D One sweep=7, E Clean handoffs=44, F Viable=8.

What did NOT move is worth as much as what did. The daemon flap loop
(self-recovering, so the user still reaches value) and the SCIP-graph
emptying (agent toolchain, not the product) both stayed on E. If F had
swept up everything infrastructural it would just be v1's problem wearing a
new letter.

## Sizing — every item now says HOW, or admits it does not know

Operator directive 341884f5 added an `Approach:` line: 1-3 sentences naming
what gets built, which EXISTING surface it builds on, and why that makes the
Cost credible. The operator's reason, verbatim: a raw note "struggles to get
to the point of how we’d actually solve it", and "I don’t think I can feel
that the sizing is credible if I don’t have a sense of the potential
solution." The renderer now shows Approach ABOVE the verbatim note — the
note is the evidence, the approach is the point.

**Coverage: 70 derived, 9 unknown.** Approaches were
derived from each note's OWN fix-shaped language — most of these notes
already carried a `FIX SHAPE:`, `Design sketch:`, `Options, cheapest first:`
or `Levers, in order:` section, which is where the text came from. Where the
body did not support one, the item says so:

| Item (on the page) | was | Why unknown |
|---|---|---|
| `9538d40a` | `0a3f47d1` | the note establishes the timeline but states plainly that causation is not established and must not be claimed — RuggedFox cannot see why a peer's daemon stopped. A fallback design cannot be sized before that. |
| `3cf28de9` | `0e4bab92` | the note lists three candidate causes to investigate and settles none. Moot in any case — verified resolved 2026-08-09, the mcp__sovereign__* tools are live. |
| `9fc63602` | `30c83eea` | the note offers two resolutions (delete the assertion as belonging to session-boot.sh's own suite, or make the notes hook write the marker) and deliberately does not choose. Picking is the design step. |
| `2244b0bf` | `39d4f9a8` | the note names two options (expose the peer's client API on the LAN, or route rpc-warm over iroh as gossip already does over exactly this pair) and records the choice as a real design decision it does not make. |
| `5d120f16` | `78e51bad` | the note names two defensible options (register `encyclopedic` as a field-model domain, or point the recipe at one of the five that exist) and records that choosing between them is a product call, not an engineering one. It also cannot be fixed without editing the KNOWN_BROKEN row that asserts in both directions. |
| `45a07661` | `96b259f4` | the cycle is characterised but its cause is not. The note rules out the known contention explanation and stops there. |
| `5e06bd6e` | `c7f7bb64` | the note ends in open questions — what allocates the ~27GB of unexplained growth (embed batch buffers, corpus caches, a leak) is not established, and nothing can be sized before it is. |
| `a6fc7e46` | `d45489a3` | the note poses whether the streaming path should refuse like the non-streaming one, argues both sides, and does not decide. Its fallback suggestion — make the substitution visible via a distinct `model` value or an OpenAI-style system_fingerprint — depends on that decision. |
| `3e97c963` | `d903dec7` | three options are named (strengthen the single-test prompt shaping, count a collection error as a failing signal, or relax the predicate to '>=1 new failing, 0 regressions') and the note records that the third changes bench-validated Red semantics (92% PASS_AS_RED) and needs a deliberate decision plus a bench re-run. |

Note the pattern: seven of the nine are not missing an idea, they are
waiting on a DECISION the note deliberately declined to make ("two answers
are defensible and the choice is a product call"; "a real design decision,
NOT decided here"). `unknown` is the honest record of that, and no solution
was invented to make any of them pullable.

**The sizing rule bit one real item.** `2244b0bf` (was `39d4f9a8`) carries a done-when AND an evidence line and would have been
pullable under the old rule. Its approach is a design decision the note
refuses to make, so it is now unvetted — which is the rule working, not
a regression.

## One thing the migration noticed in passing

Three of the 79 originals were ALREADY the target of a `supersedes` link
from a note of a different kind — `5780c214` from a `decision`, `de5c7ee4`
from a `decision`, `0a3f47d1` from an `invariant`, all written by other
sessions in early August. Those targets were nevertheless still LIVE when
this migration read the store, so a supersede link existed without the
target having been retired. It is recorded here as an observation, not
chased: it is outside this order and nothing was done about it.

## Vetting — 25 items name what they are missing

Vetted means the body carries a clean header **plus** a `Done-when:`, an
`Evidence:`, and an `Approach:` that is not "unknown". Only vetted items
are pullable.

`Done-when` was **lifted, never invented**: an item got one only where the
original note states a completion condition. Where it does not, the item
migrated unvetted and says so in its own body. That was the instruction and
it is also the safer failure: an unvetted item is a true report, whereas a
fabricated done-when is a trap the next worker walks into.

Two sub-cohorts inside the unvetted set were deliberately left unpullable:

- **Apparently already shipped — `2e16848a` (was `05a0eeaf`), `d7e2c194` (was `0214b8af`), `3cf28de9` (was `0e4bab92`).**
  Two were verified resolved during this migration (`which sovereign` now
  resolves `~/.local/bin/sovereign`; the MCP tools are live in the session
  that read the store). They were NOT retired — pruning is an operator
  decision, and this pass supersedes rather than prunes.
- **Corrected by a newer note — `910c7dc6` (was `06e0ee5d`), `dd64c958` (was `8de0d918`).**
  Each is chunked with the note that corrects it, so the operator can retire
  the stale framing in one gesture.

## The pre-2026-07 cohort — 19 items, prunable in one gesture

Requested by the seat. These predate 2026-07 (oldest 2026-04-21):

| Item (on the page) | was | Date | V/Cost | ROI | Vetted | Objective |
|---|---|---|---|---|---|---|
| `518a4e7f` | `f152dfe7` | 2026-04-21 | 3E/L | 1.00 | yes | work-queue follow-ups from the pull-queue E2E |
| `3cf28de9` | `0e4bab92` | 2026-05-01 | 1E/S | 1.00 | no | Claude Code surfaces the sovereign MCP tools |
| `d7e2c194` | `0214b8af` | 2026-05-01 | 1E/S | 1.00 | no | a `sovereign` shim resolves on PATH |
| `b811c8ab` | `b73c8df9` | 2026-05-01 | 3C/L | 1.00 | yes | glass-box voice — Wave 2 R3 and Wave 3 R5 |
| `8f0a7fe9` | `35bc5d4a` | 2026-05-01 | 3E/M | 1.50 | yes | voice eval Tier-B has a live runner |
| `6779ed1e` | `b6a24ba0` | 2026-05-11 | 2E/M | 1.00 | yes | atos_cmd/run.rs is split by runner stage |
| `5ca0c876` | `c28e82a9` | 2026-05-12 | 3C/S | 3.00 | yes | the drift report shows what is grounded, not only what is broken |
| `7ff63ae6` | `30782768` | 2026-05-12 | 3E/S | 3.00 | yes | drift_findings kind=path means what it says |
| `b4b8ffd4` | `e6e46586` | 2026-05-12 | 4C/S | 4.00 | yes | engineering_atlas code_anchors are anchors, not prose |
| `0066ef03` | `e3b4e481` | 2026-05-24 | 4C/L | 1.33 | yes | the vault typed-atom pass restores the dropped obsidian axes |
| `d0fedb28` | `f804fc62` | 2026-05-25 | 3E/M | 1.50 | yes | one select_route for both mesh routing bodies |
| `4cfc21ce` | `4f78f9f1` | 2026-05-25 | 3C/S | 3.00 | yes | dedup keeps multi-chunk depth on wiki |
| `fb0498c1` | `44096b0d` | 2026-05-25 | 2C/S | 2.00 | yes | atlas_weight is validated cross-corpus, not SEP-only |
| `071f0b8b` | `7dafcd6e` | 2026-05-25 | 1E/M | 0.50 | no | RAPTOR atlas progress is granular |
| `a21a4383` | `9897df42` | 2026-05-25 | 5C/L | 1.67 | yes | T5 anti-canonical synthesis quality |
| `66741bf4` | `e4c974b7` | 2026-05-26 | 3D/S | 3.00 | yes | the compaction-pressure sensor sees the whole prompt |
| `4f5d8d41` | `3d2fcb0f` | 2026-06-02 | 2E/L | 0.67 | yes | tech-debt refactor program — PR4-7 |
| `e75585f1` | `9c213529` | 2026-06-10 | 1E/L | 0.33 | no | transport migration — phone first, then mesh per-class |
| `bcc68868` | `3a098a87` | 2026-06-11 | 4A/M | 2.00 | yes | personal-scope retrieval includes watched-folder corpora |

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
| 3.00 | `7ff63ae6`, `5ca0c876` | the drift tool tells the truth about paths and about what is grounded |
| 2.50 | `fb0498c1`, `4cfc21ce` | wiki-side retrieval dedup and atlas weighting |
| 2.00 | `795ecbd6`, `163444e5`, `1b238605`, `3dcb4875`, `cad0e952` | the full-suite test gate: four sightings of the same flake family plus one with a found root cause |
| 1.80 | `fb4d0e0b`, `a6fc7e46`, `d0fedb28` | one routing surface keeps getting fixes the other does not; `f804fc62`'s select_route extraction is the structural fix |
| 1.50 | `2e16848a`, `4bd2d93d` | the two halves of the host disk reclaim (seat-anchored) |
| 1.50 | `910c7dc6`, `c2577ab1` | the pipeline pod phantom-cost note and its correction |
| 1.50 | `41d84a46`, `d0f0ffff` | the same in-flight-counter question from two sides |
| 1.20 | `8f0a7fe9`, `b811c8ab` | voice eval Tier-B and the glass-box voice waves that need it |
| 0.67 | `5f2908af`, `dd64c958` | the GTT ratchet finding and the note it corrects |

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
| 4.00 | `dc686fe6` | `de5c7ee4` | 4/S | A Grounded | a hop-exhausted refusal names its real cause |
| 4.00 | `141e0e68` | `9a8e7c1a` | 4/S | A Grounded | `svrn code watch` never writes zero-vector embeddings |
| 4.00 | `b4b8ffd4` | `e6e46586` | 4/S | C Well-cited | engineering_atlas code_anchors are anchors, not prose |
| 3.00 | `fb4d0e0b` | `18eee3fb` | 3/S | C Well-cited | every routing refusal emits an outcome record to join to |
| 3.00 | `3b19b6ee` | `ccb15537` | 3/S | C Well-cited | GLiNER2's threshold is calibrated for our label set |
| 3.00 | `9a237500` | `97433ad7` | 3/S | E Clean handoffs | `mesh plan` accepts a mesh-resolved model reference |
| 3.00 | `684e6be4` | `421416c7` | 3/S | C Well-cited | interop docs report the live surface, not a stale allowlist |
| 3.00 | `66741bf4` | `e4c974b7` | 3/S | D One sweep | the compaction-pressure sensor sees the whole prompt |
| 3.00 | `4cfc21ce` | `4f78f9f1` | 3/S | C Well-cited | dedup keeps multi-chunk depth on wiki |
| 3.00 | `7ff63ae6` | `30782768` | 3/S | E Clean handoffs | drift_findings kind=path means what it says |

Note what does NOT appear: the highest-ROI items in the store are not
necessarily here, because unvetted items are excluded however attractive
their numbers. That exclusion is the point of vetting.

## Full table — all 79 items

| Item (on the page) | was | Date | V | Axis | Cost | ROI | Vetted | Approach | Anchor | Objective |
|---|---|---|---|---|---|---|---|---|---|---|
| `5d120f16` | `78e51bad` | 2026-08-07 | 4 | A | S | 4.00 | no | unknown | backlog | wikipedia-article on-demand ingest must not die at enrichment |
| `141e0e68` | `9a8e7c1a` | 2026-07-25 | 4 | A | S | 4.00 | yes | derived | backlog | `svrn code watch` never writes zero-vector embeddings |
| `dc686fe6` | `de5c7ee4` | 2026-08-06 | 4 | A | S | 4.00 | yes | derived | backlog | a hop-exhausted refusal names its real cause |
| `b4b8ffd4` | `e6e46586` | 2026-05-12 | 4 | C | S | 4.00 | yes | derived | backlog | engineering_atlas code_anchors are anchors, not prose |
| `fb4d0e0b` | `18eee3fb` | 2026-08-06 | 3 | C | S | 3.00 | yes | derived | backlog | every routing refusal emits an outcome record to join to |
| `7ff63ae6` | `30782768` | 2026-05-12 | 3 | E | S | 3.00 | yes | derived | backlog | drift_findings kind=path means what it says |
| `2244b0bf` | `39d4f9a8` | 2026-07-29 | 3 | F | S | 3.00 | no | unknown | backlog | the distributed 122B load reaches the peer's daemon HTTP surface |
| `684e6be4` | `421416c7` | 2026-07-28 | 3 | C | S | 3.00 | yes | derived | backlog | interop docs report the live surface, not a stale allowlist |
| `4cfc21ce` | `4f78f9f1` | 2026-05-25 | 3 | C | S | 3.00 | yes | derived | backlog | dedup keeps multi-chunk depth on wiki |
| `9a237500` | `97433ad7` | 2026-07-30 | 3 | E | S | 3.00 | yes | derived | backlog | `mesh plan` accepts a mesh-resolved model reference |
| `5ca0c876` | `c28e82a9` | 2026-05-12 | 3 | C | S | 3.00 | yes | derived | backlog | the drift report shows what is grounded, not only what is broken |
| `3b19b6ee` | `ccb15537` | 2026-08-03 | 3 | C | S | 3.00 | yes | derived | backlog | GLiNER2's threshold is calibrated for our label set |
| `66741bf4` | `e4c974b7` | 2026-05-26 | 3 | D | S | 3.00 | yes | derived | backlog | the compaction-pressure sensor sees the whole prompt |
| `826acc14` | `654ae45a` | 2026-08-03 | 5 | D | M | 2.50 | yes | derived | backlog | settle whether GLiNER earns its 15m17s on the vault path |
| `ddb322a1` | `7767dc20` | 2026-08-06 | 5 | D | M | 2.50 | yes | derived | backlog | chat streaming reaches the 13x prefix-cache win |
| `928d148e` | `ccc99257` | 2026-08-05 | 5 | B | M | 2.50 | yes | derived | backlog | the routing bench baselines describe today's model |
| `b439e47f` | `ddb7c7bc` | 2026-07-27 | 5 | F | M | 2.50 | yes | derived | backlog | a default fresh install completes its setup tail |
| `f9701a50` | `fe12232b` | 2026-08-06 | 5 | D | M | 2.50 | yes | derived | backlog | cold start is priced honestly in mesh scheduling |
| `795ecbd6` | `246dc797` | 2026-08-07 | 2 | E | S | 2.00 | no | derived | backlog | the definition-of-done gate is trustworthy under load |
| `9fc63602` | `30c83eea` | 2026-08-07 | 2 | E | S | 2.00 | no | unknown | backlog | the lineage-boot-hook suite is green or honestly scoped |
| `fc1b5b2f` | `35a805e7` | 2026-08-07 | 2 | E | S | 2.00 | yes | derived | backlog | work-atlas claims are honest after a daemon restart |
| `4bd2d93d` | `3697c52d` | 2026-08-08 | 2 | E | S | 2.00 | yes | derived | seat | seat operations — host disk headroom |
| `bcc68868` | `3a098a87` | 2026-06-11 | 4 | A | M | 2.00 | yes | derived | backlog | personal-scope retrieval includes watched-folder corpora |
| `fb0498c1` | `44096b0d` | 2026-05-25 | 2 | C | S | 2.00 | yes | derived | backlog | atlas_weight is validated cross-corpus, not SEP-only |
| `23bb0080` | `5ee71c03` | 2026-08-06 | 2 | E | S | 2.00 | yes | derived | backlog | cache-audit comprehension tax excludes vendored paths |
| `163444e5` | `62715047` | 2026-07-27 | 2 | E | S | 2.00 | yes | derived | backlog | the definition-of-done gate is trustworthy under load |
| `d27f152c` | `6ea820ff` | 2026-07-26 | 4 | F | M | 2.00 | yes | derived | backlog | cross-network distributed inference: worker identity survives bridge re-dials |
| `1b238605` | `76a30db5` | 2026-07-31 | 2 | E | S | 2.00 | no | derived | backlog | the definition-of-done gate is trustworthy under load |
| `052fb2ef` | `84332ba0` | 2026-08-07 | 4 | B | M | 2.00 | no | derived | backlog | mesh routing: a shed peer plus a shed local must not fail the caller |
| `05ee26ff` | `91025679` | 2026-07-30 | 4 | A | M | 2.00 | yes | derived | backlog | SEP RAPTOR rows are not orphaned while still serving retrieval |
| `6f4928b8` | `c0bda2ab` | 2026-08-09 | 4 | A | M | 2.00 | yes | derived | backlog | native-grounding H0 — make the value unit measurable |
| `76f65528` | `c5678d34` | 2026-07-28 | 4 | A | M | 2.00 | yes | derived | backlog | named-model routing reaches the distributed primary |
| `5e06bd6e` | `c7f7bb64` | 2026-07-30 | 4 | F | M | 2.00 | no | unknown | backlog | the daemon's RSS growth is explained |
| `fa896d88` | `d2fc7549` | 2026-07-24 | 2 | E | S | 2.00 | yes | derived | backlog | frame provenance survives the Stop-hook distill |
| `3dcb4875` | `dd337207` | 2026-07-27 | 2 | E | S | 2.00 | yes | derived | backlog | the definition-of-done gate is trustworthy under load |
| `fcfa6289` | `ebe06232` | 2026-08-07 | 2 | E | S | 2.00 | yes | derived | backlog | the quality gate cannot pass without judging anything |
| `c2577ab1` | `ecc59649` | 2026-08-05 | 2 | E | S | 2.00 | yes | derived | backlog | the pipeline pod ledger does not report phantom cost |
| `8a5a3c80` | `f29260b5` | 2026-08-04 | 4 | F | M | 2.00 | yes | derived | backlog | one surface answers 'is this peer usable as a worker?' |
| `cad0e952` | `f60ff04e` | 2026-08-03 | 2 | E | S | 2.00 | no | derived | backlog | the definition-of-done gate is trustworthy under load |
| `04f05770` | `594d3c67` | 2026-07-13 | 5 | D | L | 1.67 | yes | derived | backlog | per-turn TTFT is owned by retrieval fan-out, not prefill |
| `a21a4383` | `9897df42` | 2026-05-25 | 5 | C | L | 1.67 | yes | derived | backlog | T5 anti-canonical synthesis quality |
| `9538d40a` | `0a3f47d1` | 2026-08-06 | 3 | B | M | 1.50 | no | unknown | backlog | a named request survives its sole holder disappearing |
| `9281bfda` | `188f06d0` | 2026-08-06 | 3 | E | M | 1.50 | yes | derived | backlog | the SCIP index keeps pace with commit velocity |
| `d407c986` | `30f49807` | 2026-08-06 | 3 | E | M | 1.50 | yes | derived | backlog | mesh reports which models a peer actually holds |
| `8f0a7fe9` | `35bc5d4a` | 2026-05-01 | 3 | E | M | 1.50 | yes | derived | backlog | voice eval Tier-B has a live runner |
| `45455f0e` | `5a56e565` | 2026-08-08 | 3 | E | M | 1.50 | no | derived | backlog | enrichment visibility — EnrichmentChecker sees every skip |
| `41d84a46` | `9e149ab7` | 2026-07-26 | 3 | E | M | 1.50 | yes | derived | backlog | settle whether the gossiped in-flight count sees inbound peer load |
| `d0f0ffff` | `a24cfdb5` | 2026-07-26 | 3 | E | M | 1.50 | yes | derived | backlog | settle whether the gossiped in-flight count sees inbound peer load |
| `4210020d` | `b680cf6b` | 2026-07-28 | 3 | D | M | 1.50 | yes | derived | backlog | the compute-child boundary tax is measured, not inferred |
| `73e5ae19` | `b87bb8ea` | 2026-08-02 | 3 | D | M | 1.50 | yes | derived | backlog | mesh warm-cache can start before the cluster fits |
| `66916ea3` | `bef03728` | 2026-08-06 | 3 | E | M | 1.50 | yes | derived | backlog | the queue-shed 503 carries Retry-After to the client |
| `a6fc7e46` | `d45489a3` | 2026-07-29 | 3 | E | M | 1.50 | no | unknown | backlog | streaming and non-streaming agree about slot availability |
| `3e97c963` | `d903dec7` | 2026-07-07 | 3 | E | M | 1.50 | no | unknown | backlog | SOLVE's pin-then-green path works on a no-tests repo |
| `d0fedb28` | `f804fc62` | 2026-05-25 | 3 | E | M | 1.50 | yes | derived | backlog | one select_route for both mesh routing bodies |
| `24827f08` | `16fc9204` | 2026-08-03 | 4 | F | L | 1.33 | yes | derived | backlog | the four knobs — no second user can run a distributed load |
| `539f2fbc` | `b234de5e` | 2026-07-28 | 4 | F | L | 1.33 | yes | derived | backlog | the distributed primary runs inside a sovereign-compute child |
| `0066ef03` | `e3b4e481` | 2026-05-24 | 4 | C | L | 1.33 | yes | derived | backlog | the vault typed-atom pass restores the dropped obsidian axes |
| `218255c3` | `ec73ffb4` | 2026-08-04 | 4 | F | L | 1.33 | yes | derived | backlog | on-prem pilot: the clean-VM rehearsal |
| `d7e2c194` | `0214b8af` | 2026-05-01 | 1 | E | S | 1.00 | no | derived | backlog | a `sovereign` shim resolves on PATH |
| `2e16848a` | `05a0eeaf` | 2026-08-08 | 1 | E | S | 1.00 | no | derived | seat | seat operations — host disk headroom |
| `910c7dc6` | `06e0ee5d` | 2026-08-05 | 1 | E | S | 1.00 | no | derived | backlog | the pipeline pod ledger does not report phantom cost |
| `3cf28de9` | `0e4bab92` | 2026-05-01 | 1 | E | S | 1.00 | no | unknown | backlog | Claude Code surfaces the sovereign MCP tools |
| `7a67a495` | `2f174e79` | 2026-08-08 | 2 | E | M | 1.00 | no | derived | backlog | CLI-contract nightly lane back to green |
| `b527c550` | `5056dbed` | 2026-08-05 | 2 | E | M | 1.00 | no | derived | backlog | cargo xtask quality is green or honestly baselined |
| `5f2908af` | `5780c214` | 2026-08-03 | 3 | E | L | 1.00 | no | derived | backlog | verifier-v0: a full epoch survives on the Halo |
| `9deaea9e` | `6d059d47` | 2026-08-07 | 2 | E | M | 1.00 | no | derived | backlog | the SCIP graph survives a daemon restart mid-rebuild |
| `eeb9fa61` | `7a6f4df2` | 2026-07-28 | 2 | E | M | 1.00 | yes | derived | backlog | desktop real-mode e2e has a mid-size fixture |
| `45a07661` | `96b259f4` | 2026-07-30 | 2 | E | M | 1.00 | no | unknown | backlog | the distributed child stops flapping |
| `770f5c0d` | `b1c9df45` | 2026-07-28 | 2 | E | M | 1.00 | yes | derived | backlog | desktop coverage is measured source-first, not spec-first |
| `6779ed1e` | `b6a24ba0` | 2026-05-11 | 2 | E | M | 1.00 | yes | derived | backlog | atos_cmd/run.rs is split by runner stage |
| `b811c8ab` | `b73c8df9` | 2026-05-01 | 3 | C | L | 1.00 | yes | derived | backlog | glass-box voice — Wave 2 R3 and Wave 3 R5 |
| `c61d9a8c` | `ca8523f2` | 2026-07-24 | 2 | E | M | 1.00 | yes | derived | backlog | peer continuity posture rides the mesh transport |
| `c19d6f30` | `d2af7720` | 2026-08-04 | 2 | E | M | 1.00 | yes | derived | backlog | conversation retrieval is gated by the comprehensive bench |
| `18c3bd54` | `d7404ac7` | 2026-08-06 | 1 | E | S | 1.00 | no | derived | seat | seat handoff — open business for the next seat |
| `518a4e7f` | `f152dfe7` | 2026-04-21 | 3 | E | L | 1.00 | yes | derived | backlog | work-queue follow-ups from the pull-queue E2E |
| `4f5d8d41` | `3d2fcb0f` | 2026-06-02 | 2 | E | L | 0.67 | yes | derived | backlog | tech-debt refactor program — PR4-7 |
| `071f0b8b` | `7dafcd6e` | 2026-05-25 | 1 | E | M | 0.50 | no | derived | backlog | RAPTOR atlas progress is granular |
| `dd64c958` | `8de0d918` | 2026-08-03 | 1 | E | L | 0.33 | no | derived | backlog | verifier-v0: a full epoch survives on the Halo |
| `e75585f1` | `9c213529` | 2026-06-10 | 1 | E | L | 0.33 | no | derived | backlog | transport migration — phone first, then mesh per-class |

## The protocol is already in use

2 backlog item(s) on the page were NOT migrated by this pass —
they were banked directly by the seat while it ran, carrying the header
block, an axis, and an Approach line, through the intake duty added to the
comaintainer skill. That is the first evidence that the format survives
contact with someone other than its author, which is worth more than any
number in the tables above.

## How to audit or redo this

Each migrated note carries its original **verbatim** below a
`--- the original note, verbatim ---` marker, so no scoring decision hides
what it was scored from. To re-score an item, edit its `Value:`/`Cost:`
lines; to vet one, add a `Done-when:` and an `Evidence:`. To see the result:

```
./scripts/co-backlog.py --open     # the ranked heap
./scripts/co-backlog.py --pull     # the top chunk as an order draft
```
