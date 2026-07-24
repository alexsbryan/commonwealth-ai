# Contributing to Commonwealth AI

Thanks for being here — genuinely. This started as one person building AI tools
for people, by people, and the whole point is that it gets better together. So
first, the honest version: the most useful thing you can do right now probably
isn't a pull request. Let me explain how this works, because being straight
about it up front saves us both time.

The short version: the assistant runs on your own machine, nothing phones home,
and every answer traces back to a source. Changes that hold that line and come
with a test proving they work are the easiest to merge.

## How contribution works right now

The code is AGPL — yours to run, read, fork, and change, forever. That door's
open and stays open. But I want to be completely straight with you, because it
saves everyone the wasted effort: **for now, I'm not merging pull requests from
outside the core team. Not the big ones, not the one-liners — not yet.**

Here's the honest why. The architecture and the principles this thing is built
on are still settling; `sovereign/SYSTEM_OVERVIEW.md` and
`sovereign/ARCH_PRINCIPLES.md` are living documents right now, not stone. Until
that foundation is solid, I have no fair way to decide what belongs and what
doesn't — and I'd rather say that out loud than accept your PR today and turn
away an identical one next month for reasons I couldn't put into words. Settle
the ground first, open the gates second. It isn't aloofness and it isn't
forever; it's the difference between pouring a foundation and hosting a hundred
contractors in the framing. [GOVERNANCE.md](./GOVERNANCE.md) tells the rest of
the story.

None of that means go away — it means the useful energy goes here:

- **Report a bug.** Use the bug template — it asks for your version and platform
  up front; `svrn doctor` output helps if the daemon, mesh, or models are
  involved.
- **Float an idea or a feature request.** Through the issue template. Describe
  what you're trying to do, not just the fix you have in mind. A thumbs-up on
  someone else's is a vote (see [GOVERNANCE.md](./GOVERNANCE.md)).
- **Spot a wrong turn in the docs?** Tell me in an issue and I'll fix it fast —
  yes, even the typos. For now the reliable path is an issue, not a PR.
- **Want to actually build?** That's the whole goal, and it opens as the
  foundation firms up and the team grows. Show up in the issues and discussions,
  share what you learn running a real mesh, and when there's real trust and real
  room, you get pulled in — a seat at the table, and the CLA that comes with it.
  The alpha-collaborator signup on the site is how you knock.

Same refrain as everywhere else: be patient, be constructive, then when this
actually works, demand the best.

For anything security- or privacy-related — including a way data could leave a
machine unexpectedly — don't open a public issue. See [SECURITY.md](./SECURITY.md).

## Getting set up

The whole tree is one `git clone` and one Cargo workspace.

```sh
git clone https://github.com/alexsbryan/commonwealth-ai
cd commonwealth-ai
```

**[SETUP.md](./SETUP.md) is the full walkthrough** — Rust, the native libraries,
the build, and code intelligence, for both Mac and Linux, in about half an hour.
The short version: you'll need a current Rust toolchain and a few native
libraries (protobuf and cmake at minimum; the desktop app needs more), then:

```sh
cargo build --release -p sovereign-cli -p sovereign-cli-daemon -p sovereign-cli-llm
```

On Linux, [`sovereign/scripts/bootstrap-linux.sh`](./sovereign/scripts/bootstrap-linux.sh)
installs every native dep in one shot; `scripts/bootstrap.sh` then wires the
daemon's lint/test watcher to the workspace.

## The development loop

Open the PR whenever you're ready — CI runs the full gate for you (a workspace
build, the test suite, a deterministic mesh fault-injection suite, and an
architecture gate) and reports back right on the PR. You don't have to reproduce
any of it by hand.

If you'd rather be sure before you push, these are the same commands CI runs:

```sh
cargo check --workspace --all-targets
cargo test --workspace
cargo fmt --all --check              # blocking; the toolchain is pinned (rust-toolchain.toml)
cargo run -p xtask -- quality        # every local gate, one summary table
```

`cargo xtask quality` bundles the sub-second structural gates: arch-gate
(file-size ratchet), docs-gate (every path the narrative docs cite must
resolve), boundary-gate (the studio package stays liftable), layer-gate
(dependency direction per `quality/ARCH_LAYERS.toml` + god-crate fan-in
caps), and lock-gate (no new duplicate crate versions). Each failure message
ends with the exact command that fixes it; baselines live under
`quality/baselines/` and may only shrink (see ARCH_PRINCIPLES §8.6).

Clippy runs in CI as a count ratchet (lint-gate): existing warnings are
grandfathered per crate/lint, so you only need to care about warnings YOUR
change introduces — the gate names them. The lane is advisory during its
burn-in month, blocking after. There's a friendlier test wrapper the
maintainer uses day to day, `./scripts/sovereign-test.sh --human`, which
prints a compact summary and takes `--package <crate>` / `--filter
<pattern>` to narrow a run; it's optional.

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

Commonwealth AI is free software under [AGPL-3.0-or-later](./LICENSE), and it
stays that way. Every contribution is always available under the license the
project uses on the day you submit it, or another OSI-approved open-source
license. That promise is written into the contributor agreement and cannot be
walked back.

Contributions are covered by a **Contributor License Agreement**
([CLA.md](./CLA.md)), the standard Harmony agreement (v1.0). In plain terms: you
keep your copyright, you grant the maintainer a broad license (including the
right to offer the project under other licenses, such as a commercial one
alongside the public AGPL), and in return the project is guaranteed to stay open
source. Everyone signs the identical document, so no contributor ends up holding
rights another doesn't.

You sign once, electronically, the first time you open a pull request: a bot
posts a link, you confirm, and it records your signature against your GitHub
account. If you're contributing as part of your job, use the Entity agreement in
the same file, or have your employer sign it, since your employer may own the
work.

Please don't paste in code you don't have the right to license this way.

## Code of Conduct

Participation is covered by our [Code of Conduct](./CODE_OF_CONDUCT.md). Be kind.
