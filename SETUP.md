# Set up a dev environment

This takes you from a fresh `git clone` to a built `svrn`, a green test suite,
and working code intelligence — on a Mac or on Linux. Budget half an hour, most
of it the first build compiling llama.cpp from source.

If you only want to *run* the assistant, you don't need any of this — `curl
-fsSL https://svrnme.sh/install.sh | sh` drops in a prebuilt binary. This page
is for building from source to work on the code.

The whole tree is one repo and one Cargo workspace:

```sh
git clone https://github.com/alexsbryan/commonwealth-ai
cd commonwealth-ai
```

## What you'll need

- A **Mac** (Apple Silicon) or a **Linux** box (Fedora or Ubuntu/Debian).
- **At least 15 GB free disk** — the toolchain, the dependency tree, and
  llama.cpp's build tree run large.
- **git**, and the patience to let one big first build run.

## On a Mac

Apple Silicon builds against **Metal** — nothing to choose, it's committed. One
script installs everything: the Xcode command-line tools, the Homebrew packages,
Rust with the components the build needs, and a persisted `SDKROOT`.

```sh
./sovereign/scripts/bootstrap-mac.sh
```

It's idempotent — safe to re-run after a pull. Prefer to do it by hand? It's the
same four steps:

1. `xcode-select --install` — the compiler and the macOS SDK.
2. `brew install lld protobuf cmake` — lld (the workspace links with it),
   protobuf (`protoc`), and cmake (llama.cpp's build).
3. `rustup`, then `rustup component add rustfmt rust-analyzer` — the second is
   what builds the code-intel call graph (see [Code intelligence](#code-intelligence)).
   The pinned toolchain (`rust-toolchain.toml`) installs itself on your first
   `cargo` call, and the components attach to *it* because you're in the repo.
4. `export SDKROOT="$(xcrun --show-sdk-path)"` in your shell rc — llama-cpp-sys
   resolves system headers through it and fails with `'memory' file not found`
   without it.

Skip to [Build it](#build-it).

## On Linux

Linux builds against **Vulkan** (committed; ROCm was dropped after a GPU
crash). One script installs everything — Rust, the native libraries, the
linker, and the Vulkan build deps — for Fedora (dnf) or Ubuntu/Debian (apt):

```sh
./sovereign/scripts/bootstrap-linux.sh                    # autodetects the GPU backend
# ./sovereign/scripts/bootstrap-linux.sh --backend=vulkan # force it on a generic dev box
```

It's idempotent — safe to re-run after a pull. It installs clang, cmake, `mold`
(the workspace links with it on Linux), protobuf, OpenSSL, bzip2, the GTK/WebKit
libraries the desktop app needs, plus the `rustfmt` and `rust-analyzer`
components. Prefer to install by hand? The exact package lists are the
`install_fedora_*` / `install_ubuntu_*` functions in that script.

> Working inside a Strix Halo GPU toolbox? The script preflights two known
> image quirks (a stripped `sudoers`, a dangling `ld`) and prints the host-side
> fix if it hits them. Run it with `--help` for the details.

Skip to [Build it](#build-it).

## Build it

The CLI trio is the quickest thing to build and run:

```sh
cargo build --release -p sovereign-cli -p sovereign-cli-daemon -p sovereign-cli-llm
```

**The first build is long.** It compiles llama.cpp from source and a large
dependency tree — tens of minutes is normal, and the fans will spin. Incremental
builds after that are quick. (A full release build carries thin-LTO and is slow
per change; when you're iterating on the daemon, `scripts/dev-release.sh`
overrides those knobs so you don't pay it every time.)

The binary lands at `target/release/sovereign-cli`. Put it on your PATH so you
can type `svrn`:

```sh
ln -sf "$(pwd)/target/release/sovereign-cli" ~/.local/bin/svrn
```

Then wire the daemon's lint/test watcher to the workspace and confirm it's all
real:

```sh
./scripts/bootstrap.sh                   # points the daemon at this workspace
./scripts/sovereign-test.sh --human      # the friendly test wrapper — compact pass/fail summary
```

Green means you're set up.

## The daily loop

Open a PR whenever you're ready — CI runs the full gate (workspace build, test
suite, a deterministic mesh fault-injection suite, and an architecture gate) and
reports back on the PR. You don't have to reproduce it by hand.

If you'd rather be sure before you push, these are the same commands CI runs:

```sh
cargo check --workspace --all-targets
cargo test --workspace
cargo fmt --all --check                  # blocking; the toolchain is pinned
cargo run -p xtask -- quality            # every structural gate, one summary table
```

After `bootstrap.sh`, the daemon's watcher lint/test-checks in the background,
so `lint_status` / `test_status` often already reflect your last edit and you
don't need to run those by hand. What makes a change easy to merge is in
[CONTRIBUTING.md](./CONTRIBUTING.md).

## Code intelligence

The daemon builds a SCIP call graph so `symbols`, `callers`, and `callees`
answer precisely instead of by grep — worth setting up, it makes navigating an
unfamiliar codebase far easier:

```sh
sovereign project refresh                # (re)build the call graph — runs rust-analyzer, a few minutes
```

One gotcha worth knowing up front: that step shells out to `rust-analyzer`, and
**a failed export wipes the graph to zero** rather than leaving the last good
one in place. So if `symbols` suddenly returns nothing, the usual cause is a
missing component — `rustup component add rust-analyzer`, then `sovereign
project refresh` again. (Both bootstrap paths above install it, so this only
bites if you set Rust up entirely by hand.)

## If something breaks

- **`'memory' file not found` (macOS)** — `SDKROOT` isn't set for this shell.
  `export SDKROOT="$(xcrun --show-sdk-path)"` and rebuild.
- **`invalid linker name` or `library not found: bz2` (Linux)** — a native dep
  is missing. Re-run `bootstrap-linux.sh`; `mold` and `bzip2-devel` are the two
  usual culprits.
- **`undefined symbol: common_speculative_*` (Linux)** — a stale system
  `libllama.so` in `/usr/lib64` is shadowing the freshly-built bundled one. The
  fix is documented at the top of `.cargo/config.toml`; the short version is to
  make sure `/usr/local/lib64` is searched first.
- **`dyld: Library not loaded: @rpath/libggml-*.dylib` (macOS)** — the rpath
  args in `.cargo/config.toml` got stripped. Don't remove them; they tell the
  loader where the bundled `.dylib`s live.
- **The daemon won't stay up, or code-intel looks wrong** — `sovereign doctor`
  is the first stop. It checks the daemon, the watcher, the indexes, and the
  mesh in one pass and tells you what to do next.

---

Built, tested, and the tools answer? You're ready to change things.
[CONTRIBUTING.md](./CONTRIBUTING.md) covers the review flow;
[sovereign/SYSTEM_OVERVIEW.md](./sovereign/SYSTEM_OVERVIEW.md) is the verifiable
map of what lives where.
