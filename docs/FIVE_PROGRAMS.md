# Five programs — the idiomatic design, and the cut that gets there

> **IN FORCE since 2026-09-21, on branch `cut` (tag `pre-cut` = 6bda3417a).**
> The `domains` campaign is superseded. Two parts of §7 were dropped as
> ceremony by operator direction: the coverage apparatus (steps 1-4 and 8 —
> llvm-cov, the twelve-row instrument table, `cut-report.py`, the
> pre-registered ratio bar) and the prose rules that went with it. The gate is
> the compiler plus `scripts/sovereign-lint.sh` and `scripts/sovereign-test.sh`.
> What remains in force is §9, and §9 now runs FIRST rather than as the
> fallback it was drafted to be — see the note at its head.

Drafted 2026-09-17. Intended to supersede the ten-context decomposition in
`quality/DOMAINS.md` §4 and the `domains` campaign's relocation plan. Keeps
`sovereign/ARCH_PRINCIPLES.md` whole and the four-verdict rule. Everything
here is either a measurement taken on 2026-09-17 against `ralph/domains-campaign`
or a decision; the decisions are marked.

## 1. The product

Sovereign answers a question from what you already have, says where the
answer came from, and declines when it cannot.

That sentence is served by five programs joined by two wires everyone already
speaks: OpenAI-compatible HTTP for turns and completions, MCP for tools.
Composition is by process. One program dials another; none embeds another;
each owns its own config file, data directory and log. The crate graph is a
build detail, not architecture.

## 2. The five

| Program | Shape | Wire it serves | Owns on disk | Exists today as |
|---|---|---|---|---|
| `svrn` | knowledge server | MCP `ask`, `search`, `atoms_lookup`; HTTP `/v1/chat/completions` | corpus indexes, conversations | `corpus-mcp/` (three verbs, lifted) + the turn path in `sovereign-core` |
| `svrn ingest` | recipe pipeline | CLI only | the index directory it writes | `sovereign-recipes/` + `corpus-engine` extractors, chunkers, index |
| `cmnwlth` | endpoint router with a roster | HTTP `/v1/*` proxied, OICP manifest | roster, adverts, decision log | `commonwealth/` package (lifted, `scripts/cw-rails-lift.sh`) + the serving cluster |
| `svrn code` | LSP for agents | MCP `symbols`, `callers`, `blast`, …; HTTP `/v1/completions` (FIM) | SCIP index, notes | `sovereign/crates/sovereign-tools/src/code/`, `corpus-engine-scip`, `corpus-engine-notes`, `packages/vscode-sovereign/` |
| `svrn bench` | evaluator | none; dials a URL | banks, baselines, verdict tables | `sovereign/bench/` lanes, `sovereign-eval` minus its product links |

Clients are not programs. A browser, a coding harness, Open WebUI and `curl`
are all clients of the two wires and are built and prioritised as clients. The
reference client is the browser (§2a); the Tauri desktop, which already links
only `sovereign-contracts`, `sovereign-time`, `sovereign-tools-base` and
`sovereign-turn-client` (its `Cargo.toml`, 2026-09-17), becomes an optional
native shell around it.

## 2a. The reference client is the browser

`svrn` serves its own UI. One static-file route in the daemon serves the built
SPA (today's Svelte under `sovereign/crates/sovereign-desktop/src` and
`packages/chat-ui`, 83k lines, unchanged in volume), and `svrn open` opens
`http://localhost:9741/`. The page talks HTTP and SSE to the daemon that
served it, same origin, no CORS, no second process holding state. Attach mode
ceases to exist because there is one mode.

What changes in the UI, measured 2026-09-17: 226 `invoke` sites become fetch
and SSE calls against daemon routes; the native surface behind them is 15
dialog calls, 25 event subscriptions and 4 others, so the Tauri command layer
(261 commands, 26k lines of Rust) goes to zero. Two `WebSocketUpgrade` sites
become SSE; SSE is already the transport on completions and inference.
Reload mid-turn resumes from the daemon's conversation stream route. Files
upload as multipart to the existing ingest job routes.

A native shell is optional and does only what a browser tab cannot: tray and
autostart, a global hotkey with a quick-ask panel, folder drop with a real
path, notifications, keychain, updater. Six commands, on the order of 2k
lines, built after the browser client is complete and only if a local user
wants them. Mobile (`sovereign-mobile`, 3.6k lines) is the same served UI in
a webview over the tailnet or iroh.

