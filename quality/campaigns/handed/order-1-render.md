---
schema: work-order/v1
id: handed-1-render
status: open
drafted: 2026-09-17
approved: pending
serves: handed
campaign: handed
lane: structural — the gate's one funnel stamps its judgement, serve.rs projects it, and `TurnFrame::Complete` cannot be built without one (E0063)
engine: ralph pool; REVIEW-build rows the stronger model
budget: 4 rows; TEST on all four; covered by REVIEW-audit-hd-1
---

# Order: handed-1-render — every turn's terminal value carries a verdict

## Objective

`kernel_types::Answer` already cannot exist without a `Judgement`, but the Answer stops at the gate's edge:
every handler flattens `outcome.answer` to a `String` (attached_doc.rs:527, complex_task.rs:363,
knowledge_query.rs:1793, simple.rs:213, streaming.rs:1846/:3410) and `TurnFrame::Complete` reports the gate
only as `metadata: Option<TurnMetadata>` whose `grounding_gate` is an untyped `Value`
(projection.rs:214-215). This order does NOT thread the Answer out. The gate has ONE funnel and it already
stamps `episode_id` into the outcome meta, which the daemon persists with the message row; this order adds
`judgement` beside it, makes the post-gate stages that REPLACE gated text overwrite it, and makes
`Complete.judgement` a required field that `serve.rs` builds from one projection — the stamped judgement,
or `never_ran` naming the routed intent. A fourth row carries the same verdict on `MessageRefined`, which
swaps the answer's text after `Complete`.

