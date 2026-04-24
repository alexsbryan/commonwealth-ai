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

The target path is: pick a toolbox image (§1–2), run
`./scripts/bootstrap-linux.sh` (§3), `cargo build --release` (§5),
go. The per-section walkthroughs below explain what the script does
and how to recover when it doesn't.

The one confirmed follow-up still open: `sovereign daemon install`
on Linux has a systemd path but hasn't been exercised on hardware
end-to-end. §7 marks that path with a warning.

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

### Unlocking > 64 GB of iGPU-addressable memory

By default `amdgpu` on Linux caps the iGPU's addressable memory
(the GTT) at roughly half of system RAM. On a 128 GB Strix Halo
that's ~64 GB — enough for Qwen3.5-27B Q4 but tight for 35B-A3B at
Q5 or for running a big model alongside a populated embedding
slot. The ceiling is a kernel parameter, not a toolbox flag, so
this is a host-side change that survives a reboot.

kyuz0's recommended boot-line for a 128 GB system (leaves 4 GB
for the OS, gives the iGPU the remaining 124 GB):

```
iommu=pt amdgpu.gttsize=126976 ttm.pages_limit=32505856
```

- `iommu=pt` — passthrough mode; reduces overhead on unified
  memory accesses.
- `amdgpu.gttsize=126976` — caps GTT at 126976 MiB (= 124 GiB).
- `ttm.pages_limit=32505856` — caps pinned memory at 32 505 856
  4 KiB pages (= ~124 GiB), the matching ceiling for the TTM
  allocator.

Apply on Fedora:

```bash
sudo grubby --update-kernel=ALL --args="iommu=pt amdgpu.gttsize=126976 ttm.pages_limit=32505856"
sudo grub2-mkconfig -o /boot/grub2/grub.cfg
sudo reboot
```

Apply on Ubuntu (edit `/etc/default/grub`, append to
`GRUB_CMDLINE_LINUX_DEFAULT`):

```bash
sudo sed -i 's|^GRUB_CMDLINE_LINUX_DEFAULT="|GRUB_CMDLINE_LINUX_DEFAULT="iommu=pt amdgpu.gttsize=126976 ttm.pages_limit=32505856 |' /etc/default/grub
sudo update-grub
sudo reboot
```

Adjust the two ceilings if your system has more or less than
128 GB — the formula is `gttsize = (total_RAM_MiB - 4096)` and
`pages_limit = gttsize * 1024 / 4`.

After reboot, confirm inside the toolbox:

```bash
cat /sys/class/drm/card*/device/mem_info_gtt_total | numfmt --to=iec-i
# Should show ~124 GiB, not ~63 GiB.
```

BIOS "UMA Frame Buffer Size" / "Variable Graphics Memory" can stay
at the minimum (Framework Desktop ships 512 MB by default and
kyuz0's own benches use exactly that) — the unified-memory ceiling
on Strix Halo is owned by the kernel parameters above, not the
BIOS reserve.

**Toolbox-side cap.** Podman / Docker / distrobox do not impose a
memory limit on containers by default, so once the kernel lets the
iGPU see 124 GB, the toolbox inherits the whole ceiling. If you
ever see the container OOM on a model that should fit, check
`podman inspect <toolbox-name> | grep -i memory` for a `--memory`
flag someone set, not the kernel side.

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

The kyuz0 images ship the llama.cpp **runtime** (libamdhip64,
libhipblas, librocblas) and `rocminfo` for sanity-checking the GPU,
but not a Rust toolchain, not `libclang` for bindgen, and — perhaps
surprisingly — not the **HIP compiler / dev headers** needed to
build llama.cpp from source. There's a one-shot script that installs
everything + wires up the runtime:

```bash
cd ~/dev/commonwealth-ai/sovereign
./scripts/bootstrap-linux.sh
```

It detects Fedora vs Ubuntu/Debian and handles:

- Rust + cargo (if not already installed),
- clang + `libclang.so` (for bindgen),
- cmake, gcc, protobuf-compiler + **devel**, OpenSSL dev,
- `rocm-hip-sdk7.2.1` (the version-matched HIP SDK — hipcc, HIP
  headers, rocBLAS/hipBLAS dev, ROCm LLVM, rocm-device-libs),
- Tauri 2's GTK/WebKit build deps (webkit2gtk4.1, gtk3, libsoup3,
  librsvg2) and runtime shim `libayatana-appindicator-gtk3`,
- `/etc/ld.so.conf.d/sovereign-rocm.conf` so libamdhip64 resolves
  at runtime without `LD_LIBRARY_PATH`,
