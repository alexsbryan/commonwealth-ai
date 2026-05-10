# commonwealth-ai

Monorepo containing the four-project workspace plus its data recipes:

- `oicp-types/` — OICP wire-protocol types. Bottom of the dep graph; depends on nothing.
- `corpus-engine/` — knowledge layer (LanceDB + Tantivy + extractors + chunkers).
- `sovereign/` — local agent runtime (CLI / desktop / server).
- `commonwealth/` — symmetric mesh daemon.
- `sovereign-recipes/` — corpus recipe TOMLs + data files (Wikipedia, SEP, etc.).

Single Cargo workspace at the root → one `cargo test --workspace` runs every
crate, one Cargo.lock pins every dep version, atomic cross-project commits.

## Layout

```
Cargo.toml              Top-level workspace + shared dependencies
.claude/
  CLAUDE.md             Agent contract — read on every session start
  settings.json         Harness config (hooks, permissions)
  hooks/                Pre/post tool hooks
.sovereign/
  sovereign.toml        Lint + test runner config (paths, timeouts)
  SOVEREIGN.md          Project-level code-intel overview
scripts/
  sovereign-lint.sh     Repo-wide cargo check
  sovereign-test.sh     Repo-wide cargo test (regression gate)
  bootstrap.sh          One-shot setup for a fresh workstation
  fetch-desktop-binaries.sh
oicp-types/
corpus-engine/          [package] + xtask member crate
sovereign/
  crates/               10 sovereign-* member crates
commonwealth/
  crates/               9 commonwealth-* member crates
sovereign-recipes/      data files (no Cargo)
```

## First-time workstation setup

```bash
mkdir -p ~/dev && cd ~/dev
git clone <url>/commonwealth-ai.git
cd commonwealth-ai
./scripts/bootstrap.sh    # wires daemon workspace pointer + smoke
cargo build --workspace   # warm the build cache
```

## Definition of done

Before any feature push, both must be `fresh_passing`:

```bash
sovereign tools call lint_status     # repo-wide cargo check
sovereign tools call test_status     # repo-wide cargo test
```

If the daemon isn't reachable, invoke directly:

```bash
./scripts/sovereign-test.sh --human                   # full repo, friendly
./scripts/sovereign-test.sh --human --package <name>  # one crate
./scripts/sovereign-test.sh --human --filter <pat>    # name filter
```

Adapter logs persist at `target/sovereign-test/latest/` for triage. See
`.claude/CLAUDE.md` for the full agent contract.

## History

This repo was assembled on 2026-05-10 by merging five previously-independent
git repositories — `oicp-types`, `corpus-engine`, `sovereign`, `commonwealth`,
`sovereign-recipes` — via `git filter-repo --to-subdirectory-filter`. Every
commit ever made in any sub-repo is preserved with original author + date,
under the destination prefix. `git blame` on
`commonwealth/crates/commonwealth-api/src/auto_recover.rs` still surfaces
the standalone-repo commits that introduced each line.

The pre-monorepo GitHub remotes
(`alexsbryan/{sovereign,commonwealth,corpus-engine,oicp-types,sovereign-recipes}`)
are kept as read-only archives.
