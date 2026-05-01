# Time to First Intelligence (TTFI)

A metric and harness for tuning how quickly the desktop chat UI proves to the user that the system is working on *their* query — not just stalling.

## The problem this exists to solve

A query goes through five stages on the backend: routing → corpus retrieval → primary model receives context → primary model thinks → primary model streams. In the worst case, the user clicks Send and stares at three pulsing dots for 5–15 seconds. The system is doing real, expensive, query-specific work the entire time. The UI just hides it.

Three pulsing dots are the lowest-fidelity feedback a chat UI can show. They prove the app *received* the click. They prove nothing about whether the system *understood* the query, *found* relevant evidence, or *began* to compose an answer. We want to be a glassbox: when the system has something specific to say, the user should see it where their eye already is.

## The metric

Six observed tiers + one derived tier, all measured in-page with `performance.now()` from the click anchor:

| Tier | Source | What it tells us |
|---|---|---|
| `generic` | first `.typing-indicator` paint | Time to *any* "we got your input" feedback. Should be near-instant. |
| `specific` | first `.doc-progress-indicator` paint (with non-empty text) | Time to first *query-aware* signal in the place the user is already looking. **The primary optimization target.** |
| `aux` | first `.narration-stack`, `.interpretation-banner`, or `.clarification-card` paint | Time to first specific signal anywhere — including auxiliary positions the user may or may not notice. |
| `visible` | first `IntersectionObserver` hit on a specific-or-aux element | Time to first specific signal that's actually inside the viewport. DOM presence ≠ visibility — a chip rendered far below the fold fires `aux` but not `visible`. |
| `thinking` | first `.think-block` paint | Reasoning models stream `<think>...</think>` tokens before prose. Without this tier `content` would understate first-content by the entire thinking duration. |
| `content` | first non-empty `.sv-ai-msg .sv-prose` text | Time to first rendered token (traditional TTFT). |
| `gap` | `content − specific` (derived on read) | The *user-perceived wait window* between "system has something specific to say" and "actual content arrives". Catches the second-order failure mode: staring at one calm sentence for seconds while the model thinks. |

Two failure modes the metric catches:

1. **`aux` fires but `specific` doesn't** — system has something to say but the user is looking in the wrong place. (The shipped `narration-in-slot` UI tweak fixes this for narration.)
2. **`aux` fires but `visible` doesn't** — system has something specific *and* it's in the right slot, but the slot is below the fold (long scrollback, narrow window, etc.). User sees nothing.
3. **`gap` is large** — the specific signal is in the right place but never updates while the model thinks. User sees one calm sentence for 2+ seconds and assumes the app froze. (Open optimization target.)

## How the harness works

```
┌──────────────────────────────┐
│ tests/e2e/scenarios/*.ts     │  scenario timelines (atMs → event)
└──────────┬───────────────────┘
           │ playScenario(page, ctx, scenario)
           ▼
┌──────────────────────────────┐
│ fixtures/scenario-player.ts  │  schedules events in-page via setTimeout
└──────────┬───────────────────┘  relative to ttfi.t0 (click anchor)
           │ window.__sovereign_test__.emit(...)
           ▼
┌──────────────────────────────┐
│ fixtures/tauri-shim.js       │  delivers events to ChatView listeners
└──────────┬───────────────────┘  exactly like real Tauri would
           │ Tauri events: turn-narration, interpretation-proposed,
           │   document:operation, message-chunk, message-complete, …
           ▼
┌──────────────────────────────┐
│ ChatView + routingStore +    │  React renders. DOM mutates.
│ chat.machine                 │
└──────────┬───────────────────┘
           │ MutationObserver
           ▼
┌──────────────────────────────┐
│ fixtures/ttfi-probe.js       │  records `performance.now() - t0`
└──────────┬───────────────────┘  on first match per tier
           │ window.__ttfi__.getReport()
           ▼
┌──────────────────────────────┐
│ specs/ttfi.spec.ts           │  collects rows, writes JSON
└──────────┬───────────────────┘
           │ .ttfi-report.json
           ▼
┌──────────────────────────────┐
│ scripts/ttfi-summary.mjs     │  markdown table, optional baseline diff
└──────────────────────────────┘
```

