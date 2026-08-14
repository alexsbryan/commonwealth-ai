# Launch checklist — handing svrn to a Go + TS/React team

The state of this file is the state of the launch. Every box is either
checked with a citation or it is open; "probably fine" is not a state.

Last walked: **2026-08-07**, against `2e56dc2b`.

---

## 0. Bottom line

**Not shippable yet, and the blocker is not the code.** Both artifacts a
teammate installs are published from **Jul 29–30** and predate everything
this handover depends on. The work is on `main`; it is not on the shelf.

| Artifact | On the shelf | Points at | Missing |
|---|---|---|---|
| CLI (`cli-v*`) | `cli-v0.4.0`, Jul 29 | `81cd1bc9` | `svrn journal`, the two-lane edit slot, setup registering the repo, 9 days of peer work |
| VS Code (`vscode-v*`) | `vscode-v0.1.0`, Jul 30 | — | outcome telemetry, `episode_id`, the lane split |

Verified, not inferred: `git show cli-v0.4.0:…/main.rs | grep -c '"journal"'`
returns `0`, and `edit_slot.rs` does not exist at that tag. The published
`.vsix` bundle contains no `edit_predictions/outcome` and no `episode_id`.

Consequence if you ship as-is: a teammate gets a CLI with no journal verb,
a setup that leaves nothing watching their code, and an extension that
never reports an outcome — so the evidence loop returns **zero** and the
acceptance rate stays "nothing judged" forever, on every machine, with no
error anywhere to explain it.

---

## 1. Publish (blocking)

- [ ] **CLI.** Bump the workspace version (`scripts/bump-desktop-version.sh
      0.5.0`), commit, let CI go green, then **Actions → Release (manual)
      → cli**. It produces a *draft*; smoke it and publish by hand.
      Pushing the tag alone does **nothing** — `cli-release.yml` has no
      `on: push: tags:` trigger (RELEASING.md says so explicitly).
- [ ] **VS Code extension.** `scripts/release-vsix-local.sh` — no CI
      pipeline, it releases from a local script. Version is already
      bumped to **0.2.0** and the artifact is built at
      `packages/vscode-sovereign/sovereign-fim-0.2.0.vsix` (verified to
      contain `edit_predictions/outcome`, `episode_id`, `superseded`,
      `diverged`).
- [ ] Publish both with `--latest=false` so the shelf's "Latest" badge
      keeps pointing at the desktop app (RELEASING.md).
- [ ] After publishing, re-check that `landing/install.sh`'s max-semver
      `cli-v*` resolver picks up the new CLI.

**Why 0.2.0 and not another 0.1.0:** VS Code will not treat an equal
version as an upgrade. `svrn setup --fim` passes `--force` so a source
checkout re-installs fine, but your team has **no checkout** — they take
the shelf artifact, where the version is the only thing distinguishing
old from new.

---

## 2. What each teammate runs

Five commands. Nothing here needs a checkout of this repo.

```bash
# 1 — install the CLI (takes the newest cli-v* release)
curl -fsSL https://svrnme.sh/install.sh | sh

# 2 — models + daemon. Run this INSIDE the repo you want indexed:
cd ~/code/your-repo && svrn setup

# 3 — the editing model + the VS Code extension
svrn setup --fim

# 4 — the call-graph exporter for your language (see the table below)

# 5 — prove it
svrn doctor
```

`svrn setup` run inside a git repo now registers that repo, so the daemon
builds its call graph and starts watching on its next start. Outside a
repo it says so and tells them what to run instead; it refuses to
register `$HOME`.

### Exporters — the commands that actually work

Doctor names a missing exporter, and until 2026-08-07 it named the wrong
install command for two of them. Both hints are now corrected in
`corpus-engine-scip/src/scip_export.rs`; these are the verified forms:

| Language | Command | Note |
|---|---|---|
| Go | `go install github.com/scip-code/scip-go/cmd/scip-go@latest` | Upstream **moved**: a `go install` of `github.com/sourcegraph/scip-go` dies with "module declares its path as". Also ensure `~/go/bin` is on `PATH` — `go install` writes there and it is off `PATH` on a default macOS shell, which looks exactly like the install failing. |
| TypeScript / React | `npm install -g @sourcegraph/scip-typescript` | Needs a `tsconfig.json` **at the repo root**. A normal React app has one. |
| Python | `npm install -g @sourcegraph/scip-python` | Despite the name and despite indexing Python, it is published on **npm**, not PyPI. `pip install scip-python` fails with "No matching distribution found". |
| Rust | `rustup component add rust-analyzer` | |

**The daemon must be able to see them.** It does not inherit an
interactive shell's `PATH`. `~/.local/bin`, `~/.nvm/*/bin` and
`~/.cargo/bin` are on it; `~/go/bin` is **not**, so `scip-go` needs a
symlink into one that is:

```bash
ln -sf ~/go/bin/scip-go ~/.local/bin/scip-go
```

Doctor's `scip_exporters` check resolves against the *CLI's* PATH, not
the daemon's, so it can report ✓ for an exporter the daemon cannot run.
Worth knowing until that is fixed.

---

## 3. Proven end to end (2026-08-07)

