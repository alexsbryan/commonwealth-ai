# ATOS Runner — improvement loop handoff

## Direction change (2026-05-07)

The sovereign-cli runner (`sovereign atos run`) is being replaced by an opencode-ralph + MCP tools approach. The daemon provides `atos_verify` (the gate), opencode-ralph provides the loop, and the agent follows an ATOS loop prompt.

## End-to-end demo command

```bash
cd ~/dev/atos-experiment-oicp-types

# Ensure daemon is running with atos_verify MCP tool
sovereign daemon start
curl -sS http://localhost:9741/mcp -X POST \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"tools/list","id":1}' \
  | python3 -c "import sys,json; print([t['name'] for t in json.load(sys.stdin)['result']['tools'] if 'atos' in t])"

# In opencode, run:
#   /ralph-loop "IMPLEMENT the OICP v0.3 spec as a Rust crate called oicp-types.
#
#   ATOS AUTONOMOUS LOOP — follow this cycle exactly:
#
#   1. PLAN: Read oicp-v0.3.md and DESIGN.md. Create PLAN.md with numbered
#      steps. Each step MUST have **Files:** and **Verify:** lines.
#      Step-01 MUST create Cargo.toml + src/lib.rs (scaffold first).
#      Verifies MUST use -- test_name filter (e.g. cargo test --test X -- name).
#
#   2. EXECUTE (per step): Read the step from PLAN.md. Write code using
#      write/edit/bash tools. Create parent dirs first (mkdir -p).
#      Before exiting, call atos_verify(workdir, verify_cmd, files_touched).
#      If passed=true: mark step [DONE] in PLAN.md, move to next step.
#      If passed=false: fix issues, retry. Max 3 retries per step.
#
#   3. REASSESS (when stuck): After 3 retries, edit PLAN.md to rewrite
#      or merge the failing step. Don't change DONE steps.
#
#   4. DONE: When ALL steps pass atos_verify, write DONE.md summarizing
#      what was built. Output COMPLETE." \
#   --completion-promise "COMPLETE" --max-iterations 50
```

## 2026-05-08 — root-cause + fix for "opencode truncation"

The "3800-byte tool-call truncation" theory in the prior brief was a misread.
Two real failure modes were happening, not one:

### Failure A — daemon 422 on content arrays (FIXED: commit `d31ae6c`, May 7)

opencode's AI SDK sends `messages[i].content` as either a string OR an array
of `{type:"text", text:"..."}` parts (OpenAI-multimodal shape). The daemon's
`ChatCompletionRequest` deserializer only accepted strings, so anything past
the first turn (which always carries assistant tool-call replies + tool
results in array form) got rejected with HTTP 422:

> `messages[N].content: invalid type: sequence, expected a string`

opencode logged the request body and surfaced it as an `AI_APICallError`,
but to the user it looked like the model "exited without writing." Truncated
write tool parts in the opencode SQLite DB were collateral from prior turns
that *had* succeeded, not from a 3800-byte ceiling.

Fix: `commonwealth-api::openai_types::deserialize_message_content` now
accepts both shapes via `#[serde(untagged)]` and concatenates `type:"text"`
parts. Verified in repro (multi-turn payload with tool_calls + array
content → HTTP 200).

### Failure B — thinking-mode eats the tool-call budget (FIXED: this commit)

Even with content arrays accepted, opencode runs against `commonwealth/primary`
(→ `FINAL-Bench_Darwin-35B-A3B-Opus-Q6_K_L`) failed with the model emitting
only fragments of a `<tool_call>{...` payload. Daemon log:

```
inference.complete: done ... tokens_used=14544 response_chars=372
inference adapter: tool_call parse failed payload={"name":"edit",...
                                       "newString":"...working with t  ← truncated mid-arguments
sovereign inference adapter: complete served ... tool_calls=0 parse_errors=1
```

Reading: the model burned 14,544 tokens on a `<think>` prelude (stripped
before the daemon parsed tool calls), then ran out of output budget partway
through the tool call. The `<tool_call>` text the parser sees is incomplete
JSON, fails to balance, returns no tool calls. opencode receives the
unstripped text in `content` and reports it as raw model output.

