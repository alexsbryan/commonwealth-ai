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

## 11. The method, and the standing worklist

Written 2026-09-21, at `boundary-gate` **119** (from 232 at declaration, 199 at the
start of that day's session). Every number here was measured on the tree at
`f92667f5e`, not estimated. `boundary-gate 0` is necessary and **not**
sufficient — the three finish conditions are at the bottom.

### The method — this is the part to follow

Operator direction 2026-09-21, and the correction that produced this section:
**do not run this as phases.** A phase plan invents an order the evidence already
carries, and it licenses "we are in phase 3, so we cannot touch that" — which is
the fuzzy plan-and-chase this initiative replaced. The loop below is what has
actually been moving the number; the worklist after it is evidence, not a
schedule.

1. **Measure.** `cd corpus-engine && cargo xtask boundary-gate` (inside the
   toolbox). Group the output BY TARGET, not by source — per-source it reads as
   N problems when it is often one.
2. **Fan out three cutters on disjoint crate paths.** Each is told: never run
   `cargo`, never touch git state, edit only inside your paths, and report what
   you changed plus the NAMED SEAM for what you could not. Read-only search
   agents map a crate before a cutter touches it.
3. **This session holds the single compile and every commit.** Cutters never
   build; the orchestrator runs `sovereign-lint.sh --human --full`, fixes the
   integration errors, and commits.
4. **A cutter that cannot close a line cleanly reports the blocker instead.**
   Half a port that does not compile is worse than a smaller honest cut, and a
   line closed by teaching something to skip is worse than the line.
5. **Repeat.** Each pass re-measures, so the ordering falls out of what the last
   pass learned rather than out of a plan written before it.

Only two things gate on order, and both are facts rather than policy: the
corpus-index sweep comes first because nothing else can be priced until it has
run, and the cli-llm split waits on the de-embed because its blocker is the dial
the de-embed creates. Everything else can be picked up whenever a cutter is free.

**Decisions are meant to be rare.** The whole list of them so far is: cut atos,
place `sovereign-work-atlas` or delete it, `corpus-mcp`'s membership, `bench`'s
leaf budget, and the three trades already priced and refused below. If a session
finds itself making a sixth, that is the signal something is being decided that
should have been measured.

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

### The corpus-index sweep — DONE 2026-09-21, two cutter waves + rule-gap passes

Run not for the edge count but because the atlas and de-embed items could not
be priced until it ran. It closed **zero** edges — every crate with leaf
spellings also has engine-owned production refs, so no `corpus-engine` dep line
fell. The original prediction is kept struck-through as the lesson:

- [x] Rewrite every `corpus_engine::<re-exported module>` path to `corpus_index::`
      across all consumers; drop the `corpus-engine` dep wherever the residue empties.
- [x] ~~Closes outright: `sovereign-cli` (4 refs), `sovereign-cli-shared` (7),
      `sovereign-code` (7, dev), `sovereign-cli-daemon` (7)~~ — those counts
      were sibling-crate noise: the grep matched `corpus_engine_{notes,scip,
      watchers,archaeology,atos}`, different crates entirely. Real counts:
      cli 5, cli-shared 8, code 16, cli-daemon 13, and the residue in each is
      engine-owned (`CorpusEngine`, `CorpusSpec`, `SovereignConfig`).
- [x] Per-crate residue AFTER the sweep: core 70, tools 294, cli-llm 386,
      daemon 224, mesh 94 — dominated by `enrichment` (~490 refs across all
      consumers) and `CorpusEngine`. `corpus-index` is now a declared dep of
      14 crates; the engine-root `oplog`/`Grain`/`Articulation` indirections
      are closed at their sites via direct deps on shared leaves (no new
      violations).

Seams the sweep learned, by name:

- `corpus_engine::index` is NOT uniformly leaf — the engine kept its own
  `index/field_skeleton.rs` and `index/raptor.rs` beside the
  `pub use corpus_index::index::*` shim (tools, daemon, cli-llm each reach
  them; they are enrichment machinery and belong to the atlas question).
- `InferenceFn` is re-exported from `types` but is ENGINE-owned — a naive
  whole-module sweep of `types` would have broken it.
- The long tail was root spellings: `CorpusIndex` (~20), `Evidence`/
  `EvidenceSet`, `InsertChunk`/`StoredChunk`, `EnrichmentChunkRow` (9),
  `read_provenance`/`set_provenance`/`CorpusProvenance`, `Retention` — all
  closed to `corpus_index::index::*`.

### The recipes tree — mechanical, one cutter, one pass

- [ ] `sovereign-recipes/` → `corpus-engine/recipes/`: **106 files, 1.7 MB,
      54 recipes, 145 files outside the tree citing the path.**
- [ ] Delete `corpus-engine/build.rs`; `include_str!` directly instead of via `OUT_DIR`.
- [ ] Closes the last rule-3a violation in the workspace.

### The atlas read surface — the long pole

After phase 1 the residue across three packages is ONE module:
`corpus_engine::enrichment`, **~490 references** — tools 171, cli-llm 214,
core 39, corpus-mcp 16, meshapp 10.

- [ ] §2 already states the answer ("the atlas is written here and read there,
      through the index"), so the shape is an atlas READER in a leaf, the way
      `corpus-index` is the reader for the index.
- [ ] Decide: a new thin reader leaf, or widen `understanding-vocab`
      (already a leaf) / lift from `understanding-atlas` (an ingest member).

### Step 10, the de-embed — PARTIAL 2026-09-21; AND A CORRECTION TO THIS SECTION

**The correction, first.** This section's closing line below used to say the
endstate greps to "only the `cmnwlth` binary's own main". That is WRONG and it
cost a session: `sovereign-daemon` is §2's knowledge server and belongs to
`svrn` — the table says svrn "exists today as `corpus-mcp/` + the turn path in
`sovereign-core`", and §7 step 9's own words ("`cmnwlth -> svrn` … [is]
already exactly what a `[[package]]` row says") only hold if the daemon is in
`svrn`. A session read the old line, moved `sovereign-daemon` into `[cmnwlth]`
in ARCH_LAYERS.toml, and the gate fell 117 → 102. That 15-edge "gain" was
fake: it hid the rule-6 work (a program never links another's crates — the
serving cluster must be DIALED, never absorbed) behind a re-homing. Reverted
in the next commit; the honest number was **115**. The lesson is the one §11
already preaches: a zero reached by promotion is fake. Read §2 before moving a
crate between programs; the daemon is svrn's host, `cw-rails` is cmnwlth's.

**What actually landed.** Scouts found only TWO production construction sites
(the "fourteen" was edge counts, not sites): `cli-daemon daemon_cmd/mod.rs` (the
`daemon run` verb) and `setup_cmd/terminal.rs` (wizard MeshAdmin one-shot),
plus two cli-mesh fallbacks.

- [x] `sovereign-daemon` gained its own `[[bin]]` — the svrn host's entry
      point, with the run body forked from cli-daemon's daemon_cmd.
- [x] `svrn daemon run` execs the sibling (agent-bench pattern,
      `SOVEREIGN_DAEMON_BIN` override; pid preserved through exec(2)).
- [x] cli-mesh create/join fallbacks dial: `ServingHost::ensure_reachable` +
      `TurnClient` mesh create/join over HTTP; no bundled spawn.
- [x] cli-daemon dropped 8 emptied serving deps + 4 pre-existing zeros
      (genuine closures: `cli-daemon -> {sovereign-runtime-recipe,
      corpus-engine-notes, corpus-engine-watchers}`).
- [ ] Done when `grep -rn EmbeddedDaemon sovereign/crates --include=*.rs`
      returns only the svrn daemon binary's own main (NOT cmnwlth's — see the
      correction).
- [ ] **terminal.rs wizard seam**: `find_holders` needs
      `peer_inference_endpoints()` (roster + TrafficClass::Inference rewrite);
      no HTTP equivalent — `/v1/mesh/status` `MemberDto.addresses` would dial
      the wrong port class. Until the daemon exposes it, the wizard keeps its
      in-process construction (cli-daemon → sovereign-daemon edge).
- [ ] **cli-llm wire types**: 4 uses (`corpus_watch_http::RegisterRequest` —
      blocked on `WatchedFolderConfig`'s home — and `reading_http::{AtomCard,
      AtomSpan, SectionRef}`, clean DTOs) belong in `contracts::daemon_wire`;
      until they move, cli-llm → sovereign-daemon is red.
- [ ] **mesh test tree**: ~60 `EmbeddedDaemon::new` sites in
      `sovereign-mesh/tests/**` are the parked daemon tests; moving them to
      `sovereign-daemon/tests` also closes the two `[[forbid]]` dev edges.
      Census tests hard-coding construction lists (mesh
      `daemon_variant_census.rs:214`, desktop `attach_construction_census`)
      must move/update with them.

### The leaf lever — MEASURED 2026-09-21 (103 violations at e8fee31a6)

The gate counts ONE edge per (source-crate, target-crate). Promoting a target
to `[[package_leaf]]` closes every inbound edge at once — but only if the crate
is honestly a leaf (§11's rule: shared vocabulary or a thin reader, never a
store a program owns on disk; a zero reached by promotion is fake). Eligibility
of every non-leaf target, by "are all its workspace deps leaves/crates.io":

| target | inbound edges | leaf-eligible? | verdict |
|---|---|---|---|
| `corpus-engine-scip` | **7** | deps all external | closest — but it READS (`ScipGraph::open`, `build_symbol_trace`) and EXPORTS (`scip_export`) the SCIP index, and §2 gives that index to `svrn code`. Fix = split: a thin reader leaf + the exporter stays [code]; repoint the reader-shaped uses (core, mesh, daemon `routes_edit_predictions.rs:596`). |
| `sovereign-pods` | 2 | deps all external | program logic (pod provisioning) — promotion would be fake |
| `commonwealth-core` | 2 | deps all external | cmnwlth substrate (NodeId …) — plausible vocabulary, but it is the program's own crate |
| `sovereign-scheduler` | 1 | deps all external | "arithmetic over the published language" — logic |
| `serving-policy` | 1 | deps all external | "the admission decider" — logic |
| `sovereign-peer-wire` | 1 | needs `commonwealth-rail` | blocked on the rail-core wire types below |
| `corpus-engine` 13, `sovereign-mesh` 4, `sovereign-core` 4, `sovereign-daemon` 4, `sovereign-tools` 3, `sovereign-store` 3, `sovereign-enrichment-*` 5+3, `sovereign-inference` 4 | — | no | real program crates: dial, trait, or split |

The recurring seam list from the cheap-edge pass (each is one task, file:line in
the commit at `e8fee31a6`): `ScipGraph` (4 sources — the one shared home closes
core+mesh+daemon+cli-shared), `SovereignConfig`/`WatchersConfig`/`RunnerConfig`
(3 sources: cli-daemon `checks_sovereign.rs:662`, cli-dev `tools_cmd/registry.rs:327`,
daemon `bootstrap.rs:2582`), the rail wire types (`RailAct`/`Admission`/`Roster`/`Payload`,
cli-shared `rail.rs:36`), the drift fingerprint codec (`sovereign-code`, cli-llm +
cli-dev `drift_cmd_orchestrator.rs:617`), `plan_schema` (core→inference dev, move to
`sovereign-contracts`), `guest_route::open_route` (a security decider — KEEP the edge),
`enrich_cmd::paths`/`inference_client` (already leaf re-exports — repoint the consumers),
the enrichment catalog reader (`list_enriched_corpora_in` — port trait in contracts),
the authoring-harness drive (`run_over_frozen_sample` — bench host or leaf).

### The scip reader split — the next big lever, spec'd (100 violations at 28fc22ff4)

`corpus-engine-scip` carries **7 inbound red edges** (`corpus-engine` [ingest];
`cli`, `cli-daemon`, `cli-shared`, `core`, `daemon`×2 [svrn]) and all-external
deps, but it both READS (`ScipGraph::open/open_with_integrity`, `ScipSymbolRecord`,
`Caller`, `ScipGraphStats`, `build_symbol_trace`, `render_trace`) and WRITES
(`scip_export::{export_all, check_exporters}`, `lsp_tier`, `tool_path`) the SCIP
index §2 gives to `svrn code`. Promotion would be the fake zero — the writer
owns the program's data. The split:

- READER cluster → a new leaf: `scip_graph.rs` (3,691 lines), `scip_graph_edges.rs`,
  `scip_proto.rs`, `trace.rs`, `error.rs`. Name it with `sovereign code converge
  noun <Name> --corpus-id commonwealth-ai` first (candidates: `scip-read`,
  `scip-graph`); add the `[[package_leaf]]` row + root `Cargo.toml` member and
  `[workspace.dependencies]` entry (orchestrator's edits, not a cutter's).
- `corpus-engine-scip` re-exports the reader at its old paths, so every [code]-package
  user (`sovereign-code`, `code-facts`, `code-next-edit`, `corpus-engine-watchers`,
  `cli-dev`) is untouched.
- Repoint the READER-shaped cross-package uses: `sovereign-core/src/runtime/code_trace.rs:38`
  (`build_symbol_trace`, `render_trace`, `ScipGraph`), `sovereign-daemon/src/project_http.rs:300`
  + `routes_edit_predictions.rs:596` + `tests/main/next_edit_symbol_lane_e2e.rs:21`,
  `sovereign-cli-shared/src/scip.rs:17`. Expected: −4 edges (core, daemon×2, cli-shared).
- WRITER-shaped cross-package uses are seams needing an operator decision, not a
  repoint: `sovereign-cli-shared/src/observation.rs:26` (`scip_export` — does a
  non-code host write the code index?), `corpus-engine` ([ingest] exporting a SCIP
  index — one owner per data dir says no), `sovereign-cli` (find its site). Each is
  a dial to the code program or a dropped feature.

### The cli-llm split — 24 edges; the partition is MEASURED (scouts, 2026-09-21)

- [x] Inventory + dep matrix + dispatch shape measured (3 read-only scouts at
      3a59e08da). Three groups, sized: **bench 60,519 lines** (bench_cmd,
      eval_cmd, inner_chaos, voice_eval, search_gym_cmd, knowledge_gym_cmd,
      gym_judge, quality_lane_cmd), **ingest 45,600** (enrich_cmd, corpus_cmd +
      5 corpus_* files + corpus_resolve, atlas_cmd, meta_atlas_cmd,
      pipeline_cmd, recipe_cmd/{.rs,/}, recipe_agent_cmd, recipe_agent_live_trial,
      workflow_cmd, worker_pod_provider, alignment_cmd), **svrn 16,975**
      (chat_cmd, awareness_cmd, mcp_cmd, mcp_demo_server, govern_cmd, turn_sink,
      newsworthy, mobile, reading_diag, proxy, portfolio, router_*, lib/main).
- [ ] Mechanically: each moving group becomes `[lib] + [[bin]]` (the
      `sovereign-agent-bench` precedent), and the DISPATCHER (sovereign-cli,
      `main.rs:877` and `:1204`) get a sibling exec module (`bench_bin::exec`,
      `ingest_bin::exec`) — the per-cluster `llm_bin`/`dev_bin` pattern. Do NOT
      make bin-only: the moving trees link each other and staying trees link
      moving helpers (below).
- [ ] Entry points a new crate re-exposes: `run_bench(&[String]) -> i32`
      (bench_cmd/mod.rs:173), `run_enrich` (enrich_cmd/mod.rs:188),
      `run_corpus` (corpus_cmd/mod.rs:38), `run_govern` (govern_cmd/mod.rs:65).
- [ ] The seams that block a clean cut, by name: `chat_cmd::bootstrap`
      (`build_session`, `ChatSession`, `build_inference`) + `chat_cmd::config`
      (`parse_globals`) are used by ALL THREE groups (32 files) — the heaviest
      seam; `eval_cmd::{bank,runner}` used by ingest+bench; `bench_cmd →
      enrich_cmd` (atlas.rs:40, all.rs:31, adjudicate.rs:28, obsidian.rs:278,
      scaffold.rs:22, governance.rs:212) forces bench-crate → ingest-crate;
      `bench_cmd → govern_cmd::ask` (governance.rs:119); staying→moving helpers
      that are re-exports of leaves (`enrich_cmd::paths` →
      `sovereign-enrichment-catalog::paths`; `enrich_cmd::inference_client` →
      `sovereign-enrichment-build`) repoint, they do not move.
- [ ] Deps that die with the split: `sovereign-eval`, `sovereign-gliner`,
      `oplog`, `sovereign-code`; six declared deps already have ZERO refs
      (`commonwealth-{media,rail,transport}`, `oicp-types`, `sovereign-meshapp`,
      `sovereign-work-atlas`) — drop them first, that is free.
- [ ] Verification: `boundary-gate`'s own count line is the only burn-down
      number; `scripts/evidence-verdict.py <commit>` for any test-evidence
      claim. Commit bodies quote the script output, never an interpretation.

### The ports — independent of each other

- [x] NoteStore — **the port is BUILT** (2026-09-21). `sovereign-contracts/src/notes.rs`
      declares `trait AgentNotes: RecipeNotes` — the supertrait means
      `write_note_full`/`read_notes_scoped` keep ONE declaration — plus eight
      methods (`read_notes`, `read_notes_by_related_entity`,
      `has_active_note_with_content`, `write_note_with_source`,
      `write_note_with_relation`, `update_note_payload`, `log_tool_call`,
      `tool_call_log_rows`) and one new DTO, `ToolCallLogRow`. The DTOs are the
      existing `contracts::recipe::notes::{Note, NoteScope, NoteSource,
      ScopeFilter}`, reused rather than re-declared. The single implementation
      lives in the OWNING crate — `corpus-engine-notes/src/port.rs`, with five
      drift tests that walk `NoteScope::ALL`/`NoteSource::ALL` and cross every
      field — which let `sovereign-tools/src/recipe_notes_adapter.rs` (275 lines,
      an orphan-rule newtype) be deleted outright: from inside the owning crate
      the type is local, so the newtype had no reason to exist and leaving it
      would have been a second `RecipeNotes` impl over one store.
      Closed: `[ingest] sovereign-runtime-recipe → corpus-engine-notes`.
      `sovereign_core::{memory, dossier, lessons}` and `Runtime`/`RuntimeParts`
      now take `&dyn AgentNotes` / `Option<Arc<dyn AgentNotes>>`.
- [ ] The five NoteStore edges that remain are NOT more porting. Every method
      each crate calls is already on the port; what is left is **construction and
      three decisions**:
      - `sovereign-core` — production is 100% on the port. Three TEST sites keep
        the edge (`src/lessons.rs:1061,1063`, `src/memory.rs:2342,2345`,
        `tests/main/core_tests.rs:2279`). Faking them swaps real-SQL proof for a
        green gate (§5). Decide where those tests live.
      - `sovereign-tools` — blocked on `knowledge_view/manager.rs:829`
        `NoteStore::open`: construction must move into
        `KnowledgeViewManager::new`'s caller, and one caller is
        `sovereign-mesh/tests/main/landscape_digest_http_e2e.rs:66` — i.e. it is
        gated behind the mesh test-tree move. Also `src/notes/mod.rs:37`
        `pub use corpus_engine_notes::response_mine;`, a module re-export with
        its own downstream importers, which needs a home decision.
      - `sovereign-cli-daemon` and `sovereign-cli-llm` — pure construction
        (`NoteStore::open` at `daemon_cmd/mod.rs:538`,
        `awareness_cmd/store_open.rs:19,86`, `chat_cmd/bootstrap.rs:276`,
        `recipe_agent_cmd.rs:286`, `recipe_agent_live_trial.rs:1279`). Both need
        the opener reachable without naming the crate, i.e. a factory in the
        `code` package (`sovereign_code::open_agent_notes(path)`), which trades
        these two edges for `svrn → code`. A boundary decision, not a refactor.
      - `sovereign-daemon` — the widest and the RIGHT owner of construction:
        16 store methods plus eight types the port deliberately does not carry
        (`EmbedFn`, `GlinerFn`, `PropagationSinkFn`, `NodeRoster`, `RosterEntry`,
        `NotePropagationEvent`, `ProjectDocsStore`, and the `decision_extractor`
        middleware re-export at `src/middleware/mod.rs:53`).
- [ ] `corpus_engine_notes::Note` still mirrors the contracts `Note`, and that is
      NOT closable by a move: unifying means repointing the whole `code` package
      (`notes.rs` 7,800 lines, `sovereign-code` 15 files, `sovereign-cli-dev` 20)
      and closes no red line. `port.rs`'s drift tests are the guard instead.
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

### The mesh test tree — diagnosed 2026-09-21, not a port job

`sovereign-mesh`'s six DEV lines — `→ sovereign-daemon` (forbidden), `→ sovereign-store`,
`→ sovereign-tools`, `→ corpus-engine-scip`, `→ sovereign-enrichment-catalog`, and
`sovereign-mesh-test-harness → sovereign-daemon` (forbidden) — have **zero `src/`
sites between them.** They are one fact, and it is not an inversion problem:
**the test tree for code that already moved to `sovereign-daemon` (`dm-daemon-mesh-edge`)
did not move with it.**

- 77 of 85 files under `sovereign-mesh/tests/main/` name daemon-owned symbols
  (`EmbeddedDaemon`, `AppState`, `assemble`, `client_router`/`internal_router`,
  14 `*_router` surfaces) — 266 sites.
- **31 of those files, ≈15.9k lines including the 952-line `common/mod.rs`
  fixture, name `sovereign_daemon` and never name `sovereign_mesh` at all.**
  They are daemon tests parked in mesh's tree: `atlas_surface_e2e`,
  `conv_surface_e2e`, `corpus_watch_http_e2e`, `d6/d8/d9a_*/d9_turn_extras`,
  `daemon_variant_census`, `enrich_surface_e2e`, `fold_ingest_{abandoned,coverage}`,
  `iroh_transport_e2e`, `landscape_digest_http_e2e`, `lc_surface_e2e`,
  `local_only_boot`, `meshapp_{parcels,surface}`, `node_id_persistence`,
  `pattern_observation_e2e`, `port_config`, `reading_http_e2e`,
  `recipe_surface_e2e`, `research_surface_e2e`, `rotate_pre_split_guard`,
  `spec_gate_e2e`, `storage_snapshot_e2e`, `try_resume_first_gossip`,
  `turn_surface`, `wire_view_drift`, `common/mod.rs`. 46 more name both.

- [ ] Move the 31 pure-daemon files to `sovereign-daemon/tests/`. That is the
      whole of the two `[[forbid]]` lines and most of the other four.
- [ ] `sovereign-tools`, 21 sites / 10 files, all inside the pure-daemon set.
      Four of them are **wire types** deserialised out of HTTP answers
      (`atlas_view::{AtlasCorpusSummary, AtomListPage, AtomDetail,
      AtlasMemberSummary}`, `atlas_surface_e2e.rs:354,381,396,440,476`) and
      `sovereign-contracts::daemon_wire` exists for exactly that case by its own
      stated purpose — a client that only parses an answer should not link the
      serving host to name it.
- [ ] `sovereign-store`, 21 sites / 8 files: `memory::InMemoryStateStore` (13),
      `sqlite::SqliteStateStore` (5). **Do not fake these.** `StateStore` is a
      supertrait of 12 sub-traits, and `InMemoryStateStore` already IS the
      in-memory reference implementation — a harness fake would be a
      thirteenth-trait second copy of it (§8).
- [ ] `corpus-engine-scip`, **exactly one site** (`loopback_parity.rs:73`),
      forced by a signature mesh re-exports:
      `Reindexer::new(PathBuf, ScipGraphHandle)` where
      `ScipGraphHandle = Arc<ArcSwap<ScipGraph>>` and only
      `ScipGraph::open_in_memory` can mint one. Closes with an in-memory
      constructor on `Reindexer` in `corpus-engine-watchers`.
- [ ] `sovereign-enrichment-catalog`, **one site** (`enrich_surface_e2e.rs:81`):
      `CONFIG_SCHEMA_VERSION` written into a fixture `config.json`. A literal
      there is a second copy of a one-decider version and breaks on the next
      legitimate bump. Closes by moving the constant (with `EnrichConfig`) into
      contracts — the same argument that moved `guest_link`.
- [ ] `sovereign-mesh-test-harness → sovereign-daemon` is one file,
      `tests/integration.rs` (831 lines), binding `SimulatedMesh<AppState>` and
      driving the real routers over TCP against 200/400/503 plus JSON bodies.
      No fake holds that. Its real home is `sovereign-daemon/tests/`, which puts
      `sovereign-daemon → sovereign-mesh-test-harness` on the table as a new dev
      edge. Moving it to `sovereign-mesh/tests/` instead would gate it out behind
      the optional `dst` feature — a test taught to skip, refused.
- [ ] `sovereign-mesh → corpus-engine` is a genuine SRC edge, 8 sites:
      `canonical_pull.rs:46,47`, `gossip.rs:46`, `capabilities.rs:43,269,283,292`,
      `ring_roster.rs:260`. 96 more in tests. `CorpusEngine` is spelled two ways
      (`corpus_engine::` and `corpus_engine::engine::`).
- [ ] `sovereign-mesh → corpus-engine-notes` is ALREADY dev-only in fact — 0 src
      sites, 7 test sites — but declared in `[dependencies]`, so the gate reads
      it as normal. One line in mesh's manifest.

**Do not fake any of these to make the number move.** The assembled-host e2e
tests that cannot be inverted without going vacuous, by name: `turn_surface.rs`
(1,636 lines, real turns over a live socket with a real store),
`loopback_parity.rs` (asserts the mounted router and the in-process call agree —
a fake on either side IS the subject), `daemon_variant_census.rs` plus
`common/mod.rs::{desktop_services, mesh_admin_services}` (they drive
`sovereign_daemon::assemble` deliberately, because a fixture composing a variant
directly is the one site able to build a shape no launch can produce — the
comments name this "Falsifier 3"), `d9a_documents_e2e.rs` and
`conv_surface_e2e.rs` (SQLite rows and conv-tiered projections),
`enrich_surface_e2e.rs`.

**A dev edge still counts.** `quality/arch-layers/src/packages.rs:171`, pinned by
its test at `:549`, enforces dev edges in the package pass — so demoting a dep
from `[dependencies]` to `[dev-dependencies]` makes the line re-read as DEV
rather than closing it. It is still worth doing: it removes the edge from the
shipped closure, which is what liftability actually means.

### Seams with prerequisite chains — MEASURED 2026-09-21 (87 at 1c307943c)

Each of these was probed this session and REFUSED rather than forced; the
refusal named the chain, which is the value.

- **The enrichment-catalog reader** — SOLVED 2026-09-21 for the daemon edge,
  and the first prescription was WRONG. Moving `EnrichConfig` to contracts (the
  chain printed here originally) drags `understanding-vocab` below the contract
  seam, and layer-gate then reports that the thin surfaces transitively link a
  backend — a real violation, so that attempt was reverted. The honest shape,
  landed: a SLIM projection in `contracts::daemon_wire::enrich_catalog` that
  serde-reads only the four fields the wire shape renders, skips unloadable
  configs, and enforces `schema_version` with the ONE const (moved down; the
  catalog re-exports it). `daemon -> catalog` closed (82). `cli-llm -> catalog`
  REMAINS: its refs are the writer-side config (`save`/`ontology`), so it stays
  until the writer half gets a dial or the cli-llm split moves it.
- **`SqliteStateStore` ports** (`sovereign-store`, 3 edges: cli-dev, gliner, and
  mesh's dev edge): the concrete type is `sovereign-store/src/sqlite.rs:41` and
  `sovereign-store` itself deps `sovereign-core`, so it is not leaf-eligible.
  Shape: a `StateStore` trait in `sovereign-contracts` mirroring the existing
  `RecipeNotes` port, the impl staying in the owner, and construction moving to
  the composition root (a caller that receives the store rather than opening it).
- **`sovereign-daemon`'s 30 edges** are the dial program in one place: ~8 to
  `commonwealth-*` (the mesh membership the daemon implements — §4 rule 6 says
  it dials the cmnwlth rails process instead), ~5 to `corpus-engine*`, plus
  code/ingest crates reached by its routes. Each is a wire call or a trait in a
  leaf; none is a repoint (a repoint from [svrn] to [ingest]/[code] is still red).
- **The `recipe_author` `FeatureStore` port** (`sovereign-tools -> recipe-author`,
  plus the daemon/cli-llm/mesh consumers of the tools re-export): a trait in
  `sovereign-contracts` mirroring `RecipeNotes`, then all shim consumers repoint
  to `sovereign_recipe_author::*` directly.

### Probes REFUSED 2026-09-21 (84 at f49fd7cf1) — do not re-probe these

- **`DaemonInferenceClient` cannot enter `sovereign-turn-client`.** turn-client
  is `contract` layer (index 0) with the hand-pinned budget
  `sovereign-contracts + oicp-types + workspace-hack`; the client names
  `ChatPrompt` (corpus-engine/src/enrichment/pipeline/types.rs:202) and
  `InferenceFn` (corpus-engine/src/types.rs:45) — engine-owned, and a
  contract-layer leaf cannot reach the `knowledge`-layer `corpus-index` either.
  (`Error`/`Result`/`EmbedFn` ARE leaf re-exports already.) The two types must
  land in a contract-layer leaf first. An existing dial covers the same wire:
  `oicp-client::RemoteApiProvider` implements `InferenceProvider`
  (`sovereign-contracts/src/traits.rs:292`) over `/chat/completions`; repointing
  cli-dev's one scoring call to it closes the edge but substitutes the dial
  `svrn enrich` uses — an operator decision.
- **`sovereign-code -> corpus-engine` needs a minted fixture.** No committed
  index exists (git ls-files: zero `.lance`/`_corpus_meta`); `e2e_code_intel.rs`
  ingests three fixtures (21 tokio tests) and `CorpusEngine` is the only
  `IndexSource` impl. The fixture must be built by the ingest program with the
  deliberate 30-day-backdated mtimes `recent_changes` tests rely on
  (executor.rs:70). Alternative: move those tests beside the ingest program.
- **`sovereign-gliner -> sovereign-store`**: the `sqlite` module is not
  leaf-clean (`sovereign_core::{error,observer,traits,types,time}`). Candidate A
  (the fix): extract a chunk-entity store leaf from `conv_tiered.rs:841-1335` +
  DDL `migrations.rs:890-968` + `map_db`, deps rusqlite/async-trait/tokio +
  sovereign-contracts + sovereign-time; open question: share `sovereign.db`
  (WAL, second connection) or own a file. Candidate B (gliner dials) is wrong:
  gliner is linked into the daemon and the store is on the per-chunk ingest hot
  path.

### Probe REFUSED 2026-09-21 (82 at 69c69a0d5)

- **`sovereign-cli-daemon -> sovereign-mesh`** (2 refs: `setup_cmd/terminal.rs:229,239`
  `deep_link::{parse_join_argument, DeepLink}`). Cannot be closed by moving the
  `deep_link` module to a leaf: it calls
  `crate::membership::validate_join_key_format` (deep_link.rs:340), and
  `commonwealth-discovery::membership` pulls `commonwealth-core`. Worse, two
  [[forbid]] rows block the shim route outright — `from = "commonwealth-discovery"
  to = "sovereign-*"` (ARCH_LAYERS.toml:699, no except) and the same for
  `commonwealth-rails` (:663) — and a forbid outranks the leaf budget, so a
  `pub use sovereign_contracts::deep_link` would fail layer-gate in both crates.
  The layer map says `sovereign-* -> mesh-foundation` is legal, which is why the
  FORMAT was extracted from mesh once already; closing the package edge needs the
  join-key VALIDATION re-homed below the contract seam first. Chain: extract
  `validate_join_key_format` (pure) from `membership.rs`, then the format move.

### Probe REVERTED 2026-09-21 (82 at d69595b16)

- **`sovereign-cli-llm -> commonwealth-state`** (portfolio + newsworthy open the
  daemon's `MeshStore` SQLite directly — a §4 rule-1 violation). A wire was built
  (daemon `state_http.rs` routes + `turn-client` `state_get/scan/set`) and then
  REVERTED: the daemon's `MeshStore` is `in_memory()` (bootstrap.rs:2548,
  daemon_services.rs:831), so routing portfolio through it would silently stop
  persisting across a daemon restart (today it is `~/.svrnmesh/portfolio.db`).
  Prerequisite before this edge can close: the daemon's state store must be
  PERSISTENT, or the portfolio store must keep its file owner and expose it.
  The route/client code is described in this session's transcript; do not rebuild
  it without settling persistence first.

### The decisions — these are the operator's, and they are meant to be few

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
      **EXECUTED 2026-09-21**: three cutters + orchestrator deletions removed
      ~26k lines — the three bodies, the `project design`/`project plan`/
      `amend design` flow (design_signals + plan_items WERE the atos state
      layer), the daemon's ENTIRE middleware framework (it was atos-only,
      feature-gated), contracts' middleware seam trimmed to what the surviving
      decision-extractor consumes, ~100 cli-contract rows/journeys, the leaf
      row, backstage rows, both docs. Surface note: `project_context` MCP tool
      is GONE (its implementation died with sovereign-atos); `drift_findings`
      SURVIVES (`sovereign_code::DriftFindingsTool`). Kept, with live
      consumers: `corpus-engine-notes::decision_extractor` (tools +
      cli-dev audit-recover), `NoteScope::Feature` notes plumbing.
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

### Singletons

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

### The risk the worklist has to hold

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
