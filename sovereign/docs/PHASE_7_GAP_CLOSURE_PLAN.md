# Phase 7 gap-closure plan

The two demo docs (`PANICKED_ENGINEER_DEMO.md`,
`CLINICAL_TELEMED_DEMO.md`) explicitly listed five capabilities as
"wired but partial" or "trait + stub ready." This doc enumerates
each gap, names the work to close it, and proposes a sequencing.

> The audit's "non-empty floor" already holds without any of these
> — `agent`/`committed`/`observed` are persisted end-to-end.
> Everything below is **additive**: each gap closure widens the
> stream of decisions captured, but the floor doesn't depend on
> any one of them.

---

## Inventory

Verified via grep + file read on 2026-04-29:

| # | Gap | Symptom | Verified |
|---|---|---|---|
| **A** | `DecisionExtractor` middleware not in default pipeline | The `sovereign-coder` middleware list in `default_pipelines.toml` is `[approval_gate, session_briefing, context_injector, tool_injector, artifact_surface]`. `decision_extractor` is missing → per-turn extraction never fires in production despite passing all 7 unit tests. | `default_pipelines.toml:18` |
| **B** | `StructuralNudgeGenerator` not wired into MCP responses | The generator is fully implemented and tested, but no caller ever invokes `pending_text(...)` against a real observation. The "[note worth recording? …]" line never appears in a real tool response. | grep over crates: only test references. |
| **C** | `DiffDecisionExtractor` has no production backend | The trait + adapter + prompt builder + parser are all implemented and tested with a stub. There's no `LocalLlmBackend` impl that calls the daemon's primary slot. Audit's `extracted` stream is therefore agent-side only (per-turn middleware, when wired). | `notes/diff_extract.rs::DecisionExtractorBackend` only has `StubBackend` in tests. |
| **D** | `--recover` doesn't read `messages` table | `audit_recover` replays only the `tool_call_log` against `ToolPatternMatcher`. ResponseMiner is never run over conversation transcripts → SIGKILL'd sessions lose `inferred`-source notes. | `audit_recover.rs::cmd_audit_recover` body. |
| **E** | `DiffDecisionExtractor` not invoked from `sovereign audit` | Even with a backend, no caller runs the extractor at audit-assembly time over the cumulative session diff. The `extracted` stream stays per-turn-only. | grep: no `DiffDecisionExtractor::new(...)` outside tests. |

---

## Gap A — Default pipeline includes `decision_extractor`

### Goal
Per-turn decision extraction fires for every `sovereign-coder`
session in production. `[Noted: "<snippet>". Auto-recording unless
corrected.]` lines actually appear in the agent's prompt context
on turn N+1 after a decision-shaped response on turn N.

### Approach
1. Add `decision_extractor` to the `sovereign-coder` middleware
   chain in `commonwealth-core/src/default_pipelines.toml` —
   AFTER `artifact_surface` per the spec.
2. Verify the executor's middleware registry resolves the id.
   The `Middleware::id()` on `DecisionExtractor` already returns
   `"decision_extractor"`; just need to confirm the registry has
   a builder for it. (Look in `routes_inference.rs` or wherever
   the pipeline assembles middleware by id.)
3. Add a registry entry if missing.

### Files
- `commonwealth/crates/commonwealth-core/src/default_pipelines.toml`
- `commonwealth/crates/commonwealth-api/src/middleware/mod.rs` — the
  `MiddlewareRegistry` (verify or add the `"decision_extractor" =>
  Arc::new(DecisionExtractor::new())` arm).

### Tests
- New integration test: spin up a pipeline by id `sovereign-coder`,
  verify the middleware list includes `decision_extractor`.
- Existing 7 unit tests on `DecisionExtractor` keep guarding the
  per-turn behaviour.

### Risk
- **Low**. Pure config + (maybe) one-line registry addition. No
  schema changes, no API surface changes. The middleware is
  already idempotent and stateless beyond the
  `pending_decision` field that's already plumbed end-to-end.

### Estimate
~30 minutes.

### Why first
Smallest possible fix, biggest demo-claim impact. Both demo docs
state per-turn extraction "fires" in production; this makes that
true.

---

## Gap B — Wire `StructuralNudgeGenerator` into MCP responses

### Goal
After a tool call that touches structural signals, the response
body carries an appended `[note worth recording? …]` line. Rate-
limited to 1 per 15 calls, gated on signal score.