Two throwaway fixtures, registered with the daemon, then queried through
it:

| Fixture | Exporter | Result | Symbol lookup |
|---|---|---|---|
| Go module (3 files) | `scip-go` 0.2.7 | 10 symbols, 20 refs | `Greet → main.go:5`, `LogOrder → service.go:9` |
| React-TS (tsconfig at root) | `scip-typescript` 0.4.0 | 14 symbols, 26 refs | `Button → Button.tsx:8`, `useLogger → App.tsx:4` |

So both of your team's stacks work: register → the daemon exports →
`symbols` answers with `file:line`.

**The journal round trip, witnessed on the real path** (live daemon,
`~/.svrnmesh/journal`, no test harness):

```
POST /v1/edit_predictions          → engine rule, 2 edits,
                                     episode_id bfc1a3b4-ceb0-405a-…
POST …/outcome {accepted}          → HTTP 204
POST …/outcome {"outcome":"maybe"} → HTTP 400   (refused, not defaulted)
svrn journal                       → accepted 1 · unknown 24 · coverage 4%
                                     "1 judged episode is not a measurement"
```

That `unknown 24 / coverage 4%` is not a defect, it is the design being
honest on live data: those episodes were served to a VS Code window still
running the **0.1.0** extension, which has no outcome reporting. They are
counted as unknown rather than folded into dismissals, and the coverage
line is what stops anyone quoting the 100%. It is also a live
demonstration of the degradation path — against the pre-change daemon the
same probe returned no `episode_id` and the outcome route answered 404,
and the extension's contract is to post nothing in that case.

**Not yet witnessed:** the extension half of that trip. A VS Code window
must be reloaded to pick up 0.2.0; the installed-but-not-activated case is
exactly what produced the 24 unknowns above.

**Caveat worth stating plainly.** Registration builds the **call graph**.
It does not build the LanceDB chunk index, which is what backs
`code_search` and `symbols`' semantic half — that is `svrn code index .`.
`svrn setup` therefore leaves a *watched, call-graph-indexed* repo, which
is what the console message claims and no more.

---

## 4. Known rough edges to tell them about

- **This monorepo's TypeScript call graph is empty**, and that is *our*
  shape, not a tool fault: there is no `tsconfig.json` at the repo root,
  so `scip-typescript index` finds no files and errors. A React app has
  one at root (proven above). Irrelevant to them; relevant if they open
  our source.
- **`svrn journal` will be empty for a day or two.** It records an
  episode per prediction, so it stays empty until they have actually been
  editing with the extension live. It says so rather than printing zeroes.
- The acceptance rate needs ~20 judged episodes before it prints as a
  number rather than an early signal. That is deliberate.

---

## 5. Fixed on the way through (2026-08-07)

- **`svrn project refresh --name X --local` rebuilt the wrong project.**
  The local arm parsed `--name` and then discarded it
  (`let _ = explicit_name;`), rebuilding whatever repo the caller stood
  in and reporting ✓ with that repo's symbol counts. Observed:
  `--name go-demo --local` from this workspace re-exported
  commonwealth-ai and printed "250484 symbols (+0)". **Doctor's own
  repair hint for an empty graph is exactly this form**, so following the
  advice silently refreshed the wrong thing. Now resolves the root from
  the registry, and refuses (exit 1, listing what *is* registered) on an
  unknown name.
- Two wrong exporter install hints (above).
- `vsce package` was bundling the previous release's `.vsix.sha256`
  **into** the new artifact; added to `.vscodeignore`.
- Orphaned index dirs (`preflight-probe`, `e2e-demo`) left by test runs
  were holding four doctor findings open.
- **A daemon restart during a SCIP rebuild no longer leaves an empty
  graph (2026-08-14).** The rebuild now writes to a staging
  `scip_graph.db.new` under a cross-process `.rebuild.lock` (flock) and
  renames it over the live graph only on success (`ScipGraph::export_to_live`,
  corpus-engine-scip) — an interrupted export cannot empty the live graph,
  and the wipe guard refuses to rename over a populated one. Same change
  makes `project refresh` honest: it polls the daemon to a named verdict
  (completed/failed/crashed/wedged/daemon-gone), prints `✓ SCIP graph at
  HEAD` or a loud ✗ reason, and falls back to a local export through the
  same lock. The old unverified `✓ Rebuild nudged` success line is gone,
  and the self-deadlocked follow-up pass (status `active` for hours, every
  nudge coalescing silently — live 2026-08-14) is fixed structurally:
  follow-up passes run under the SAME permit (bounded to 4), a 45-minute
  watchdog plus an RAII guard clear the rebuild claim on hang or panic and
  record the failure.

---

## 6. Still open

- [ ] Publish both releases (§1). Everything else is ready.
- [ ] The daemon-side `PATH` vs. doctor's `PATH` split (§2) — doctor can
      report an exporter ✓ that the daemon cannot execute.
- [ ] `svrn journal attach --last` (per-episode code opt-in) is not
      built. The metadata-only bundle is the whole hand-back path today.
- [ ] Reload a VS Code window on the 0.2.0 extension and watch an
      `accepted` land from a real Tab. The daemon half is witnessed (§3);
      the editor half is unit-tested but not yet seen end to end.
