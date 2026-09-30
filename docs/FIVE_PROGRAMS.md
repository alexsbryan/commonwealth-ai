# Five programs — the idiomatic design, and the cut that gets there

> **DRAFT — not in force (2026-09-17).** The `domains` campaign continues to
> completion; this document supersedes nothing until then, and the banner it
> briefly put on `quality/DOMAINS.md` was reverted (operator: the edit was
> premature). Kept as the design record for the cut that follows.

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

**Step 9 — boundaries.** In `quality/ARCH_LAYERS.toml`: five `[[package]]`
rows, `svrn`, `ingest`, `cmnwlth`, `code`, `bench`; `[[forbid]]` rows
`cmnwlth -> svrn`, `svrn -> cmnwlth`, `svrn -> code`, `bench -> *` except
`oicp-types` and `sovereign-contracts`. `sovereign-tools` and
`sovereign-cli-llm` split by program before their rows land, each split done
when its in-crate containment test passes. Each red edge is one task: a
trait in a shared leaf or a wire call, done when green. Step done when
`cd corpus-engine && cargo xtask boundary-gate` exits 0.

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

## 11. Dependencies — own the tuned path, absorb the inert, meet the running

Drafted 2026-09-30 from a survey of Lemonade (`lemonade-sdk/lemonade` at
`6f8e87b`) and nine neighbouring categories. Star counts are the GitHub API on
that date. Like the rest of this document, it is a draft and not in force.

### 11.1 The rule

A connection costs roughly: how wide the seam is, times how often the other
side changes, times how much we tune across it. Depending on community work is
not the same as integrating community systems, and the two are priced
differently.

- **Own both sides where we tune across the seam.** The answer path is one
  instrument. Its parts are: the embedder that built the snapshot, the
  reranker and its fitted margin, evidence ordering for KV reuse, synthesis,
  claim splitting, the checker and its fitted threshold, and the abstention
  cut. A process boundary inside it can be debugged but not tuned. The engine
  belongs here because the gate's prefix-state restore and evidence ordering
  are tuned across it.
- **Absorb what is inert, as a pinned dependency.**
  - Libraries: llama.cpp, Tantivy, Lance, tree-sitter, `ort`, iroh.
  - Weights: GGUF models, GLiNER, ONNX rerankers and claim checkers.
  - Formats: Arrow, SCIP, JSON Schema, OTel.
  - Community data: Linked Open Vocabularies ontologies, schema.org, public
    benchmarks.

  None of these has a lifecycle we manage. This is where the community's
  heaviest investment went.
- **Meet running systems as a guest, at two doors.** Open WebUI, LM Studio,
  Claude Desktop, LiteLLM and coding harnesses reach us through OpenAI HTTP
  and MCP. We maintain two faces, not an adapter per host. A host's
  configuration of us is that host's code.
- **One exception per category: a "bring your own" door.** Examples are an
  OpenAI-compatible engine for hardware we do not build for (the NPU via
  Lemonade) and an external parser for scanned PDFs. The door is optional,
  never on the default path, and any degradation it causes is named in the
  verdict.

Our own five programs still dial each other (§4 rule 2). The guest rule is
about third parties. A third-party process whose lifecycle we hold is the
"component holding another's lifecycle" smell (ARCH principle 12).

### 11.2 Per program

| Program | Own | Absorb | Meet / bring-your-own |
|---|---|---|---|
| `svrn` | Router and plans (§11.4); evidence assembly; synthesis; claim lifecycle and verdicts (§11.5); in-process engine | llama.cpp; ONNX embedder, reranker and checker weights | Hosts via OpenAI HTTP and MCP; external engine door |
| `svrn ingest` | Recipe runner; source-aware extractors for structured sources (mail, wiki dumps, threads, JSONL, docx); profile-constrained extraction; snapshot format | Lance, Tantivy, GLiNER, ontologies via profiles (§11.3) | Scanned-PDF parser door |
| `cmnwlth` | Consent and provenance headers only | iroh | Exo and mesh-llm own pooling; we do not compete |
| `svrn code` | `callers`, `blast` and convergence over the SCIP graph | Upstream SCIP indexers (they emit files) | Serena, ast-grep, FIM editors |
| `svrn bench` | Banks, calibration, judge validation, per-package grading | Public benchmarks as data | Results export as JSONL and OTel; no adopted harness |

