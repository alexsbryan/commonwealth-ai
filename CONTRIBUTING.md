# Contributing to Commonwealth AI

Thanks for being here. Commonwealth AI has been a single-maintainer project so
far, and it's now opening up. The contribution paths are still young — if
something here is unclear or the process gets in your way, open an issue and
it'll get fixed.

The short version: the assistant runs on your own machine, nothing phones home,
and every answer traces back to a source. Changes that hold that line and come
with a test proving they work are the easiest to merge.

## Ways to help

- **Report a bug.** Use the bug template — it asks for your version and platform
  up front. If the bug involves the running daemon, mesh, or models, the output
  of `sovereign doctor` helps too.
- **Suggest something.** Feature ideas go through the issue template too.
  Describe what you're trying to do, not only the fix you have in mind.
- **Send a change.** Small, focused pull requests are far easier to review than
  large ones. Planning something big? Open an issue first so we can agree on the
  shape before you spend the effort.
- **Improve the docs.** If a guide sent you the wrong way, a PR that fixes it is
  genuinely valuable.

For anything security- or privacy-related — including a way data could leave a
machine unexpectedly — don't open a public issue. See [SECURITY.md](./SECURITY.md).

## Getting set up

The whole tree is one `git clone` and one Cargo workspace.

```sh
git clone https://github.com/alexsbryan/commonwealth-ai
cd commonwealth-ai
```

You'll need a current stable Rust toolchain and a few native libraries
(protobuf and cmake at minimum; the desktop app needs more). The
platform-specific package lists live in
[`scripts/bootstrap.sh`](./scripts/bootstrap.sh) and, for Linux native deps,
[`sovereign/scripts/bootstrap-linux.sh`](./sovereign/scripts/bootstrap-linux.sh).
The build-from-source walkthrough is in the [Sovereign guide](./sovereign/README.md).

Build the CLI:

```sh
cargo build --release -p sovereign-cli -p sovereign-cli-daemon -p sovereign-cli-llm
```

## The development loop

Open the PR whenever you're ready — CI runs the full gate for you (a workspace
build, the test suite, a deterministic mesh fault-injection suite, and an
architecture gate) and reports back right on the PR. You don't have to reproduce
any of it by hand.

If you'd rather be sure before you push, these are the same commands CI runs:

```sh
cargo check --workspace --all-targets
cargo test --workspace
cargo run -p xtask -- arch-gate      # file-size ratchet + doc contracts
```

Formatting and clippy also run in CI, but they're advisory for now — don't sweat
them. There's a friendlier test wrapper the maintainer uses day to day,
`./scripts/sovereign-test.sh --human`, which prints a compact summary and takes
`--package <crate>` / `--filter <pattern>` to narrow a run; it's optional.

## What makes a change easy to merge

None of this is a checklist to clear — it's just what tends to earn a quick yes:

- **A test that pins the change.** This project leans on end-to-end tests, so a
  test that would fail without your change is the fastest way to build trust in
  it. If one genuinely isn't possible, a sentence on why is plenty.
- **The map stays honest.** If you added, removed, or reshaped a subsystem, a
  note in [`sovereign/SYSTEM_OVERVIEW.md`](./sovereign/SYSTEM_OVERVIEW.md) keeps
  it current.
- **Green CI.** Red just means not-ready-yet, not that you did something wrong —
  push again. If CI seems confused or wrong, say so in the PR.

## How the code is expected to read

Two documents are the compass, and reviews lean on them:

- [`sovereign/SYSTEM_OVERVIEW.md`](./sovereign/SYSTEM_OVERVIEW.md) — the map of
  every subsystem and how they fit. Read it before a non-trivial change; update
  it when your change moves the map.
- [`sovereign/ARCH_PRINCIPLES.md`](./sovereign/ARCH_PRINCIPLES.md) — the design
  rules used to weigh trade-offs.

Two habits carry a lot of weight here:

- **Build glassbox systems.** Someone running this should be able to see what
  it's doing. New behaviour should surface through logs, status, or clear
  errors, not happen silently. When you're unsure why something misbehaves, add
  instrumentation and reproduce it rather than guessing.
- **Write for the next reader.** Match the surrounding code's naming and idiom.
  Traceable beats clever.

## Commits and pull requests

- Commit messages in the imperative are appreciated; where it fits, the
  `type(scope): summary` form runs through the history (`feat(mesh): …`,
  `fix(atlas): …`) — but don't overthink it.
- The PR template is deliberately short. CI does the mechanical checking, so it
  just asks what changed and how you looked at it.
- Rebasing on `main` keeps history readable, but it isn't a blocker.

## Licensing

Commonwealth AI is free software under [AGPL-3.0-or-later](./LICENSE). By
contributing, you agree your contribution is licensed under the same terms
(inbound = outbound). Please don't paste in code you don't have the right to
license this way.

## Code of Conduct

Participation is covered by our [Code of Conduct](./CODE_OF_CONDUCT.md). Be kind.