Embeddedness is three things and none of them is a window: the daemon is an
OS service (`install_service_cmd.rs`, `sovereign-service`) and is present
when no window is; the daemon is an MCP server, so Claude Desktop, Cursor,
VS Code and every coding harness mount `ask` inside their own surface; and
the FIM endpoint puts `svrn code` in the editor.

## 2b. The hosted case — one daemon per organisation, SSO at the edge

The same binary and the same UI run inside an organisation's compute
boundary. What differs is who is on the other end of the wire.

**Identity.** SSO is not in the daemon. An identity-aware proxy (oauth2-proxy,
Pomerium, Cloudflare Access, Tailscale, Caddy) does OIDC or SAML and forwards
a signed JWT carrying `sub`, `email` and `groups`. The daemon validates it
against the issuer's JWKS and maps it to a third `Principal` arm,
`Asserted { sub, groups }`, beside today's `LocalOwner` and `RemoteClient`
(`sovereign/crates/sovereign-contracts/src/principal.rs`).

**The loopback trap.** Loopback is an authentication signal at roughly 400
sites across `sovereign-api` and `sovereign-daemon`, and `LocalOwner` is
derived from it: the owner's own chat, always admitted. Behind a proxy every
request arrives from loopback or the proxy's address, so a hosted daemon
under today's rule admits every SSO user as the owner. Decision: when an
issuer is configured, loopback grants nothing, the owner is a role claim, and
`LocalOwner` is reachable only in a process with no issuer. The test
`a_principal_key_is_never_derived_from_the_connection_address` already states
the rule; the arm has to obey it.

**Tenancy.** One daemon per organisation, one data directory, inside their
boundary. A second organisation is a second container. Groups within the
organisation are corpus grants: each corpus carries the group claims allowed
to read it, checked in one place at the retrieval boundary, so a corpus the
caller cannot read never enters a prompt and the absence is reported as a
verdict. Today's `enabled_corpora` (347 sites) is a per-conversation
preference, not an access control, and stays one.

**Deployment.** `sovereign/container/Containerfile` (ROCm base) plus a proxy
and a TLS terminator in one compose file, a GPU device, and two volumes:
models and data. Ingest is a mounted volume plus a recipe, or an acquirer
(`corpus-engine/src/acquirers/`: local file, HTTP API, Hugging Face, bulk
download). Egress such as web search is the daemon's job under the
organisation's proxy policy; the custody rule that kept it in the app no
longer applies because there is no app.

**What this deletes.** `sovereign-server` (7k lines, 26 routes re-implementing
conversations, corpora, documents, search and MCP for a tenant model, with
its own `TenantPrincipalResolver`) is a second host for one idea. The daemon
is the server. The resolver's job moves into the `Asserted` arm and the crate
goes.

**Sequence.** (1) the static route and `svrn open`; (2) the 226 `invoke`
sites to fetch and SSE, adding the routes that are missing; (3) `Asserted`,
JWKS validation, the loopback-grants-nothing mode, corpus grants as one
decider; (4) the compose file, deployed once inside a friendly
organisation's boundary as the journey that proves it; (5) delete
`sovereign-server`, attach mode, and the Tauri command layer. Each step is
usable on its own.

## 3. The verdict is a schema

The differentiator is that an answer carries its own checking. It is a type,
not a runtime:

```
Answer { text, claims: [Claim], verdict: Verdict }
Claim  { text, evidence: [Citation], verdict: Verdict }
Verdict = Supported | Unsupported | CouldNotJudge | Abstained
```

`Verdict`, `GroundingDecision`, `Citation` and `ClaimCitation` already exist
in `sovereign-core`. The design moves them to `sovereign-contracts` so every
client and the bench read one definition, and makes `Abstained` a value the
wire carries rather than a branch the runtime takes. Absence is reported,
never defaulted (ARCH principle 6).

## 4. Rules each program obeys

1. One data directory, one owner. A second process never opens it; it dials.
2. Dial, never embed. `EmbeddedDaemon` (constructed at
   `sovereign/crates/sovereign-cli-daemon/src/daemon_cmd/mod.rs:1221`) is the
   last in-process composition and it goes.
3. An unreachable peer program is `CouldNotJudge` or absent, never empty.
4. Every decision visible at `tracing=debug`.
5. Config that can change without a code change is a file, not a constant.
6. A program never links another program's crates. Shared types live in
   `oicp-types` and `sovereign-contracts` only.

## 5. What the design does not contain

