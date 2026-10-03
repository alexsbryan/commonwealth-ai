# Panicked Engineer Demo — system mapping

The motivating demo: an engineer with a 64 GB Mac inherits a 400K-line
monorepo and three P0s. They have to fix the bugs by tomorrow, then
refactor the codebase by Friday. This doc traces each hour of that
demo against the specific Phase 1–7 refactor work that delivers the
capability.

> The demo is the deliverable. The audit that gets emailed to the
> founders at end-of-week — that's the user-visible artifact. Every
> phase of the refactor was shaped by what that audit needed to
> contain.

---

## Hour 0–1 · Triage cold-start

### `sovereign init` finishes in <90 seconds

```
cd giant-monorepo
sovereign init
```

| What happens | Phase | Where |
|---|---|---|
| Tree-sitter symbol indexing + SCIP export | (pre-existing) | `sovereign-cli::init` |
| `sovereign serve` spawned in background, PID written to `.sovereign/server.pid` | Phase 3 | `sovereign-cli/src/init.rs`, `serve_cmd.rs` |
| MCP HTTP listener at `127.0.0.1:9741/mcp` | Phase 1 namespace + Phase 3 spawn | `sovereign-mesh::mcp_router` |

The user types **one command**. They don't `setup`, `found`, or
`provision` anything. Phase 6 retired all three ceremonies — `init`
plus a committed spec is sufficient.

### `sovereign daemon` first run absorbs the wizard

```
sovereign daemon
```

If `~/.svrnmesh/config.toml` is absent, the hardware-detection
+ model-selection wizard runs **inline** (Phase 4: `daemon_cmd.rs`).
On a 64 GB M2 Max it suggests:

```
  Quick:   Qwen3-1.7B-Q8_0 (1.8 GB)
  Main:    Qwen3.5-32B-Q4_K_M (19 GB)
  Code:    Qwen3-Coder-14B-Q4_K_M (8 GB)  (hot-swap)
  Embed:   Qwen3-Embedding-0.6B (0.6 GB)
```

When the daemon starts after the wizard, it **takes over `:9741`**
from the standalone `serve` process via the PID-file handshake added
in Phase 3 (`daemon::start_daemon`). The user never sees a port
conflict or a "stop the old server first" message.

### MCP tool list at this moment

`tools/list` returns **5 always-on tools** because no spec exists yet:

```
callers, callees, blast, symbols, build
```

The `note`/`notes`/`spec`/`drift` tools are absent. This is the
spec-presence gate from **Phase 5/5b**:

- `sovereign-tools/src/mcp_surface.rs::render_tools_list_gated` filters
  on disk state (a 1-second TTL cache + an FS watcher cache-bust).
- `MCP_TOOLS_SPEC_GATED = ["spec", "drift"]` is dropped when no
  `.sovereign/features/*/spec.md` and no `ARCHITECTURE.md` exist.
- The note-family is in `MCP_TOOLS_ALWAYS` but the **approval gate**
  (`commonwealth-api::middleware::approval_gate`) blocks unapproved
  writes — agent calls to `note` would be rejected pre-inference.

The agent has tools to **read** the codebase. It can't yet write
anything durable. That's the right state when there's no spec.

---

## Hour 0–1 · Spec-as-approval (the 5-minute setup)

```
mkdir -p .sovereign/features/p0-payments
cat > .sovereign/features/p0-payments/spec.md << 'EOF'
# P0: Payments failing intermittently
...
EOF
git add . && git commit -m "spec: P0 payment failures"
```

What this ceremony does for the system:

### 1. Spec-presence gate flips on

The `SpecWatcher` (Phase 5b: `sovereign-tools/src/spec_watcher.rs`)
sees `.sovereign/features/p0-payments/spec.md` materialise. It:

1. Invalidates the `mcp_surface` cache for the repo root.
2. Calls `McpNotifier::notify_tools_list_changed`, which broadcasts a
   `notifications/tools/list_changed` JSON-RPC frame down every
   subscribed SSE client.
3. opencode receives the notification, refetches `tools/list`, sees
   `spec` and `drift` appear.

Latency from spec write to client awareness: ~100ms typical on macOS
FSEvents.

### 2. Commit = approval

`git commit` makes the spec a tracked file with a recorded author.
The next chat completion targeting `feature_id=p0-payments`:

