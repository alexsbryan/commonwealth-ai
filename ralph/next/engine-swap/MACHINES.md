# engine-swap — which machine each open row needs

Written 2026-10-23, when the queue moved to eutropia on two machines, for the
`needs [..]` field eutropia is gaining on rows (hosts gain `provides`). The
field landed (eutropia 945d3a0) and STATE.md carries it since 2026-10-23, in
the words queue.toml declares: a Halo-only reason below (the pinned GGUF,
gpu-exclusive timing, the host outside the toolbox) is `halo`; `none` rows
carry no field.

The two machines:

| host | provides |
|---|---|
| the Mac | `macos`, `metal` (64 GB; its daemon in remote mode, no model resident) |
| the Halo | `linux`, `rocm`, `vulkan`, `halo-daemon` (128 GB; runs inside the `sovereign-vulkan` toolbox; hosts the deployed daemon on :9741) |

`gpu-exclusive` is not a host property but a hold: a row whose bar is a timing
must have the host's GPU to itself (heavy.txt). `none` means either machine.

Two facts every live row on the Mac rests on, neither proved yet:

- `build.sh`'s macOS arm64 Metal mode (es-p1) was built and proved on Linux
  Vulkan only; the other backends were left to HUMAN-es-p5-release-legs. The
  first live row run on the Mac proves the Metal build, or fails there.
- The Mac's `sovereign/models` lacks `Qwen3.5-0.8B-UD-Q6_K_XL.gguf`, which
  ROUTER-E2E pins today, so ROUTER-E2E cannot pass on the Mac until
  es-portable lands (models by role) or the file is copied there. Until then
  a row whose check names ROUTER-E2E runs on the Halo, so those rows say
  `none` and carry `linux` in brackets: drop it when es-portable lands.

| row | needs | why |
|---|---|---|
| es-portable | none (linux) | Rust and the e2e; its first proof runs the two files ROUTER-E2E pins, which only the Halo has today |
| es-r2-router-parity | gpu-exclusive (linux) | re-measures `cost.concurrent-fast` (C16), a timing; ROUTER-E2E |
| es-r3-fim | none (linux) | fim_eval on both engines with a small FIM model, a match rate, not a timing; ROUTER-E2E |
| es-r4-rerank | none (linux) | the rerank spike R1-R3 are ranking bars, R4 is tracked only; ROUTER-E2E |
| es-r5-router-owns-model-processes | none (linux) | SIGKILL a model process under E2E's own daemon; ROUTER-E2E |
| REVIEW-audit-es-auto-2 | none | git log, `sovereign-cli code` reports and LINT |
| es-t4-responses-native | none | counts calls, retries and misses on a scratch daemon, not a timing |
| es-t5-delete-hermes | none | a deletion with crate tests and ENV |
| es-p4-ship-onprem-and-pods | linux, rocm | builds the ROCm pod image with podman on the host (`flatpak-spawn --host`) and boots its daemon |
| es-p5-ci-and-docs | none | workflows and docs; DOCS and LINT |
| HUMAN-es-p5-release-legs | (the operator's) | the release runners (macos-14, self-hosted macos-x86_64, ubuntu) after a push |
| es-f1-rpc-peer-child | linux | the supervised rpc-server child and a two-load cache test on one host, bound to that host's LAN address; its last step (no orphans after a SIGKILLed daemon) rests on PR_SET_PDEATHSIG, Linux-only (`supervisor/child.rs:170`), and the e2e reads `/proc` (engine-swap-3) |
| es-f2-router-rpc-placement | gpu-exclusive | D2 is a decode tok/s bar on a loopback split of Qwen3.5-9B |
| REVIEW-audit-es-auto-3 | none | as audit 2 |
| HUMAN-es-f3-mac-peer | (the operator's) | starts the Mac's pinned `ggml-rpc-server -c` on the LAN/tunnel address and gives its host:port |
| es-f3-d4-split-on-uneven-memory | halo (with the Mac's peer from HUMAN-es-f3-mac-peer) | D4 between the Mac and the Halo over a direct link: one host's head, the other's peer, both GPUs held. The second host is the operator's row before it |
| HUMAN-es-host-flip | halo-daemon (the seat's) | flips the deployed daemon to `kind = "llama-server"` and runs gate bars 1 and 2 on it |
| HUMAN-es-measure-sampler-pins | halo-daemon (the seat's) | quality checks and the E1 perf check against the deployed daemon |
| es-c1-sampler-outcome | none | a code change on P's verdict; TESTALL |
| es-c2-battery-rejudge | halo | builds the I3/I4 instruments and proves them on a battery against its own daemon and router with small GGUFs (director, 2026-10-25: split from the deployed-router run) |
| HUMAN-es-c2-battery-rejudge-run | halo-daemon (the seat's) | `battery.sh` stops the resident and swaps its config per target, so the 51 cells run under the seat's host grant |
| es-c2-battery-rejudge-fix | halo | fixes or retires each failed row, proved on its own cells against its own daemon and router |
| es-e2-flip-default | none | the release default in code; TESTALL, DOCS, PREPUSH |
| HUMAN-es-soak-signoff | (the operator's) | one release on the new default |
| es-g1-move-factory-and-kinds | none | a move; LAYER and crate tests |
| es-g2-delete-engine | none | a deletion; TESTALL |
| es-g4-delete-mesh-fabric | none | a deletion; crate tests, ENV, BOUNDARY |
| es-m3-move-agnostic-after-g2 | none | a move; LAYER, BOUNDARY and crate tests |
| REVIEW-audit-es-auto-4 | none | as audit 2 |
| es-g3-drop-vendor-and-toolchain | none | a deletion in Cargo and CI; TESTALL, DOCS, PREPUSH |
| REVIEW-DEMO-es-native-host-build | linux | `cargo build --workspace` on the Fedora host OUTSIDE the toolbox, where it dies on `stdbool.h` today: the Halo's host, not its toolbox, a distinction `provides` will need |

Counts over the 28 open rows: none 18 (es-portable, es-r3, es-r4 and es-r5
among them, on the Halo only until es-portable lands); gpu-exclusive 3 (es-r2,
also Halo-only until then; es-f2; es-f3); linux 3 (es-p4 with rocm, es-f3, the
native host build); macos and metal 1 (es-f3); halo-daemon 3 (two HUMAN- rows
and es-c2); the operator's own 2 (HUMAN-es-p5-release-legs,
HUMAN-es-soak-signoff). On the Mac today, without es-portable: 14 of the
none rows, es-f2 and its half of es-f3.