- `/etc/profile.d/sovereign-rocm.sh` so new shells have
  `ROCM_PATH` / `HIP_PATH` / `PATH` / `CMAKE_PREFIX_PATH` pre-set.

If you want to do it by hand — or see exactly what goes wrong when
a given dep is missing — the rest of this section still holds. The
Fedora install boils down to:

```bash
sudo dnf install -y rust cargo protobuf-compiler protobuf-devel \
    cmake gcc gcc-c++ pkg-config openssl-devel \
    clang clang-devel \
    rocm-hip-sdk7.2.1 \
    webkit2gtk4.1-devel gtk3-devel libsoup3-devel librsvg2-devel \
    libayatana-appindicator-gtk3
```

(Ubuntu: `sudo apt install -y cargo rustc protobuf-compiler
libprotobuf-dev cmake build-essential pkg-config libssl-dev
libclang-dev rocm-hip-sdk libwebkit2gtk-4.1-dev libgtk-3-dev
libsoup-3.0-dev librsvg2-dev libayatana-appindicator3-1` — the
rocm-hip-sdk meta-package name comes from the AMD apt repo, which
the kyuz0 Ubuntu image already configures.)

The GTK/WebKit deps are only needed for `sovereign-desktop` (the
Tauri 2 frontend). If you're CLI-only, build with `cargo build
--release -p sovereign-cli -p sovereign-server` and you can drop
them.

Three of those dependencies are load-bearing in non-obvious ways:

- **`protobuf-devel` / `libprotobuf-dev`.** `lance-encoding`'s build
  script pulls in `google/protobuf/empty.proto` from the well-known
  proto set, which ships with the `-devel` / `-dev` package, not the
  bare compiler. Skip it and you get:

  ```
  Error: protoc failed: google/protobuf/empty.proto: File not found.
  ```

- **`clang-devel` / `libclang-dev`.** `llama-cpp-sys-2`'s `bindgen`
  step needs `libclang.so` to parse llama.cpp's headers. Skip it and
  you get:

  ```
  thread 'main' panicked at bindgen-0.72.1/lib.rs: Unable to find
  libclang: "couldn't find any valid shared libraries matching:
  ['libclang.so', ...]"
  ```

- **`rocm-hip-sdk7.2.1`.** The kyuz0 ROCm 7.2.1 image only ships the
  runtime side; building llama.cpp's HIP backend from source needs
  `hipcc`, the HIP headers, rocBLAS/hipBLAS dev headers, ROCm LLVM,
  and `rocm-device-libs`. The version-pinned `rocm-hip-sdk7.2.1` is
  the meta-package that pulls all of those at exactly the version
  the runtime ships (`rocm-hip-sdk` without the suffix may pull a
  Fedora-packaged 6.x ROCm and conflict). Skip it and you get:

  ```
  CMake Error at /usr/share/cmake/Modules/CMakeDetermineHIPCompiler.cmake:174 (message):
    Failed to find ROCm root directory.
  ```

- **GTK / WebKit dev packages.** `sovereign-desktop` is a Tauri 2
  app; its Linux deps are `webkit2gtk4.1` (note the .1 — Tauri 2
  uses libsoup3, the older 4.0 won't work), `gtk3`, `libsoup3`, and
  `librsvg2`. The `pango-sys` / `atk-sys` / `gdk-pixbuf-sys` system
  libs are pulled in transitively, so installing the four above is
  enough to build. Skip them and you get a cascade of `pkg-config
  exited with status code 1` errors mid-build.

  At **runtime**, the `tray-icon` feature dlopens libappindicator —
  so install `libayatana-appindicator-gtk3` as well (runtime-only,
  no rebuild needed):

  ```bash
  sudo dnf install -y libayatana-appindicator-gtk3
  ```

  Without it, `cargo tauri dev` compiles fine and then panics on
  startup with `Failed to load ayatana-appindicator3 or
  appindicator3 dynamic library`.

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

## 4. Pick a non-ROCm backend (optional)

The Linux default in `sovereign-inference/Cargo.toml` is ROCm, gated
on `cfg(target_os = "linux")`. If you picked a Vulkan toolbox variant
in §1, edit the linux block to `features = ["vulkan"]`:

```diff
 [target.'cfg(target_os = "linux")'.dependencies]
