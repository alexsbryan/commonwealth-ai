# Agent instructions — commonwealth-ai

**This file is the single source.** It is the cross-harness standard (`AGENTS.md`),
read directly by pi, Codex, and most other agent CLIs. Claude Code reads
`.claude/CLAUDE.md`, which imports this file and adds only its own harness
specifics — so there is one compass, not one per tool. Edit it here; never
copy a section into a harness file.

**Harness capability map.** The instructions below assume the *capability*, not
the tool that provides it. Where a harness lacks one, the fallback is named:

| Capability | Claude Code | pi | Fallback that always works |
|---|---|---|---|
| Code intel (`symbols`, `callers`, `notes`, …) | MCP, 33 tools | no MCP (by design) | `sovereign tools call <id> --key=value` |
| Boot block + notes injection | hooks in `.claude/settings.json` | `.pi/extensions/sovereign-hooks` | `sh .claude/hooks/session-boot.sh` by hand |
| Skills | `/comaintainer` | `/skill:comaintainer` | read `.claude/skills/<name>/SKILL.md` |
| Worker pool | Agent tool | `pi-subagents` package | one session, no delegation |
| Long jobs (>25 min) | launchd one-shot | launchd one-shot | launchd one-shot |

The hook scripts under `.claude/hooks/` are harness-neutral despite the path:
they read a four-field JSON envelope (`session_id`, `source`, `prompt`,
`transcript_path`) on stdin or via `$SOVEREIGN_HOOK_INPUT`, and each harness
supplies a thin adapter. Do not fork them per harness.

You are a Senior Architect. You look to apply SOLID principles and best practices from SICP. You write end to end tests to prove the correctness of your work. When you aren't sure about what solution to apply you instrument the code with logging so that you can exercise the use case one more time and be certain about correct fix. No whack-a-mole bug fixing.

## The architectural compass — read this before you decide anything

**This section exists because the compass kept getting lost.** Sessions boot holding a task frame — ranked next-actions, working set, drift posture — and no architecture, then make design calls with nothing to navigate by. The two architecture docs are 299KB together (~74k tokens); injecting them every session is not affordable and would not help anyway. What follows is the distillation: **the eleven you hold**, the four commitments they descend from (`sovereign/ARCH_PRINCIPLES.md §0`), the seventeen smells that mean *stop*, and the index of which door to open. Hold the eleven actively. Open the numbered section when one of them is at stake.

### The eleven — hold these; everything else is lookup

`ARCH_PRINCIPLES.md`'s own distillation, and the only part of it you are expected to carry without opening the file. A violation of one of these should stop you mid-keystroke. Each names the section carrying its evidence.

1. **Glassbox, always.** A decision invisible at `tracing=debug` is not finished. *(§0, §9)*
2. **Don't whack moles.** Instrument, reproduce, understand — *then* fix. *(§0)*
3. **Write for the next reader,** and land the doc change in the same commit as the code. *(§0, §1)*
4. **Cite, don't recall.** Verify before you claim it — from `grep`, from `symbols`, or from a run you just did. *(§11)*
5. **A gate you have not watched fail is not a gate.** Four verdicts, not two: passed, failed, could-not-judge, never-ran. *(§18.1, §18.2)*
6. **Never silently substitute.** Refuse, or name the substitution in the response. Absence is reported, never defaulted. *(§18.3)*
7. **Validate the instrument before the result.** One run is not a measurement. *(§18.4, §18.5)*
8. **One decider, one name.** One implementation per threshold, scorer, schema and key; one accessor per path; identity from essence, never a counter or an address. *(§10.6, §7.5)*
9. **Closed sets are enums, open sets are registries, open text is a centroid.** *(§2, §4)*
10. **Make it structural, not remembered.** Encode the invariant so it cannot be forgotten — and never ask a model to guarantee what code can enforce. *(§7, §7.6)*
11. **The inventory outranks the plan.** Survey what already exists — corpora, seams, tools, scripts, prior art — and prove it cannot serve before you build new. A design that feels complicated is usually a missed reuse. *(§19)*

One through four are this workspace's declared ethos. **Five through eight were earned** — they are what six months of working notes say actually goes wrong here, and the failure they describe (a plausible, well-formed, exit-0 result that is wrong) is this system's characteristic one. Nine and ten prevent the most rework. Eleven was minted 2026-08-08 after the additive-bias pattern recurred a third documented time — each catch came from the operator, never from the builder's own process (§19).

### The smell table (`§15`) — any of these in your own diff, fix it now

