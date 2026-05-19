# URL-allowlist constraint — integration plan

**Status**: core constraint landed + tested (2026-05-19, search-gym Phase 3c).
Integration into the inference path is the remaining work. This doc
hands off the next session.

## What this fixes

Empirical finding from search-gym Phase 3c iter5-10:

The model receives a tool result containing real URLs (e.g.
`https://marketsentry.io/quote/NVDA`) and is asked to cite them in
its synthesis. Prompt-level rules + an explicit in-context allowlist
trailer lift citation behaviour from ~20% to ~70-80% (iter6 → iter9).
Beyond that, the model still character-fabricates URLs by pattern
extrapolation: given `/after-hours` in the result, it routinely emits
`/after-years` or `/after-posts` as sibling "citations." Observed on
fixture 08 (NVDA stock price) and fixture 07 (SpaceX flight 14).

Prompt iteration cannot fix this — the model's training prior on
"sensible-looking URLs that fit the pattern" is stronger than any
prompt nudge. Token-level masking at sampling time CAN, by literally
making the fabricated tokens have logit `-INFINITY` so they're never
chosen.

## What's landed

`sovereign-inference/src/url_constraint.rs` exposes
`UrlAllowlistConstraint`, modelled on `JsonConstraint`'s public shape:

```rust
pub struct UrlAllowlistConstraint { /* trie + vocab_bytes + cursor */ }

impl UrlAllowlistConstraint {
    pub fn new(allowed_urls: &[String], vocab_bytes: Arc<Vec<Vec<u8>>>) -> Option<Self>;
    pub fn mask(&self, data: &mut LlamaTokenDataArray);
    pub fn accept(&mut self, token: LlamaToken);
}
```

**Mechanism**:
- Byte-keyed trie of allowed URLs (one path per URL, terminal flag at the leaf).
- Cursor mode: `InProse(window)` or `InUrl(node_idx)`.
- In `InProse`, a sliding 16-byte window watches for `http://` or `https://` start markers; on match, walk the trie marker bytes and transition to `InUrl`.
- In `InUrl`, the cursor advances by trie edges; URL terminator bytes (whitespace, `,`, `[`, `]`, `(`, `)`, `.`, etc.) at a terminal node return to prose; otherwise the byte is rejected as a fabrication.
- `mask()` per-candidate simulates the token's bytes on a clone of the cursor; if any byte breaks the state machine, the token's logit gets clamped.

**Tests** (10/10 pass at the time of handoff):
- Empty allowlist returns None
- Prose-only bytes accepted
- Valid URL followed by terminator → returns to prose
- Fabricated URL rejected
- Extension past terminal (e.g. allowed = `/x`, model emits `/xz`) rejected
- Prefix-sharing URLs (`/a` is a prefix of `/ab`) both reachable
- Markdown link form `[label](https://a.test/x)` works
- HTTP/HTTPS schemes outside the trie rejected
- Multi-URL response with both URLs valid
- URL start straddling byte-by-byte input

## Remaining integration scope

Six concrete steps. Estimated 3-4 hours of focused work. Order
matters: 1→2→3→4→5→6. Each step compiles and passes tests
independently; commit between steps.

### Step 1 — `CompletionRequest` field

File: `sovereign/crates/sovereign-core/src/types.rs:77`

