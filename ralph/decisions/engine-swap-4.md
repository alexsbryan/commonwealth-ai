<!-- ledger -->

**engine-swap-4 · 2026-10-23 · es-p4-ship-onprem-and-pods · director** — this commit
- Needed: the row's fix (build ggml position-independent, or `-no-pie` on the binaries alone) assumed the ROCm daemon links ggml-hip. It links ggml-vulkan: the linux llama-cpp-4 feature has been `"vulkan"` since 0de1ac115 (2026-05-18), no crate enables `hip`, and the ROCm Containerfile has no feature swap. The build image has no Vulkan SDK, so clearing `-no-pie` only moves the failure to llama-cpp-sys-4.
- Chose: the ROCm image restores hip, in this row: the CUDA image's feature swap (reused) to `"hip"`, the HIP link libs in RUSTFLAGS, `HIPFLAGS=-fPIC` so `-no-pie` goes, and `AMDGPU_TARGETS` into the HIP compile. Acceptance unchanged.
- Because: hip is the image's documented backend (serve/container/README.md ROCm tables, `GGML_HIP=ON`), so this repairs a regression, not a change of engine; embedded stays the default until es-e2. Vulkan SDK in the image would serve MI300X-class parts on CPU, and CPU-only would too (ARCH 6). A split row would carry the same proof, so it moves the work without letting p4 finish.

<!-- appendix -->

## engine-swap-4 · 2026-10-23 — es-p4's ROCm daemon links ggml-vulkan, not ggml-hip; the image restores hip

<details><summary>reasoning, evidence, package</summary>

Reproduced in the lane at 0d1802ab7:

- `serve/crates/sovereign-inference/Cargo.toml:163` is
  `llama-cpp-4 = { version = "0.5.1", features = ["vulkan"] }` under
  `cfg(target_os = "linux")`; `git grep '"hip"' -- '*Cargo.toml'` finds only the
  feature definitions (`vendor/llama-cpp-4/Cargo.toml:66`,
  `vendor/llama-cpp-sys-4/Cargo.toml:112`).
- `serve/container/Containerfile.cuda` rewrites that token to `"cuda"` in-image
  and passes `-lcudart -lcublas -lcublasLt -lcuda` in RUSTFLAGS (`:142`); the ROCm
  Containerfile does neither, and its `-no-pie` comment (`:107`) is about ggml-hip.
- `vendor/llama-cpp-sys-4/build.rs:2220` defines `GGML_HIP` from the feature and
  emits no HIP `rustc-link-lib`; the only PIC switch in ggml is under
  `BUILD_SHARED_LIBS` (`ggml/src/CMakeLists.txt:626-628`).
- `ggml-hip/CMakeLists.txt:35-40` reads `AMDGPU_TARGETS`/`GPU_TARGETS` as CMake
  variables, so the image's `ENV GPU_TARGETS` does not reach it.
- Build-stage image 0084f970915a (2026-10-12): cmake 3.22.1, no
  `/usr/include/vulkan`, zero vulkan packages.

Not chosen: install the LunarG Vulkan SDK (builds, but the package reports no Vulkan
driver for MI300X-class parts, not verified here; if so the engine falls to CPU
on the image's gfx942 default without saying so); embedded CPU-only (the same substitution, and
it moves what a pod serves ahead of es-e2-flip-default); a split row
`es-p4-...-rocm-daemon` (p4's proof is the ROCm image building, so the split
would hold the same proof and p4 could not close without it). A GPU_TARGETS
passthrough in the vendored build.rs works too, but adds a third vendor
divergence; HIPFLAGS needs none.

Package: `~/.local/share/eutropia/lanes/engine-swap/es-p4-ship-onprem-and-pods/needs-director.md`.

</details>