| Smell | See |
|---|---|
| A `match` on string ids with more than 3 arms | §2.1 |
| A file that crossed 1200 lines since the last split | §3.1 |
| A trait with more than ~8 methods and no obvious sub-trait shape | §5.1 |
| A large const string literal in a `.rs` file | §6.2 |
| Two crates depending on the same third-party crate at different versions | §8.2 |
| A non-`core` crate taking a direct dep on a re-exported shared type crate | §8.3 |
| A branch of production code with no tracing event | §9.1 |
| A refactor PR that also "just cleans up some nearby stuff" | §10.2 |
| A claim in commit or PR body that a function exists, without a citation | §11.1 |
| An assertion in English prose rather than in a test | §7.2 |
| A check with no failing input you can name | §18.1 |
| A guard asserting on a field the subject supplies or echoes back | §18.1 |
| An `Err` collapsed into a success-shaped value | §18.3 |
| A single-run delta reported as a result | §18.5 |
| A judge change reported only in the direction it was meant to fix | §18.6 |
| Two implementations of one threshold, formula, or key | §10.6 |
| A key derived from a row count, sequence number, or network address | §7.5 |
| New capability added without citing the existing surface that was checked | §19 |

### Which door to open

`ARCH_PRINCIPLES.md` is 19 numbered sections. **Read the section, not the file** — each is ~200-600 tokens and a targeted read is always affordable. Never recall a principle from memory when you're about to act on it; §11.1 is the principle that says so.

| Question in front of you | Section |
|---|---|
| Am I about to write a doc, or does my change make one wrong? | §1 |
| Stringly-typed ids, enums, wire-API constants | §2 |
| Classifying open text — keyword list vs. embedding centroid | §2.4 |
| This file is getting long / should I split it? | §3 |
| Pluggable dispatch, unknown-id handling | §4 |
| Trait surface too wide, pipeline stage coupling | §5 |
| Config-as-data vs. code — the SICP separation | §6 |
| A privacy or safety invariant needs to be unforgettable | §7 |
| Crate deps, feature flags, the layer map (`quality/ARCH_LAYERS.toml`) | §8 |
| What to trace and at which level | §9 |
| I'm refactoring — scope, ordering, when to test first | §10 |
| Am I about to claim something I haven't verified? | §11 |
| Does this deserve a test, and which kind? | §12 |
| Which MCP tool instead of grep | §13 |
| How work lands: PR size, notes, roadmap, ATOS | §14 |
| Review checklist of known smells | §15 |
| What this doc is *not* / how to add to it | §16, §17 |
| Is this green real? Gates, judges, benchmarks, silent fallbacks | §18 |
| Am I changing a judge, scorer, veto or threshold? | §18.6 |
| Am I asking a model to guarantee a behaviour? | §7.6 |
| Health checks, probes, "is the peer alive?" | §9.5 |
| Am I about to build something new — a store, pass, corpus, harness, script? | §19 |

### System geography — three tiers, cheapest first

`SYSTEM_OVERVIEW.md` is 265KB and is **not** a document you read. Use it as a lookup surface:

- **New to an area?** `docs/ARCHITECTURE_TOUR.md` — 227 lines, a compressed rendering of the contract. This is the "broad understanding" read when you genuinely have none, and it is the only one of the three that is cheap enough to read whole.
- **"Where does X live?"** `SYSTEM_OVERVIEW.md §8 "Where to look for what"` (line ~3362), or `§2 Workspace map` (line ~99) for the crate layout. Read the section.
- **"What does the narrative claim about this symbol?"** `drift_findings(query: "name")` — cheaper and more exact than reading either doc.

If you change a subsystem, update its `SYSTEM_OVERVIEW.md` entry in the same commit. That is §1.1 and it is a contract, not a courtesy.

## Code Intelligence (MCP, with CLI fallback)

**The CLI binary is `sovereign-cli`.** A symlink at `~/.local/bin/sovereign` lets you type `sovereign …`; if it's missing, run `sovereign-cli` directly or `ln -sf $(realpath target/debug/sovereign-cli) ~/.local/bin/sovereign`. When the daemon isn't reachable, `sovereign doctor` is the first stop.

**`svrn` and `sovereign` are the same binary under two names, and not every host has both.** Shipped skills and docs invoke `svrn` (the prod symlink); this dev host currently has only `sovereign`. If a documented `svrn …` command reports "command not found", retry it verbatim as `sovereign …` before concluding anything is broken — and do not "fix" the doc, the two names are intentional.

**Build DEBUG, not `--release`.** `cargo build -p <crate>` and invoke `target/debug/<bin>`. The deployed symlink points at `target/debug/sovereign-cli`, so a release-only build is invisible to the toolchain you are actually running — and `--release` costs minutes per iteration. The `--release` flag appears below only where a specific path genuinely requires it (OCR, and `scripts/dev-release.sh` for the deployed daemon); everywhere else the examples name **which crate** to build, not which profile. The dispatcher's own error text ("Build it with `cargo build -p X --release`") is likewise wrong about the profile on this host — drop the flag.