- Hits `approval_gate.process` (`approval_gate.rs`).
- `find_approval_via_git` shells out to `git log` for the spec path.
- Returns the commit hash, hash of the committed spec, author, ts.
- `session.approval_validated = true`. Write tools are now permitted.

No `sovereign provision` step. No `features.db` row required. The
test `committed_spec_alone_grants_approval_no_provision_needed`
(Phase 6) is the regression guard.

### 3. The audit already knows the feature exists

`collect_feature_rows` (Phase 6: `project_cmd.rs`) merges
`features.db` rows with `.sovereign/features/<id>/` directory
listings, sorted alphabetically. `sovereign audit` immediately shows
`p0-payments` with state `(directory only)` and `Spec: ✓`.

---

## Hour 0–3 · Triage with the call graph

```
> Trace from the API endpoint to the payment execution.
```

The agent's tool calls flow through `mcp_router::handle_tool_call`
(`sovereign-mesh/src/mcp_router.rs`). For each call:

1. Dispatch to the registry, capture the result.
2. Log to `tool_call_log` (10 K row ring buffer in
   `corpus_engine::NoteStore::log_tool_call`).
3. **Spawn `ToolPatternMatcher::observe_and_record`** in a tokio task
   (Phase 7.1).

The matcher's sliding window catches workflow shapes the agent
doesn't articulate explicitly:

| Tool sequence | Pattern fired | Note body |
|---|---|---|
| `blast` then `build` | `InvestigateThenAct` | "Investigated impact (blast) before running `build`." |
| `spec` then `build` | `SpecThenBuild` | "Read the spec, then ran `build`." |
| `notes` then `note` | `NotesInformedDecision` | "Queried `notes` then wrote a new `note` — decision informed by prior recorded context." |
| 3+ `callers`/`callees`/`symbols` with no `build`/`note` | `IsolatedInvestigation` (cooldown'd) | "Three or more code-intel calls (...) with no `build` or `note` follow-up." |
| `build` after another tool | `BuildFollowsAction` | "Ran `build` after a previous tool call." |

Each match writes a `kind='reflection' source='observed'` note. The
audit reader sees these even though the agent never explicitly called
`note(...)`.

### What's load-bearing about this for the demo

Without Phase 7.1's matcher, the only audit input would be the agent's
explicit `note(...)` calls. Engineers under triage pressure don't pause
to write notes — they trace, fix, commit, move on. The matcher fills
that gap automatically.

---

## Hour 0–3 · Each commit becomes a note

```
git commit -m "fix: parse_gateway_response unwraps optional transaction_ref \
  field that the gateway dropped for debit cards in v2.3"
```

The daemon's reindexer (Phase 7.1: `sovereign-mesh/src/reindexer.rs`)
polls git HEAD on its existing 30-second tick. When it sees
`old_head != new_head`:

1. Calls `commit_harvest::harvest_between` (new in Phase 7.1).
2. Filters noise — `^(wip|fix typo|save|merge|bump|format|rename)`,
   <10-word messages.
3. Infers note kind from conventional-commits prefix (`fix:` →
   `decision`, `docs:` → `reflection`).
4. Writes `kind='decision' source='committed'` note via
   `NoteStore::write_note_with_source`.

Daemon wired in both production paths:
- `sovereign-cli/src/daemon_cmd.rs::run_daemon` — mutable
  `Reindexer::with_commit_harvester` before sharing the Arc.
- `sovereign-desktop/src-tauri/src/state.rs` — same.

Cap of 50 commits per harvest run prevents `git pull` of 500 commits
from spamming the audit's Decisions section.

---

## Hour 0–3 · Per-turn decision extraction (daemon mode)

When the agent is running through the daemon's pipeline middleware
(`commonwealth-api::middleware`), Phase 7.2's `DecisionExtractor`
fires on every turn:

**Turn N (`post_process`)**: scans the assistant response with
`response_mine::mine`. Matches phrases like:

- "I'll use X because Y" → `Commitment`
- "chose X over Y" → `Comparison`
- "decided to X" → `ExplicitDecision`

Stoplist filters mechanical sentences (rename, format, import,
whitespace, lint). First match → `session.pending_decision = Some(...)`.

**Turn N+1 (`process`)**: inspects `pending_decision`:

- If user's latest message contains a correction phrase ("scratch
  that", "not a decision", 11 phrases total) → drop without
  persisting.
- Otherwise: `NoteStore::write_note_with_source(... NoteSource::Extracted ...)`
  + inject `[Noted: "<snippet>". Auto-recording unless corrected.]`
  into the system prompt.

Two-turn lookahead means decisions get recorded automatically while
the user retains the ability to correct in conversational style.

The session field round-trips via `MiddlewareSession.pending_decision`
↔ `AtosSessionState.pending_decision` (`routes_inference.rs` plumbing
+ `sovereign-atos::session`).

---

## Hour 3–5 · Architecture mapping flips one switch

The user runs the agent through symbol/callee enumeration, then drops
`ARCHITECTURE.md` at the repo root. Two things happen:

### 1. Spec-presence gate stays on (or just turned on)

`mcp_surface::spec_present_in_dir` looks for either
`ARCHITECTURE.md` at top level OR `.sovereign/features/*/spec.md`.
The architecture doc alone is sufficient — even an early-stage
project without features sees the gated tools.

### 2. Spec-invariant keywords feed the structural nudge

Phase 7.1's `extract_spec_invariant_keywords`
(`sovereign-tools/src/notes/nudge.rs`) parses ARCHITECTURE.md (and
every feature spec) for headings, `**bold**` spans, and `` `backtick` ``
spans. These become the keyword set for nudge signal 5.

When the agent later modifies code containing one of those keywords —
e.g. touches `canonical_fingerprint` while ARCHITECTURE.md has a
`# Canonical Fingerprint` heading — the structural nudge fires:

```
[note worth recording? You modified a struct/trait/impl definition;
 touched code matching a spec invariant. Call note(decision, …).]
```

The matcher normalises across separators so `canonical_fingerprint`
matches the heading "Canonical Fingerprint."

---

## Hour 5–8 · Refactor planning with `blast`

```
sovereign plan
```

Phase 1 collapsed `sovereign project plan` into the flat namespace.
The agent uses Phase 2's renamed tool `blast` (was `blast_radius`) to
rank trait-extraction candidates by impact. Result: phases ordered
smallest-blast-first, each becoming a feature spec with milestones.

Each new spec at `.sovereign/features/refactor-http-port/spec.md`
goes through the same mechanism as Hour 0:

1. SpecWatcher fires → cache invalidation + `tools/list_changed`
   notification.
2. `git commit` → `find_approval_via_git` returns success → write
   tools allowed for that feature_id.

No additional ceremony. The same flow used for emergency P0 specs
scales to a refactor plan with seven phases.

---

## Day 2–5 · Refactor execution

Each turn of refactoring work flows through the same matcher +
extractor + nudge layer that Hour 0–3 did. Three things accumulate
in the background:

| Stream | Source tag | Trigger |
|---|---|---|
| Agent's explicit `note(...)` calls | `agent` | Manual |
| Commit messages (`refactor: extract HttpPort trait — needed `with_timeout` for gateway`) | `committed` | Daemon git poll |
| Per-turn decision phrases ("I'll use a generic `Send + Sync` bound because...") | `extracted` | Phase 7.2 middleware |
| ResponseMiner over conversation transcripts (used by `audit --recover`; see below) | `inferred` | Lazy |
| Tool-call patterns | `observed` | Phase 7.1 matcher |

The structural nudge (Phase 7.1) fires when the agent modifies a
struct/trait definition + touches 3 files + manifest is touched +
test assertion is modified. Rate-limited to 1 nudge per 15 tool
calls. If the agent acts on the nudge, the `note` call lands as
`source='agent'` (highest priority); the original observed-pattern
note stays as a lower-priority cross-reference.

---

## End of week · `sovereign audit`

Phase 7.3 rewrote `project_cmd::build_audit_report` around the
multi-source assembly:

```
## Decisions
- `[note:abc...]` Switch storage to async channels because sync deadlocked  _[agent]_
- `[note:def...]` Extract HttpPort trait — gateway needs custom timeout    _[committed]_
- `[note:ghi...]` Generic Send+Sync bound on Port trait                    _[extracted]_

## Deviations
- ...

## Open questions
- `[note:jkl...]` Should StoragePort own connection-pool lifetime?         _[agent]_
- `[note:mno...]` Is the cache TTL correct?                                _[inferred]_ _(low confidence)_

## Observed patterns
- `[note:pqr...]` Investigated impact (blast, callers) before running `build`.  _[observed]_

## Notes by kind
| Kind | Count |
|...|...|
```

Implementation:

- `gather_audit_notes` reads every active note in one pass and
  buckets by kind/source.
- Decisions sort key: `(source priority desc, created_at desc)`.
  `agent > committed > extracted > inferred > observed` per
  `corpus_engine::NoteSource::priority`.
- `_[<source>]_` suffix per row so the reviewer can read trust
  level at a glance.
- `_(low confidence)_` flag on `kind=uncertainty` rows with
  `source=inferred` — the regex-mined ones are the noisiest stream.

### Reversal display via `supersedes`

If the agent (or the diff extractor) reverses a prior decision
mid-week, the new note carries `supersedes = Some(<prior_id>)`. The
audit renders both:

```
- `[note:abc...]` BTreeMap over HashMap — ordered iteration  _[agent]_
  ↳ REVERSED 2026-04-22: HashMap over BTreeMap — random access pattern  _[extracted]_
```

The reversal does NOT also appear at the top level (already-rendered
guard). Orphan reversals (target row missing) gracefully render at
top level so nothing's lost.

---

## SIGKILL recovery — `sovereign audit --recover`

If the daemon crashes mid-session, `tool_call_log` is durable (every
write is a SQLite transaction) but the in-process pattern matcher's
`tokio::spawn` may not have flushed its observed-source notes.

Phase 7.3's `audit_recover` module (`sovereign-cli/src/audit_recover.rs`):

1. `tool_call_log_rows(0, 10_000)` → group by session.
2. For each session: `ToolPatternMatcher::scan_for_recovery` over
   the rows with a fresh empty cooldown map.
3. Dedup against existing observed-source notes for that session
   (body equality).
4. Persist remaining matches as `source='observed'`.

Idempotency proven by
`recover_is_idempotent_via_body_dedup` test — second pass writes
zero new notes.

Caps: 200 sessions / 256 rows-per-session per run.

---

## What's wired vs. partial

| Capability | Status |
|---|---|
| Phase 5b spec-presence gate + `tools/list_changed` | Wired end-to-end |
| Approval gate accepts committed spec | Wired (5 tests, incl. real-git e2e) |
| ToolPatternMatcher observed notes | Wired (e2e test against live MCP wire) |
| Commit-message harvester | Wired in both daemon paths |
| Per-turn DecisionExtractor middleware | Wired (7 unit tests; needs to be added to default pipeline config) |
| Multi-source audit assembly + reversal display | Wired |
| `sovereign audit --recover` for SIGKILL'd sessions | Wired |
| `DiffDecisionExtractor` LLM-backed pass | **Trait + stub backend ready;** production backend (qwen-27B/gemma-31B) is the natural next step. Pure prompt + parser tested. |
| ResponseMiner over the `messages` table | Pure miner tested; recovery integration over `sovereign-store::messages` is the recovery v2 hook. |

The audit's "non-empty floor" is held up by **agent + committed +
observed** even before the LLM-backed extractor is plugged in. Adding
the production backend is additive, not load-bearing.

---

## Cross-references

- `sovereign-tools/src/mcp_surface.rs` — spec-presence cache + gated
  rendering (Phase 5/5b)
- `sovereign-tools/src/spec_watcher.rs` — FS watcher + cache invalidation (Phase 5b)
- `sovereign-mesh/src/mcp_router.rs` — Notifier broadcast, SSE
  forwarding, pattern matcher hook (Phase 5b + 7.1)
- `sovereign-mesh/src/commit_harvest.rs` — git → committed-source notes (Phase 7.1)
- `sovereign-mesh/src/reindexer.rs` — `with_commit_harvester` (Phase 7.1)
- `sovereign-tools/src/notes/patterns.rs` — 5-rule matcher (Phase 7.1)
- `sovereign-tools/src/notes/nudge.rs` — structural nudge generator (Phase 7.1)
- `sovereign-tools/src/notes/response_mine.rs` — phrase miner (Phase 7.2)
- `sovereign-tools/src/notes/diff_extract.rs` — LLM-backed extractor scaffolding (Phase 7.2)
- `commonwealth-api/src/middleware/decision_extractor.rs` — two-turn lookahead (Phase 7.2)
- `commonwealth-api/src/middleware/approval_gate.rs` — `WRITE_INTENT_TOOLS` shrunk (Phase 6)
- `sovereign-cli/src/project_cmd.rs::build_audit_report` — multi-source assembly (Phase 7.3)
- `sovereign-cli/src/audit_recover.rs` — `--recover` (Phase 7.3)