Deleted, not migrated: the in-process daemon; the two-name state database;
`sovereign/SYSTEM_OVERVIEW.md` and the drift machinery that keeps it honest;
the work atlas, claims, orders, campaigns, cursors, journals and demos under
`.sovereign/features/`; the canon store; notes injection; the ten-context
registry `quality/DOMAINS.toml` and its census script; `sovereign-server` as a
second host (§2b; the phone and the browser dial `svrn`); the Tauri command
layer and attach mode (§2a); `studio` and `atos` unless a journey in
§7 reaches them. Each program gets one README under 500 lines. The gates that
survive are the ones with a fix command: size, boundary, layer, lock, env,
concept, and the two build scripts.

## 6. Measurements the design rests on (2026-09-17)

Cross-program crate edges, grouping today's crates by the five: assistant into
code intel 43, assistant into mesh 42, mesh into assistant 14, code intel into
assistant 10. Reference sites behind those edges: assistant runtime into code
intel 16 in 7 files; mesh into assistant 47 in 21 files; assistant api, daemon
and cli into commonwealth crates 463 in 63 files; `sovereign-tools` into
runtime and corpus 800 in 183 files, of which the `code/` module accounts for
187 runtime references and they are almost all the tool ABI (`Tool`,
`ToolContext`, `DeclaredTool`, `error`). `sovereign-tools` and
`sovereign-cli-llm` are each two programs in one crate. No coverage tooling is
installed. The bench does not gate conversation retrieval
(`sovereign/bench/README.md`).

## 7. Migration — the cut

Procedure, not plan. Every step has a command, a done condition and a stop
condition. Agents follow it; the operator closes the instrument table in step
3 and reads `kept-branches.tsv` in step 7. Nothing else is decided by an agent.

**Standing rules.** One cargo worker at a time, every cargo call through
`scripts/with-cargo-lock.sh`. No prose: code, a commit body in the fixed
format, nothing else. Subject `cut(<crate>): -<lines> <path>`; body exactly
`reached-by: none` and `restore: git show pre-cut:<path>`. A restore is by
name with the demanding instrument: `restore(<crate>): <fn> demanded-by
<instrument>`. No new capability until step 8 is green. Gate on exit codes.

**Step 0 — archive and clear the field.** `git tag pre-cut`. Delete
`sovereign/SYSTEM_OVERVIEW.md`, the drift tooling, and the process apparatus
listed in §5; drop `docs-gate` from `scripts/pre-push.sh` for the duration.
Done when the tag resolves and `./scripts/sovereign-lint.sh --human --full`
exits 0.

**Step 1 — toolchain.** `rustup component add llvm-tools-preview && cargo
install cargo-llvm-cov`. Done when `cargo llvm-cov --version` prints.

**Step 2 — instrumented build.**
```
source <(cargo llvm-cov show-env --export-prefix)
export LLVM_PROFILE_FILE="$PWD/target/llvm-cov-target/%p-%m.profraw"
cargo llvm-cov clean --workspace
scripts/with-cargo-lock.sh cargo build --workspace --bins --features corpus-engine/treesitter,sovereign-cli/dev-tools
```
Done when exit 0 and every `target/debug/sovereign-*` binary is newer than
the tag. Every step 3 command runs in this shell; a daemon started by launchd
inherits nothing and its run does not count.

**Step 3 — instruments.** The operator closes this table before the first
run; a surface with no row is being deleted on purpose and the tag message
says so. Cheap rows run twice and profiles are unioned. Done when every row
exited 0 and every bench lane reports `passed` or `failed`, not
`could-not-judge` or `never-ran`.

| Instrument | Command |
|---|---|
| bench, all lanes | `sovereign quality check` |
| conversation retrieval | a real journey, written before step 3 — the scaffold bank does not count |
| desktop chaos, kill-mid-turn included | `scripts/desktop-soak.py 30 --mode chaos` |
| desktop persona | `scripts/desktop-soak.py 30 --mode persona` |
| CLI journeys | `sovereign contract nightly` |
| pipeline, text | `sovereign pipeline run` over `sovereign-recipes/brothers-karamazov-book-1` |
| pipeline, code | `sovereign pipeline run` over the repo's own code recipe |
| mesh join | `scripts/mesh-soak.sh` |
| install and stale-lock start | `scripts/install-journey-nightly.sh`, then a start against a held run lock |
| migration | boot against a `pre-cut` data directory |
| browser client | the desktop chaos and persona soaks driven through `http://localhost:9741/` with no Tauri process |
| hosted | the same soak against the compose deployment through the proxy, as an `Asserted` principal in two groups with disjoint corpus grants |