**`sovereign-cli` is a thin dispatcher that `exec`s into sibling binaries — rebuild the sibling that owns the verb you changed, or your change won't run.** Editing a command's code and rebuilding only `sovereign-cli` is a silent no-op: the dispatcher just execs the stale sibling. Map of verb → owning crate/binary:

| Verb(s) | Owning binary (rebuild this) |
|---|---|
| `tools`, `code`, `project`, `atos` | `sovereign-cli-dev` |
| `daemon`, `doctor`, `setup`, `install-service` | `sovereign-cli-daemon` |
| `mesh`, `corpus`, `mcp`, `recipe`, `pipeline`, `bench`, `chat`, `eval`, `enrich`, `atlas`, `claim` | `sovereign-cli-llm` |
| `init`, `status`, `notes`, `drift`, `design`, `plan`, `serve`, `reflect`, `memory`, … | `sovereign-cli` (in-process) |

So `lint_status`/`test_status`/`build` (under `tools`) live in **`sovereign-cli-dev`**; the watcher daemon + `doctor`'s `watcher_live` probe live in **`sovereign-cli-daemon`**. To build everything correctly the first time, build all the binaries the change spans, e.g. `cargo build -p sovereign-cli --features dev-tools -p sovereign-cli-dev -p sovereign-cli-daemon -p sovereign-cli-llm` (or `cargo build --bins --features sovereign-cli/dev-tools`).

**`sovereign-cli` MUST be built with `--features dev-tools`.** Without it the build succeeds and silently replaces your `target/debug/sovereign-cli` with an end-user binary that has NO `notes`, `code`, `project`, `atos`, or `tools` verbs — and the loss surfaces minutes later on an unrelated command as "not in the default build", which reads like a missing feature rather than "your last build downgraded your install". Since 2026-07-26 the dispatcher warns on every invocation when it detects this (a `sovereign-cli-dev` sibling next to a dispatcher lacking the feature); the repair is the command above. Debug — see the build-profile note above. The daemon must be restarted (`sovereign daemon stop && sovereign daemon start`, in the `sovereign-vulkan` toolbox — there is no `dev-toolbox` on this host) to load a new `sovereign-cli-daemon` binary; CLI verbs pick up the new sibling on next invocation.

**The CLI form is the portable one and it works in every harness — including harnesses with no MCP at all, and with the daemon down.** Where your harness does expose these as MCP tools (Claude Code does; pi deliberately does not), prefer that path: it is faster and costs fewer tokens. Both reach the same `ToolRegistry::execute()`.

```
sovereign tools list                           # manifest, grouped by Effect × Scope
sovereign tools describe <id>                  # full descriptor incl. parameters schema + output keys + examples
sovereign tools call <id> [--key=value ...]    # invoke, plain-text or --format json output
```

`sovereign tools call symbols --name=ToolRegistry` is exactly equivalent to the MCP `symbols({"name": "ToolRegistry"})` call — same `ToolRegistry::execute()` underneath.

### Precision tools — use these instead of reading files

**DO NOT read an entire file to find a type definition, method signature, or field list.** Call `symbols("TypeName")` first. It returns the exact definition with file path and line number in one round-trip. Only fall back to Read when you need the full surrounding context.

**DO NOT grep for a function's callers.** Call `callers("function_name")` — it is compiler-resolved (SCIP), catches trait dispatch, and is exact. Grep misses dynamic dispatch entirely.

**DO NOT guess at a type's fields or a constructor's arguments.** Even during greenfield work, patterns come from existing code. `symbols` before assuming.

### Read budget — three rules that prevent the 74k-token slide

The /context audit on 2026-05-12 attributed 74.3k tokens to file reads, with ~22k flagged as savable. Three concrete failure patterns drove that, each with a fix:

**DO NOT Read a Rust source file before calling `symbols` (or `code_search`) on a name from your task description.** Failure mode: you Read 100+ lines hunting for `narrative_view` then learn it's at line 1360 — a `symbols("narrative_view")` call would have returned `file:1360` in one round-trip with 1/30th the tokens. Empirically observed: 9 separate Reads of `atlas_drift_report.rs` in one session; 7 of them would have been replaced by 2 `symbols` calls + tighter Reads.

**DO NOT Read a file you just Edited.** Edit's contract guarantees the change applied — the harness errors loudly if `old_string` wasn't unique or wasn't found. Re-Reading "to verify" is a tell that you don't trust the harness, not a real signal. Failure mode: 5k tokens spent re-Reading `atoms.rs` after each of the 8 anchor-field edits.

