# Gemma 4 E4B-it gym findings (2026-05-19)

First pass on `gemma-4-E4B-it-Q6_K.gguf` against the codex-harness gym
(9 fixtures, the same suite Qwen3.6-A3B distill passes 45/45 on).

Starting point: invisible — the pipeline failed cold because Gemma's
chat template was never reaching the model. Two correctness bugs in
the chat-template path were masking everything else. Once both are
fixed, **Gemma scores 12/27 (44%) at N=3** with a clean failure
signature: passes on recovery/retry fixtures, fails on the apply_patch
emission fixtures.

## Bugs found and fixed in this pass

### 1. `chat_template` lookup truncation

`sovereign-inference/src/llama.rs::chat_template()` called
`model.get_chat_template(8 KiB)` and silently dropped the result on
any error via `.ok()`. Gemma 4's tool-aware Jinja template is ~12 KiB
encoded (it carries `format_parameters` / `format_function_declaration`
/ `strip_thinking` macros for native tool-calling). The 8 KiB cap
caused `BuffSizeError(needed)` on every call, returning `None`, which
sent every Gemma request through the plain-text-concat fallback. The
daemon's loud-warn at `embedded.rs:6404` was firing on every chat
turn — visible only to operators tailing logs.

Effect: no `<|turn>` role markers, no `<|tool>` declarations, no stop
discipline. Gemma generated until the 300s inference deadline (~14k
tokens of role-play) on the first apply_patch fixture. The "longest
known template ≈1200 bytes" upstream-comment is from llama.cpp's
pre-tool-calling era; HF-grown chat templates routinely cross 10 KiB.

Fix: retry on `ChatTemplateError::BuffSizeError(needed)` up to 256 KiB.
Same shape as the `token_to_piece_bytes` retry in `json_constraint.rs`.

### 2. minijinja renderer missing Python-shape methods

`embedded.rs::apply_chat_template_minijinja()` ran the Jinja template
through a stock `minijinja::Environment` with only `raise_exception`
registered. Gemma's template uses `message.get('reasoning')`,
`message.get('tool_calls')`, `value['type'] | upper`,
`part.split('<|channel>')`, and similar Python-dict / Python-str
methods that minijinja 2.19 doesn't ship.

Error surfaced as: `minijinja: render chat template: unknown method:
map has no method named get (in chat:236)`. The fallback to llama.cpp's
built-in template parser then *also* failed (`apply_chat_template
failed — BuffSizeError`, same family of issue: the binding's output
buffer is sized for ~1 KiB rendered prompts and Gemma's tool-laden
prompt cleared 4 KiB easily). Both fallbacks failing dropped to plain-
text concat.

Fix: register an `unknown_method_callback` that implements `.get()`,
`.split()`, `.startswith()`, `.endswith()`, `.upper()`, `.lower()`,
`.strip()` inline. Six lines per method — preferred over pulling
`minijinja-contrib`'s full `pycompat` dep for this surface.

After both fixes: minijinja renders Gemma's template cleanly. The
model now sees `<|turn>system\n…<turn|>` markers, stops on its
trained `<|tool_call>` boundary, and emits real tool-shaped output.

## Gym baseline (Gemma after correctness fixes)

| Fixture                          | Pass | Rate | Class                  |
|----------------------------------|------|------|------------------------|
| 001_write_stage_baseline         | 0/3  | 0%   | apply_patch emission   |
| 002_rg_loop_recovery             | 3/3  | 100% | recovery               |
| 003_path_typo_recovery           | 3/3  | 100% | recovery               |
| 004_xattr_loop_recovery          | 3/3  | 100% | recovery               |
| 005_write_under_codex_context    | 3/3  | 100% | apply_patch emission   |
| 006_write_with_failed_history    | 0/3  | 0%   | apply_patch emission   |
| 007_compressed_history_read_loop | 0/3  | 0%   | apply_patch emission   |
| 008_malformed_apply_patch        | 0/3  | 0%   | apply_patch repair     |
| 009_cargo_toml_as_json           | 0/3  | 0%   | apply_patch + TOML     |
| **total**                        | **12/27** | **44%** | |

Bucket pattern: every recovery-shape fixture (002/003/004) passes.
Most apply_patch-emission fixtures fail. The one apply_patch fixture
that passes (005) does so because the system prompt explicitly names
the heredoc form.

## Failure mode — Gemma emits sibling DSLs, not codex's apply_patch

Sample emissions caught from raw `/v1/chat/completions` probes:

```
fixture 001 (canonical task):
{"cmd":"apply_patch src/lib.rs\npub fn answer() -> u32 {\n    42\n}"}

fixture 006 (after a failed-history nudge):
{"cmd":"cat <<EOF > src/lib.rs\npub fn answer() -> u32 {\n    42\n}\nEOF",
 "prefix_rule":["oicp-types"]}
```

Two distinct shapes, neither of which `canonicalize_apply_patch_heredoc`
recognises:

1. **Bare apply_patch + body**, no heredoc opener. Has the
   `apply_patch` token (so the existing `must_contain: "apply_patch"`
   predicate would pass) but lacks `<<TAG`, `*** Begin Patch`,
   `*** Add File: <path>`, and the `+` body prefix. The canonicalizer
   strips at the `<<` check on line 594 of frontdoor.rs.