Cut at round 1 (2026-09-17): the `template:<id>` `Attribution` substitution (a new kernel semantics for 14
exits), the `Response` and `StreamHandle` carriers and the two ~1,000-line spawn rows (a second carrier of
one turn's result can disagree with the metadata projection — principle 8), and widening the release
census. What is given up, named: a future gated exit that forgets to persist its meta reports `never-ran`
instead of passed/failed. That is understatement, never overclaim, and the hd-7 grounded probe catches it.

## Premises (verified 2026-09-17, file:line)

- **The funnel.** `gate_answer_with_progress` — sovereign/crates/sovereign-core/src/runtime/grounding/gate.rs:37;
  `gate_answer` (:7) delegates to it, so all six production gate call sites go through it:
  streaming.rs:610 (shared by the KQ and deep spawns), collaboration.rs:656, attached_doc.rs:724,
  complex_task.rs:352, knowledge_query.rs:1784, simple.rs:204. Its doc already says it is "the ONE funnel
  through which every gate decision reaches the local grounding journal … It stamps `episode_id` into the
  outcome meta, which the daemon persists with the message row" (gate.rs:30-36). The stamp itself is
  `m.insert("episode_id", …)` inside `record_gate_decision`, gate.rs:271-276 (`outcome.meta.as_object_mut()`
  at :270).
- `GateOutcome { answer: kernel_types::Answer, meta: serde_json::Value, claims }` —
  grounding/mod.rs:559-582 (`answer` :572, `meta` :575). `Answer::judgement(&self) -> &Judgement`
  kernel-types/src/answer.rs:389.
- `Judgement` derives `Serialize, Deserialize` (kernel-types/src/judgement.rs:319-320); four constructors
  `passed`/`failed`/`could_not_judge`/`never_ran` at :346/:351/:357/:363, each taking a `Reason` by value;
  `Reason::literal(&'static str)` :245; `Verdict` is kebab-case on the wire (:91).
- **Where the stamp is persisted.** Handlers write `outcome.meta` as `metadata["grounding_gate"]`:
  attached_doc.rs:1456-1457, complex_task.rs:548-549, simple.rs:354-355, knowledge_query.rs:2109,
  streaming.rs:2366 (KQ) and :3477 (deep). `project_turn_metadata` copies it out verbatim
  (projection.rs:252, `grounding_gate: Option<Value>` :215).
- **The five post-gate stages that replace gated text** (each already in scope of a
  `grounding_gate_meta` binding):
  - GK rescue — streaming.rs:1897 (`full_text = rescued`), which already mutates the same meta at
    :1900-1904 (`meta["rescued_from"]`, `meta["action"] = "gk_rescue_released"`); binding `let mut` :1846.
  - Authority guard `Blocked` — streaming.rs:2007 (`kq_stream`, binding `let mut` :1846),
    streaming.rs:3575 (`deep_stream`, binding at :3410 is **NOT `mut`** — the row adds it),
    knowledge_query.rs:1938 (binding `let mut` :1681), simple.rs:278 (binding `let mut` :176).
    `GuardVerdict::Blocked { replacement, .. } => replacement` at all four; `GuardVerdict` is
    authority_guard.rs:179 and its doc says "`Blocked` answers are REPLACED by `replacement`" (:175).
- **`TurnFrame::Complete`** — sovereign-contracts/src/types/turn.rs:110-144 (six fields, five of them
  `Option`/`Vec` with `skip_serializing_if`). Production builds: serve.rs:306 (graceful guard), :425
  (`drive_stream_handle`), :506 (`serve_non_streaming_turn`) — the only three. Exhaustive destructures:
  serve.rs:554 (`Collector`), sovereign-turn-client/src/lib.rs:4256 (`drain_turn`),
  sovereign-mobile/src-tauri/src/remote/stream.rs:279. Test builds:
  sovereign-contracts/tests/main/turn_wire_form.rs:61,76,133 and
  sovereign-mobile/src-tauri/tests/turn_wire.rs:257,371,459. Every other `TurnFrame::Complete` match uses
  `..` (`git grep -n 'TurnFrame::Complete {' -- '*.rs'`, 32 hits).
- **serve.rs already projects from the persisted metadata** after the stream ends: `message_metadata(…)`
  then `project_message_metadata` / `project_epistemic_state` / `project_task` / `project_turn_metadata`
  at :420-431 (streaming) and :501-512 (non-streaming). serve.rs is 645 lines.
- `CollectedTurn` derives `Default` (serve.rs:521) and `Collector` is `#[derive(Default)]` (:540);
  `collect_turn` returns `Ok(collector.inner.…clone())` at serve.rs:637-644 — it already returns `Err` on a
  `StreamError` (:629-636) but NOT when no `Complete` arrived.
- `TurnOutcome` derives `Default` (sovereign-turn-client/src/lib.rs:119-120); `drain_turn` uses it as an
  accumulator (:4219) and mutates it at 8 sites — 2 in the `Token` arm, 6 in the `Complete` arm
  (:4256-4270); it already returns `Err` when the socket closes before `Complete` (:4222-4227).
  `send_message` builds a `TurnOutcome` from `MessageResponseWire` (:722-738; the wire struct :3707-3720).
- `MessageResponse`: sovereign-daemon/src/turn_http.rs:326 (filled at :597-602 from a `CollectedTurn`),
  sovereign-server/src/routes.rs:55 (filled at :320-325 from a `CollectedTurn`).
- **`MessageRefined`.** `MessageRefinedPayload { conversation_id, message_id, new_content }` —
  sovereign-contracts/src/types/mod.rs:450-457; `TurnNotice::MessageRefined(MessageRefinedPayload)`
  types/turn.rs:377. **Three** production emit sites, all in
  sovereign-core/src/runtime/collaboration.rs — :679 (re-gate rejected the refinement, the original text is
  re-sent), :705 (the refined text is accepted), :729 (`NoChange | Failed`, original re-sent). The other two
  constructions are TESTS: turn_approval.rs:609 (inside `#[cfg(test)]` at :484) and turn_http.rs:1971
  (inside `#[cfg(test)]` at :1914); a third test build is turn_wire_form.rs:358.
  `run_post_stream_refinement(… original_content: &str, …, original_metadata: Option<serde_json::Value>, …)`
  — collaboration.rs:482-506; `original_metadata` is already read at :512 and is moved into the rewritten
  `Message` at :693, i.e. after :679 and never in the :729 arm. The re-gate is CONDITIONAL — inside
  `if let Some(guard) = grounding_guard {` (:594) — and its `outcome` is dropped after `outcome.meta` is
  read for `action` (:665-669). The desktop forwards the payload whole (`app_handle.emit("message-refined",
  payload.clone())`, sovereign-desktop/src-tauri/src/commands/chat.rs:406-407), so a new field rides to the
  UI without a frontend change; mobile only names the variant (remote/stream.rs:405).
- **Surfaces.** `svrn chat ask --format json` hand-builds its payload in `render_json_typed`,
  sovereign-cli-llm/src/chat_cmd/ask.rs:393-412. Desktop `MessageCompletePayload` chat.rs:29-35, emitted
  from the `Complete` arm at chat.rs:777. The frontend type is hand-written
  (`ChatView.svelte:33,825`); an added Rust field it does not read costs no `npm run check`.
- **Type reachability.** `pub use kernel_types::Grain;` — sovereign-contracts/src/types/mod.rs:155, the
  re-export precedent. `kernel-types` is also in `thin_surfaces.may_reach` (quality/ARCH_LAYERS.toml:1196-1204),
  so neither the re-export nor a direct dep is layer-blocked; the row uses the re-export (one accessor).
- **Size.** arch-gate: `LINE_LIMIT = 1200`, `GROWTH_SLACK = 50` for oversized files, and the 800-1200
  approach band is a COUNTER ratchet with **no slack** (corpus-engine/xtask/src/arch_gate.rs:14, :34, :38,
  :291-313); baseline `quality/baselines/approach_band.txt` = 207 files / 202,703 lines. `arch-gate` is
  `enforcement = "hard"`, `runs_in = ["prepush", …]` (quality/instruments.toml:200-211) and the `prepush`
  trigger is `on_fail = "block"` (:2473). Of every file these rows touch, **exactly one is inside the band**:
  `collaboration.rs` at 885. The rest are either under 800 (gate.rs 688, serve.rs 645, types/turn.rs 610,
  routes.rs 673, ask.rs 512, simple.rs 380, turn_approval.rs 747, turn_wire_form.rs 696, mobile
  stream.rs 458 / turn_wire.rs 598) or already oversized with slack — streaming.rs 4,673 vs baseline 4,689;
  knowledge_query.rs 2,131 vs 2,127; grounding/tests.rs 2,944 vs 2,944; types/mod.rs 1,796 vs 1,796;
  turn-client lib.rs 4,811 vs 4,800; turn_http.rs 2,105 vs 2,105; turn_surface.rs 1,636 vs 1,636; desktop
  chat.rs 1,479 vs 1,479 (quality/baselines/oversized.txt).
- **The trace must be greppable.** The CLI subscriber is built with `.with_target(false)`
  (sovereign-cli-shared/src/tracing_init.rs:35), so a `target:` string never appears in the printed line —
  which is why the message text below carries the literal `turn.verdict`. `RUST_LOG` is honoured verbatim
  (`EnvFilter::try_from_default_env`, tracing_init.rs:28) and a DOTTED target parses as a directive
  (`Directive::parse` accepts any non-`=` char after the first, tracing-subscriber-0.3.23
  directive.rs:300-306; the daemon's own filter already carries `gate.call=info` and its test expects
  "dotted targets included", sovereign-cli-daemon/src/lib.rs:65, :358).

## Steps

1. **The funnel stamps, and the five replacers overwrite** (`REVIEW-build-hd-1-stamp`).
   In `record_gate_decision` (gate.rs), beside the `episode_id` insert at :271-276, add
   `m.insert("judgement", serde_json::to_value(outcome.answer.judgement()).unwrap_or(Value::Null))`
   (clone the judgement before taking the `as_object_mut` borrow). Then the five stages that replace GATED
   text overwrite the stamp — and ONLY when `grounding_gate_meta` is already `Some`, so no stage mints a
   `grounding_gate` block on a turn the gate never touched (that block's absence is load-bearing:
   projection.rs:214 "Absent when the gate was off or out of scope"):
   - GK rescue, streaming.rs:1900-1904 — one line beside `meta["action"]`:
     `never_ran("answer", Reason::literal("general-knowledge rescue replaced the gated abstention with a caveated parametric answer"))`.
   - The four authority-guard `Blocked` arms (streaming.rs:2007, :3575, knowledge_query.rs:1938,
     simple.rs:278) — `failed("answer", Reason::literal("the authority guard blocked the released text; the reader sees the guard's replacement"))`.
     `streaming.rs:3410` gains `mut`.
   Reasons are `&'static str` literals: `Reason::literal` panics only on a placeholder, so no `format!`,
   no `expect`, no panic risk, and the violation counts stay where they already are (the
   `authority_guard` metadata block).
   One test in `grounding/tests.rs` (the file that already drives `gate_answer` at :465, :909, :943):
   `the_gate_funnel_stamps_the_judgement_it_released` — run the funnel and assert
   `outcome.meta["judgement"]["verdict"]` parses and equals `outcome.answer.judgement().verdict()`.
   grounding/tests.rs may grow ≤ 40 lines (2,944 baseline + 50 slack).
2. **`Complete` requires it; one projection builds it** (`REVIEW-build-hd-1-complete`).
   - `TurnFrame::Complete { …, judgement: Judgement }` — required, **no** `serde(default)`, no
     `skip_serializing_if`. Re-export `Judgement`, `Verdict` and `Reason` in
     sovereign-contracts/src/types/mod.rs beside `pub use kernel_types::Grain;` (:155), with the same
     one-line rationale that re-export carries.
   - `pub fn project_judgement(meta: &Option<Value>) -> Option<Judgement>` in
     sovereign-contracts/src/types/projection.rs, immediately after `project_turn_metadata` (:240-262):
     reads `meta["grounding_gate"]["judgement"]`, `serde_json::from_value`, and degrades a malformed value
     to `None` — the same bargain the other projectors make (projection.rs:236-239).
   - serve.rs gains ONE private `fn turn_judgement(conversation_id, message_id, metadata: &Option<Value>,
     served: Option<&TurnMetadata>) -> Judgement` that (a) returns the projected judgement, else
     `Judgement::never_ran("answer", Reason::new(format!("no grounding gate ran on this turn (routed intent: {})", intent)))`
     where `intent` is `served.and_then(|s| s.routed_intent.as_deref()).unwrap_or("none reported")`, and
     (b) emits the one trace:
     `tracing::info!(target: "turn.verdict", conversation_id, message_id, verdict = j.verdict().as_str(), reason = %j.reason(), source = <"gate"|"absent">, "turn.verdict: serve_turn complete")`.
     **The message text carries the literal `turn.verdict` on purpose** — the CLI subscriber prints no
     target (tracing_init.rs:35) and hd-7's paste greps this line. All three `Complete` builds call it;
     the graceful guard (:306) calls it with `(&None, None)` — it has no message row at all, so
     "no grounding gate ran" is the honest answer and `source=absent` is the fact. One decider, no arms.
   - Readers the compiler names: `CollectedTurn` loses `Default` (a `Judgement` has none, deliberately —
     there is no default verdict), so `Collector` accumulates `message_id`/`text` separately and builds the
     `CollectedTurn` in its `Complete` arm; `collect_turn` returns `Err` when no `Complete` arrived, never a
     default. Same shape for `TurnOutcome` in `drain_turn` (8 mutation sites, 2+6). `MessageResponseWire`,
     both `MessageResponse` structs (filled from `turn.judgement`), mobile `remote/stream.rs:279` binds
     `judgement: _` (no new dep), and the byte pins in turn_wire_form.rs / turn_wire.rs.
   - Extend sovereign-mesh/tests/main/turn_surface.rs:104 to assert the frame's judgement, and add one turn
     over `install_corpus` (:176-190) asserting a retrieving turn's verdict is NOT `never-ran`.
   - One sentence on the `sovereign-turn-client` line of sovereign/SYSTEM_OVERVIEW.md (:251).
3. **Surfaces print it** (`hd-1-surfaces-print`). `"judgement"` into `render_json_typed`'s payload
   (ask.rs:398-403); `judgement` on the desktop's `MessageCompletePayload` (chat.rs:29-35), bound from the
   `Complete` arm (:777). Rust only — the hand-written TS type is not touched and does not read the field.
4. **`MessageRefined` carries it** (`REVIEW-build-hd-1-refined`).
   `MessageRefinedPayload` gains `judgement: Judgement` (required). Without it, refined text is shown under
   a verdict describing text the reader no longer sees. At the three production sites:
   - :679 and :729 re-send the ORIGINAL content, so the honest verdict is the one that content already
     carried: `project_judgement(&original_metadata)` (step 2's projector — one decider), falling back to
     `never_ran("answer", Reason::literal("the original answer was released without a gate on this turn"))`.
   - :705 is the refined text. Bind `let mut refined_judgement: Option<Judgement> = None;` before the
     `if let Some(guard) = grounding_guard` at :594 and set it from `outcome.answer.judgement().clone()`
     right where `action` is read (:665) — the re-gate outcome that is dropped today. When no guard was
     carried the re-gate did not run: `never_ran("answer", Reason::literal("post-stream refinement re-synthesised the answer with no re-gate on this path"))`.
   **Line budget: `collaboration.rs` ends at ≤ 885 lines.** It is the ONE file these four rows touch that
   sits inside arch-gate's 800-1200 approach band, and that band has no slack. The way to pay it is the
   mechanism itself: one local closure or a `MessageRefinedPayload::new(conversation_id, message_id,
   content, judgement)` constructor in types/mod.rs (50 lines of slack there) collapses the three 5-line
   struct literals. If it cannot be met without deleting code this row does not own, that is §6 — the
   answer is an accepted raise ledgered in SYSTEM_OVERVIEW §10 with an `origin/main` re-pin, which is the
   operator's call, not a worker's `--update-baseline`.

## Seams

- Does NOT touch: `TurnMetadata` / the `grounding_gate` block's existing keys, `GateOutcome`, the four gate
  door bodies (`release_as` / `release_held` / `release_flawed` / `abstain`), `Judgement::roll_up`,
  `kernel_types::Answer`, the epistemic ledger, or any rendering. No new door, no new type, no census change.
- **Named wire break, now unowned.** `Complete.judgement` and `MessageResponseWire.judgement` are required
  with no serde default, so a surface built after step 2 cannot finish a turn against a daemon built before
  it — and ralph/PROMPT.md §7 forbids the loop restarting the daemon. Round 1 deleted
  `HUMAN-hd-7-daemon-at-head` because the bench DEMO is in-process and does not need it, which is true; the
  consequence is that `svrn chat ask` and the desktop are broken against the resident daemon from step 2
  until the operator next restarts it. Nothing in the loop uses those two. The 0-row alternative, if the
  operator wants it, is a NAMED serde substitution (`#[serde(default = …)]` yielding
  `never_ran("answer", "this frame came from a host built before the verdict field")`) — principle 6 permits
  a named substitution and it costs no compile-time coverage, since the E0063 plant is on the BUILD side.
- **Known dark spot, priced at 2 lines and not taken.** `DAEMON_TRACING_FILTER`
  (sovereign-cli-daemon/src/lib.rs:53-85) does not list `turn.verdict`, so a daemon-served turn's verdict
  line is visible only under `RUST_LOG`. hd-7's instrument is the in-process bench, so this is not needed;
  adding it is one entry plus one line in `daemon_filter_lists_grounding_targets` (:355) if a daemon-served
  DEMO is ever taken.
- Files other rungs also touch: `sovereign/SYSTEM_OVERVIEW.md` (every rung; uncommitted peer edits on the
  tree now); `sovereign-server/src/routes.rs` (hd-2 converts sovereign-server in place — if hd-2 lands
  first, step 2's edit there is a field on the same struct, no conflict);
  `sovereign-mesh/tests/main/turn_surface.rs:181` passes `mesh_sharing` to `CorpusIndex::create`, whose
  signature hd-5 does not change (hd-5 changes a serde default, not the constructor).
- Peer state now: `sovereign/crates/sovereign-desktop/src-tauri/src/commands/chat.rs` and
  `sovereign/SYSTEM_OVERVIEW.md` carry uncommitted edits. `git merge` refuses to overwrite locally modified
  files and the pool turns that into a halt, so `hd-1-surfaces-print` (a lane row) must not run until the
  desktop file is committed.
- The running domains pool may relocate `sovereign-daemon` / `sovereign-api` / `sovereign-mesh` modules
  (`dm-daemon-api-edge`, `dm-daemon-cli-composition`); `turn_http.rs` and `turn_surface.rs` paths may move.
  A `file:line` in these rows is a locator — re-find the symbol by grep.

## Done when

- Every landing row's commit body pastes its PLANT red and the same gate green after `git checkout --`:
  step 1 — delete the `m.insert("judgement", …)` line → `TEST(sovereign-core)` red on
  `the_gate_funnel_stamps_the_judgement_it_released`;
  step 2 — (a) write `judgement: hint.clone()` in serve.rs's graceful-guard `Complete` → LINT red **E0308**,
  (b) delete `judgement` from `drive_stream_handle`'s `Complete` → LINT red **E0063**;
  step 4 — delete `judgement` from the `MessageRefinedPayload` at collaboration.rs:705 → LINT red **E0063**.
- `./scripts/sovereign-lint.sh --human --full` and `./scripts/sovereign-test.sh --human` exit 0.
- The ambient path is gone — each returns NOTHING:
  - `rg -U --type rust 'TurnFrame::Complete \{[^}]*\}' sovereign/crates/sovereign-core/src/runtime/serve.rs | rg -v judgement`
  - `git grep -n 'metadata: None,' -- sovereign/crates/sovereign-core/src/runtime/serve.rs` returns only the
    graceful guard's line, and that build carries a `judgement`.
- These FIND their subject (a required field is proven by its presence, not its absence):
  `git grep -n 'judgement: Judgement' -- sovereign/crates/sovereign-contracts/src/types/turn.rs sovereign/crates/sovereign-contracts/src/types/mod.rs`
  and `git grep -n '"judgement"' -- sovereign/crates/sovereign-core/src/runtime/grounding/gate.rs`.
- `cd corpus-engine && cargo xtask arch-gate` exits 0 (the `collaboration.rs` budget in step 4).

## Kill

- A post-gate stage produces text that none of passed / failed / could-not-judge / never-ran describes
  honestly — e.g. a partial rescue that keeps half a gated answer: stop and redraw. That is HT bar
  `hd-structural`'s kill in spirit and the only judgment step 1 carries.
- The stamped judgement cannot be read back at `serve.rs` for a turn the gate DID run — i.e. a handler
  persists `grounding_gate` without the funnel's meta: the funnel is not the funnel. Stop and name the site.
- `collaboration.rs` cannot carry the verdict within its line budget without deleting code this order does
  not own: stop, and hand the operator the accepted-raise decision (§Steps 4).
- More than 3 turn exits need a verdict `kernel_types::Verdict` lacks (HT bar `hd-structural` kill).