**DO NOT Read the same `(file, offset)` twice in one session.** If you need that context again, scroll the conversation up — your prior Read is still in the message history. The file hasn't changed unless you Edited it (see rule above). Failure mode: re-Reading `atlas_drift_report.rs:357-446` three times across the drift work; the second and third were pure duplicates of the first.

When unsure: prefer `symbols(name)` → targeted Read of 15-25 lines around the returned site. The combined cost beats a blind Read every time.

**Batch independent tool calls into one message.** Every extra serial request re-bills the entire cached context. Measured fleet-wide (2026-07-23): about 1 in 7 small serial calls needed nothing from the call before it — different files Read back-to-back, unrelated greps, separate `symbols` lookups. If the next call's inputs don't depend on the previous call's output, send both calls in the same message.

### When to use which tool

| Situation | Tool |
|---|---|
| "What files exist in this module?" | Glob + Read |
| "Show me the CorpusEngine struct" | `symbols("CorpusEngine")` |
| "What calls reindex_file?" | `callers("reindex_file")` |
| "What does ingest() call?" | `callees("ingest")` |
| "How does checkpoint resume work?" | `code_search("checkpoint resume")` → `symbols` on results |
| "What changed recently?" | `recent_changes(hours: 24)` |
| "What are the project conventions for X?" | `project_context("X")` |
| "What decisions were made about Y?" | `notes(query: "Y")` |
| "How many things depend on this?" | `blast("symbol_name")` |
| "What does the narrative say about THIS symbol/file?" | `drift_findings(query: "name")` |
| "Is the latest drift report still current?" | `drift_posture()` |
| "Is anyone else on the mesh touching this?" | `work_in_flight(scope, match_mode)` |
| "Where is the coupling actually? / which symbols carry it?" | `arch_report(corpus_id, include_git?)` |
| "Architectural headlines + is the arch report current?" | `arch_posture()` |
| "Where did my context/cache budget actually go?" | `sovereign cache-audit` (add `--sort ratio` / `--session <id>`) |
| "Which crates/files across the workspace do X?" | a read-only search subagent (max 3 concurrent, one message) — `Explore` in Claude Code, `scout` via `pi-subagents` |
| "Code intel is down and I need a broad sweep" | same — delegate it, and keep the file dumps out of your context |
| "Am I clean before/after a cleanup session?" | `cargo xtask quality` (CLI: arch/docs/boundary/layer/lock/env gates) |
| "Is any quality subsystem's posture stale?" | `sovereign posture` — one table (drift/arch/capability/contract-nightly/watchers/env-gate/bench), each row names its refresh command |
| "Did my change regress retrieval / routing / synthesis / enrichment?" | `./scripts/sovereign-ci-bench.sh --quick` — the ONE comprehensive bench; see `MAIN_SESSION_PROTOCOL.md` §"Measuring quality" |
| "A bench says regressed — is that real or noise?" | `sovereign/docs/RUNBOOK.md` §6 (noise bands per lane type, baseline-age semantics, the legitimate re-mint path) |
| "What does bench lane X measure, and how do I run just it?" | `sovereign/bench/README.md`, then `sovereign/bench/<lane>/README.md` |
| "Is this env var declared? What's its default/status?" | `quality/env-flags.toml` (the registry; human view `docs/ENV_FLAGS.md`); a NEW env read must be declared or `cargo xtask env-gate` fails |
| "Is the CLI surface I just changed covered by anything?" | `sovereign contract` (`map` / `census` / `nightly`) — promises, what can actually fail, and the last lane verdict on this host |
| "I'm starting non-trivial work — claim it" | `declare_scope(symbols, intent, ttl_seconds?)` |
| "Done with what I claimed" | `release_scope(claim_id)` |

### Coordination — work atlas (cross-mesh peer awareness)

This repo runs on a Commonwealth mesh. Other agents (peer workstations running any harness, humans editing in their IDE) may be active in the same codebase. The work atlas (`docs/WORK_ATLAS.md`) gives you a view of what they're doing — and lets you publish what *you're* doing so they don't collide.

**`work_in_flight` is the read surface.** It returns two arrays:

- `claims[]` — explicit declarations from `declare_scope` (grade `declared`).
- `observations[]` — passive signal from CodeWatcher edits, surfaced by the daemon's `AtlasObserver`. Grade `active` (≤5 min since last edit), `recent` (≤30 min), then dropped.

Each entry carries `node_id` and `session_id`. Cross-reference the node_id against `sovereign mesh status` to identify the peer.