What this reverses from the same survey's earlier drafts:
- The engine stays in-process. The cost was the vendored divergences and the
  OS matrix, not the idea. Upstream the two patches in
  `vendor/llama-cpp-sys-4` and let the engine door cover the rest.
- No Docling or BookNLP sidecar on the default path.
- No per-host integration adapters.

### 11.3 The package bank: recipes and profiles

The bank carries two orthogonal package kinds. A legal profile applies to a
PDF folder and to a mailbox alike, so the bank grows on two axes that combine.

- **Recipe** — how a source is shaped: acquire, extract, chunk, sections,
  update. This is today's schema (`sovereign-recipes/SCHEMA.md`, 30 entries in
  `sovereign-recipes/registry.toml`).
- **Profile** — what the content means. It contains:
  - a subset of a published ontology, written as SHACL shapes, from which the
    JSON Schema for constrained extraction is generated;
  - extraction guidance;
  - routes (§11.4);
  - question templates, surfaced as MCP prompts;
  - a small grading bank.

  Atoms export as JSON-LD carrying the ontology's IRIs. We author profiles,
  never ontologies.

"Just works" means a local model picks the pair; the user does not.

1. The input lands in a watched inbox, or through the MCP `add_source` tool.
2. A sample is fingerprinted.
3. The fingerprint is matched against package descriptions by centroid, and a
   small model confirms the match.
4. At a narrow margin, one elicitation question is asked.
5. With no match, a recipe and profile are drafted under schema,
   `test_recipe` is run on the sample, and the draft is previewed
   (`studio/crates/sovereign-recipe-author`).
6. The build runs as an MCP task.
7. The profile's bank grades the build, or reports could-not-judge.
8. Drafted packages, never content, go back to the bank on opt-in. CI builds
   and grades each one.

### 11.4 The router is owned

Embedding-exemplar routing is available off the shelf
(`aurelio-labs/semantic-router` 3.9k, NeMo Guardrails canonical forms 7.2k).
Plans exist without a router: GraphRAG and LightRAG search modes are picked
by the caller. What nobody ships is a router that:
- maps query shape to a retrieval-and-grounding plan;
- is decided by code, not by a model's tool call;
- has a calibrated margin that can refuse.

The shapes are: lookup, comparison, whole-corpus synthesis, state at a point,
change over time, inventory, contradiction, and not-in-corpus. The speech-act
intents stay as a tone layer above them.

Today there are 281 exemplars in `sovereign/router/exemplars.toml`. The embed
pass takes about 50 ms, against 0.5–2 s for the LLM classifier
(`sovereign/crates/sovereign-core/src/router_embed.rs`). Its thresholds (0.55
top, 0.10 margin) are hand-set and were never fitted to a bank.

The target has four parts:
- routes as data, shipped by profiles;
- one decider returning plan, runner-up, margin and abstain, with the
  threshold fitted by bench;
- plans composed from a few primitives;
- one MCP `ask` tool on the enforced path, with plans also exposed as tools
  for hosts that run strong models.

### 11.5 Claims — one type, two owners

- **Source claims** belong to ingest. They carry spans, are keyed to the
  snapshot, recipe version and profile version, and are re-extracted by
  `[update]` when their spans change. Tensions are computed here.
- **Answer claims** belong to `svrn`. They are checked and persisted with the
  conversation, and stamped with the snapshot they were checked against. A
  replaced snapshot makes them stale, not wrong: the conversation shows the
  verification age and re-checks lazily.
- **The checker** is stateless (`verify`) and backed by a registry.
- **Calibration** belongs to bench.

When a host's model writes the answer from `search` results, `svrn` can
verify only if `verify` is called. That answer is labelled unverified rather
than implied to be checked (ARCH principle 10). The enforced path is `ask`, or
`svrn` as the model behind an OpenAI connection.

### 11.6 Correctness features are carried; cost features degrade

`svrn` carries the correctness features itself: the embedder, reranker and
claim checker, about 1 GB of ONNX weights on CPU. That keeps three things
independent of whichever engine is present:
- snapshot embedding identity;
- answerability (the rerank margin);
- verification.

JSON Schema is the only constraint language assumed. The engine is probed at
start, and each feature's path is chosen from that probe, never from the
vendor's name.

