# Pre-registration: engine conformance, embedded engine against llama-server

Written 2026-10-10 at `3be34324b`, before the battery exists. The rows,
their checks and their predicted verdicts are in `conformance.toml` beside
this file. This file fixes the method, and it is not edited after the first
run. A finding that changes a row is a new row, with its date.

## The claim under test

The daemon's model work all crosses one boundary: `InferenceProvider`
(`sovereign-contracts/src/traits.rs:309`, 29 methods) and the 23 fields of
`CompletionRequest` (`oicp-types/src/completion.rs`). If the remote engine
matches the embedded one at every layer below, for every request shape a
production caller emits, the two engines behave the same by construction. A
quality check or e2e-swe run after that confirms the result; it does not
carry the proof.

## Layers, each with an exact check

- **prompt.** The token ids the model is given. Embedded: the engine's
  rendered prompt, tokenized in-process. Remote: llama-server's
  `/apply-template` then `/tokenize` on the body the client sends. Equal or
  not.
- **decode.** What constrains the next token: the sampler parameters and
  their order, stops, the token caps, thinking, and grammars or masks.
  Parameters are compared as tuples (the embedded engine's debug trace
  against llama-server's `/slots` view of the same request). Grammars and
  masks are compared as languages, by accept/reject parity over a fixed
  corpus, as `decode_allowlist_parity_tests.rs` already does for the
  allow-lists.
- **compute.** The forward pass. Both engines build llama.cpp 035e227
  (`vendor/llama-cpp-sys-4/LLAMA_CPP_COMMIT`,
  `target/llama-server-vanilla/source.txt`). The engine's patches 0003-0008
  add decode hooks, speculative state and mmap behaviour and change no math.
  The check runs under the matched configuration below: top-20 logprobs at
  each of the first 64 greedy positions, and the greedy tokens themselves.
- **projection.** How the output is read back: text and reasoning, tool
  calls, finish reason, usage, stream frames. The check feeds identical raw
  server output to both readers, or identical generated tokens, and compares
  the result each caller receives.
- **cost.** Counted, not timed, where a counter exists. Prompt tokens
  evaluated per call (`timings.prompt_n` on the server, the engine's own
  prefill count), and resident bytes. One row (`k.concurrent-fast`) has no
  counter and is timed; it is the only timed row.
- **host.** What the provider reports about itself: ids, context size, slot
  residency, lifecycle calls. The check is the truth of the report against
  the server's own state (`/props`, `/models`), not equality with the
  embedded engine.

## Configurations

**Deployed.** The configuration a swap would ship, and the one every
prediction in `conformance.toml` is made against, unless the row says
otherwise.
- Embedded: the resident `[models]` of 2026-10-09. The primary is
  Qwen3.6-35B-A3B-MTP-UD-Q6_K, the fast model Qwen3.5-4B-UD-MTP-Q6_K_XL and
  the embedder Qwen3-Embedding-0.6B-Q8_0.
- Server: llama-server at 035e227, built with `LLAMA_LLGUIDANCE=ON`, in
  router mode. The flags are those of `PREREG_ENGINE_SWAP_20261009.md` (§R).
  Everything else is left at the server's defaults, including `--jinja` (on)
  and `--reasoning-format` (`deepseek`).
- Client: `[engine] kind = "remote"`, `grammar = "llguidance"`,
  `embed_inputs = "client"`.

**Matched.** Used only for the compute rows: both sides cold, no
speculative decoding, one sequence, `n_ubatch` 512, F16 KV, the same
flash-attention setting, the same `n_ctx`. The server runs with
`cache_prompt: false`.

## Verdicts

Each row gets one verdict: passed, failed, could-not-judge or never-ran.

A row that refuses loudly on the remote side, where the embedded engine
serves, is failed with the cause `refused`. A remote that answers while
dropping the request's meaning is failed with the cause `silent`. The two
are reported apart, because a refusal is honest (ARCH 6) and a silent loss
is not.

A row is in scope only if a production caller reaches it. Field values that
no production code sets are listed under `[[unreachable]]` with the reason,
so that coverage of the request struct stays complete without testing dead
shapes.

## Predictions

Every row carries `predict` (pass or fail, with the cause), and `basis`:
- `read`: from the code at `3be34324b`;
- `measured`: from an earlier run, cited;
- `unverified`: a reading that rests on something not confirmed.

The battery is validated against these. A row predicted to fail that
passes, or the reverse, is investigated before any verdict is reported (ARCH
5, 7). The prediction is never edited to match.

## Swap-ready

The bar is:
- every in-scope row passes;
- or a row fails as `refused`, with a named owner and the operator's
  acceptance recorded in the inventory.

No in-scope row may fail as `silent`.