### Approach
The challenge: at MCP `tools/call` time we don't have direct file-
diff visibility. The agent's actual `Edit`/`Write` calls happen in
the IDE (Claude Code, opencode), not through MCP. So the nudge
needs a different observation source.

Two viable paths:

**Path 1 — Watch the working tree.** Hook into the existing
`SpecWatcher` infrastructure (`sovereign-tools/src/spec_watcher.rs`)
or the reindexer's FS watcher (`sovereign-mesh/src/reindexer.rs`)
to accumulate a `(files_changed, diff_text)` window since the last
nudge. On each tool call, build a `DiffObservation` from that
window. Cleanest, but couples the nudge to a watcher start.

**Path 2 — Read git's working-tree diff at observation time.**
On each MCP `tools/call`, run `git diff HEAD --numstat` (cheap)
plus `git diff HEAD` (capped, ~50 KB) to build the
`DiffObservation`. Per-call overhead is ~5–20ms which is below the
typical tool latency. Less wiring; runs in the existing tool-call
hot path.

Recommend **Path 2**. It's the lower-coupling option and gives the
nudge correctness independent of which watcher is running.

### Concrete shape

In `sovereign-mesh/src/mcp_router.rs::handle_tool_call`, after the
pattern matcher fire-and-forget spawn, add a synchronous nudge
check:

```rust
let nudge_line = match build_nudge_observation(&repo_root, ...) {
    Some(obs) => nudge_generator.pending_text(&obs).map(|(line, _)| line),
    None => None,
};
// Append nudge_line (if any) to the tool response body before returning.
```

`build_nudge_observation` shells out to git from the repo root,
caps the diff at 50 KB, and resolves spec invariant keywords
once per session via the existing
`extract_spec_invariant_keywords` helper.

The `StructuralNudgeGenerator` itself goes onto an `Arc` Extension
on the router — same pattern as `ToolPatternMatcher`.

### Files
- `sovereign-mesh/src/mcp_router.rs` — Extension + per-call hook.
- New helper `sovereign-tools/src/notes/nudge.rs::build_observation_from_git`
  to keep the git-shell logic with the rest of the nudge module.

### Tests
- Pure: `build_observation_from_git` against a tempdir git repo
  with known modifications; assert the resulting
  `DiffObservation` has the right files / diff text.
- Wire: extend `pattern_observation_e2e.rs` (or a new e2e file) to
  drive a sequence of tool calls in a tempdir repo with a
  staged edit, assert the response contains the nudge line.

### Risk
- **Medium**. The git-shell-out is fast on small repos but could
  be slow on a giant monorepo with millions of unstaged lines.
  Mitigation: timebox the git call (250ms) and skip the nudge on
  timeout — better silence than a slow tool response.
- The nudge is a TEXT line appended to `call_tool_text`'s output.
  We need to verify MCP clients render it cleanly. The agent
  parser usually treats the body as JSON-or-text; appending a
  bracketed line is benign for text outputs but might break
  JSON-output tools. Mitigation: only append when the response is
  the text variant (`StepOutput::Text`).

### Estimate
~3 hours including tests.

---

## Gap C — Production `DecisionExtractorBackend` over the daemon's primary slot

### Goal
The `DiffDecisionExtractor` has a real LLM behind it in daemon
mode. End-of-week audits include `extracted`-source decisions
distilled from the cumulative session diff plus existing-notes
context.

### Approach
Implement `LocalLlmBackend` in
`sovereign-tools/src/notes/diff_extract_backend.rs` (new file).
It calls the daemon's `/v1/chat/completions` endpoint with:

- `model = "<configured primary slot>"` (typically Qwen3.5-32B).
- `messages = [system: <empty>, user: build_prompt(req)]`.
- Grammar constraint or `response_format = json_object` if the
  model supports it (the existing M5+ infra has LLGuidance — see
  `project_grammar_constrained_phase1` memory).
- `max_tokens = ~800` — cap so the model can't run away with
  the audit budget.

Parse the response with the existing `parse_extractions` helper.

### Wiring
The backend takes a daemon URL + model id at construction. The
caller (audit assembly, gap E below) constructs one with values
read from the user's `~/.config/sovereign/config.toml`.

In MCP-only mode (no daemon running) the audit warns and skips
the extracted-source contribution rather than failing. The other
streams keep the floor non-empty.