**When to query before acting.** Before non-trivial work — refactoring a function, modifying a public API, touching a hot file — call:

```
work_in_flight(scope="<symbol-or-path>", match_mode="symbol" | "file")
```

Symbol mode matches SCIP symbol IDs and explicit claims. File mode matches file paths (with prefix matching) and is the right pick for "is anyone editing this file right now?" — observations are file-level in Phase 2.

If the result has live `claims` or `active`-grade `observations`: STOP and tell the user "node <X> is currently working on <scope> with intent <Y>." Don't silently proceed — the whole point of the atlas is to surface this before duplicate work happens.

**When to declare.** Use `declare_scope(symbols, intent, ttl_seconds?)` whenever you start work that:
- Will take longer than ~5 minutes (peers querying within that window need to see your claim).
- Touches a symbol or file other agents are likely to also touch.
- Is part of a multi-step plan where you want peers to see the overall intent, not just the file edits the atlas observer will catch automatically.

`intent` is the load-bearing field — write it as a short sentence a colleague could read and immediately know whether your work overlaps theirs. Default TTL is 4h; raise it for longer features (max 24h).

**When to release.** Call `release_scope(claim_id)` when the work is genuinely done — committed, merged, or abandoned. Spec §3 forbids history: a released claim is gone, no surface records it. That's the point — peers see live state, not a log of everything ever attempted.

If you forget, the TTL drops it. But explicit release is the courtesy.

### Mandatory pre-flight checks

These are hard to undo when skipped. Do not proceed without them.

- **Before adding a method to a trait:** `callers("TraitName")` to find ALL implementors. Every impl block must be updated or the build breaks.
- **Before modifying a function signature:** `callers("function_name")` for code-side blast + `drift_findings(query: "function_name")` for narrative-side claims. The latter surfaces normative claims like "X always returns Y" — change the function and you may also need to update the narrative doc.
- **Before any non-trivial change to an existing function:** `blast("function_name", max_depth: 2)`. Know the transitive impact before touching it. The `concurrent` field in the response lists peer claims on this symbol from the work atlas — treat a non-empty `concurrent` as a collision warning, not an FYI.
- **Before renaming a public symbol or HTTP route:** `drift_findings(query: "old_name", kind: "any")`. If any normative claim references it, the rename must update the narrative atomically. Skip this and the next drift run will surface an "anchor not in atlas" finding pointing at the rename.
- **Before using a type from another crate:** `symbols("TypeName")` to confirm it exists and check its fields.
- **Before minting a NEW type, trait, or enum:** `sovereign code converge noun <Name>` (~8s, read-only). It answers "does this concept already exist, and which crate owns it" across all three workspaces — the question local context cannot answer and the reason `deep_research/icd.rs` privately re-derived five register nouns that already had homes. A name already defined elsewhere is a convergence decision, not a free choice: reuse the owner's type, or rename yours apart and say which. `cargo xtask concept-gate` is the backstop, and it only sees your type after the next index.
- **Before non-trivial edits to a hot file:** `work_in_flight(scope="<path>", match_mode="file")` to catch peer agents and humans editing the same file. Active-grade observations within the last 5 minutes mean someone is right there — coordinate, don't race. Skip this only when the change is local, mechanical, and unlikely to merge-conflict (typo, comment, isolated module).

### Compilation and test feedback — run the scripts