Add field, **end** of struct so default serialization picks it up:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub url_allowlist: Option<Vec<String>>,
```

Update all `new()` / `Default` constructors in the same file (2 sites)
to initialize to `None`. Also touch `CompletionRequest` builders in
`sovereign_mesh::inference_adapter` if any (likely
`build_completion_request`).

Test: workspace `cargo check --features corpus-engine/treesitter`
should remain green.

### Step 2 — Per-model vocab_bytes cache

File: `sovereign/crates/sovereign-inference/src/embedded.rs`

`json_constraint::JsonConstraint::new` already walks the model's
vocab on first call and caches it (see the `vocab_cache` /
`vocab_trie_cache` patterns at the top of `json_constraint.rs`).
The URL constraint needs the SAME `Arc<Vec<Vec<u8>>>`.

Two options:
- **Reuse JsonConstraint's vocab cache** by exposing a public function
  in `json_constraint.rs` that returns the cached `Arc<Vec<Vec<u8>>>`
  for a given `LlamaModel` (cheapest).
- Build a parallel cache for URL constraints (defensive isolation,
  more code).

Recommended: option 1. Add to `json_constraint.rs`:

```rust
pub fn vocab_bytes_for(model: &LlamaModel) -> Arc<Vec<Vec<u8>>> {
    // exposes the existing internal cache; same key as JsonConstraint::new
}
```

### Step 3 — `ConstrainedSampler` extension

File: `sovereign/crates/sovereign-inference/src/embedded.rs:6799`

Add field:

```rust
pub struct ConstrainedSampler {
    inner_explore: LlamaSampler,
    inner_content: LlamaSampler,
    constraint: Option<crate::json_constraint::JsonConstraint>,
    url_constraint: Option<crate::url_constraint::UrlAllowlistConstraint>,  // NEW
    non_latin_denylist: Option<std::sync::Arc<Vec<bool>>>,
}
```

In `sample()` (around line 6829), after the JsonConstraint mask runs
but before the chain samples, also apply the URL constraint mask:

```rust
if let Some(jc) = self.constraint.as_mut() { jc.mask(&mut data); }
if let Some(uc) = self.url_constraint.as_ref() { uc.mask(&mut data); }  // NEW
// ... then chain.sample(&mut data, ...)
```

Note: URL constraint's `mask` is `&self` not `&mut self` (the simulation
clones the cursor). That's intentional — the state only advances on
`accept`, not on `mask`.

In the `accept` flow (wherever the sampler reports the chosen token
back), call both:

```rust
if let Some(jc) = self.constraint.as_mut() { jc.accept(token); }
if let Some(uc) = self.url_constraint.as_mut() { uc.accept(token); }  // NEW
```

**Hot-path concern**: `mask()` iterates the full vocab (152K tokens)
per generation step. Per token: clone the cursor (small — a u32 or
a ≤16-byte Vec), simulate bytes (avg 2-3 per token), check trie edges
(O(1) per byte). Total: ~3-5 simple operations × 152K = ~500K ops per
generation step. Should be well under 1ms on hot CPU. If profiling
shows it's a bottleneck, the JsonConstraint pattern of per-state mask
caching applies here too (see `mask_cache` in `json_constraint.rs`).

### Step 4 — `build_sampler` wiring

File: `sovereign/crates/sovereign-inference/src/embedded.rs` — find
`build_sampler` (~line 6925).

```rust
let url_constraint = request
    .url_allowlist
    .as_deref()
    .and_then(|urls| {
        let vocab_bytes = crate::json_constraint::vocab_bytes_for(model);
        crate::url_constraint::UrlAllowlistConstraint::new(urls, vocab_bytes)
    });

ConstrainedSampler {
    // ... existing fields ...
    url_constraint,
}
```

### Step 5 — HTTP request → CompletionRequest

File: `commonwealth/crates/commonwealth-api/src/routes_inference.rs`

The OpenAI chat-completions handler builds `CompletionRequest` from
the JSON body. Find where existing extension fields like
`sampling_mode` and `chat_template_kwargs` are extracted; add a
sibling extraction for `url_allowlist`:

```rust
let url_allowlist = body
    .get("url_allowlist")
    .and_then(|v| v.as_array())
    .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect::<Vec<_>>());

let req = CompletionRequest {
    // ... existing ...
    url_allowlist,
};
```

If `routes_inference.rs` uses a typed extension struct, add the field
there instead of raw `Value::get`.

### Step 6 — Gym runner populates the allowlist

File: `sovereign/crates/sovereign-cli/src/search_gym_cmd/runner.rs`

After each tool call returns mock_urls, accumulate them and pass
into the NEXT turn's request body:

```rust
// Before turn 1's POST:
if !tx.mock_urls.is_empty() {
    request["url_allowlist"] = serde_json::Value::Array(
        tx.mock_urls.iter()
            .map(|u| serde_json::Value::String(u.clone()))
            .collect()
    );
}
```

### Validation

Run the existing focused gym subset:

```
sovereign search-gym run \
  --fixture 07_multicorpus_tangential_local \
  --fixture 08_multicorpus_stale_local \
  --replays 5 --judge-model Qwen3.5-9B-UD-MTP-Q6_K_XL --json \
  > /tmp/search-gym-runs/3c_grammar.json