### Files
- New: `sovereign-tools/src/notes/diff_extract_backend.rs` with
  `LocalLlmBackend`.
- `sovereign-tools/src/notes/mod.rs` — export it.
- Likely need `reqwest` (already a dep on the crate via existing
  recipe-author code).

### Tests
- Unit: stub a tiny HTTP server (axum on port 0) that returns a
  canned JSON-per-line body, drive `LocalLlmBackend::extract`
  against it, assert the parsed extractions.
- Skip-on-no-daemon: a test that points at an unbound port and
  asserts `Err(...)` plus the extractor's existing
  `backend_error_yields_empty_vec` invariant takes over.

### Risk
- **Medium**. JSON-per-line output from a non-grammar-constrained
  model is messy. The `parse_extractions` helper already
  tolerates non-JSON lines, but real-world output may be
  prose-heavy. Mitigation: enforce LLGuidance JSON schema if
  available; otherwise truncate to first-N JSON lines.
- Token budget: 80 KB diff + existing-notes context can blow past
  most local models' context. The cap is already in
  `MAX_DIFF_INPUT_BYTES = 80_000` but a 32K-context model still
  truncates. Mitigation: detect context-window exhaustion via
  the daemon's response and fall back to a per-feature scope.
- Latency: a single audit-assembly LLM call could take 30+
  seconds on the 32B. The audit runs on user demand; that's
  acceptable.

### Estimate
~4–6 hours including the prompt-tuning round-trip.

---

## Gap D — `--recover` reads the `messages` table for inferred-source notes

### Goal
A SIGKILL'd session's conversation transcript gets mined for
decision phrases on the next `sovereign audit --recover` run.
`source='inferred'` notes land for sessions that had assistant
turns but no `tool_call_log` patterns.

### Approach
Extend `audit_recover::cmd_audit_recover` to also:

1. Open the `sovereign-store` `messages` table (read-only).
2. Group rows by `conversation_id` (which is the session id in
   the daemon's wire format — verify this mapping).
3. For each session not yet covered by step 1 (or even if covered),
   read assistant-role rows, run `response_mine::mine` over each
   `content`, and write `source='inferred'` notes.
4. Dedup by content body, same as the observed-source dedup.

### Files
- `sovereign-cli/src/audit_recover.rs` — extend `cmd_audit_recover`.
- Possibly new helper in `sovereign-store` if the messages table
  isn't already exposed via a clean reader. Check
  `sovereign-store/src/lib.rs` for an existing
  `read_messages_for_conversation` style API.
- `sovereign-cli/Cargo.toml` — likely already depends on
  `sovereign-store` via the daemon path; confirm.

### Tests
- Unit: seed a tempdir messages db with three assistant rows
  containing decision phrases + one without, run the recover
  pass, assert three `inferred` notes land + dedup on second
  pass.
- Cross-check: ensure assistant rows with stoplist words are
  filtered (the existing `response_mine` stoplist covers this).

### Risk
- **Medium-low**. The messages table schema is stable (predates
  this refactor). The risk is in `conversation_id ↔ session_id`
  mapping — these may not be 1:1 if the daemon spawns sub-
  conversations. Mitigation: walk every `conversation_id` and
  use the conversation's metadata to recover the originating
  session id; fall back to `conversation_id` itself if absent.
- ResponseMiner runs on every assistant turn — for a long-lived
  session this could produce dozens of inferred notes. The
  `MAX_MATCHES_PER_CALL = 12` cap on the miner mitigates per-turn,
  but `--recover` doing 100 turns × 12 matches could spam the
  audit. Mitigation: cap recovered inferred notes per session
  at ~20.

### Estimate
~3 hours including tests.

---

## Gap E — Wire `DiffDecisionExtractor` into `sovereign audit`

### Goal
Running `sovereign audit` lazily invokes the LLM-backed extractor
over the cumulative session diff (since the project's last
"baseline" commit, or last invoked audit, whichever is later).
Resulting decisions land as `source='extracted'` notes that the
multi-source assembly picks up automatically.

### Approach

Strategy: persist the "last extracted-up-to" git SHA in
`.sovereign/audit_state.toml` (or as a row in the existing notes
DB metadata). On each `sovereign audit`:

1. Compute `current_head = git rev-parse HEAD`.
2. Read `last_extracted = audit_state.last_extracted_head` (or
   default to the project's first commit if missing).
3. If `last_extracted == current_head`, skip — extractor already
   ran for this state.
4. Otherwise: build `git diff <last_extracted>..<current_head>`,
   feed to `DiffDecisionExtractor::extract` (with the production
   backend from Gap C), persist results as `source='extracted'`
   notes.
5. Update `last_extracted_head = current_head`.

This is **incremental** — re-running the audit doesn't re-extract
from history; it only catches up since the last run.

### Files
- New: `sovereign-cli/src/audit_extract.rs` — the orchestration.
- `sovereign-cli/src/audit_cmd.rs` or
  `project_cmd.rs::cmd_audit` — call into `audit_extract::run`
  before the report renders.
- `.sovereign/audit_state.toml` schema — small TOML file.

### Tests
- Unit: stub `DiffDecisionExtractor` (using the existing
  `StubBackend`), simulate two `sovereign audit` runs against
  a tempdir git repo with one commit between them, assert
  the extractor was called once on the second run only.
- Integration: with a no-op backend, verify the audit doesn't
  double-write extracted notes on repeated invocations.

### Risk
- **Low**. The skip-if-head-unchanged guard makes the operation
  idempotent. Backend errors are already swallowed by
  `DiffDecisionExtractor::extract` per its contract.
- The "last_extracted_head" file becomes part of the project's
  state — should it be committed or `.gitignore`'d? Probably
  gitignored (per-developer state). A future enhancement could
  promote it to `notes.db` so it travels with the project.

### Estimate
~2 hours given Gap C is already done.

---

## Sequencing

The five gaps stack with one hard dependency: **E depends on C**
(can't extract without a backend). Otherwise they're independent.
Recommended order:

```
Day 1 (4h)
  A — wire decision_extractor in default_pipelines.toml          [30m]
  D — --recover reads messages table → inferred notes            [3h]

Day 2 (4–6h)
  C — LocalLlmBackend over daemon's primary slot                 [4–6h]

Day 3 (5h)
  E — wire DiffDecisionExtractor into sovereign audit            [2h]
  B — StructuralNudgeGenerator into MCP responses                [3h]

Total: ~14 hours of focused work, plus prompt-tuning iteration on C.
```

Rationale:
- **A first**: 30-minute unblock. Both demo docs claim per-turn
  extraction works; flipping the toml entry makes it true.
- **D second**: independent of any backend, expands the audit's
  cross-session coverage. Catches the IRB-relevant case where a
  daemon crash mid-grant doesn't lose the discussion history.
- **C third**: the heaviest single piece (prompt tuning + grammar
  constraint + token-budget correctness). Buy a clear afternoon.
- **E fourth**: trivial once C exists. Closes the
  weekly-summary loop the panicked-engineer demo claims at end of
  Hour 0–3 and the clinical-telemed demo claims at end-of-week-1.
- **B last**: highest-touch on the hot path (every tool call now
  shells out to git). Worth deferring until the others prove
  stable so the nudge isn't blamed for unrelated MCP regressions.

---

## What this plan does NOT cover

Both demos already claim wired behaviour for these — keep them
that way:

- The pattern matcher's `ToolPatternMatcher` (Phase 7.1).
- The commit-message harvester (Phase 7.1).
- The multi-source audit assembly + reversal display (Phase 7.3).
- `audit --recover` for `tool_call_log`-only patterns (Phase 7.3).
- `find_approval_via_git` accepting committed specs (Phase 6).
- Spec-presence gate + `tools/list_changed` SSE (Phase 5b).

A regression suite covering all of the above already exists; gap
closure shouldn't touch it.

---

## Cross-references

- `PANICKED_ENGINEER_DEMO.md` — brownfield triage demo, partial list at "What's wired vs. partial"
- `CLINICAL_TELEMED_DEMO.md` — greenfield clinical demo, partial list at "What's wired vs. partial here too"
- `commonwealth-core/src/default_pipelines.toml` — Gap A target
- `sovereign-mesh/src/mcp_router.rs::handle_tool_call` — Gap B insertion point
- `sovereign-tools/src/notes/diff_extract.rs` — Gap C/E foundation
- `sovereign-cli/src/audit_recover.rs` — Gap D extension point
- `sovereign-store/src/migrations.rs:17` — `messages` table schema for Gap D