The Commonwealth daemon already supports `think_budget=0` (injects
`/no_think` into the system prompt) and `chat_template_kwargs.enable_thinking=false`
(OpenAI-extension shape). Either knob makes the same FINAL-Bench request
emit a clean tool call in ~50 tokens instead of 14K. But opencode doesn't
know about either, so neither was reaching the daemon.

Fix: `inference_adapter::resolve_think_budget()` defaults `think_budget`
to `Some(0)` when (a) the caller didn't set it AND (b) `tools` is present
and non-empty. Caller-supplied values (any explicit `Some(n)`, including
`>0` for debugging) still win. Tests in
`sovereign/crates/sovereign-mesh/src/inference_adapter.rs::tests`.

### Verification path

After rebuild + daemon restart:

1. Smoke (no opencode): `curl /v1/chat/completions` with one tool, one
   user message asking the model to use it. Expect `finish_reason="tool_calls"`,
   one entry in `tool_calls[]`, completion_tokens < 200.
2. Opencode CLI: `cd ~/dev/atos-experiment-oicp-types && opencode run --pure
   --model commonwealth/primary --dangerously-skip-permissions "<small task>"`.
   Expect a real edit/write tool execution and the file change on disk.
3. Full ralph-loop: from opencode TUI, run the demo command above against
   the OICP repo with a clean state.

## 2026-05-08 — what scales vs. what's still demo-shaped

The goal is a **scalable system for any local-Commonwealth developer
workflow**, not "make this OICP repo finish." Below: what generalizes
already, what's still glued to this experiment.

### Generic primitives (work for any workflow)

- **Daemon `think_budget=0` default when tools present** — any
  tools-using OpenAI-compatible client benefits without knowing about
  Commonwealth extensions. (Just landed in inference_adapter.rs.)
- **`atos_verify` MCP tool** — workflow-agnostic verify gate that
  takes `{workdir, verify_cmd, files_touched}` and runs hollow-file +
  untouched-file gates. Exposed at `localhost:9741/mcp`.
- **`sovereign-atos` opencode plugin** — fire-and-forget ledger of
  every tool.execute.before/after into the run ledger; degrades
  silently when env vars absent. No-op outside of an ATOS run.
- **opencode `config.json`** — provider + MCP wiring is one file,
  copy-pasteable to any repo (see
  `~/dev/atos-experiment-oicp-types/.opencode/config.json`).

### Demo-shaped (still glued to this experiment)

- **Loop driver `/tmp/atos_loop.sh`** — bash script with the prompt
  and stop condition baked in. Doesn't generalize.
- **Ralph plugin lives in `.opencode/disabled/`** — the in-TUI loop
  driver isn't active. It's the natural home for "run this prompt
  on a session-idle event with a completion-promise" but we haven't
  validated it against the current opencode SDK.
- **PHASE convention (PLAN.md / DONE.md)** — encoded only in the
  prompt, not enforced anywhere. No template, no validator.
- **No bootstrap CLI** — to start a new workflow you currently:
  install opencode → write `.opencode/config.json` → install plugin →
  craft prompt → write loop driver. Five manual steps that should be
  one `commonwealth workflow init <repo>` invocation.

### Concrete next steps to scale

1. Validate ralph plugin against current opencode SDK; restore from
   `disabled/` if it works, otherwise document the breakage and ship
   a small replacement.
2. Promote the staged-action prompt template into a versioned
   resource (e.g., `~/.commonwealth/workflows/atos.prompt.md`) so the
   prompt isn't ad-hoc per-experiment.
3. Wrap `opencode run --pure --model commonwealth/primary
   --dangerously-skip-permissions <prompt>` + the loop logic into a
   single CLI: `commonwealth workflow run <repo> <prompt-name>`.
4. Doc one paragraph + one shell snippet in `commonwealth/README.md`:
   "How to point a fresh repo at your local Commonwealth daemon and
   run an autonomous workflow."

