# sovereign-mesh worker container — Containerfile internals

This directory holds the build artifacts for the cloud-peer images.
The user-facing operational guide (provisioning R2, generating
Tailscale auth keys, deploying on RunPod, running ingests against the
cloud peer, teardown) lives in
[`svrn/docs/CLOUD_PEER_DEPLOY.md`](../../svrn/docs/CLOUD_PEER_DEPLOY.md). This
README covers what's in *this* directory and why each piece is shaped
the way it is — read it when the build itself misbehaves or you need
to extend the image.

## Files

| file | role |
|---|---|
| `Containerfile`       | ROCm flavor (AMD MI300X / MI250X / MI100). Repo default. |
| `Containerfile.cuda`  | CUDA flavor (NVIDIA H100 / A100 / L40S / RTX 4090). |
| `swap-inference-backend.py` | Rewrites the linux `llama-cpp-4` backend feature in `sovereign-inference/Cargo.toml` (`"cuda"` or `"hip"`). Both flavors run it; it refuses loudly if the line is absent or already swapped. |
| `entrypoint.sh`       | Runtime: tailscale up → rclone sync GGUFs → write `config.toml` → exec daemon. Same script for both flavors. |

## Build

From the workspace root (`commonwealth-ai/`), because the root `Cargo.toml`
is the one workspace manifest and cargo will not load the workspace with any
member missing. Both Containerfiles copy every program's `crates/` dir plus
the members outside one; `scripts/tests/release-build-context.sh` checks that
COPY set against the manifest's members and `[patch]` paths, without podman.

```bash
cd ~/dev/commonwealth-ai
podman build -t ghcr.io/<you>/sovereign-rocm:latest \
             -f serve/container/Containerfile .

podman build -t ghcr.io/<you>/sovereign-cuda:latest \
             -f serve/container/Containerfile.cuda .
```

`docker build -f Containerfile[.cuda]` is interchangeable. Both
produce OCI images RunPod, ECS, etc. accept.

First build per flavor is ~30-45 min — most of that is compiling
llama.cpp + sovereign-cli inside the image. Subsequent builds with
only sovereign-cli source changes are 1-3 min thanks to layer
caching.

## Build-time system dependencies

Both flavors share a Cargo-workspace base. Only the math-libs differ.

Common to both:

| package | reason |
|---|---|
| `build-essential`                       | C/C++ toolchain |
| `cmake`                                 | llama.cpp build |
| `git`                                   | submodule fetches in some build scripts |
| `pkg-config`, `libssl-dev`              | `openssl-sys` |
| `protobuf-compiler`                     | `protoc` for `prost-build` |
| `libprotobuf-dev`                       | well-known protos used by `lance-encoding` / `lance-table` |
| `clang`, `libclang-dev`, `llvm-dev`     | `bindgen` (llama-cpp-rs etc.) |
| `python3`                               | build helpers in llama.cpp + `swap-inference-backend.py`, the Cargo.toml feature swap both flavors run |
| `rustfmt` (via rustup component)        | llama-cpp-sys-2's bindgen pipes through `rustfmt`; without it the build errors with `'rustfmt' is not installed for the toolchain` |

ROCm-only:

| package | reason |
|---|---|
| `hipblas-dev`, `rocblas-dev` | math libs llama.cpp's `GGML_HIP=ON` links against — `rocm/dev-ubuntu-22.04` ships HIP runtime but not these |

The ROCm runtime stage installs `hipblas` + `rocblas` (no `-dev`) for
dlopen at slot-load time. ROCm core (`libamdhip64`,
`libhsa-runtime64`) comes from the base image.

CUDA-only:

| package | reason |
|---|---|
| `libnccl-dev` (build), `libnccl2` (runtime) | recent llama.cpp's `GGML_CUDA` calls NCCL all-reduce primitives unconditionally — symbols must resolve at link time, library must be dlopenable at slot init |

cuBLAS + the rest of the CUDA toolkit ship in-base from the
`nvidia/cuda:*-devel`/`-runtime` images.

## Build-time env vars

ROCm:

| var | purpose |
|---|---|
| `ROCM_PATH=/opt/rocm`, `HIP_PATH=/opt/rocm` | base location |
| `<pkg>_ROOT=/opt/rocm` (hip / hipblas / rocblas / hipblaslt) | replaces `CMAKE_PREFIX_PATH` because `cmake-rs` (the build-script crate llama-cpp-sys-2 uses) hard-overrides `CMAKE_PREFIX_PATH=""` regardless of what we set in env. CMake 3.12+ honors `<Pkg>_ROOT` for `find_package()` and `cmake-rs` leaves it alone |
| `AMDGPU_TARGETS=gfx942` (default), `GPU_TARGETS=...` | GPU ISA at compile time; override with `--build-arg AMDGPU_TARGETS=gfx90a` for MI250X etc. |
| `HIPFLAGS="-fPIC --offload-arch=${AMDGPU_TARGETS}"` | `-fPIC` makes ggml-hip's static archive PIC, so the final link needs no `-no-pie`; `--offload-arch` carries the ISA into the HIP compile, where no `-D` reaches (ggml-hip reads `GPU_TARGETS` as a CMake variable). `grep offload-arch` on the build log shows which ISA was baked. |
| `RUSTFLAGS="-L/opt/rocm/lib -lamdhip64 -lhipblas -lrocblas -fuse-ld=lld"` | the HIP math libs cmake adds to ggml-hip's target but the vendored build.rs never emits into the Rust link line (CUDA passes `-lcudart -lcublas` the same way); `-fuse-ld=lld` for a faster final link |

