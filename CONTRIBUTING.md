# Contributing to Commonwealth AI

Thanks for being here — genuinely. This started as one person building AI tools
for people, by people, and the whole point is that it gets better together. So
first, the honest version: some doors are open and some aren't yet, and I'd
rather tell you which is which up front than have you find out from a closed PR.

The short version: the assistant runs on your own machine, nothing phones home,
and every answer traces back to a source. Changes that hold that line and come
with a test proving they work are the easiest to merge. If you want the
shortest path in, it's [a recipe](#contributing-a-recipe).

## What I can merge today

The code is AGPL — yours to run, read, fork, and change, forever. That door's
open and stays open. Pull requests are open too, but not yet for everything, and
I'd rather draw that line clearly than waste your afternoon.

**Open now:**

- **Recipes.** A recipe turns a source into a searchable, cited corpus, and it's
  one TOML file. The schema is generated from the code and test-gated, so review
  is mechanical rather than a matter of taste — which is exactly why this is the
  first door open. See [Contributing a recipe](#contributing-a-recipe).
- **Documentation fixes.** Typos, wrong commands, stale paths, a walkthrough
  that doesn't work on your machine. Send the PR; you don't need to file an
  issue first.
- **Client configs and interop.** If you got Commonwealth working with a tool
  [INTEROP.md](./docs/INTEROP.md) doesn't cover, add it. Real config that you
  actually ran beats a guess, so say which version you tested against.

**Not yet:**

- Core architecture, new crates, and changes to the runtime, inference, or mesh.

Here's the honest why for that last line. The architecture and the principles
this thing is built on are still settling; `sovereign/SYSTEM_OVERVIEW.md` and
`sovereign/ARCH_PRINCIPLES.md` are living documents right now, not stone. Until
that foundation is solid I have no fair way to decide what belongs and what
doesn't — and I'd rather say so than accept your PR today and turn away an
identical one next month for reasons I couldn't put into words. Settle the
ground first, open the gates wider second. It isn't aloofness and it isn't
forever; it's the difference between pouring a foundation and hosting a hundred
contractors in the framing. [GOVERNANCE.md](./GOVERNANCE.md) tells the rest of
the story.

If you want to work on something in the "not yet" list, open an issue describing
what you're trying to do. Sometimes the answer is "actually, yes" — and if it
isn't, the issue is still how the roadmap gets steered.

Other useful energy, no PR required:

- **Report a bug.** Use the bug template — it asks for your version and platform
  up front; `svrn doctor` output helps if the daemon, mesh, or models are
  involved.
- **Float an idea or a feature request.** Through the issue template. Describe
  what you're trying to do, not just the fix you have in mind. A thumbs-up on
  someone else's is a vote (see [GOVERNANCE.md](./GOVERNANCE.md)).
- **Run a real mesh and say what broke.** This is worth more than it sounds.
  Most of what's hard here only shows up on someone else's hardware.

Same refrain as everywhere else: be patient, be constructive, then when this
actually works, demand the best.

For anything security- or privacy-related — including a way data could leave a
machine unexpectedly — don't open a public issue. See [SECURITY.md](./SECURITY.md).

## Contributing a recipe

A recipe turns a source — a public archive, a dataset, an API — into a
searchable, cited corpus. It's one TOML file describing six stages, and no Rust:

```toml
[corpus]
id = "my-corpus"
name = "Human-readable name"
description = "One sentence on what's in it and who it's for."
license = "CC-BY-SA-4.0"      # the SOURCE's license, not ours
mesh_sharing = true

[acquire]                      # where the bytes come from
type = "bulk_download"

[extract]                      # bytes → text
type = "markdown"

[chunk]                        # text → retrievable units
type = "passthrough"

[index]
fts = true
vector = true
```

Two files change: `sovereign-recipes/<id>/recipe.toml`, and a matching
`[[recipes]]` entry in `sovereign-recipes/registry.toml` — the canonical catalog
that `corpus-engine` vendors at build time. Start by copying the closest
existing recipe; [`GETTING_STARTED.md`](./sovereign-recipes/GETTING_STARTED.md)
walks the first one, and [`SCHEMA.md`](./sovereign-recipes/SCHEMA.md) is the
field reference — it's generated from the code and test-gated, so it can't drift
from what the loader accepts.

**Run these before you open the PR.** They're the same checks review runs, so
there's no reason to find out from me:

```sh
svrn recipe validate sovereign-recipes/<id>/recipe.toml
svrn recipe test sovereign-recipes/<id>/recipe.toml
```

**Get the licensing right — it's the part I can't fix for you.** The `license`
field is the *source's* license, and two flags decide what peers may do with the
result:

- `mesh_sharing` — may a peer **copy** the built index? Set it `false` when the
  source license forbids redistribution. The Stanford Encyclopedia recipe is the
  worked example: freely searchable, not redistributable.
- `query_sharing` — may a peer **search** it and receive cited snippets back?
  Narrower than copying, and often the right answer when `mesh_sharing` is false.
- `scope = "local"` — never advertised to peers at all. Right for anything
  personal.

If you don't have the right to redistribute a source, that isn't a blocker — it's
a `mesh_sharing = false` recipe, and those are welcome. Please don't submit a
recipe pointing at something you can't legally redistribute while marked as
though you can.

**Prebuilt indexes.** Large corpora ship a pre-built index so nobody has to
embed 51,000 articles on a laptop. The registry entry carries a `[recipes.prebuilt]`
block naming a Hugging Face repo, filename, sha256, and the embedding model it
was built with — restore refuses on a model mismatch rather than corrupting
silently. If your corpus is big enough that a cold build is painful, publish the
index to Hugging Face and reference it there. This is the cheapest way to make a
corpus genuinely usable by someone else.

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

Open the PR whenever you're ready and CI reports back on it. Be aware of exactly
what it does and doesn't cover, so a green check doesn't tell you more than it
means.

**What CI blocks on today** (`.github/workflows/ci.yml`, aggregated by `ci-ok`):
`fmt` (rustfmt, pinned toolchain), `desktop` (svelte-check + vitest + the
Playwright suite), and `test` (`./scripts/sovereign-test.sh --human` over the
whole workspace). A separate workflow, `docs-reconcile`, checks that every repo
path cited by the narrative docs still resolves.

**What is currently shelved** — commented out in `ci.yml` since 2026-07-14 and
gating nothing: the structural gates (arch, layer, lock, boundary), `cargo deny`,
the clippy count-ratchet, and the deterministic mesh fault-injection suite. They
still work locally, and running them is the difference between "CI is green" and
"this is actually clean":

```sh
cargo run -p xtask -- quality        # every structural gate, one summary table
```

`cargo xtask quality` bundles arch-gate (file-size ratchet), docs-gate (every
path the narrative docs cite must resolve), boundary-gate (the studio package
stays liftable), layer-gate (dependency direction per `quality/ARCH_LAYERS.toml`
plus god-crate fan-in caps), lock-gate (no new duplicate crate versions),
env-gate (every env read declared in `quality/env-flags.toml`), and concept-gate
(one noun, one owner — no NEW name defined as a type in two crates).
Each failure message ends with the exact command that fixes it; baselines live
under `quality/baselines/` and may only shrink (see ARCH_PRINCIPLES §8.6).

concept-gate is ADVISORY inside `cargo xtask quality` and hard on its own: it
counts type definitions in the SCIP graph at the last indexed commit, not in the
working tree, so a pre-push habit-run must not go red for an indexer that is
minutes behind. CI and landing verdicts call `sovereign code converge status`
directly and gate on its exit code. The summary carries four verdicts, not two —
PASS / FAIL / COULD-NOT-JUDGE / NEVER-RAN — because a gate that could not reach
its evidence did not pass.

If you'd rather be sure before you push:

```sh
./scripts/sovereign-lint.sh --human   # workspace check, friendly summary
./scripts/sovereign-test.sh --human   # the same suite CI runs
```

Both take `--package <crate>` / `--filter <pattern>` to narrow a run. Note that a
run matching zero tests exits non-zero on purpose — a filtered run that verified
nothing is not a pass.

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