### Open question — long-tool-call reliability

Even with `think_budget=0` (no `<think>` block), FINAL-Bench-35B drops
the closing `</tool_call>` and sometimes the inner closing braces on
emissions over ~300 chars. The lenient JSON-balance parser
(`parse_tool_calls_with_errors`) recovers most of these but not all.
The procedural workaround the staged-action prompt uses: every tool
call stays under ~1500 chars, splitting any larger work across
iterations. This is a model-driver pact, not a structural fix; a
grammar-constrained tool-call sampler (the same path Phase 1 atlas
uses for JSON Schema output) would close it for good.

### 2026-05-08 evening — autonomous-loop status check

What we ran end-to-end: opencode → daemon → tools → file edited →
`cargo check` clean. ✅ The plumbing works.

What stalls in the multi-step autonomous loop (driving an empty repo
to DONE.md against PLAN.md): FINAL-Bench-35B does not reliably emit a
balanced `<tool_call>` envelope when the call's arguments push past
~700 chars. The daemon's lenient parser recovers some of these (drop
the missing `</tool_call>` if the JSON is balanced) but not the cases
where the inner `}}` closers are also missing — and at that emission
length they often are. Symptom: `tool_calls=0 parse_errors=1` in
daemon log, opencode receives the unstripped fragment as raw text and
ends the session. We saw this both on a "write the whole PLAN.md"
turn and on a "write a 7-step skeleton" turn — the model is fragile
above a length threshold that varies with prompt complexity.

Procedural mitigation that did work for one-tool-call EXECUTE turns
(the daemon-side smoke test): ask for one tiny edit, verify with
`cargo check`, exit. Daemon log shows `tool_calls=1 parse_errors=0`,
file changed on disk. So **a tools-driven loop on FINAL-Bench works
when every individual model turn fits inside the model's reliability
envelope** — empirically ~300 chars of tool-call payload. Anything
larger should be decomposed in the prompt or written by a smaller
sequence of edits.

### Two structural moves to harden this for any workflow

1. **Grammar-constrained tool calls in the daemon.** When the request
   has `tools` (and especially `tool_choice: required`), install a
   JSON-Schema-derived grammar in `LlamaSampler::llguidance` so the
   sampler is forbidden from emitting an unbalanced tool-call body.
   The atlas Phase 1 path already does this for `response_format:
   {type: json_schema}`; the same plumbing applied to tool schemas
   would close the FINAL-Bench drop-closing-tag bug for good.
2. **Primary slot bound to a model with cleaner native tool-call
   emission.** Qwen3-Coder-30B and Qwen3.5-9B-vOP are advertised
   models on this daemon but neither is loaded as a runtime slot.
   Loading-by-`/internal/models/load` is gated to "embedded llama.cpp
   provider" mode — the current daemon refuses with HTTP 400 ("this
   inference provider does not support runtime slot load"), so the
   slot binding has to come from `setup_config.toml` + restart, not
   a runtime call. Worth a separate spike.

Until one of those lands, the procedural pact is: every workflow
prompt must keep individual tool-call payloads small (≤300 chars
arguments). The state-injection driver in `/tmp/atos_loop.sh`
demonstrates one path: inline current state into the prompt so the
model never has to read, and constrain every step's edit envelope
to one struct or one fn.

### 2026-05-08 (later) — landed structural moves + new bottleneck

Two of the structural moves landed and were observed in a real run:

1. **Coder slot bound** — added `code = ".../Qwen3-Coder-30B-A3B-Instruct-Q6_K.gguf"`
   in `~/Library/Application Support/sovereign/config.toml`. Daemon
   auto-installs `coder` + `commonwealth/coder` aliases. `~/.config/opencode/opencode.json`
   was extended with a matching `"coder"` model so opencode can address
   it. End-to-end confirmed: opencode → commonwealth/coder → write
   tool → file written → cargo check exit 0.