**Step 4 — report and bar.**
```
cargo llvm-cov report --json --output-path target/cut/cov.json
scripts/cut-report.py target/cut/cov.json > target/cut/functions.tsv
```
`cut-report.py` (written here if absent) emits one row per first-party
function, `crate file function line count`, aggregated by file and line range
with the max count across instantiations, and one summary line
`unreached_lines / first_party_lines = <ratio>`. Pre-registered bar, decided
before the number is read: continue only at 0.30 or above. Below it, stop; §9
is the plan instead.

**Step 5 — module cut.** A file is cut when every function in it has count
0. Leaves first: `git rm <file>`, fix `mod` and `use` lines only, lint,
commit. A pre-commit check in cut mode refuses any commit whose added lines
are not `mod` or `use` edits. Waves by crate, largest first, on a `cut`
branch that is not pushed until the final step 8. Done when the list is empty
and `sovereign-lint.sh --human --full` exits 0.

**Step 6 — tests.** `./scripts/sovereign-test.sh --human`. A failing test
whose subject was cut is deleted. A failing test whose subject was kept names
a cut function; restore it by file, `git checkout pre-cut -- <file>`, re-cut
the file under step 5. Done when exit 0.

**Step 7 — function cut.** For each count-0 row in a kept file:
`callers(<fn>)`. No callers, or every caller count 0: delete. Any reached
caller: keep, append to `target/cut/kept-branches.tsv`. The operator reads
the top fifty by line count; the rest stay kept. One crate per agent, leaves
first. Done when every count-0 row is deleted or listed and both scripts
exit 0.

**Step 8 — re-verify.** Uninstrumented build, then all of step 3 again.
A lane that moved from `passed` names its functions; restore per the rule;
repeat. Done when every lane's verdict equals its step 3 verdict.

**Step 9 — boundaries. LANDED 2026-09-21; this is now the first step, not the
last.** In `quality/ARCH_LAYERS.toml`: five `[[package]]` rows, `svrn`,
`ingest`, `cmnwlth`, `code`, `bench`, replacing the six packages that were
there (`studio`, `code-intel`, `corpus-mcp`, `commonwealth`, `serving`,
`understanding`) — the validator refuses a crate in two packages, so the six
could not nest inside the five. Every `[[exception]]` scoped to a package was
deleted with them, eleven rows, and none was written in their place. Each red
line is one task: a trait in a shared leaf or a wire call, done when green.
Step done when `cd corpus-engine && cargo xtask boundary-gate` exits 0.

**Why it moved to the front.** Operator direction 2026-09-21, correcting nine
commits of incremental cutting on this branch: "I don't want the fuzzy grep and
chase stuff. I want the define statically the endstate, break everything red,
let the initiative be a return to green." The session being corrected had also
scored itself with a scratchpad Python script carrying its own crate-to-program
map and its own forbid matrix — a second implementation of the question this
gate answers, and a tunable one. The script is deleted; `boundary-gate`'s
violation count is the only burn-down number this initiative reports.

**Three departures from the paragraph above, each deliberate.** (1) The four
`[[forbid]]` rows are NOT added. Three of them — `cmnwlth -> svrn`,
`svrn -> cmnwlth`, `svrn -> code` — are already exactly what a `[[package]]`
row says, so spelling them again would be two implementations of one rule. The
fourth is real and not expressible today: `bench -> *` except `oicp-types` and
`sovereign-contracts` narrows the leaf budget for one package, and the
`[[package_leaf]]` set is global. The fix is a per-package leaf budget in
`quality/arch-layers/src/packages.rs`, not a hand-copy of the membership list.
(2) `sovereign-tools` and `sovereign-cli-llm` are claimed by `svrn` rather than
held back until their splits. Unclaimed, every edge INTO them from all five
programs would go red, which measures the absence of a decision rather than a
boundary; claimed, the edges OUT of them are the split task, and that is the
red the gate now shows. (3) The shared-leaf set is unchanged at ten. §4 rule 6
("shared types live in `oicp-types` and `sovereign-contracts` only") is a
tightening of the leaf list, a different dimension from the program partition,
and mixing the two would make the burn-down number unreadable.