The harness deliberately does *not* mock at HTTP. The desktop chat UI's input boundary is Tauri events, not SSE — by mocking at that boundary we get fast, deterministic, in-page tests that measure exactly the thing the user perceives. A future bridge-level harness (Node mock daemon + real src-tauri) is layered work; not needed for UI tuning.

## Running it

```sh
cd sovereign/crates/sovereign-desktop

npm run test:ttfi                  # run the 5 scenarios, write .ttfi-report.json
npm run report:ttfi                # pretty-print the table (with baseline diff if present)
npm run report:ttfi:save-baseline  # snapshot the current report as the comparison baseline
```

Workflow for tuning:

1. `npm run test:ttfi && npm run report:ttfi:save-baseline` — capture pre-change baseline.
2. Make a UI change.
3. `npm run test:ttfi && npm run report:ttfi` — see the side-by-side delta column.
4. If `specific` dropped on the scenarios you targeted and `content` didn't move, the change worked.

## Scenarios shipped

The harness covers the eight distinct response shapes a real user can encounter today. Each one is compressed to keep tests under ~5s while preserving the user-perceived ordering of events:

| Scenario | Shape | What it tests |
|---|---|---|
| `fast-local` | Direct chat answer, no retrieval, fast first token (~250ms) | The lower bound — content arrives before any specific narration matters. |
| `knowledge-grounded` | Routing → retrieval → synthesis → stream (~1.2s to first token) | The most common chat shape — 1+ seconds of waiting while real work happens. |
| `heavy-reasoning` | Normal retrieval + 2.5s primary thinking pause before tokens | The worst dot-stare — a long synthesis pause where the user is most likely to think the app froze. |
| `disambiguation` | Low-confidence routing → ClarificationCard, synthesis suppressed | Tests that "we're not sure, can you clarify" lands quickly and clearly. |
| `off-target-suppressed` | Retrieval came back dispersed → honest "I didn't find a confident answer" | Tests the anti-flattering path — *not finding* something is also intelligence. |
| `with-thinking` | Model streams `<think>...</think>` before prose | Validates the `thinking` tier fires early and `content` waits for prose. Surfaces the *content vs thinking* gap that's invisible without this scenario. |
| `complex-task-fallback` | Non-streaming fallback inside `send_message_stream` — zero chunks until message-complete | The worst-case TTFI shape current production produces. Models the silent wait so we can see how long it really is. |
| `document-asset-progress` | `ask_document` path UX — rich `document:operation` events fill the slot, then full answer arrives at once | The opposite shape from streaming: many specific signals during the wait, but no incremental content. The biggest `gap`, but a pleasant feel because the slot keeps updating. |

These cover all three chat paths in the desktop bridge:

- **Streaming** (`send_message_stream`) → `fast-local`, `knowledge-grounded`, `heavy-reasoning`, `with-thinking`, `disambiguation`, `off-target-suppressed`
- **Non-streaming fallback** (inside `send_message_stream`) → `complex-task-fallback`
- **Doc-asset bypass** (`ask_document`) → `document-asset-progress`

Web search (`search_web`) is structurally similar to the doc-asset path (non-streaming, single complete event) and is covered by the same shape.

## Recording scenarios from real usage

The harness can replay scenarios; the recorder captures them. Anything you do during a real session in the desktop app — hitting a corpus, getting a clarification, watching a long synthesis — can be turned into a scenario file the harness will replay deterministically forever.

**Activation** (any one of these; all inert until set):