2. **Parser hardening** — `escape_unescaped_control_chars_in_string_values`
   in `sovereign-inference::embedded` runs as a normalize-and-retry
   pre-pass when serde rejects a tool-call body. Specifically targets
   the Qwen3-Coder failure mode where the model emits a balanced JSON
   envelope but with raw `\n`/`\r`/`\t` bytes inside string values
   instead of the escape forms. 4 unit tests pin the behaviour.
   Empirical effect on the Qwen-Coder smoke: `tool_calls=1, args_len=4822,
   content_len=4636 lines=134, completion_tokens=1220`. Pre-fix:
   `tool_calls=0, parse_errors=1`.

**The new dominant bottleneck is character corruption on long edits.**
Qwen-Coder produces syntactically clean tool calls of single-shot
file writes (the smoke test was clean), but multi-iteration loops
that progressively edit the same file accumulate character-level
corruption inside the source: `f3 2` for `f32`, `Lat encyClass` for
`LatencyClass`, `Inference Requirements` split with a space across
the struct name. The cargo-check gate catches these and prevents
bad steps from being marked DONE — but each broken iter costs ~5–25
minutes of wallclock and the model occasionally fails to recover.
On the OICP test (DESIGN.md → PLAN.md → src/lib.rs):

- Iter 1 (8.6 min): wrote PLAN.md skeleton + initial enums (corrupted).
- Iter 2 (3 min): fixed corruption, marked steps 1+2 DONE. cargo OK.
- Iter 3 (3 min): added CapabilityHint (corrupted again).
- Iter 4 (5 min): fixed corruption + simplified Hint to compile. cargo OK.
  Did not flip step 3 to DONE — the simplified shape diverges from
  the spec, the model judged the step incomplete.
- Iter 5 (24 min): tried to add CapabilityClaim + InferenceRequirements
  in one go. Compounding corruption (`Lat encyClass`, `Inference Requirements`)
  + many tool calls failing schema validation (truncated `command`/
  `pattern`/`filePath` keys). cargo failed with 6 errors. No DONE flips.

So the loop *does* drive progress against a real spec — two real
steps DONE, ~74 lines of valid Rust on disk — but stalls on the
fourth onwards without supervision. The pattern looks like a model
fluency cliff at ~80–100 lines of edited Rust per turn.

### Next structural move — grammar-constrained tool calls

`LlamaSampler::llguidance` is already wired into the daemon for
`response_format: {type: json_schema}`. The change is:

- When `tool_choice = "required"` *and* `tools[]` has a single
  function, build the schema for the chosen function's `parameters`
  and pass it to the sampler.
- When `tool_choice = "required"` with multiple tools, build a
  `oneOf` schema over the tools' parameter schemas, plus the
  envelope `{name: enum<tool_names>, arguments: <schema_for_name>}`.
- For `tool_choice = "auto"`, defer — would need an alternation
  grammar (Lark) allowing either tool-call envelope or plain text.

The single-tool / `"required"` case is the smallest viable slice.
It would close FINAL-Bench's drop-closing-tag bug, Qwen-Coder's
unescaped-newline bug, and the character-drop-in-Rust pattern (the
grammar would force the emitted Rust to be syntactically valid).
Tools-using clients that don't pass `tool_choice` would still hit
the model's native discipline; opencode is one such client, so a
companion change in opencode/AI SDK provider config to pass
`tool_choice: "required"` would be needed too.

### 2026-05-08 (later) — grammar-constrained tool calls landed

Implemented in `inference_adapter::tool_envelope_schema_for` +
`tool_envelope_schema_for_with_env` + `parse_tool_envelope_direct`.
When the daemon sees tools + `tool_choice="required"` (or any
non-`"none"` value if `SOVEREIGN_FORCE_TOOL_CALLS=1` is set on the
daemon), it builds a JSON Schema `oneOf` over each tool's parameter
schema with `name` pinned to a single-value enum, installs it as
`req.structured_output`, and the existing `LlamaSampler::llguidance`
path masks tokens to that grammar.