CUDA:

| var | purpose |
|---|---|
| `CUDA_PATH=/usr/local/cuda`                                | toolkit location (set by base image too) |
| `CMAKE_CUDA_ARCHITECTURES="80;86;89;90"` (default)         | SM gencodes baked into the image; multi-arch covers Ampere → Hopper |
| `RUSTFLAGS="-C link-arg=-lnccl"`                           | injects `-lnccl` into rustc's final link because llama-cpp-sys-2's build.rs doesn't emit `cargo:rustc-link-lib=nccl` despite the static archive having unresolved NCCL refs |

Both flavors rewrite the linux `llama-cpp-4` backend feature in
`crates/sovereign-inference/Cargo.toml` through `swap-inference-backend.py`:
CUDA flips it to `"cuda"`, ROCm to `"hip"`. The repo default is `"vulkan"`,
which neither image can build (the CUDA image carries no Vulkan SDK, and the
ROCm image's daemon then links ggml-vulkan instead of ggml-hip). The script
refuses when the line is absent or already swapped, so a layout change fails
the build rather than shipping an image whose daemon links the wrong backend.

## Runtime stage

Both flavors install (in addition to the math-lib runtime
counterparts above):

| package | reason |
|---|---|
| `iproute2`, `iptables` | tailscaled's userspace networking mode requires them |
| `unzip`                | `rclone`'s install script needs it to extract the binary |
| `tailscale`, `rclone`  | installed via vendor curl-pipe scripts |

The runtime stage is built `FROM` the same base image's
`*-runtime-ubuntu22.04` variant where one exists (CUDA) or the same
`*-dev-ubuntu22.04` (ROCm — they don't ship a separate runtime
variant). The CUDA runtime image is ~3 GB smaller than the devel
image for the same GPU surface.

The entrypoint expects the daemon binary at
`/usr/local/bin/sovereign-cli` — copied from `--from=build`'s
`target/release/sovereign-cli`, with `sovereign-cli-daemon`,
`sovereign-stock` (the loader `daemon run` execs) and
`sovereign-pod-worker` beside it on the same chain.

The image also carries the **inference server**: the build stage runs
`serve/crates/sovereign-compute/llama-server/build.sh` in the flavor's own
backend (`--backend rocm`, reading the ROCm table's `GPU_TARGETS` and
`HIPFLAGS` so the binary is PIE-friendly and the ISA is the same one
`AMDGPU_TARGETS` names; or `--backend cuda`, reading
`CMAKE_CUDA_ARCHITECTURES`), and the runtime stage copies `llama-server`,
`ggml-rpc-server` and `llama-server.manifest.toml` into `/usr/local/bin`
beside the daemon, where `kind = "llama-server"` finds them by looking next to
its own executable and verifies them against the pin compiled into it. A pod
whose daemon runs `kind = "llama"` simply never execs them. Unlike the
daemon's cargo build, `build.sh` unsets `RUSTFLAGS` in that step: its own
llguidance pass would otherwise apply the daemon's link flags (and a
`-no-pie`) to every artifact it links, proc-macros included.

## Customising

To target different GPU classes:

```bash
# ROCm — MI250X
podman build --build-arg AMDGPU_TARGETS=gfx90a \
             -t ghcr.io/<you>/sovereign-rocm-mi250x:latest \
             -f serve/container/Containerfile .

# CUDA — H100 only (slim image, faster cold-start)
podman build --build-arg CUDA_ARCHITECTURES=90 \
             -t ghcr.io/<you>/sovereign-cuda-h100:latest \
             -f serve/container/Containerfile.cuda .
```

To bump the model set, edit `entrypoint.sh`'s `*_GGUF` defaults or
override them at pod-start time via env vars.

To deploy on a non-RunPod target (Lambda Cloud, Vast.ai, AWS
EC2 with NVIDIA driver passthrough, …), nothing in this directory is
RunPod-specific. The pod runtime needs to expose `/dev/kfd` +
`/dev/dri` (AMD) or run with the NVIDIA Container Toolkit (NVIDIA);
everything else — networking, secrets, ports — is provider-agnostic.

## Going further

For the actual deployment workflow (R2 setup, Tailscale auth keys,
RunPod pod template, smoke testing, batch ingests, teardown,
troubleshooting), see
[`svrn/docs/CLOUD_PEER_DEPLOY.md`](../../svrn/docs/CLOUD_PEER_DEPLOY.md).