```

Expected results post-integration:
- **Fixture 07**: zero `must_not_cite_url_outside_mock` failures (was 4-5/5 in iter5-10). Some replays may still fail on other axes (semantic predicate framing), but URL fabrication should be impossible.
- **Fixture 08**: same — `marketsentry.io/quote/NVDA/after-years` etc. should be unreachable.
- **No regressions** on 01-06, 09, 10. The constraint is no-op when `url_allowlist` is absent, so requests without allowlist behave unchanged.

Then run full bench:

```
sovereign search-gym run --replays 10 --judge-model Qwen3.5-9B-UD-MTP-Q6_K_XL --json \
  > /tmp/search-gym-runs/3c_final.json
```

Aggregate target: ≥85%. Per-fixture target: ≥80% (closer to ≥90% with judge variance smoothed by multi-judge consensus already in place).

## Risk register

| Risk | Mitigation |
|---|---|
| **Constraint kicks in inappropriately** (e.g. when model is generating tool-call JSON args that include a URL) | Constraint should be disabled when JsonConstraint is active. Check in `sample()`: if `self.constraint.is_some()`, skip the URL mask. Tool-call argument URLs are validated by JsonConstraint's schema instead. |
| **Mask iteration too slow** on hot path (152K vocab × per-token cost) | Profile after wiring. Per-state mask caching (mirror JsonConstraint's pattern) is the fallback if needed. Pre-bake: most token bytes don't contain `http`/URL chars, so the simulation short-circuits fast for them. |
| **Cursor desync with constraint vs. text** | The cursor advances ONLY in `accept(token)`. If the upstream sampler emits a token we didn't predict (e.g. a forced token from jump-forward), `accept` still feeds those bytes. Important: ALWAYS call `accept` for every emitted token, never skip. |
| **`http://` or `https://` straddling multi-byte tokens** | Already handled by the 16-byte sliding window in `InProse`. Test `url_start_straddling_byte_boundary` exercises this. |
| **URL inside a `<think>` block** | The model may emit a URL inside a thinking block; we don't want to constrain that. Two options: (a) accept and constrain anyway (think-block URLs aren't user-visible so the constraint just trims them — harmless). (b) detect `<think>` open/close in the byte stream and pause the constraint. Option (a) is simpler; revisit if observed problems. |

## Out of scope (v1)

- MTP path: when MTP-tools gate is lifted (task #18), the constraint will need to run on each MTP iteration's accept. Plumbing exists in `generate_sync_mtp` (the per-accept loop); the constraint mask call drops in the same way.
- Streaming path: same shape as non-streaming once mtp_session lifecycle is sorted (task #17).
- Production desktop integration: the gym proves the mechanism. Production wiring goes in `sovereign-tools::SearchTool` result-rendering + system prompt assembly (task #22 — already filed).
- Performance optimization beyond what's needed for the gym. If users complain about per-token latency, profile with `tracing` events first.

## File map

```
sovereign/crates/sovereign-inference/src/url_constraint.rs  ← landed (this session)
sovereign/crates/sovereign-inference/src/lib.rs             ← landed (module export)
sovereign/crates/sovereign-inference/src/json_constraint.rs ← step 2 (vocab cache export)
sovereign/crates/sovereign-inference/src/embedded.rs        ← steps 3, 4 (sampler integration)
sovereign/crates/sovereign-core/src/types.rs                ← step 1 (request field)
commonwealth/crates/commonwealth-api/src/routes_inference.rs ← step 5 (HTTP extraction)
sovereign/crates/sovereign-cli/src/search_gym_cmd/runner.rs ← step 6 (gym wiring)
```

## Phase 3c progress at handoff

| Iter | Aggregate | Notes |
|---|---|---|
| baseline | 50% | Raw starting point |
| iter6 (no-preface narration) | 50% on focused | Fixture 05 unstuck |
| iter8 (allowlist trailer) | **67%** | Citation behaviour materially lifted |
| iter9 (multi-judge consensus) | **73%** | Judge variance cut on borderline cases |
| iter10 (extract_urls bracket terminators) | 60% | Variance dip — sampling noise dominates between iters at 5 replays |

Grammar URL emission is the structural fix that breaks past the
~75% prompt-level ceiling cleanly. The core is tested; integration
is the remaining work tracked in this doc.

## Reference: related tasks

- #18 — Lift MTP gate for tools-bearing requests (perf-orthogonal; would 2× iteration speed)
- #20 — Generalize prefix-cache hybrid-detector (covers non-MTP hybrids like Darwin)
- #22 — Wire SEARCH_SYSTEM_PROMPT into production desktop chat assembly
- #23 — This work (URL grammar constraint)
