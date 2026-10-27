<!-- ledger -->

**engine-swap-10 · 2026-10-26 · REVIEW-DEMO-es-native-host-build · operator** — this commit
- Needed: engine-swap-9 left the row parked on a host `dnf install` of clang, mold, protoc, gcc-c++ and the GTK/WebKit/dbus `-devel` packages, so that `cargo build --workspace` could run on the Fedora host outside the toolbox.
- Chose: the operator withdrew that arm ("We only run in the sovereign-vulkan toolbox, none of that gets installed here") and restated P G's done-when (queue commit ee87bd595): Cargo.lock names no llama*, ggml*, bindgen or clang-sys package, `cargo tree -i cmake` and `-i cc` name no consumer that builds llama.cpp or ggml, and `cargo build --workspace` in the toolbox exits 0. This supersedes engine-swap-9's chosen arm; its host evidence and the corrected AGENTS.md line stand.
- Because: the host never gets build packages, so a host build measures the host's package set rather than whether the engine's toolchain left the build. Measured at 001bfd63f, the restated bar holds (appendix).

<!-- appendix -->

## engine-swap-10 · 2026-10-26 — the native-host-build demo is judged in the toolbox

<details><summary>reasoning, evidence, package</summary>

Measured at 001bfd63f (the lane after merging the queue head ee87bd595),
inside the `sovereign-vulkan` toolbox (`/run/.containerenv` name="sovereign-vulkan"):

- `grep -c '^name = "<p>' Cargo.lock` for llama, ggml, bindgen, clang-sys:
  0, 0, 0, 0. whisper, sherpa and candle also count 0.
- `cargo tree --workspace -i cmake -e normal,build --depth 1`: one consumer,
  `aws-lc-sys` v0.41.0 (rustls → aws-smithy → aws-config → lance-io).
- `cargo tree --workspace -i cc -e normal,build --depth 1`: cmake,
  embed-resource, aws-lc-sys, blake3, bzip2-sys, esaxx-rs, lance-linalg,
  libsqlite3-sys, lz4-sys, onig_sys, openssl-sys, ring, tree-sitter and four
  grammars, workspace-hack, zstd-sys. None builds llama.cpp or ggml.
- `cargo build --workspace` (via `scripts/with-cargo-lock.sh`): rc=0, wall
  208 s, tail `Finished \`dev\` profile [unoptimized] target(s) in 3m 28s`.
  Log: `target/host-native-logs/toolbox-build.log` (lane tree).

engine-swap-9's findings 1 and 2 still stand and are not changed here:
esaxx-rs is still a C++ build (through gline-rs → tokenizers' default
`esaxx_fast`), and the `.cargo/config.toml` `-L native=/usr/local/lib64`
comment still names the deleted llama-cpp-sys-4.

</details>