The model emits a clean `{"name":"...","arguments":{...}}` body with
no `<tool_call>` markers. A new direct-envelope parser
(`parse_tool_envelope_direct`) handles that shape; the legacy
marker-based parser is the fallback.

Test coverage: 9 new unit tests in `inference_adapter` (envelope
schema generation, env override, direct-envelope parsing, including
the raw-newline normalization). 28 inference_adapter tests pass.

#### What this fixed
- FINAL-Bench drop-closing-tag: gone (sampler can't pick a token that
  unbalances the body).
- Qwen-Coder raw-`\n` inside string values: gone (grammar requires
  the escape form).
- Tool-call envelope corruption that previously surfaced as
  `parse_errors > 0` in the daemon log: empirically gone on the
  smoke test (`finish=tool_calls, parse_errors=0`, clean 5298-char
  envelope on a 152-line file write).

#### What this did NOT fix — and why

Grammar constrains the *envelope shape*, not the *content of a string
field*. Inside the `arguments.content` string the model can still
emit corrupted Rust: in the OICP demo loop, `LatENCYClass::Extended`
(capital ENCY) appeared mid-source-code. The grammar's
JSON-Schema-driven mask sees a valid JSON string value; only a Rust
grammar applied to that string's contents would catch it. That's a
separate, much larger lift.

Net effect on the autonomous loop: the wire-format failure modes are
closed, but the file-content corruption modes still cause cargo
check to fail and the step to stay PENDING. The cargo gate is doing
its job; the loop just iterates more.

#### What surprised — throughput

The grammar pass is expensive at opencode's prompt size. Every token
of every turn runs the Rust JSON validator over `buffer + token`
candidates; with a ~31–34K-token prompt accumulated over the
multi-turn opencode session, each turn cost ~100s on Qwen-Coder Q6_K
on Apple Metal. After 43 turns (~70 min wallclock) the loop had only
flipped Step 1 to DONE; the rest were still failing on character
corruption inside content strings. Unworkable at this scale.

#### Three follow-on moves to make this practical

1. **Trim opencode's per-turn prompt.** opencode bundles its full
   tool-schema descriptor every turn (~13K tokens of system prompt +
   tools alone). For a tools-driven autonomous loop, most of that is
   redundant — a smaller schema set + a tighter system prompt would
   roughly halve the token bill, halve the per-turn latency.
2. **Cheaper grammar evaluator.** The current `JsonConstraint`
   re-walks the partial JSON for every candidate token. A
   tokenized-prefix tree or pre-compiled DFA would reduce
   per-token cost from O(prompt_so_far) to roughly O(1).
3. **Per-tool / per-string-field Rust grammar option.** When a tool
   has a `content` parameter that the operator declares is "Rust
   source", install a Rust-grammar mask over those bytes specifically.
   Closes the `LatENCYClass` failure mode at the source. Big lift —
   needs a working Rust grammar lib that integrates with
   `LlamaSampler` — but is the structural answer to "model corrupts
   characters mid-edit."

### 2026-05-08 (later) — three follow-on moves landed as observability layer

All three were landed as opt-in / always-on observability surfaces
rather than as full optimisations. The reason: the diagnosis behind
each move is partly inferential — the next iteration needs DATA from
a real grammar-active opencode loop to know which lever to pull
hardest. Shipping observability first turns the next session into
data-driven optimisation rather than another guess.

#### (1) Prompt compactor — `sovereign-mesh/src/prompt_compactor.rs`

- `PromptSizeReport::measure(&request)` runs at the top of every
  `chat_completion` and `chat_completion_stream`. Logs at
  `tracing::info` with `phase=pre_compact` (always-on) and
  `phase=post_compact` (only when the compactor ran). Per-role
  character accounting: `system_chars`, `user_chars`,
  `assistant_chars`, `tool_chars`, `other_chars`,
  `tools_schema_chars`, plus `message_count`, `tool_count`,
  `total_chars`. Greppable as `prompt_size:`.
