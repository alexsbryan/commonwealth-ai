# Desktop launch-readiness — performance & console review

**Date:** 2026-07-13
**Branch:** `perf/desktop-launch-readiness`
**Question:** is the desktop app "blazing fast" and console-clean for a large-group launch?

Three fronts were measured: **boot time**, **query response latency** (TTFI + real coach/personas run), and **console cleanliness**. Method: three parallel subsystem audits + a live bounded personas run against the resident 35B daemon.

---

## TL;DR

- **Boot: good, and now better.** Warm attach-boot was ~1.86s; deferring the GLiNER load (this branch) takes ~950ms off the critical path → **~0.9s warm**. No boot regressions from recent work.
- **UI responsiveness: excellent.** The shell paints a query-aware signal in **~120–200ms** (optimistic dispatch). TTFI suite 9/9 green.
- **Real query latency: the actual risk.** A live query's **time-to-first-insight was 47.8s** (debug build, single sample) — dominated by retrieval fan-out + the grounding gate, not the UI. This is the biggest "blazing fast" lever and is **not** addressed by this branch. See §2.
- **Console: frontend clean; Rust noise reduced.** Frontend had zero happy-path console spam. Rust side had a debug-level firehose + raw `eprintln!` + expected-state boot warns — fixed this branch.

---

## 1. Boot time

Warm attach-boot critical path ≈ **1.86s** (verified against `bootstrap_with_progress`, `state.rs`). The 2026-06-29 fix (background SCIP merge + background meta-atlas load) is intact. No new eager loads were added by recent cartridge/KV, tiered-memory, or quality-program work.

**Dominant remaining on-path cost: `gliner_load` ~948ms** (≈half the warm boot), a synchronous `GlinerExtractor::new_default()` at `state.rs`.

**FIXED this branch.** New `LazyGlinerExtractor` (`sovereign-tools/src/gliner_ner.rs`) installs immediately and warms the model on a background thread; `extract_entities` soft-falls-through to cosine+MMR (identical to the model-absent path) until warm (~1s, before the first query). Expected warm boot after: **~0.9s**.

Lower-priority remaining items (not done — diminishing returns):
- **Cold router-embed re-embed ~9.3s** — now genuinely intermittent (Jul-10 fix `a98159d5` narrowed the trigger to real embed-model swap / missing baked artifact). Not a per-boot cost.
- **Local-mode 2nd SCIP merge** (`state.rs:932-944`, the `/mcp` arm) — still synchronous, but Local mode also loads the 35B so boot is model-bound there regardless.
- **`embed_probe`** (`state.rs:1107`) — a synchronous HTTP embed round-trip that gates `backend-ready`; a cold daemon embed slot can spike the tail. Candidate for a timeout guard.

## 2. Query response latency — the real "blazing fast" question

**TTFI measures the shell, not the answer.** TTFI ("time to first intelligence") is a frontend-perception metric on a *mocked* backend timeline. It reports `generic` ~20ms (typing dots) and `specific` ~120–200ms (first query-aware progress). The frontend query path is already well-optimized: optimistic `SEND_INITIATED` dispatch before any bridge await, a 400ms placeholder floor, staleness-bounded rotation. **The shell is genuinely fast.**

**The real answer is not.** A live bounded personas run (`drive_by` persona, Attach mode, resident 35B, `target/debug`) measured:

| metric | value |
|---|---|
| ttft (first insight) | **47.8s** |
| ttdraft (first draft glyphs) | none (draft-stream off) |
| total latency | 47.8s |

The answer arrived all at once at 47.8s. This matches the prior TTFT analysis in memory (retrieval fan-out + housekeeping own TTFT; 40–180s turns). **Caveats:** debug build (release is materially faster), single sample, and a gap-admission query. But the shape is clear: real latency lives in **retrieval fan-out + the grounding gate + 35B generation**, not the frontend.

