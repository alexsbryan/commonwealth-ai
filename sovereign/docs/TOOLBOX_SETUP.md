# Running sovereign on AMD Strix Halo via a toolbox

This walks through running the whole stack (sovereign daemon, corpus
enrichment, CLI) on an AMD Ryzen AI Max ("Strix Halo") Linux
machine, inside one of [kyuz0/amd-strix-halo-toolboxes][kyuz0]. The
toolbox ships a pre-built llama.cpp against the chosen GPU backend
and leaves your host untouched; sovereign builds + runs inside it
and reads/writes `~/.sovereign/` on the shared host home.

[kyuz0]: https://github.com/kyuz0/amd-strix-halo-toolboxes

**You will end up with**: sovereign-cli binary inside the toolbox,
daemon auto-loading your GGUF models onto the Strix Halo iGPU via
ROCm or Vulkan, and the `sovereign enrich ...` flow running against
corpora in `~/.sovereign/enrichment/`.

This is a first-pass walkthrough. The two follow-up items it
flags (a `cfg(target_os = "linux")` feature swap in
`sovereign-inference/Cargo.toml`, and a validated `sovereign daemon
install` on Linux) stay hand-operated until the walkthrough is
confirmed end-to-end on real hardware; they then move into the
project's install defaults.

---

## 0. Before you start

Hardware + OS assumed:

- AMD Ryzen AI Max (Strix Halo) with the Radeon 8060S iGPU.
- Fedora 40+ **or** Ubuntu 24.04+ on the host.
- Kernel ≥ 6.11 (Strix Halo support landed there; older kernels
  miss the `amdgpu` bits).
- Your user in `render` and `video` groups:

  ```bash
  sudo usermod -a -G render,video $USER
  # log out + back in (or `newgrp render`) to pick the groups up
  ```

Confirm the GPU is visible to the host:

```bash
ls -la /dev/kfd /dev/dri/renderD*
# Should list the kfd device (ROCm) + one or more renderD128 files (DRI)
```

If `/dev/kfd` is missing, `amdgpu` probably didn't initialise —
check `dmesg | grep -i amdgpu` and make sure the kernel is recent
enough before going further.

---

## 1. Choose a toolbox variant

The kyuz0 repo publishes five tags with different backends. For
this project, the calculus is driven by **which GGUFs you want to
host**:

| Variant | Strix Halo behaviour | Pick when |
|---|---|---|
| **ROCm 7.2.1** (recommended default) | Full feature parity with upstream llama.cpp; no buffer caps; biggest image | You want the 27B / 35B-A3B thoughtful-slot models to load. |
| ROCm 6.4.4 | Stable older ROCm | You hit a regression on 7.2.1 specifically. |
| ROCm 7 Nightly | Tip of tree | You're upstreaming fixes. |
| Vulkan — Mesa RADV | Stable, broad compatibility | ROCm won't load on your kernel and you want to keep moving. |
| Vulkan — AMDVLK | Fastest small-model decode in the author's benches | You're only running ≤ 8B models (**2 GiB buffer cap** means 27B / 35B-A3B Q4 loads won't fit). |

This doc uses **ROCm 7.2.1** throughout. If you pick a different
variant, swap the image tag in §2 and the cargo feature flag in
§4 (`rocm` → `vulkan`).

---

## 2. Enter the toolbox

Fedora host:

```bash
toolbox create --image docker.io/kyuz0/amd-strix-halo-toolboxes:rocm-7.2.1 \
    sovereign-rocm
toolbox enter sovereign-rocm
```

Ubuntu host (use `distrobox` — Fedora's `toolbox` command won't
cooperate on non-Fedora hosts):

```bash
distrobox create --image docker.io/kyuz0/amd-strix-halo-toolboxes:rocm-7.2.1 \
    --name sovereign-rocm \
    --additional-flags "--device /dev/dri --device /dev/kfd \
        --group-add video --group-add render --group-add sudo"
distrobox enter sovereign-rocm
```

Toolbox containers **bind-mount your host home** by default — your
`~/.sovereign/`, `~/.cargo/`, `~/dev/commonwealth-ai/` are all
visible at the same paths inside. That's the whole point: build and
run happen in the container, but persistent state (models,
enrichment caches, git worktree) stays on the host.

Sanity check the GPU is reachable from inside:

```bash
# ROCm variants
rocminfo | grep -A1 "Agent 2"     # Should show the Strix Halo device
rocm-smi                          # Utilisation / VRAM

# Vulkan variants
vulkaninfo --summary              # Lists the AMD GPU as a reported device
```

If these fail, leave the toolbox, double-check `/dev/kfd` +
`/dev/dri` on the host, recreate the toolbox.

---

## 3. Install build deps inside the toolbox

The kyuz0 images ship llama.cpp + ROCm, not a Rust toolchain. Inside
the toolbox:

```bash
sudo dnf install -y rust cargo protobuf-compiler cmake gcc gcc-c++ \
    pkg-config openssl-devel
```

(On the Ubuntu-based distrobox: `sudo apt install -y cargo rustc
protobuf-compiler cmake build-essential pkg-config libssl-dev`.)

Prefer `rustup` if you want a newer toolchain than Fedora's
package repo ships:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
```

`~/.cargo/` is on the host bind mount, so installed toolchains
persist across `toolbox enter` sessions **and** are shared with host
cargo — be aware that running `cargo build` on the host and inside
the toolbox against the same `target/` dir will thrash each other.
Either pick one side, or set `CARGO_TARGET_DIR=$HOME/target-toolbox`
inside the toolbox to keep builds separate.

---

## 4. Swap the llama.cpp feature flag

The Metal feature in `sovereign-inference/Cargo.toml` is macOS-only.
Until that file carries a `cfg`-gated feature selection, make the
swap by hand for the Linux build:

```diff
 # sovereign/crates/sovereign-inference/Cargo.toml
-llama-cpp-2 = { version = "0.1.145", features = ["metal"] }
+llama-cpp-2 = { version = "0.1.145", features = ["rocm"] }
```

Use `features = ["vulkan"]` if you picked a Vulkan toolbox variant
in §1. Both features pull `llama-cpp-sys-2`'s vendored llama.cpp
source and compile it against the chosen backend, so the first
`cargo build` takes 3–8 minutes.

Do not check this change in. The eventual fix is in
`sovereign-inference/Cargo.toml`:

```toml
[target.'cfg(target_os = "macos")'.dependencies]
llama-cpp-2 = { version = "0.1.145", features = ["metal"] }

[target.'cfg(target_os = "linux")'.dependencies]
llama-cpp-2 = { version = "0.1.145", features = ["rocm"] }
```

…which lands as follow-up work once this walkthrough is confirmed
on hardware.

---

## 5. Build sovereign-cli

From the project root inside the toolbox:

```bash
cd ~/dev/commonwealth-ai/sovereign  # or wherever your clone lives
cargo build -p sovereign-cli --release
```

The first build compiles the vendored llama.cpp against ROCm — this
is the 3–8 minute step. Subsequent builds reuse the cache.

Verify the binary is linked against the expected GPU library:

```bash
ldd target/release/sovereign-cli | grep -E "rocm|hip|vulkan|amdgpu"
# ROCm variant → you should see libhipblas / libamdhip64
# Vulkan variant → you should see libvulkan
```

Run the built-in tests to make sure nothing regressed in the port:

```bash
cargo test -p sovereign-cli --bin sovereign-cli
cargo test --manifest-path ../corpus-engine/Cargo.toml --lib
```

Both suites should pass with the same counts as CI (the test paths
don't touch the GPU).

---

## 6. Models + first-run config

Download a Qwen3.5-9B GGUF (or whatever you want in the primary
slot) to `~/.sovereign/models/`:

```bash
mkdir -p ~/.sovereign/models
cd ~/.sovereign/models
curl -L -o Qwen3.5-9B.Q5_K_M.gguf \
    'https://huggingface.co/.../Qwen3.5-9B.Q5_K_M.gguf'
```

Alternately, `sovereign setup` detects the hardware tier and fetches
an appropriate model mix for you — run it once and accept the
prompts:

```bash
./target/release/sovereign-cli setup
```

Setup writes `~/.config/sovereign/config.toml` with your chosen
model paths. You can also hand-edit that file:

```toml
[models]
primary = "/home/youruser/.sovereign/models/Qwen3.5-9B.Q5_K_M.gguf"
fast    = "/home/youruser/.sovereign/models/Qwen3-1.7B-Q8_0.gguf"
embed   = "/home/youruser/.sovereign/models/qwen-embedding-0.6b.gguf"

[daemon]
client_port   = 9741
internal_port = 9742
autostart     = true
```

---

## 7. Run the daemon

Foreground first — easiest way to catch errors on a fresh
environment:

```bash
./target/release/sovereign-cli daemon run
```

Watch the log for lines like
`ggml_cuda_init: found 1 CUDA devices` (actually a ROCm device
reported through the HIP→CUDA compatibility path) or the Vulkan
equivalent. If the model loads and serves at `http://localhost:9741/v1/models`,
you're done with the GPU side:

```bash
# from another terminal inside the toolbox
curl -sf http://localhost:9741/v1/models | jq .
```

Once the foreground run looks healthy, install as a systemd user
service. The project already ships a unit template at
[`contrib/systemd/sovereign.service`](../contrib/systemd/sovereign.service)
and `service_install.rs` has a Linux path — but it has not been
exercised on hardware yet. The intended install flow is:

```bash
./target/release/sovereign-cli daemon install  # writes ~/.config/systemd/user/sovereign.service
systemctl --user daemon-reload
systemctl --user enable --now sovereign
journalctl --user -u sovereign -f              # tail the live log
```

If any of those steps break, please file a note at
`~/.sovereign/notes/` describing what failed — the Linux install
path is first-pass and I expect rough edges.

---

## 8. Run an enrichment end-to-end

At this point the toolbox has no idea it's not macOS. The full
atlas flow works as-is:

```bash
# Pick any literary text — Project Gutenberg plaintext works.
sovereign-cli enrich init brothers_karamazov \
    --source ~/books/brothers_karamazov.txt \
    --pipeline literary_atlas

sovereign-cli enrich build brothers_karamazov --full
sovereign-cli enrich report brothers_karamazov
sovereign-cli enrich query brothers_karamazov "Who is Alyosha?"
```

Phase 1 extraction speed is the main thing to watch — on Strix Halo
ROCm with a 9B-Q5 primary, you should see sections completing
faster than on an M2 Max (Strix Halo's memory bandwidth + iGPU
decode rate wins, the lack of Metal unified-memory KQV offload
doesn't matter here). If it's much slower, check `rocm-smi` during a
run — the iGPU should sit > 60 % utilisation. If it stays near
idle, llama.cpp probably fell back to CPU — the likeliest cause is
`HSA_OVERRIDE_GFX_VERSION` not being set correctly inside the
toolbox image. The kyuz0 image sets this; if you're on a
non-kyuz0 image you'd need `export HSA_OVERRIDE_GFX_VERSION=11.5.0`
(Strix Halo's gfx1151).

---

## 9. Gotchas

- **Shared `target/` with host cargo builds.** If your host (macOS
  or another Linux box) uses the same `target/` directory, the
  toolbox build will stomp its artifacts and vice versa. Isolate
  with `CARGO_TARGET_DIR=~/target-strix` inside the toolbox.
- **Shared `~/.cargo/`.** Same issue, same fix
  (`CARGO_HOME=~/.cargo-strix`). Less catastrophic — the registry
  cache cohabits fine; the issue is if you've installed toolchains
  via rustup on the host they'll be used inside the container too.
- **`cargo build` inside `toolbox enter` vs `toolbox run`.** Use
  `toolbox enter` for an interactive session; one-shots via
  `toolbox run cargo build` sometimes drop env vars. If GPU
  isn't found, re-enter instead of running one-shot.
- **`rocm-smi` shows GPU at 0 %.** Either the model is loading
  (pre-decode) or llama.cpp fell back to CPU. Compare with `top`
  — if one thread is pegged at 100 %, it's the CPU fallback; check
  the daemon log for a `ggml_cuda_init` or
  `ggml_vulkan_init` line at startup.
- **`gh` / `claude-code` / `sovereign` all want `~/.claude/` etc.**
  All of these live on the host bind mount, so the toolbox session
  inherits your auth + configs. No separate auth inside the
  container.
- **Protobuf compile errors on `lancedb`.** If `cargo build` fails
  in `lance-table` with a missing-protoc error, install `protobuf-compiler`
  (Ubuntu) or `protobuf-devel` (Fedora) and retry.

---

## What changes upstream once this is confirmed

When a Linux user works through this doc end-to-end successfully,
two changes land in the repo so the next user doesn't need the
workaround:

1. `sovereign-inference/Cargo.toml` gains the `cfg`-gated feature
   selection from §4, so `cargo build` picks the right backend
   automatically.
2. `daemon install` on Linux gets a confirmed-working path +
   whatever smoothing this walkthrough uncovered (env exports,
   device permissions, extra systemd directives).

File a note or edit this doc directly when you hit a rough edge —
the goal is that by the second or third pass through, the
walkthrough is 5 commands instead of 9 sections.