- `PromptCompactor::from_env()` — opt-in. One knob:
  `SOVEREIGN_TOOL_RESULT_MAX_BYTES=N` caps individual `role="tool"`
  message bodies, replacing the middle with a truncation marker.
  Generic protection: any client's bash/cargo tool result can
  produce arbitrarily-large output, model rarely needs more than a
  few KB. Char-boundary safe (multibyte UTF-8 handled).
- 13 unit tests pin per-role accounting, cap behaviour, multibyte
  safety, env-parsing edge cases (zero, garbage, unset).
- Disabled-default = bit-identical behaviour to pre-change daemon.

What this gives us: per-turn data on what opencode actually sends —
how the 31–34K is composed across system/user/tool/assistant +
schema. Run one opencode iter, read the daemon log, design the
next-iteration trim against actual byte-class proportions instead
of guessing.

#### (2) Grammar timing — `sovereign-inference/src/json_constraint.rs`

- `JsonConstraint::timing: Option<Mutex<TimingState>>`. Built via
  `build_timing` which reads `SOVEREIGN_GRAMMAR_TIMING=1`. Disabled
  default → zero overhead.
- `mask()` and `accept()` wrap their bodies with `Instant::now()`
  → cumulative `mask_calls`, `mask_total_us`, `accept_calls`,
  `accept_total_us` counters.
- `Drop` impl emits one `tracing::info` line at the end of every
  generation: `grammar_timing: per-constraint summary` with all
  cumulative counters + per-call averages + final emitted_len.
- `build_timing` factored as `fn build_timing(env_get: F)` so unit
  tests can pin every truthy/falsy/garbage shape without mutating
  process-global env.
- 4 unit tests for env parsing.

Hypothesis to test (current read): the dominant per-turn cost on
the 100s/turn observation is *prompt-eval* (Apple Metal at ~300
tok/s prefill × 31K context ≈ 100s), NOT mask cost. The mask is
vocab_size × ~10ns per candidate ≈ 2ms/token. Across a 1000-token
generation that's ~2s. If a real run with `SOVEREIGN_GRAMMAR_TIMING=1`
confirms `mask_total_us << 100M`, the "cheaper grammar evaluator"
move drops in priority and "trim prompt" stays the bottleneck.

#### (3) Source-content validator framework — `sovereign-mesh/src/source_content_validator.rs`

- `SourceContentValidator` trait: `validate(&self, source: &str)
  -> Result<(), String>`. Per-language. Send + Sync.
- `ValidatorRegistry`: `String → Box<dyn SourceContentValidator>`.
  Empty default; future PRs register via
  `registry.register("rust", Box::new(RustValidator::new()))`.
- `walk_schema_for_markers` recurses through `properties` looking
  for the `x-source-content` extension keyword on string fields.
  Builds a dotted JSON path (`edits.newSource`) per match.
- `lookup_value` resolves a dotted path inside a parsed-arguments
  object.
- `validate_tool_calls` is the public entry. Walks each call's
  schema, extracts marked-field values, runs registered validators,
  returns `Vec<Finding>`. Each Finding logs at
  `tracing::warn`/`error` as `source_content_validation:`.
- Wired into `chat_completion` after `parsed_calls` is computed —
  registry is empty for now (zero overhead) but the integration
  point is fixed.
- 15 unit tests pin schema walking (top-level/nested/absent),
  value lookup (root/dotted/missing), and the validation surface
  (empty registry, missing tool, unmarked fields, malformed args,
  clean source, language-not-registered, two-call mixed).

Schema marker convention (documented in module docs):

```json
{ "type": "object",
  "properties": {
    "filePath": { "type": "string" },
    "content":  { "type": "string", "x-source-content": "rust" }
  } }
```

The `x-source-content` keyword is unknown to standard JSON Schema
validators (they ignore unknown keywords cleanly) but recognised
here. Tools-using clients that want their writes validated can
publish their tool schemas with the marker — including opencode
(via a config hook) and any future first-party tool definitions on
the daemon side.