| Feature | Affects | Path, best first |
|---|---|---|
| Constrained output | correctness | Native JSON Schema → tool-call arguments → validate plus one repair → `CouldNotJudge` |
| Rerank / answerability, checking, embeddings | correctness | `svrn`'s own weights, so never degraded |
| Prefix cache | cost | Engine-automatic; evidence ordered shared-first; latency reported |
| MTP, jump-forward, sibling contexts, llguidance | cost | Our engine only; optional speedups |

With no engine at all, the structure tier still works: sections, tiers, NER,
retrieval, rerank and `verify`. The semantic tier shows "not built: needs a
local model". MCP sampling is used only for small consented jobs, such as
drafting a recipe, and never for bulk extraction.

### 11.7 Forfeited, with the evidence

| Category | Leaders (stars, 2026-09-30) | Our position |
|---|---|---|
| Local serving | Ollama 182k, llama.cpp 130k, vLLM 93k | Absorb llama.cpp as a library; engine door for the rest |
| Device pooling | exo 47.7k; mesh-llm 3.5k (Rust, iroh, layer splits, pushed daily) | Forfeit; keep the two headers |
| Gateways | LiteLLM 60k | Forfeit |
| Chat and RAG apps | Open WebUI 154k, Dify 158k, AnythingLLM 67k | Guest via the two doors. They show sources; none checks the answer at runtime |
| Parsers | markitdown 188k, MinerU 81k, Docling 68k | Scanned-PDF door only |
| Code intel for agents | Serena 30k | Hold at current size |
| Eval libraries | promptfoo 26k, DeepEval 19k | Keep ours slim; none validates its judge |

Runtime claim-level verification with abstention is fragmented, not crowded.
Checker models exist:
- MiniCheck-Flan-T5-L (0.8B) scores 75.0 on LLM-AggreFact;
- FactCG-DeBERTa (0.4B) scores 75.6;
- GPT-4o scores 75.9.

Hosted verifiers (Vertex, Azure) return proprietary shapes and never decide
to abstain. No wire standard carries claim-level verdicts; OTel's
`gen_ai.evaluation.result` is per response.

### 11.8 Gates that keep it structural

- Every third-party dependency is a row in one registry: kind (library,
  weights, format, data, or running system) and whether we tune across it.
- The default install spawns zero third-party processes.
  `scripts/install-journey-nightly.sh` asserts the process tree.
- Every running-system row is optional. Its absence is `CouldNotJudge` or a
  named degradation, never a failed turn.
- The adapter count is a size-gate key. If it rises, that is drift toward
  maintaining connections.

### 11.9 Measurements that decide, bars before data

1. **Checker bake-off.** Our judge against MiniCheck-FT5, FactCG and Granite
   Guardian as the per-claim checker, on our banks and on LLM-AggreFact.
   - Bar: honesty parity at five times lower gate latency, the H0 bar of
     `sovereign/docs/specs/NATIVE_GROUNDING.md`. The baseline is a gate that
     took 121.5 s of a 150 s turn (2026-08-12).
   - If our pipeline scores below 75.0 on LLM-AggreFact, adopt their checker
     and stop tuning ours.
2. **CPU floor.** The claim-checking and answerability lanes run with only
   the ONNX weights on CPU.
   - Bar: honesty within 0.03 of the in-daemon path.
   - Failing it moves the floor to a laptop GPU, still independent of the
     engine.
3. **Router.** A routing bank covering whole-corpus, state-at and
   contradiction shapes across three profiles. Three routers compete:
   calibrated embed-plus-margin, an 8B model tool-calling across the plan
   tools, and semantic-router with the same exemplars.
   - Bar: calibrated embed wins on accuracy at under a tenth of the latency.
   - Failing that, plans are exposed as tools and the router stops there.
4. **Just works.** Twelve unlabelled inputs of different shapes go into the
   inbox. Count three things:
   - how many are matched with zero questions, and with one;
   - how many drafted packages pass their own grading;
   - whether the first profile prompt returns a supported answer.

   If a wrong match is more common than a question, the confirmation step
   becomes mandatory.
5. **First hour.** Fresh install to first verified answer on a Wikipedia
   snapshot, timed on Strix Halo and on an M-series Mac, plus the build time
   of one full-length public-domain novel under the fiction profile.
   Unmeasured today; taken first.