**The number, measured on the commit that declared it.** `boundary-gate` fails
with **232** violations: 141 normal dependency escapes, 4 dev, 2 forbidden by a
`[[forbid]]` row that now reaches the package pass (`sovereign-mesh` and
`sovereign-mesh-test-harness` → `sovereign-daemon`, dev edges), and 85 from the
three filesystem rules a manifest cannot express — 2 `build.rs` (`corpus-engine`,
`sovereign-pods`), 33 `include_str!` escaping a crate root, 50 runtime
reach-outs (`CARGO_MANIFEST_DIR` climbs and `git` shelled with no
`current_dir`). By package: `svrn` 162, `code` 24, `ingest` 20, `cmnwlth` 17,
`bench` 7. By source crate, the dependency escapes concentrate:
`sovereign-daemon` 35, `sovereign-cli-llm` 26, `sovereign-cli-dev` 14,
`sovereign-cli-daemon` 11, `sovereign-cli` 10, `sovereign-mesh` 10,
`sovereign-tools` 9, `sovereign-code` 6.

§6 measured 118 cross-program crate edges under a four-way grouping; the
five-way partition plus the filesystem rules gives 232. The earlier figure was
not wrong so much as narrower — it counted normal dependency edges only, which
is the same undercount the deleted script carried.

**Step 10 — de-embed.** Fourteen construction sites of `EmbeddedDaemon`
outside `sovereign-daemon` become one dial through `sovereign-turn-client`.
Done when `grep -rn EmbeddedDaemon sovereign/crates --include=*.rs` returns
only the `cmnwlth` binary's own main.

**Step 11 — size lock.** `cargo xtask size-gate --tighten`, commit the
baseline, promote size-gate to blocking in `scripts/pre-push.sh`. Every push
after is net negative or its body names what the lines bought.

## 8. Predictions, written before step 4 runs

- The step 4 ratio is between 0.40 and 0.60. Below 0.30 kills the cut.
- The module cut alone removes over 300k first-party lines.
- Fewer than 40 restores are demanded by step 8 across all lanes.
- `sovereign-mesh` after the cut and split is under 25k lines, of which
  membership and reach are the majority.
- The `code/` lift needs no change to the twelve MCP tools it serves.
- The browser client reaches parity with the desktop soaks with fewer than
  20 routes added to the daemon.

A prediction that misses is recorded in this file with the number, and the
step it invalidates is re-decided by the operator, not the agent.

## 9. If the bar is not met

Steps 9 through 11 run alone. The 118 crate edges become red forbid rows and
are fixed where they fail. That is the boundary cut without the reachability
cut, and it is what the `serving` package has been doing since 2026-09-14.

## 10. Risks accepted

A user of a path no journey exercises will hit a missing feature; the answer
is a restore with their report as the demanding instrument. The Windows leg
is cut and restored when `cargo-xwin check` goes red. Unreached means not
exercised, not unnecessary; step 7's reached-caller rule and step 3's failure
journeys are the only protection, and they are named so nobody believes the
cut proved more than it did.

## 11. The burn-down, as a checklist

