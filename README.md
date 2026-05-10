# commonwealth-ai workspace shell

Cross-cutting tooling and agent contract for the four-project workspace
([`sovereign/`](https://github.com/), [`commonwealth/`](https://github.com/),
[`corpus-engine/`](https://github.com/), [`oicp-types/`](https://github.com/),
plus [`sovereign-recipes/`](https://github.com/)).

This repo deliberately contains **only** the things that span all four
projects: lint/test runners, the sovereign daemon's watcher config,
the agent contract (`.claude/CLAUDE.md`), and shared docs. Each
sub-project is an independent git remote, gitignored from this shell
and cloned into place by `bootstrap.sh` on a fresh workstation.

## Layout

```
.claude/
  CLAUDE.md           Agent contract — read on every session start
  settings.json       Harness config (hooks, permissions)
  hooks/              Pre/post tool hooks
.sovereign/
  sovereign.toml      Lint + test runner config (paths, timeouts)
  SOVEREIGN.md        Project-level code-intel overview
scripts/
  sovereign-lint.sh   Repo-wide cargo check (parallel × 3 workspaces)
  sovereign-test.sh   Repo-wide cargo test (parallel × 3 workspaces)
                      Definition-of-done gate. See --help for flags.
  bootstrap.sh        One-shot setup for a fresh workstation
  fetch-desktop-binaries.sh
AGENTS.md             Agent-mode entry point
HANDOFF.md            Cross-session handoff log
```

## First-time workstation setup

```bash
mkdir -p ~/dev && cd ~/dev
git clone <url>/commonwealth-ai-workspace.git commonwealth-ai
cd commonwealth-ai

# Clone each sub-repo into place. Each is its own remote.
git clone <url>/sovereign.git
git clone <url>/commonwealth.git
git clone <url>/corpus-engine.git
git clone <url>/oicp-types.git
git clone <url>/sovereign-recipes.git

# Wire the daemon + adapters; restart the watcher.
./scripts/bootstrap.sh
```

## Definition of done

Before any feature push, both must be `fresh_passing`:

```bash
sovereign tools call lint_status     # repo-wide cargo check
sovereign tools call test_status     # repo-wide cargo test
```

If the daemon isn't reachable, invoke directly:

```bash
./scripts/sovereign-test.sh --human                   # full repo
./scripts/sovereign-test.sh --human --workspace <id>  # one workspace
./scripts/sovereign-test.sh --human --filter <pat>    # name filter
```

See `.claude/CLAUDE.md` for the full agent contract.

## Why a workspace shell?

The four sub-projects are versioned independently — `corpus-engine`
ships separately from `sovereign`, etc. But the lint/test scripts
fan out across all of them, the watcher config points at all three
test runners, and the agent contract describes the cross-cutting
behavior. Putting these in any one sub-repo creates a fiction that
the others depend on it; putting them in their own thin shell keeps
the scope honest. One extra `git clone` on a fresh workstation;
zero path churn for existing checkouts.
