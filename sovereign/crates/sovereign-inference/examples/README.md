# sovereign-inference examples

Standalone Rust binaries that exercise specific corners of the
inference stack — useful for benchmarking, debugging, and
reproducing upstream issues without standing up the daemon.

## `grammar_smoke` — minimal `LlamaSampler::grammar` reproducer

Loads a GGUF, attaches a 14-byte grammar (`root ::= "yes"`) plus
a dist sampler, runs ONE decode step. Prints `GRAMMAR_SMOKE_OK`
on success or aborts via the upstream
`GGML_ASSERT(!stacks.empty())` panic.

Used to determine whether `LlamaSampler::grammar` works on a given
backend. The Strix Halo Vulkan **daemon** crashes on this assertion
but **the standalone smoke does not**, even when matched
variable-for-variable on model, ctx params, chat template, prefill
size, and sampler chain — narrowing the bug to something
process-wide that the daemon has and the smoke doesn't (most likely
the shared `Arc<LlamaBackend>` with fast + embed + primary slots
all loaded).

### Running it

```bash
cargo run --release -p sovereign-inference --example grammar_smoke -- \
    --model <path-to-any-gguf>
```

Optional flags:

| flag                   | purpose |
|------------------------|---------|
| `--grammar <str>`      | Override the grammar (default: `root ::= "yes"`) |
| `--gpu-layers <n>`     | Layers to offload (default: 999 = all) |
| `--prompt <str>`       | The user prompt (default: `Say yes:`) |

Optional env vars (extra debug variables matching the daemon flow):

| var                    | purpose |
|------------------------|---------|
| `SMOKE_N_CTX=32768`    | Match the daemon's batch context size |
| `SMOKE_CHAT_TEMPLATE=1`| Apply the model's chat template |
| `SMOKE_WARMUP=1`       | Decode a few tokens, KV-clear, then attach grammar |
| `SMOKE_FULL_CHAIN=1`   | Use the full daemon sampler chain (DRY + penalties + top_k + min_p + temp + dist) |

### Test matrix for cross-backend confirmation

To find out whether the daemon-grammar bug is Vulkan-specific or
cross-backend, run the smoke + the daemon's `cluster_name_synth`
bench task on at least these two backends:

#### Mac (Metal)

```bash
cd ~/dev/commonwealth-ai/sovereign
git pull

# Smoke first — confirms the binding works at all on this backend.
cargo run --release -p sovereign-inference --example grammar_smoke -- \
    --model <path-to-any-gguf-you-have-locally>

# Then the daemon-routed bench — the crash repro.
# (Requires sep-al-farabi corpus + daemon running.)
sovereign daemon restart
sleep 3
sovereign bench atlas --tasks cluster_name_synth --output bench-mac-metal.json
```

#### Linux ROCm (sovereign-rocm toolbox)

```bash
toolbox enter sovereign-rocm
cd ~/dev/commonwealth-ai/sovereign
git pull

# The default Cargo.toml is `vulkan` for Linux. The bootstrap
# script swaps it to `rocm` (one-line edit; revertable).
./scripts/bootstrap-linux.sh --backend=rocm

cargo build --release -p sovereign-inference --example grammar_smoke
./target/release/examples/grammar_smoke \
    --model <path-to-any-gguf>

cargo build --release -p sovereign-cli
sovereign daemon restart
sleep 3
sovereign bench atlas --tasks cluster_name_synth --output bench-ruggedfox-rocm.json

# When done switching back:
./scripts/bootstrap-linux.sh --revert-cargo
```

### Reading the result

For each system, post the smoke result + the daemon journal lines
matching `grammar|GGML_ASSERT|stacks`:

```bash
journalctl --user -u sovereign.service --since "5 minutes ago" \
    --no-pager | grep -E "grammar|GGML_ASSERT|stacks"
```

Four possible outcomes:

| smoke | bench/daemon | what we learn |
|---|---|---|
| ✓ | ✓ | The bug is Vulkan-specific. Atlas can ship on Mac/ROCm. File upstream issue against ggml-org/llama.cpp's Vulkan grammar path. |
| ✓ | ✗ crashes | The bug is daemon-architecture-specific (matches our Vulkan finding). Process state in the daemon — most likely shared `Arc<LlamaBackend>` with multiple loaded contexts — is the trigger. |
| ✗ smoke crashes | ✗ crashes | The bug is in `LlamaSampler::grammar` itself on this backend, not daemon-specific. File a clean reproducer (smoke binary alone) upstream. |
| ✗ smoke crashes | ✓ works | Implausible; would mean smoke has its own bug. |

The Strix Halo Vulkan answer was row 2 (smoke ✓, daemon ✗). Mac
and ROCm tell us whether row 1 is also reachable, which would
unblock atlas immediately on those backends.

## Other examples

- `complete` — full chat completion against a daemon.
- `bench_decode` — microbenchmark for chat-slot decode tok/sec.
- `bench_embed` — microbenchmark for embedding throughput.