Written 2026-09-21 at `boundary-gate` **121** (from 232 at declaration, 199 at the
start of that day's session). Every number here was measured on the tree at
`f92667f5e`, not estimated. `boundary-gate 0` is necessary and **not**
sufficient — the three finish conditions are at the bottom.

**Read the remaining lines by TARGET, not by source.** Per-source the list says
`sovereign-daemon` 32, which reads like 32 problems; 18 of those are one
problem. By target, the 119 dependency edges are:

| Target family | Edges | Reached by |
|---|---|---|
| `corpus-engine` 13 + `-scip` 8 + `-notes` 8 + `-watchers` 3 | 32 | daemon 8, cli-llm 8, mesh 5, cli-daemon 5, tools 4, cli 4, core 3, … |
| cmnwlth (serving cluster + `commonwealth-*`) | 40 | daemon 18, cli-llm 11, cli-daemon 4, cli-dev 3, cli 2 |
| ingest orchestration (`enrichment-*`, gliner, recipe, pipeline) | 16 | spread |
| svrn crates, reached from outside | 16 | mesh's test harness, cli-dev, gliner |
| `code` + unassigned (`work-atlas`, `atos`) | 11 | daemon, cli-dev, cli-llm |
| bench (`authoring-harness`, `eval`, `tdd`) | 4 | daemon, cli-llm |

Plus two filesystem rules: `corpus-engine/build.rs` and
`sovereign-core/src/router_calibration.rs:1253`.

### The measurement that reprices half of it

`corpus-index` is **already a declared `[[package_leaf]]`**, and `corpus-engine`
shims eight modules into it: `error`, `corpus`, `stream_axes`, `filters`,
`types`, `index::*`, `chunkers::CommittedChunk`, and two `recipe` items.
`ScoredChunk`, `CorpusKind`, `IndexInfo`, `CorpusIndex`, `Corpus`, `Evidence`,
`EmbedFn` and `DEFAULT_EMBED_DIM` all live in the leaf. `sovereign-core` names
`corpus_engine::` 290 times and **195 of those resolve into the leaf**
(`ScoredChunk` 111, `index` 41, `CorpusKind` 17, `IndexInfo` 15, `Result` 11).
Two thirds of the turn path's apparent coupling to the ingest engine is a
spelling. Engine-owned and therefore real: `CorpusEngine`, `enrichment::*`,
`CorpusSpec`, `IngestResult`, `SovereignConfig`, `InferenceFn`, `ChatPrompt`,
`progress`, `update`, `snapshot`, the canonical-merge functions.

### Phase 1 — the corpus-index sweep (mechanical, 3 cutters, no decisions)

Do this FIRST, not for the edge count but because phases 3 and 6 cannot be
priced until it has run.

- [ ] Rewrite every `corpus_engine::<re-exported module>` path to `corpus_index::`
      across all consumers; drop the `corpus-engine` dep wherever the residue empties.
- [ ] Closes outright: `sovereign-cli` (4 refs), `sovereign-cli-shared` (7),
      `sovereign-code` (7, dev), `sovereign-cli-daemon` (7) — all mostly
      `Error`/`EmbedFn`/`DEFAULT_EMBED_DIM`.
- [ ] Report the per-crate residue for the five deep consumers: core 290,
      tools 339, cli-llm 396, daemon 135, mesh 85.

### Phase 2 — the recipes tree (mechanical, 1 cutter, 1 pass)

- [ ] `sovereign-recipes/` → `corpus-engine/recipes/`: **106 files, 1.7 MB,
      54 recipes, 145 files outside the tree citing the path.**
- [ ] Delete `corpus-engine/build.rs`; `include_str!` directly instead of via `OUT_DIR`.
- [ ] Closes the last rule-3a violation in the workspace.

### Phase 3 — the atlas read surface (the long pole; a campaign)

After phase 1 the residue across three packages is ONE module:
`corpus_engine::enrichment`, **~490 references** — tools 171, cli-llm 214,
core 39, corpus-mcp 16, meshapp 10.

- [ ] §2 already states the answer ("the atlas is written here and read there,
      through the index"), so the shape is an atlas READER in a leaf, the way
      `corpus-index` is the reader for the index.
- [ ] Decide: a new thin reader leaf, or widen `understanding-vocab`
      (already a leaf) / lift from `understanding-atlas` (an ingest member).

### Phase 4 — step 10, the de-embed (40 edges, the biggest single win)

- [ ] Fourteen `EmbeddedDaemon` construction sites become one dial.
      **Unblocked 2026-09-21:** `sovereign-turn-client` became a
      `[[package_leaf]]`, so the dial is now nameable from every package —
      before that, step 10 had nowhere to dial from.
- [ ] Sources: daemon 18, cli-llm 11, cli-daemon 4, cli-dev 3, cli 2.
- [ ] Done when `grep -rn EmbeddedDaemon sovereign/crates --include=*.rs`
      returns only the `cmnwlth` binary's own main.

### Phase 5 — the cli-llm split (23 edges; phase 4 unblocks it)

- [ ] 60.6k lines of bench, 44.7k of ingest, 18.8k of svrn in one crate.
- [ ] The blocker was always `chat_cmd/bootstrap.rs`'s `build_session`, which
      §2 says should be a dialled URL — after phase 4 it IS a dial, so the
      split stops needing a new mechanism and becomes `git mv` waves.

### Phase 6 — the ports (independent of each other; 3 cutters in parallel)

- [ ] NoteStore, 8 edges. `sovereign_contracts::recipe::notes::RecipeNotes` is
      the partial port that already exists. The blocker is the three
      `NoteStore::open` sites in `sovereign-tools`: construction moves up to
      whichever bootstrap already knows the data root.
- [ ] SCIP, 8 edges. Two cheap pieces first: `sovereign_cli_shared::scip`
      (109 lines) has **exactly one consumer workspace-wide** —
      `sovereign-cli-dev/src/project_cmd/mod.rs:410`, in the package that owns
      `corpus-engine-scip` — so moving the file there is free; and after that
      the entire remaining reason for `sovereign-cli-shared → corpus-engine-scip`
      is ONE call, `observation.rs:160 scip_export::all_exporters()`, a static
      `&'static [ScipExporterConfig]` table — data, not program (§9).
      Also: `sovereign-cli-{daemon,llm,mesh}` all enable `features = ["scip"]`
      and none of them uses the module.
- [ ] Watchers, 3 edges. Note deleting the two `pub use` shims in
      `sovereign-mesh/src/lib.rs` is **−1/+2** (13 consumers, and mesh's own
      test keeps the dev-dep) — it needs the reindexer's real owner, not a shim cut.
- [ ] Enrichment catalog + build, 10 edges. `DaemonInferenceClient`
      (32 consumer files) is THE dial and belongs in a leaf; blocked on four
      corpus-engine types it names: `ChatPrompt`, `error::{Error,Result}`,
      `EmbedFn`, `InferenceFn`.
- [ ] `sovereign-cli-shared` splits cleanly and the thin half is leaf-shaped:
      18 modules / ~3.3k lines / **zero escapes**, budget = `sovereign-contracts`
      + `sovereign-time` + `kernel-types`, all already leaves
      (`args cli_contract cli_contract_report deprecation dirs dispatcher
      flag_surface guest_link help host_load lane_verdict models project_toml
      prompts repo tracing_init urls mcp_client`). Promoting it would also close
      `[code] sovereign-cli-dev → sovereign-cli-shared`. Fat half, 4 modules /
      ~2.4k lines, spans three packages and belongs to none: `code_index` +
      `code_index_incremental`, `scip`, `observation`, `rail`.

### Phase 7 — decisions, not cuts

- [x] **`atos` is CUT COMPLETELY** (operator, 2026-09-21). Footprint:
      `sovereign-atos` 6,467 lines + `corpus-engine-atos` 2,850 +
      `sovereign-cli-dev/src/atos_cmd/` 8,256 = **17,573 lines**, plus the
      daemon's ATOS middleware chain (`middleware/`, `routes_inference.rs`,
      `routes_responses.rs`, `frontdoor.rs`, `state.rs`, `tool_registry.rs`),
      `sovereign-tools`'s `mcp_surface`/`lib`/`knowledge_view::strategic`
      surfaces, and `serving-policy`'s `default_pipelines.toml` pipeline aliases.
      Closes ~6 red lines: `cli-dev → {sovereign-atos, corpus-engine-atos,
      commonwealth-state}`, `daemon → {sovereign-atos, corpus-engine-atos}`,
      `cli-llm → corpus-engine-atos`.
      **Also DELETE the `[[package_leaf]] corpus-engine-atos` row in
      `quality/ARCH_LAYERS.toml`** — it was admitted 2026-09-21 and is the
      weakest of the four promotions from that day (a store, not vocabulary;
      it survived only because no program claimed ATOS).
- [ ] **`sovereign-work-atlas`** — what it is, since the question came up:
      2,344 lines, "coordination layer for agents sharing a mesh repo". Sessions
      + Claims over a `sovereign_contracts::peer::ReplicatedKv`, a TTL GC task
      the daemon spawns, and the three MCP tools `declare_scope` /
      `release_scope` / `work_in_flight` that `AGENTS.md` makes a pre-flight
      ("is anyone else on the mesh touching this?"). Phase 1 of a v0.1 spec —
      Observations are not implemented. Consumers: `sovereign-cli-dev` 4 files,
      `sovereign-daemon` 3, `sovereign-code` 1.
      **It is agent-facing, which makes it `code`'s** by the same argument that
      puts `symbols`/`callers` there. Blocked on its own deps: `sovereign-core`
      (svrn — may be re-export-only after phase 1) and `commonwealth-state`
      (dev, cmnwlth). Decide after phase 1 re-measures it. The live alternative
      is that nothing on this mesh has more than one agent at a time and the
      crate is inventory, in which case it joins atos.
- [ ] **`corpus-mcp`'s membership.** §2's table puts it in svrn; its 32
      `corpus-engine` sites are `corpus ingest` + atlas reads, which are
      ingest's work. Its own manifest defends the `sovereign-enrichment-build`
      edge: "without it a person needs our daemon to build a corpus, and the
      binary's whole claim is that they do not."
- [ ] **`bench`'s leaf budget.** The one `[[forbid]]` row §9 admits it cannot
      express — `bench -> *` except `oicp-types` and `sovereign-contracts` —
      needs a per-package leaf budget in `quality/arch-layers/src/packages.rs`,
      not a hand-copied membership list.
- [ ] **`sovereign-cli → commonwealth-{work,rail}`, priced and refused twice.**
      `oicp-types` cannot serve `quality_check_cmd/distribute.rs` (1,502 lines):
      it needs 15 names that are cmnwlth's work MODEL, fold and refusal decider
      (`Submission`, `WorkAct`, `ProcessPayload`, `may_take`, `WORK_NAMESPACE`,
      `RailAct`, …), and pushing those into a leaf widens every package — a
      bigger violation than the one it closes. Moving the verb to
      `sovereign-cli-dev` buys two new red lines for two; to `sovereign-cli-llm`
      forks the 3,885-line verdict roll-up. **Recommended: the wire boundary.**
      `sovereign-daemon` is already red on both, is already the work donor
      (`src/work_donor.rs`), already mounts `/v1/rail/{log,append,live}`, and
      `sovereign-cli` already dials loopback elsewhere to avoid a link. A
      submitter route plus `daemon_wire` types closes BOTH lines at zero new ones.
- [ ] **`sovereign-cli → sovereign-cli-dev`: keep the link.** Measured on the
      Halo: `sovereign-cli-dev` 576 MB, `sovereign-cli` 507 MB. Exec'ing instead
      closes the line and re-acquires exactly the failure `AGENTS.md` names —
      a rebuild of the dispatcher alone silently execs a stale sibling — on
      `code converge noun`, which `AGENTS.md` makes a MANDATORY pre-flight
      before minting any type. `sibling::warn_if_stale` softens it, it does not
      fix it. One red line is cheaper than a stale mandatory gate; grandfather
      it if the count matters.

### Phase 8 — singletons

- [ ] `sovereign-core/src/router_calibration.rs:1253` embeds
      `bench/routing/calibration/axes_v1.toml`. Moving the bank breaks
      `router fit`'s `DEFAULT_BANK_DIR` `read_dir` plus two committed baselines
      and a python fitter; moving the check to `xtask` means a second
      `parse_bank` (§8). Third option: relocate the whole routing-calibration
      corpus into `sovereign-core/data/calibration/` and repoint
      `DEFAULT_BANK_DIR` — `baseline_dir_for_bank` keeps yielding
      `calibration-fit/` if the directory keeps the name `calibration`.
- [ ] `sovereign-code → corpus-engine` (dev) is `tests/e2e_code_intel.rs`
      building a real `CorpusEngine` and ingesting a recipe. Faking it makes the
      e2e vacuous, and §18.1 says that is worse than the red line. It needs a
      package decision, not a cut.
- [ ] Two `ScoredChunk` structs: `sovereign-contracts::types`
      (`{chunk: DocumentChunk, score}`, deliberately not serializable) and
      `corpus-index::types` (flattened, serializable). Possibly a deliberate
      wire/in-process split; name it either way (§8).
- [ ] `sovereign-cli → corpus-engine` via `project_init` is NOT a fork of
      `cli_shared::code_index::rebuild_code_corpus`: init deliberately does
      FTS-only with a zero-vector `EmbedFn` so a `curl | sh` install can index
      its own repo with no daemon. Collapsing them needs a daemon at init (a
      regression) or a zero-vector fallback in `code_index` (§18.3 substitution).
      Closing condition: route init's index step through the daemon, as
      `project register` already does.

### Dependency order

Phase 1 before everything — it reprices 3 and 6. Phase 4 before 5. Phases 2, 6,
7 and 8 are independent. Phase 3 is the long pole and should not start until
phase 1 has re-measured it.

### The risk this checklist has to hold

The shared-leaf set went from **10 at declaration to 15 on 2026-09-21**
(`sovereign-turn-client`, `sovereign-workflow`, `corpus-engine-atos` — that last
one now deleted with atos — plus two already in flight). Leaves are 21% of the
governed set, and every promotion widens all five packages at once. **A path to
zero that promotes another twenty leaves reaches a number that means nothing.**
The test, applied on the day and to be applied again: a leaf is shared
VOCABULARY or a thin reader with a one-or-two-crate in-repo budget, never a
store a program owns on disk. That is why `corpus-engine-notes` was NOT
promoted even though one row would have closed eight edges.

### Done is three conditions, not one

- [ ] `cd corpus-engine && cargo xtask boundary-gate` exits 0.
- [ ] `grep -rn EmbeddedDaemon sovereign/crates --include=*.rs` returns only the
      `cmnwlth` binary's own main (step 10).
- [ ] `bench`'s per-package leaf budget exists and the row is expressed
      (phase 7), so the evaluator cannot link the thing it measures.
