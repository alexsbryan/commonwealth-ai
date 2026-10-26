<!-- ledger -->

**engine-swap-9 · 2026-10-26 · REVIEW-DEMO-es-native-host-build · director** — this commit
- Needed: P G's done-when, which the charter's objective repeats, is "a native `cargo build --workspace` succeeds on the Fedora host, outside the toolbox". At 5e9f81db4 it fails, though no longer on the engine. The worker offered two arms: the operator `dnf install`s the missing host packages and the build is rerun, or the bar is restated as "no engine C toolchain needed".
- Chose: the install arm, left to the operator. The restatement is refused because it weakens the row's acceptance and the charter's objective, which the director may not do. The install is a host mutation that needs root, and this session has no non-interactive sudo (`sudo -n true` → "a password is required"). The row stays `[ ]` with its text and `needs [halo]` unchanged. A worker reruns it once the packages are on the host.
- Because: the bar is meetable as written with one `dnf install` and needs no code change. What remains is ordinary distro build dependencies for lance (protoc), the desktop and mobile shells (GTK/WebKit/dbus) and the repo's own linker choice (clang+mold), not the llama.cpp toolchain the bar was minted against. The C++ compiler is the exception, see finding 1.

<!-- appendix -->

## engine-swap-9 · 2026-10-26 — the native host build waits on host packages, not on the engine

<details><summary>reasoning, evidence, options, findings</summary>

Reproduced at 5e9f81db4, from inside the `sovereign-vulkan` toolbox via
`flatpak-spawn --host`:

- `grep -c 'name = "llama' Cargo.lock` → 0. `grep -c stdbool` over the
  worker's three logs (`target/host-native-logs/{verbatim,cc-linker,keep-going}.log`,
  lane tree) → 0, 0, 0. The engine's blocker is gone.
- On the host: `clang`, `mold`, `protoc`, `c++` and `g++` are missing; `gcc`,
  `cc` and `pkg-config` are present. `rpm -q` reports not installed for
  `clang mold protobuf-compiler gcc-c++ dbus-devel gtk3-devel
  webkit2gtk4.1-devel libsoup3-devel`.
- The `--keep-going` log has 17 failed build scripts: lance-{encoding,file,
  table,datafusion,index} (protoc); glib-, gobject-, gio-, gdk-, gdk-pixbuf-,
  atk-, pango-, soup3-, javascriptcore-rs-sys, cairo-sys-rs and libdbus-sys
  (pkg-config); esaxx-rs (c++).
- `.cargo/config.toml` links x86_64 Linux with clang as the driver and mold as
  the linker, which is why the verbatim build stops in 1 s.
- `sudo -n true` on the host → "a password is required".

Options, with costs:

1. **Install the packages on the host and rerun (recommended).** One command
   for the operator:
   `sudo dnf install clang mold protobuf-compiler gcc-c++ dbus-devel gtk3-devel webkit2gtk4.1-devel libsoup3-devel`
   (`dnf` pulls the glib2/cairo/pango/atk/gdk-pixbuf2 `-devel` packages in as
   dependencies of gtk3-devel). After that, a worker runs the row verbatim
   (`flatpak-spawn --host cargo build --workspace`, no linker override) and
   pastes the tail and the wall time. Expect a cold build of several minutes
   into a host-only target dir. Cost: a few hundred MB of host packages. The
   bar stays as written.
2. **Restate the bar as "no engine C toolchain needed".** Refused: it weakens
   the row and the charter's objective. Only the operator can choose it.
3. **Narrow the build to exclude desktop and mobile.** Refused for the same
   reason: `--workspace` is the bar.

Findings filed and not acted on, because scope is frozen:

1. The objective says the C++ toolchain is deleted, but one C++ build remains:
   `esaxx-rs` (C++), through `tokenizers` 0.21's default `esaxx_fast` feature,
   which `gline-rs` 1.0.1 enables by depending on `tokenizers` with defaults
   (`cargo tree -i esaxx-rs -e features`: sovereign-gliner → sovereign-compute
   → sovereign-serve → sovereign-stock/-onprem). Our own
   `serve/crates/sovereign-gliner/Cargo.toml:47` (`tokenizers = "0.21"`) also
   takes defaults, so turning the feature off on our side alone does not
   remove it; it needs gline-rs patched or replaced. The operator decides
   whether that is a row.
2. `.cargo/config.toml`'s Linux block still explains its
   `-L native=/usr/local/lib64` override with stale `libllama.so`/`libggml*.so`
   from llama-cpp-sys-4, which G3 deleted. The override and its comment are
   probably dead, which a G3 docs pass could confirm.
3. `AGENTS.md:289` said host builds die on `stdbool.h` in llama-cpp-sys-4. That
   line is corrected in the commit that follows this decision.

</details>