- URL param: `?ttfi=record` on the desktop dev URL
- Storage flag: `localStorage.setItem('ttfi_record', '1')` once, persists across reloads
- Programmatic: `window.__ttfi_recorder__.enable()` in devtools

When active, the recorder watches for the first click on `.send-btn`, captures the textarea value as `query`, anchors `t0`, and accumulates every relevant Tauri event (`turn-narration`, `interpretation-proposed`, `clarification-request`, `document:operation`, `message-chunk`, `message-complete`, `message-error`) with `atMs` offsets. It auto-finalizes on `message-complete` / `message-error` / `clarification-request`.

**Capture flow:**

```sh
# 1. Open the desktop dev URL with the flag
open http://localhost:5173?ttfi=record

# 2. In the app, send a real query and let it complete

# 3. In devtools console:
window.__ttfi_recorder__.download('my-real-session')
# → my-real-session.ts lands in your Downloads

# 4. Move it in:
mv ~/Downloads/my-real-session.ts \
   sovereign/crates/sovereign-desktop/tests/e2e/scenarios/

# 5. Add it to the scenarios list in tests/e2e/specs/ttfi.spec.ts
#    and re-run:
npm run test:ttfi
```

The exported file is wire-compatible with `playScenario()` — same `Scenario` type, same event union, same `terminal` shape. Recorded scenarios sit alongside hand-authored ones with no conversion.

The recorder ships in the production bundle but is dormant unless explicitly activated. The cost when off is one Tauri event listener registration (no-op if no events) and a click handler. Both nil-impact.

## UI improvements driven by the harness

Each item below was a measurable failure mode the harness surfaced first, then a calibrated change verified by the same metric.

### 1. Narration in the loading slot

**Problem**: `aux` fired at 200ms but `specific` was null on every scenario. The system had something to say (narration text) but rendered it below the bubble where the user wasn't looking.

**Fix**: `ChatView.svelte` preparing-state indicator now prefers `docProgressText` → latest narration text → typing dots. Reuses the existing `.doc-progress-indicator` styling so the change is one block, no new CSS.

**Result**: `specific` dropped from null → 121–202 ms across 6 scenarios. Heavy-reasoning saves 2.4 s of dot-staring.

### 2. Placeholder narration for the silent-fast path

**Problem**: When the runtime emits no narration and no doc-op events at all (fast-path queries with `think_budget=0`, narration suppressed below 5s elapsed), even the narration-in-slot tweak doesn't help — the slot stays empty until content arrives. Surfaced by the `silent-fast` scenario: 741 ms of pure dot-stare from click to first content.

**Fix**: After 400 ms of `isLoading` with no specific signal, `ChatView.svelte` renders a calm placeholder ("Working on it…") in the same loading-slot styling. Suppressed entirely if narration/doc-op arrive before the threshold. Replaced (no flicker) if narration arrives after.

**Result**:

| Scenario | `specific` baseline | `specific` after |
|---|---|---|
| `silent-fast` | null (dots until 759ms content) | **420 ms** (43% reduction in dot-stare window) |
| All other scenarios | unchanged | unchanged (signals always arrive before 400ms threshold) |

Pinned by `chat-placeholder.spec.ts`: threshold timing, narration-suppresses-placeholder, narration-replaces-placeholder, completion-clears-placeholder.

### 3. Sentence-stare rotation + pulse for long waits

**Problem**: Even with a specific signal in the slot, the user can stare at one calm sentence for 2–3 s on heavy reasoning, with-thinking, and the non-streaming complex-task-fallback path. Real-world the wait is often 30–60 s. Surfaced by the new `staleness` tier (max time the slot text was unchanged).

| Scenario | Staleness baseline |
|---|---|
| `heavy-reasoning` | 3.40 s |
| `complex-task-fallback` | 3.06 s |
| `with-thinking` | 2.89 s |
| All other scenarios | < 1.4 s (natural rotation via multiple narrations / doc-ops) |

**Fix**: Two complementary mechanisms in `ChatView.svelte`:

1. **CSS pulse** on `.progress-mark` — the diamond accent breathes opacity 0.55 ↔ 1.0 over 2.4 s, always-on while loading. Provides ambient "still alive" feedback that doesn't depend on text changes.
2. **Textual rotation** — after 1500 ms with no slot-text update, append `(still working)` to the existing text. After 3000 ms, escalate to `(taking longer than usual)`. The original text is preserved (we don't overwrite information). Resets when a fresh narration / doc-op / placeholder activation lands. Suspended while a `<ClarificationCard>` is up — the system is then waiting on the user, not crunching.

**Result**:

| Scenario | Staleness baseline | Staleness after | Δ |
|---|---|---|---|
| `heavy-reasoning` | 3.40 s | **1.50 s** | **−1.90 s** |
| `complex-task-fallback` | 3.06 s | **1.50 s** | **−1.56 s** |
| `with-thinking` | 2.89 s | **1.50 s** | **−1.39 s** |
| All other scenarios | unchanged | unchanged | natural rotation kept them under threshold |

Each long-wait scenario is now bounded by the 1500 ms rotation interval. For real-world 30–60 s waits, the user sees rotations at 1.5 s and 3 s, then the diamond pulse handles the rest — better than continuing to escalate text (which would falsely imply progress).

Pinned by `chat-placeholder.spec.ts › preparing-state rotation` (4 tests): rotation appears, resets on new narration, suspended on clarification, pulse class always present.

## Companion UI tweak (already shipped)

The preparing-state indicator in `ChatView.svelte` now prefers, in order:

1. `docProgressText` (DocumentAssetManager paths — unchanged)
2. **NEW**: latest narration text from `routingStore.narrationLog`, rendered in the same `.doc-progress-indicator` styling
3. Plain typing dots (only when nothing specific has arrived)

Effect on the harness:

| Scenario | `specific` baseline | `specific` after tweak | Saved |
|---|---|---|---|
| fast-local | never | 122 ms | — |
| knowledge-grounded | never (`content` at 1.21 s) | 201 ms | ~1.0 s |
| heavy-reasoning | never (`content` at 2.51 s) | 151 ms | ~2.4 s |
| disambiguation | never | 151 ms | — |
| off-target-suppressed | never | 152 ms | — |

`generic`, `aux`, and `content` tiers unchanged. The change is precisely scoped: surface what we already had, where the user already looks.

## Where to take it next

Ordered by leverage, not difficulty.

### 1. Reduce `gap` on the heavy-reasoning scenario

`gap` is now measured. On `heavy-reasoning` it's 2.4s — that's how long the user sees one calm sentence sitting there while the model thinks. The fix is UI-side and has several plausible shapes:

- Rotate narration text every ~800ms after the first one lands ("Drafting…" → "Still drafting — this is a deep one…" → "Almost there…")
- Pulse the diamond accent so the indicator visibly *breathes* even when text doesn't change
- Switch to a "still thinking" variant after a threshold (~1.5s with no chunks)

Pick one, ship it, watch `gap` drop on heavy-reasoning without `specific` regressing on others. This is the open optimization target the new `gap` tier was added to surface.

### 2. Variance / repeats

Each scenario currently runs once per spec invocation. On a busy CI box, `setTimeout` jitter can shift `specific` by 30–80 ms. For the wide deltas we're optimizing today this doesn't matter; once the metric tightens, it will.

Implementation: run each scenario N=5 times, report p50/p95 instead of single values. The summary CLI grows two columns.

### 3. Bridge-level harness (when needed)

Today's harness mocks at the Tauri event boundary, skipping the src-tauri Rust bridge that translates SSE → events. The bridge is unlikely to be a TTFI bottleneck, but if a future suspicion arises, the path is: a Node mock daemon serving real SSE on a controllable timeline + Playwright Chrome with real `tauri build` + the same probe. Bigger lift, more faithful, only worth doing if we ever suspect bridge-level latency.

### 4. Promote stable scenarios to hard `expect()`

Budgets today are advisory (console.warn on overrun, never fail). Once two or three iterations of UI tuning land and the variance is understood, promote `fast-local` and `knowledge-grounded` to hard assertions in CI. They're our first line of defense against future TTFI regressions.

### 5. Heuristic placeholder narration

`NarrationPhase` events are suppressed below 5 s elapsed (the runtime caps them). On a fast knowledge query that completes in 4 s, *no* narration may fire — and on the current tweak that means the user sees dots until content arrives, even though we wanted them to see something specific.

A new scenario `silent-fast` (no narration events, no doc-op events) would surface this. The fix would be UI-side: show a short generic-but-stage-aware label ("Working on it…") that only renders if no narration has arrived within ~400 ms. The harness can verify it doesn't over-fire on fast-local.

### `gap` follow-up — `content − thinking` for reasoning turns

`gap` today is `content − specific`. On `with-thinking` that's 2.0s — but the more interesting number is `content − thinking = 1.7s`, the wait between *thinking visible* and *answer visible*. Currently captured implicitly (both numbers are in the report) but not derived as its own column. Add `thinking_gap` if heavy-reasoning UI iteration starts targeting that specific transition.

### 6. Live TTFI HUD in dev mode

Render the markers as a small overlay in the dev build (gated on `import.meta.env.DEV`). Designers iterating on the preparing/streaming surface see TTFI move *as they tweak*, not after a re-run. Effectively the harness, but live.

### 7. Cross-browser

Currently chromium-only. Safari (WebKit) renders animations differently and Firefox has different `setTimeout` resolution. Adding `webkit` and `firefox` to the Playwright `projects` array would catch divergences. Cheap to add; only worth running occasionally in CI.

### 8. Telemetry parity

A future plan should add lightweight TTFI emission in the production build (a `performance.mark` on each tier, harvested via a debug menu or bug-report bundle). When a user reports "the app feels frozen", we can ask them to grab a TTFI capture and compare against lab values directly. Same metric, same names, same units.

The recorder is already half of this — it captures the *event timeline*. Adding the probe's marker timestamps to the recorder's export would close the loop: a single command in production produces both the inputs (events) and the perceived output (TTFI tiers).

## Files

```
src/lib/ttfi/                        (production-shipping)
├── types.ts                         ← shared Scenario / ScenarioEvent types
└── recorder.ts                      ← real-session scenario capture (inert unless flagged)

tests/e2e/
├── TTFI.md                          ← this doc
├── .ttfi-report.json                ← latest run output (gitignored)
├── .ttfi-baseline.json              ← reference snapshot (gitignored)
├── fixtures/
│   ├── ttfi-probe.js                ← in-page MutationObserver + IntersectionObserver
│   ├── scenario-player.ts           ← TtfiReport + playScenario() (re-exports from src/lib/ttfi/types)
│   ├── tauri-shim.js                ← (existing) Tauri event bridge mock
│   └── test-base.ts                 ← (extended) chat.api.ttfi namespace
├── scenarios/
│   ├── fast-local.ts
│   ├── knowledge-grounded.ts
│   ├── heavy-reasoning.ts
│   ├── disambiguation.ts
│   └── off-target-suppressed.ts
├── scripts/
│   └── ttfi-summary.mjs             ← markdown report renderer
└── specs/
    ├── ttfi.spec.ts                 ← runs the 5 scenarios, writes JSON
    ├── ttfi-probe.spec.ts           ← probe contract (visible/gap/reset semantics)
    └── ttfi-recorder.spec.ts        ← recorder round-trip (capture → export → replay-shape)
```

UI tweak that motivated all this: `src/lib/components/ChatView.svelte` — the `{#if isLoading}` block in the messages template, plus the `latestNarrationText` derived in the script section.