What this is NOT yet: it does not ship a Rust validator. The
framework is wired so a future PR can drop one in via
`registry.register("rust", ...)` without touching `chat_completion`
or any existing module — that's the SOLID separation
(`source_content_validator` knows nothing about specific languages,
`inference_adapter` knows nothing about validators beyond the
registry's existence).

#### What's ready for the next session

Three observability surfaces, all opt-in (or empty by default), all
glassbox. The first opencode autonomous-loop run with the env vars
set produces:

- per-turn `prompt_size:` lines → exact byte/char composition
- per-generation `grammar_timing:` summary → mask vs accept cost
- per-call `source_content_validation:` findings (once a Rust
  validator is registered) → corruption-at-emission visibility

The optimisation moves themselves (trim system-prompt prose,
DFA-cached evaluator, syn-based Rust validator) become 1–2 line
diffs against this scaffolding.

## Files changed

All edits are in `sovereign/crates/`:

### `sovereign-tools/src/code/atos_utils.rs` (NEW)
Shared validators extracted from `atos_cmd/run.rs`:
- `run_verify_cmd`, `detect_hollow_files`, `detect_untouched_files`, `snapshot_file_mtimes`
- `is_weak_verify`, `step_goal_is_scaffold`, `detect_missing_scaffold`
- `extract_verify_cmd`, `strip_failure_cruft`, `parse_inline_list`, `split_state_marker`
- `truncate`, `sha256_hex`

### `sovereign-tools/src/code/atos_verify.rs` (NEW)
MCP tool: runs verify command + hollow/untouched gates. Registered in daemon, exposed via MCP at `localhost:9741/mcp`.

### `sovereign-cli/src/atos_cmd/run.rs`
Imports from `atos_utils` instead of defining validators locally. Pre-seeder unwired from EXECUTE. Scaffold design-skip in `build_execute_prompt`. Missing-scaffold detection. Resume fix (in_progress → Failed).

### `commonwealth-api/src/openai_types.rs` (May 7, d31ae6c)
`ChatMessage::content` accepts string OR array-of-text-parts.

### `sovereign-mesh/src/inference_adapter.rs` (May 8)
`resolve_think_budget()` defaults to `Some(0)` when tools present and caller didn't override. Unit-tested.

Grammar-constrained tool calls landed (`tool_envelope_schema_for`,
`tool_envelope_schema_for_with_env`, `parse_tool_envelope_direct`).
Compactor + source-content-validator integration points wired at
the top and middle of `chat_completion`.

### `sovereign-mesh/src/prompt_compactor.rs` (NEW, May 8)
Always-on per-role char accounting; opt-in tool-result cap via
`SOVEREIGN_TOOL_RESULT_MAX_BYTES`. 13 unit tests.

### `sovereign-mesh/src/source_content_validator.rs` (NEW, May 8)
Pluggable per-string-field source-content validation framework.
Schema marker `x-source-content`, empty-default registry, post-parse
hook in `chat_completion`. 15 unit tests.

### `sovereign-inference/src/json_constraint.rs` (May 8)
Opt-in grammar-mask wall-clock timing via
`SOVEREIGN_GRAMMAR_TIMING=1`. Drop-time summary log. 4 unit tests
for env parsing.

### Other files
- `sovereign-tools/src/code/mod.rs` — added atos_utils, atos_verify modules
- `sovereign-tools/src/lib.rs` — exported AtosVerifyTool
- `sovereign-tools/src/mcp_surface.rs` — added atos_verify to MCP_TOOLS_ALWAYS
- `sovereign-tools/src/manifest.rs` — added AtosVerifyTool descriptor
- `sovereign-cli/src/daemon_cmd.rs` — registered AtosVerifyTool

### OICP experiment repo
- `.opencode/plugins/sovereign-atos.ts` — opencode plugin (ledger + digest preamble)
- `.opencode/disabled/ralph.ts` — Ralph loop plugin (currently disabled; restore to `.opencode/plugin/` to re-enable)
- `.opencode/command/ralph-loop.md` — /ralph-loop command
- `.opencode/command/cancel-ralph.md` — /cancel-ralph command
- `.gitignore` — added ralph-loop.local.md