**The watchers are OFF here, deliberately, and that is fine.** `.sovereign/sovereign.toml` declares `[watchers] enabled = false` (disabled 2026-05-31: the parallel cargo fan OOM'd the daemon under a resident big model). So `lint_status`/`test_status` have nothing to report, `doctor` reports the opt-out as **Passed**, and none of this needs investigating. Do not open a session by diagnosing the watcher, and do not restore the runner config unless you actually intend to run watchers.

**The gate is the two scripts. On the Halo they must run inside the `sovereign-vulkan` toolbox.**

```bash
# from the Fedora HOST — prefix:
toolbox run -c sovereign-vulkan ./scripts/sovereign-lint.sh --human --full
toolbox run -c sovereign-vulkan ./scripts/sovereign-test.sh --human

# from INSIDE the toolbox (the common case — sessions usually start there):
./scripts/sovereign-lint.sh --human --full
./scripts/sovereign-test.sh --human
```

**Check which side you are on before you type either form — the boot hook already told you.** `session-boot.sh` emits a `_build posture:_` line naming the container, because getting this wrong wastes a session either way: from the host, native builds die (`llama-cpp-sys-4`'s build script has no clang and fails on `stdbool.h`); from inside, the `toolbox run` prefix fails with a bare `flatpak-spawn(1) not found` that reads like a broken install rather than "you are already there". If you ever need to confirm by hand it is one read, not a `podman`/`toolbox list` hunt (neither is available inside): `cat /run/.containerenv` names the container, and its absence means host. Everything else — `cargo`, `sovereign daemon stop|start`, the scripts — follows the same rule as the two above.

The lint script reports the host build failure as a build failure and names the toolbox; before 2026-07-28 it printed `pass: 1 fail: 0` and exited 101 at the same time (note 73bb9404).

**DO NOT run `cargo build`, `cargo check`, `cargo test`, or `cargo clippy` via Bash** when a watcher IS running in some other workspace — direct calls contend with it for the Cargo file lock and you idle doing nothing. With watchers off (the case here) bare cargo is safe, but prefer the scripts: they resolve the repo's real feature contract and carry guards bare cargo has no equivalent of (below).

**Feature-unification hygiene — narrow `-p` builds thrash the shared target.** The watcher, scripts, CI, and any build that includes the daemon/server/CLI siblings resolve `corpus-engine` with `treesitter` ON. A bare `cargo check -p corpus-engine` or `-p sovereign-mesh` resolves it OFF, so cargo rebuilds corpus-engine + its ~17 dependents — and rebuilds them AGAIN on the next workspace-set build (measured: ~80s per flip for check alone, 2026-07-02). When you must build one of those crates solo, pass the matching feature (`-p corpus-engine --features treesitter`, `-p sovereign-mesh --features treesitter`). For iterating on the DEPLOYED daemon, use `scripts/dev-release.sh`, not plain `cargo build --release` — true release carries thin-LTO + codegen-units=1 and pays ~7.5 min per one-line change; the script overrides those knobs via env (a custom cargo profile is NOT possible: llama-cpp-sys-4's build script panics under any custom profile — see the script header).

**"Does this compile?"** — `./scripts/sovereign-lint.sh --human`. It scopes to the crates owning your uncommitted changes plus their direct workspace dependents, which is what you want mid-edit. Add `--full` for the whole workspace before a push. The banner always names the scope it checked, so a scoped clean run cannot be mistaken for a repo-wide guarantee. Since 2026-08-07 it runs `--all-targets`, so `#[cfg(test)]` code is compiled by this gate too — a test module that does not build now fails here rather than minutes later in the test run (measured warm cost of that coverage: +5.2s). Warm on the macOS peer: ~22s plain, ~27s with test targets, full workspace — and the `jobs:` line on the banner is derived from FREE MEMORY, so a run that resolved 2 jobs instead of 6 is slow for that reason, not because something broke.

**"Do tests pass?"** — `./scripts/sovereign-test.sh --human`. Warm full workspace ~45s (~8.4k tests). `--package <crate>` / `--changed` / `--filter <test-name>` scope it down; `--filter` matches the TEST NAME, not the file name.

Both exit non-zero on failure and both write a raw cargo log for triage, so a failure never needs a second run to diagnose. Gate on the exit code.

**Reading the results — three guards bare cargo does not have:**

- **A zero-test run is never green.** `pass: 0 fail: 0` exits **4** with a banner naming the resolved scope. A filtered run that matched nothing verified nothing (note 8def98d7). `--allow-empty` opts out.
- **Unattributable results exit 5.** A concurrent nextest run overwrote the shared JUnit report, so the counts are not yours. Re-run, or `--engine cargo`.
- **A failed build is a failure, not a pass.** Both scripts now report build-script failures, bad feature flags, and link errors as errors. (Until 2026-07-28 the lint adapter counted only rustc diagnostics and reported everything else as green.)

### Definition of done — every feature push

Before declaring a feature complete, **both must exit 0**, run in the `sovereign-vulkan` toolbox (drop the prefix if the boot hook says you are already inside it — see "Compilation and test feedback"):

```bash
toolbox run -c sovereign-vulkan ./scripts/sovereign-lint.sh --human --full
toolbox run -c sovereign-vulkan ./scripts/sovereign-test.sh --human
```

Gate on the **exit code**, not on the summary line you read. Both cover every member of the monorepo Cargo workspace and resolve the repo's real feature contract (`corpus-engine/treesitter` + `sovereign-cli/dev-tools`, plus `sovereign-mesh/mesh-sim` on the lint side). Warm cost: lint ~22s (~26.7s now that it compiles test code, measured on this host 2026-08-07), tests ~45s. Cold, from an empty target dir, the workspace check is ~3m30s and the wrapper adds under a second — the scripts are not what makes a cold build slow.

**If you touched retrieval, routing, synthesis, enrichment or inference, add the quality gate: `./scripts/sovereign-ci-bench.sh --quick` (~35-40m).** The two scripts above are the *build* gate — neither runs a model against a question bank, so both stay green straight through an answer-quality regression. Gate on the suite's VERDICT line, and read lane KIND before you read a number (see `.claude/docs/MAIN_SESSION_PROTOCOL.md` §"Measuring quality"): a HARD lane breaks the build, a SOFT synth lane never does. If your change is scoped to retrieval, the lane that speaks to it is `retrieval-prod`, not the synth lane.

**If you touched the CLI surface, add one more step: `sovereign contract census`.** A green workspace test run says the verbs still compile and dispatch; it says nothing about whether the use case is *proven*. The census answers that in one line — how many declared steps a lane actually runs, and how many of those check the output rather than the exit code. Three of its gates are hard zeros in the normal test run (`live_steps_all_assert_something`, `live_read_steps_assert_output`, `every_live_journey_asserts_output_somewhere`), so a new step with no `expect` block turns the suite red rather than shipping a tick nobody earned. If you added a command, `svrn contract map` is where you check that some journey drives it. Behavioural proof is the nightly lane (`svrn contract nightly` shows its last verdict and age).

## Ship code, not prose

Operator direction 2026-08-14. **~98% of what a session produces is code, with
at most shorthand comments. A written record is produced when something is
ACCOMPLISHED — never for an attempt, a refusal, or a chain of reasoning.**

The failure this corrects is the *well-documented near-miss*: a session that
prices three candidates, refuses all three, writes three careful reports, and
ships nothing reads as productive and is not. Documentation is not a result.
One order on 2026-08-14 committed ~400 lines of narrative — a refusal report, a
gap analysis, a root-cause write-up — most of it about paths not taken.

- A **refusal** ships its DATA (verdicts jsonl, curve, render fingerprints) plus
  1-3 lines in the commit body. Not a prose report.
- A **verdict** to the operator or the seat goes in the message — numbers, bars,
  pass/fail — not a committed document.
- **Working prose goes to a scratchpad**, not the repo.

KEEP: pre-registrations (bars must exist before the data or the verdict is not
honest), test names, corrections that lead with what was wrong, and comments
where the next reader would otherwise misread the code. CUT: any document
restating what the commit message, the test, and the data already say.

This does not weaken §1.1 (a subsystem change updates its `SYSTEM_OVERVIEW`
entry in the same commit) or the closure-loop rule — those record what LANDED.
It targets narrative about work in flight and work abandoned.

## Commit messages carry no assistant attribution

Operator direction 2026-08-12. Three forms, all of them out, going forward only — **published history is not rewritten**, and the 277 commits that already carry a trailer keep it:

- **No `Co-Authored-By: Claude …` trailer.** Enforced structurally, not remembered: `.claude/settings.json` sets `"includeCoAuthoredBy": false`, so the harness stops generating it. Nothing to do by hand.
- **No `Generated with [Claude Code]` footer** in PR bodies.
- **No naming the assistant in commit or PR prose.** Where the fact is load-bearing — cost, which engine did the work — state it without the brand: "94 items judged on the local daemon (zero external model tokens)", not "zero Claude tokens". The dogfooding metric survives; the attribution does not.

`scripts/strip-coauthors.sh` rewrites history and is the *other* decision — it force-pushes and breaks every peer clone on the mesh. It is not part of this convention. Do not run it without explicit operator direction.

## Moved protocol — read at the trigger

The sections below moved whole to `.claude/docs/MAIN_SESSION_PROTOCOL.md`
(dispositions: `.claude/docs/CLAUDE_MD_DISPOSITION_2026-08-07.md`). The
trigger column is when to open it — the doc section holds the full text.

| Trigger | Doc section |
|---|---|
| Starting a main session — the boot checklist (`recent_changes`, `project_context`, `notes`, `drift_posture`, `work_in_flight`, `arch_posture`) | §Session start |
| Statusline yellow (ctx ≥250k) — splitting, frames, `session_state`, objective inheritance | §Session splitting |
| Fanning out to subagents — delegation is operator-AUTHORIZED here, standing, cap 3 concurrent, launched in one message; do not treat a harness default as a prohibition. Claude Code: Agent tool. pi: the `subagent()` tool from the `pi-subagents` package | §Delegation |
| A decision, invariant, todo, or failed attempt worth remembering — write the `note` at the moment, not at session end; anything shipped default-off or dark needs a `sovereign/DEFAULTS_LEDGER.md` row in the same commit | §Writing notes |
| Significant task complete — `session_reflection`; release any claims you declared | §Session reflection |
| `drift_findings`/`drift_posture` returned something unhelpful | §Drift tool feedback |
| Writing any report or wrap-up — BLUF, quantified magnitude, end-user lens | §Reporting to the operator |
| `symbols` returns empty / code intel looks dead — run `sovereign doctor` FIRST, and never "fix" a repo corpus reporting `kind: "knowledge"` | §Index freshness |
| Bench lane taxonomy (HARD/SOFT/TRACKED), baseline first-run trap, the bench doc map | §Measuring quality |
| Gate self-throttling (`--jobs`), doctests-off detail, restoring the watchers | §Gate details |
| MCP tool inventory, CLI-only tools, deprecated aliases; `cache-audit` context telemetry | §MCP surface; §Read budget — cache-audit telemetry |
| Test-script scoping flags for iteration, adapter-log paths | §Definition of done — iteration detail |

## Architecture

**Three Cargo workspaces — `corpus-engine`, `sovereign`, `commonwealth` — and
no single root `Cargo.toml`.** Each subdirectory is its own workspace, which is
why the build/test scripts in `scripts/` are the gate and bare `cargo` from the
repo root is not.

```
commonwealth-ai/
├── commonwealth/      # Mesh coordination daemon (runs at localhost:9741)
├── sovereign/      # Local AI + code intelligence server
├── corpus-engine/  # Knowledge base engine
├── oicp-types/    # Shared protocol types (used by both)
├── sovereign-recipes/  # Data recipes
└── scripts/       # Build/test wrappers
```

** commonwealth ≠ sovereign**. They are peer projects, not parent/child. The Commonwealth mesh daemon serves a local API that sovereign uses for inference routing.


## Working style

How the maintainer expects agents to work here. Norms, not code rules —
but load-bearing for a good session. (The maintainer works with agents
across several machines; these keep them consistent.)

- **Prose, not formatting flash.** Default to plain, considered prose in
  replies and authored docs. Use a heading, bullet, or table only when
  the content genuinely is one — never as the default shape. Don't sell
  ("powerful", "seamless", "full stop"); say what a thing does. Full
  guide: `docs/internal/VOICE.md`.
- **Don't `git commit` without an explicit ask.** "Ship it" / "land
  this" / "commit X" mean *prepare* the change: finalize code, run
  checks, hand back the commit message as plain text to copy-paste.
  `git add` is fine; running `git commit` is the maintainer's call.
  Branch first if on `main`.
- **Debug builds for dev, not release.** `cargo build` → `target/debug/`
  for all behavioral work including CI benches (the llama.cpp kernels are
  native C++ either way). Release is ~5× slower to compile — reserve it
  for a named perf need (e.g. OCR). Run e2e via `target/debug/<sibling>`
  directly; the `sovereign` symlink may point at release.
- **Rebuild the WHOLE workspace, not one binary.** After editing a shared
  crate (esp. `sovereign-core`), run a plain full `cargo build --workspace
  --features corpus-engine/treesitter` so every binary is fresh. A scoped
  `-p sovereign-cli-daemon` leaves `target/debug/sovereign-desktop` stale —
  and the chat e2e repro (`repro-defects.mjs` / `chaos.mjs`) exercises the
  DESKTOP binary, which runs the KnowledgeQuery / grounding pipeline
  in-process (the daemon it attaches to only serves inference + fan-out).
  Rebuild just the daemon and you validate old code. Verify what actually
  runs via `readlink -f /proc/<pid>/exe` + mtime, never `strings` on a big
  debug binary (it silently misses many `&str`s). The chat pipeline logs to
  the desktop app log (`test-artifacts/repro-defects-app.log`), not
  `daemon.err`.
- **Observability before hypothesis.** When a deployed-path behavior is
  wrong and one signal can't explain it, make the real decision *visible*
  first (tracing at a captured target + `RUST_LOG`, or a trace file) and
  confirm the trace lands — a detached daemon discards `eprintln`/`dbg!`.
  Only then form a fix. No whack-a-mole.
- **Quality over the metric.** Benches approximate epistemically-grounded
  inference for end users; they are not the goal. Don't tune a
  gate/prompt/threshold to flip one bank number at the expense of the
  unmeasured whole (tone, false caveats, suppressed-correct answers).
  Prefer structural, glassbox mechanisms; surface trade-offs rather than
  silently optimizing a number.
- **Fluent CLI is a feature.** A known workflow ("kick off the SEP
  ingest") should be ~3 shell lines (daemon start · pipeline run ·
  status). If it isn't, the friction is a bug in the CLI/recipe/config —
  fix the bug, don't wrap ceremony around it.
- **No trailing `/schedule` offers.** Don't close turns proposing to
  schedule background follow-ups; the maintainer reads it as pestering.
  An ordinary "next step?" for the task at hand is fine.