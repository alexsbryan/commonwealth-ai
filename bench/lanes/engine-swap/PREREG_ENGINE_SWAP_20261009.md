# Pre-registration: the resident daemon on llama-server instead of its engine

Written 2026-10-09, before any run of either arm. The question comes from
`svrn-docs/DAEMON_ENGINE_INVENTORY.md`: the daemon already has a second engine,
`[engine] kind = "remote"`. What does the resident stack lose when that
engine is pointed at upstream llama-server?

## Arms

Both arms run the same daemon binary: debug, built from HEAD at the time of
the run, with the env of the resident daemon as it ran on 2026-10-09. Both use
the same `~/.sovereign` data and corpora. Only `[engine]` differs.

- **L, the engine as shipped.** `kind = "llama"`, with the resident
  `[models]`:
  - primary: Qwen3.6-35B-A3B-MTP-UD-Q6_K;
  - fast: Qwen3.5-4B-UD-MTP-Q6_K_XL;
  - embed: Qwen3-Embedding-0.6B-Q8_0;
  - no reranker configured.
- **R, remote.** `kind = "remote"`. The daemon talks to `engine_proxy.py`,
  which talks to one llama-server in router mode, built from the vendored
  commit 035e227 with `LLAMA_LLGUIDANCE=ON` (`build-llama-server.sh
  --llguidance`).
  - The router serves the same three GGUFs under their file stems.
  - `model_id` is the primary stem, `fast_model_id` the fast stem, and
    `embed_model_id` the embed stem. The quality-check fingerprint is
    therefore identical for both arms. The fingerprint does not include the
    engine kind, so the arm is recorded beside every run.
  - Each model gets `-c 65536 -ngl 99`. The two chat models also get
    `--spec-type draft-mtp --spec-draft-n-max 3`. The embed model gets
    `--embedding --pooling last`. Everything else stays at llama-server's
    defaults.

## Runs

The instrument is `svrn quality check` (venue `check`, eight lanes, no
`--mint`), run in the order L1, L2, R1.
- L1 and L2 are the noise band: no comparable baseline exists on this host
  for the fingerprint.
- R2 runs only if a lane's R1 number falls outside the L band.
- Nothing else runs on the GPU during a run, because `throughput` is one of
  the lanes.

## Probe before R1: embedding parity

The resident corpora were embedded by L's engine, so R's vectors must match
L's.

**What the code says, before measuring.** L's embed slot adds Qwen3-Embedding's
input preparation on the server side: the query instruction on queries, and
`<|endoftext|>` on every input (`embed_slot.rs:145-160`,
`EmbedQuirks::qwen3_embedding`). R's embed half is a `RemoteApiProvider` built
without `with_query_instruction` (`oicp-client/src/outbound.rs:351-378`), and
no oicp-client code appends an EOS. So R as built is expected to send bare
text.

**Inputs.** Twenty bank questions (the retrieval-prod subset plus routing
items) and twenty corpus chunks. For each text there are three vectors:
- **L:** the daemon's `/v1/embeddings` on the text, with the query instruction
  prepended for questions. The daemon adds the EOS itself.
- **R-as-built:** the router's embed model on the bare text.
- **R-prepared:** the router's embed model on the text prepared as L prepares
  it, with the instruction for questions and a trailing `<|endoftext|>`.

**Bar:** the minimum cosine between L and R-as-built is at least 0.995.

- **If R-as-built misses and R-prepared passes:** the gap is the client's
  input preparation, not llama-server. R1 then runs with the remote embed half
  preparing inputs the way L does, as a named code change that is committed
  and cited in the report. A swap would need that change anyway.
- **If R-prepared misses too:** the run stops before R1. Retrieval would be
  measuring a vector mismatch, not the engine.

## Bars

1. **Invariant lanes** (routing, retrieval-prod, enrichment-f1). Each lane's
   primary number for R1 lies within [min(L1,L2), max(L1,L2)]. A value
   outside the band is a regression, named with its cause from the run's logs.
2. **Throughput.** R1's lane number is at least 0.8x the mean of L1 and L2.
3. **Judged lanes** (chat-ask, chaos-monkey, knowledge-gym, synth).
   - R1 must not fail a lane that both L runs passed.
   - Score differences inside the L band are reported, not judged.
4. **No server faults.** llama-server returns no 5xx during R1 and does not
   crash.
5. **Census, reported and not judged.** `engine_proxy.py` records every
   extension field the remote engine sent, per lane, as translated or
   dropped. Each dropped field with a non-zero count is a production use that
   has no llama-server counterpart on this path. That list is this test's
   main output: it is what an engine swap would still have to replace.

## What the result can decide

If bars 1-4 hold, the measured surface does not need the embedded engine. The
remaining work is the census list, plus the uses this instrument does not
reach. The inventory names those: ggml-RPC distribution, NER, directed pins
under load, FIM, and next-edit.

If a bar fails, the failure names a dependency the inventory missed or
underpriced.

One run of R cannot show that the two arms are equal. It can show that
nothing broke, and what was dropped.