**Recommendations (not done — need a decision):**
1. **Re-measure on a release build** to get the true number before drawing launch conclusions. A `target/release` desktop + daemon is the honest baseline.
2. **Turn on draft streaming** (`SOVEREIGN_DRAFT_STREAM`) so the user sees draft glyphs (`ttdraft`) tens of seconds before the verified answer — the single biggest *perceived*-latency win for grounded turns.
3. **Profile retrieval fan-out** — the memory specimen puts a 76s cost in fan-out; that's where end-to-end time is won, not boot.

**How to run the harnesses (for follow-up):**
- Real UI latency: `cd sovereign/crates/sovereign-desktop && node tests/e2e/scripts/personas.mjs --attach --spawn --sessions 1 --personas drive_by` → journals `ttftMs`/`ttdraftMs`/`latencyMs` to `test-artifacts/persona-journal.jsonl`.
- TTFI shell suite (mocked, headless): `npx playwright test specs/ttfi.spec.ts --project=chromium` + `npm run report:ttfi`.
- Answer quality/latency: `sovereign eval inner-chaos` (needs daemon+models); `sovereign bench chaos-monkey run --bank <toml>`.

## 3. Console cleanliness

**Frontend was already clean** — 92 `console.*` calls, all in catch-blocks/opt-in; 0 in `chat-ui`. No happy-path spam.

**Rust side had the noise. Fixed this branch:**
- `main.rs` EnvFilter default — the four inference crates (`core`/`tools`/`inference`/`corpus_engine`) firehosed **debug** on every query. Now `debug` in **dev** builds (glass-box intact) / `info` in **release**; `RUST_LOG=sovereign_core=debug` still restores it on demand, so the glass-box contract is preserved as opt-in.
- `commands/corpus.rs`, `commands/document_asset.rs` — raw `eprintln!` (bypassed the filter, printed always) → `tracing::info!`/`warn!`.
- `state.rs` "Vector index not built" — fired `warn!` per-corpus every boot while indexes build (expected first-launch) → `info`.

**Frontend: added a global unhandled-rejection / uncaught-error safety net** in `src/main.ts` — previously any un-awaited rejecting promise surfaced raw with no shaping, and there was no frontend equivalent of the Rust panic hook. Now shaped + tagged, with a hook for future crash-capture forwarding.

**Investigated, left as-is (correct behavior, not bugs):**
- `App.svelte:259` `console.error("Backend error:")` — cancels flow through a *separate* path (`chat.rs` `was_cancelled`, status `"cancelled"`), so this only fires on genuine errors. Correct.
- `InnerWorkSurface.svelte:660` — deliberate diagnostic on real witness errors (context overflow), paired with a friendly UI line. Correct.
- `governance_commands.rs` `eprintln!` — inside a test/regen fixture helper, not the product path.

**Deferred (low priority):** `SettingsPanel.svelte` 5 `console.warn("daemon offline?")` and `KnowledgeStatus.svelte` newsworthy-probe warns fire only during the brief boot window while the daemon is unreachable — expected-transient; could be gated but not launch-blocking.

---

## What this branch changes

| File | Change |
|---|---|
| `sovereign-tools/src/gliner_ner.rs` | New `LazyGlinerExtractor` (background-warm decorator) |
| `sovereign-desktop/src-tauri/src/state.rs` | Use deferred GLiNER load; downgrade transient vector-index warn → info |
| `sovereign-desktop/src-tauri/src/main.rs` | EnvFilter: debug in dev, info in release (glass-box opt-in) |
| `sovereign-desktop/src-tauri/src/commands/corpus.rs` | `eprintln!` → `tracing::info!` |
| `sovereign-desktop/src-tauri/src/commands/document_asset.rs` | `eprintln!` → `tracing::info!`/`warn!` |
| `sovereign-desktop/src/main.ts` | Global unhandled-rejection / error safety net |

**Not addressed (needs product decision):** real query latency (§2) — the biggest lever, and the one thing "blazing fast" ultimately rests on.