-llama-cpp-2 = { version = "0.1.145", features = ["rocm"] }
+llama-cpp-2 = { version = "0.1.145", features = ["vulkan"] }
```

Don't check that swap in — the repo's default should stay ROCm
since that's what this doc targets.

---

## 5. Build the workspace

```bash
cd ~/dev/commonwealth-ai/sovereign
cargo build --release
```

First build compiles the vendored llama.cpp against the GPU backend —
this is the 3–8 minute step. Subsequent builds reuse the cache.

Everything else (the cfg-gated llama-cpp-2 feature, the HIP `-fPIC`
cmake flag, the runtime library path, the shell env) is handled by
`scripts/bootstrap-linux.sh` + the workspace `.cargo/config.toml`.
The walkthroughs below exist so you know what to look at if something
doesn't work, not as required manual steps.

### If you skipped bootstrap-linux.sh

You'll need these exports in scope before `cargo build`:

```bash
export ROCM_PATH=/opt/rocm
export PATH="$ROCM_PATH/bin:$PATH"
export HIP_PATH=$ROCM_PATH
export CMAKE_PREFIX_PATH=$ROCM_PATH
```

`CMAKE_HIP_FLAGS=-fPIC` and `HIPFLAGS=-fPIC` come from
`.cargo/config.toml` at the workspace root, so you get them for
free. Without them you'd hit this at the end of a 5+ minute build:

```
/usr/bin/ld: ggml-cuda.cu.o: relocation R_X86_64_32 against
'.rodata.str1.1' can not be used when making a PIE object;
recompile with -fPIE
```

CMake's HIP language doesn't inherit the `-fPIC` that C / C++ flags
set; pushing the flag via env vars is how we get it through.

If you previously failed mid-build and are retrying, wipe the
cached llama-cpp-sys cmake dir so cmake re-configures from scratch
with the new env:

```bash
rm -rf target/release/build/llama-cpp-sys-2-*
```

Verify the binary is linked against the expected GPU library:

```bash
LD_LIBRARY_PATH=$ROCM_PATH/lib ldd target/release/sovereign-cli \
    | grep -E "rocm|hip|vulkan|amdgpu"
# ROCm variant → you should see libhipblas / libamdhip64 / librocblas
# Vulkan variant → you should see libvulkan
```

Bare `ldd` reports `libamdhip64.so.7 => not found` because Fedora's
default linker doesn't include `/opt/rocm-7.2.1/lib` in its search
path. Either prefix every invocation with
`LD_LIBRARY_PATH=$ROCM_PATH/lib`, or make it permanent:

```bash
# permanent, system-wide (toolbox-only — won't leak to host)
echo "$ROCM_PATH/lib" | sudo tee /etc/ld.so.conf.d/rocm-7.2.1.conf
sudo ldconfig
```

Either works; `ldconfig` is the cleaner option once you've verified
the build does what you expect.

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
- **`Unable to find libclang` from bindgen.** `clang` alone is not
  enough — install `clang-devel` (Fedora) or `libclang-dev` (Ubuntu)
  for the shared library bindgen actually dlopens.
- **Build artefact mixing across hosts.** §9's first warning is about
  building on macOS *and* a Linux toolbox against the same `target/`.
  When the host *is* Fedora (same arch as the toolbox), the
  artefacts are interchangeable and you can ignore the warning;
  the issue is mac dylibs vs. Linux .so files cohabiting, not Linux
  vs. Linux.
- **PIE link error after a long HIP compile.** Symptom is the
  `R_X86_64_32 against '.rodata.str1.1' can not be used when making
  a PIE object` line at the very end of a 5+ minute build. Fix with
  the `CMAKE_HIP_FLAGS=-fPIC HIPFLAGS=-fPIC` exports from §5; if
  the error persists, the cmake configure was cached without those
  flags — `rm -rf target/release/build/llama-cpp-sys-2-*` and
  rebuild.
- **`Failed to find ROCm root directory`.** CMake can't find hipcc.
  Either you're missing `rocm-hip-sdk7.2.1` (kyuz0 image only ships
  the runtime), or `ROCM_PATH` / `PATH` weren't exported into the
  shell that ran `cargo build`.

---

## Still open

- **`sovereign daemon install` on Linux.** The systemd path exists
  in `service_install.rs` and the unit template is at
  `contrib/systemd/sovereign.service`, but it hasn't been exercised
  end-to-end on hardware. File a note when you run it.
- **A project-owned toolbox image** (kyuz0 ROCm 7.2.1 base + our
  dep layer pre-baked) would cut §3 out entirely. Worth doing once
  there are multiple Linux contributors; skip it while the kyuz0
  image + bootstrap script are enough for one or two.

File a note or edit this doc directly when you hit a rough edge —
the goal is that by the second or third pass through, the
walkthrough is 3 commands instead of 9 sections.