2. **Raw `cat <<EOF > path` heredoc**, dispensing with apply_patch
   entirely. No `apply_patch` substring at all, fails the predicate
   at the first `must_contain`. Also fabricates an extra field
   (`prefix_rule`) — Gemma is freer with the tool schema than the
   instruction block requested.

Both are intent-correct ("write this file with this content") but
syntax-wrong for codex's apply_patch contract. Same family as the
Qwen-era smokes that produced gym 005 (heredoc body without `+`
prefix) — just in a new shape we haven't taught the canonicalizer.

## Sampling — universal params hold, no per-mode quirks needed yet

The Gemma 4 model card publishes **one** decode profile across all
use cases: `temperature=1.0, top_p=0.95, top_k=64`. No instruct vs
code split is published. `ModelFamily::Gemma4::default_quirks()`
already pins exactly these values, so the three-mode sampler
currently picks the same params regardless of role — which matches
Google's recommendation.

Empirically across the 27 gym runs there's no sign of repetition
collapse, mode collapse, or temperature instability that would
motivate carving out an `instruct_*` or `code_*` override. The
failure mode is **emission shape**, not sampling.

Recommendation: keep `ModelFamily::Gemma4` quirks as-is. Revisit if
a future fixture shows distribution-shape symptoms.

## Architecture notes (ARCH_PRINCIPLES compliance)

- **Glassbox**: the chat_template lookup failure was visible as a
  `warn!` on every chat call; the operator could see it but the
  fix-it loop required reading two layers down to understand what
  the truncation meant. Memory note worth writing: "any
  `chat_template lookup returned None` on a model whose gguf
  actually has the metadata is a buffer-size bug, not a missing-
  template bug" (ARCH §9.1).
- **No whack-mole**: bumped the buffer with a retry-on-needed-size
  pattern (data-driven, not "double the constant"). Same shape as
  the existing `token_to_piece_bytes` retry in `json_constraint.rs`
  — consistent fix discipline.
- **Single responsibility**: minijinja method shim lives next to its
  one caller (`apply_chat_template_minijinja`). Six methods total,
  all string/dict — well under the §3.1 file-size ceiling.
- **Data vs program**: the new methods are program (Rust) because
  they translate ABI, not policy. Per-method behaviour matches
  Python's stdlib — no editable config surface needed.

## What's still broken (next iteration)

### High-leverage, gym-shaped

- **Bare `apply_patch <path>\n<body>` canonicalization**. Extend
  `canonicalize_apply_patch_heredoc` to also accept the no-heredoc
  shape. Detect `^apply_patch\s+<path>\n` followed by raw body
  lines; re-emit as canonical
  `apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: <path>\n+...\n*** End Patch\nEOF`.
  Captures fixtures 001 and probably 006-008.

- **Raw `cat <<TAG > <path>` → apply_patch rewrite**. A second
  pre-canonicalize pass that detects the cat-heredoc-redirect form
  and rewrites the intent into apply_patch form. The body content
  is preserved verbatim, the path is captured from the redirect
  target, the surrounding shape is re-emitted. Captures fixture 006
  and probably 009. Independent of Gemma — improves any backend
  that ever emits this shape (Gemma is the canary).

- **Stray-field strip**. When the model emits `{"cmd":..., "prefix_rule":[...]}`,
  the args still parse but carry fields the tool didn't declare.
  Strip anything that isn't in the tool's declared schema. Touches
  `canonicalize_exec_command_arguments`. Same rule applies family-
  agnostically.

### Architectural

- **Pass full messages + tools to minijinja, not the
  flattened `(system, user)` pair**. The current
  `apply_chat_template_minijinja` signature loses per-message
  structure — assistant tool_call history, tool result messages,
  multi-turn user turns all flatten into one giant user blob. Gemma's
  template iterates `messages` looking for tool_calls etc, so the
  rendered prompt is structurally degraded. This is a substantial
  refactor (touches `flatten` in `inference_adapter.rs`, the
  function signature, every call site) — file separately as a
  multi-PR thread, not blended with the canonicalizer work.

- **Output-buffer retry for `apply_chat_template`** mirroring the
  `chat_template` lookup retry. When the binding's built-in path
  is the fallback, it currently dies on `BuffSizeError` for prompts
  above ~2 KiB. Same data-driven retry pattern.

## Suite outcome vs Qwen baseline

| Suite     | Qwen pass | Gemma pass | Notes |
|-----------|-----------|------------|-------|
| Codex gym | 45/45 (100%) | 12/27 (44%) | Family-correctness path now clear; canonicalizer-shape additions needed |

Gemma's quality on the recovery half of the suite is on par with Qwen.
The apply_patch-shape half is where the family-specific work concentrates,
and the work is in the *pipeline canonicalizer*, not in `ModelQuirks`. A
single canonicalizer extension covering the two new shapes should
recover most of the gap.

## Configuration restore

Daemon config swap recorded at `~/.sovereign/config.toml.before_gemma_gym`.
Restore with:

```
cp ~/.sovereign/config.toml.before_gemma_gym ~/.sovereign/config.toml
sovereign daemon restart
```
