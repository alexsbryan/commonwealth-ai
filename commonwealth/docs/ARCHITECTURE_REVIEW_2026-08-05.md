# Commonwealth + OICP architecture review — 2026-08-05

**Status: COMPLETE, static-only.** Reviewed against HEAD `d3c5261d`. Three
questions were asked and all three are answered below with `file:line`
citations. One operator decision was taken during the review and is recorded
in §7.

**Coverage warning — read this before reading anything else.** This review
issued **zero HTTP requests, ran zero benches, and started zero daemons.**
Every claim below is verified *in source* — by `grep`, by SCIP `symbols`, or
by reading the cited lines — and none is verified *at runtime*. Where this
matters most: the llama-server endpoint list in §4 is taken from that
project's documented surface, not probed against a running `llama-server`, so
it is the one input here that is not first-hand. Treat §4's parity table as
"our side confirmed, their side asserted."

**Also not examined:** the mesh/gossip protocol, knowledge sharding, the
desktop application, ATOS middleware behaviour, security posture beyond the
auth-layer shape, and performance of any kind. Their absence from this
document is not a clean bill of health for any of them.

## 1. What produced this

Static analysis on 2026-08-05 at HEAD `d3c5261d`:

| pass | what it covered | method |
|---|---|---|
| HTTP surface | every route in `commonwealth-api`, request/response field parity, SSE + error shapes | read `server.rs` router registrations + all `routes_*.rs`; grepped each llama-server path string |
| extension seams | `pub trait` + registry + feature-flag + config inventory across `commonwealth/crates/*`, `oicp-types`, `oicp-client` | trait/impl census, `include_str!` audit, `#[non_exhaustive]` census |
| layering + simple path | crate dependency graph, minimum viable run, config surface, integrator docs | `Cargo.toml` graph, `main.rs` startup trace, `quality/ARCH_LAYERS.toml` |

Each of the three passes ran as an independent read-only agent; every
load-bearing claim was then re-verified directly by the reviewer before being
written here. Claims that were *not* independently re-verified are marked
**(single-source)**.

## 2. Bottom line

**The seams are correctly shaped and incorrectly reachable.**

OICP is a well-designed protocol — additive versioning, feature-gated
capability negotiation, a normative degrade ladder, an explicit non-goals
list, and a conformance suite whose dependency budget is deliberately minimal.
The runtime plug-points (`PeerTransport`, `Clock`, `AppRegistry`,
`LocalInferenceService`, `ProviderFactory`) are real traits with public
installers.

Three things sit between that design and its intent:

1. **Every crate is unpublished with `path =` dependencies** (`Cargo.toml:112-116`,
   `:166-207`). "Add an implementation from outside this repo" is not a
   position anyone can occupy, regardless of how open a trait is. This makes
   the entire open/closed question moot until fixed, and the fix for the two
   crates that matter is a policy decision plus one manifest key each.
2. **Three OpenAI sampling fields are silently dropped** on the only shipping
   path, and 17 of 21 llama-server endpoints return 404.
3. **Four of the five branches in the busiest handler in the system are
   unreachable in production.**

None of the three goals is far away. The distance is mostly policy and wiring,
not design.

## 3. The three questions

| # | Question | Verdict |
|---|---|---|
| Q1 | Can someone who normally runs `llama-server` plug in seamlessly? | **Partially.** An OpenAI-protocol client works. A llama-server-protocol client does not. Field fidelity is the sharper problem than endpoint coverage. |
| Q2 | Is it open for extension, closed for modification? | **By shape yes, by reach no.** Runtime seams are open and well-executed; build-time seams collapsed into `include_str!`; distribution closes all of them. |
| Q3 | Simple as you need, useful as you can imagine? | **The ladder is missing its bottom rung and its middle.** |

## 4. Q1 — llama-server parity

The only shipping interface is `sovereign-cli` + desktop (operator, 2026-08-05),
so the target surface is the **embedded** daemon's `:9741`. That is also the
path with the worst field fidelity.

### 4.1 Silently dropped sampling fields — the most actionable finding

`ChatCompletionRequest.stop`, `.frequency_penalty`, `.presence_penalty` are
declared at `openai_types.rs:20-24` and have **zero readers**. The field
mapping at `sovereign-mesh/src/inference_adapter.rs:330-345` carries
`max_tokens`, `temperature`, `top_p`, `sampling_mode` and the Commonwealth
extensions, and never touches them. There is no trace event on the drop.

This violates `ARCH_PRINCIPLES` principle #6 / §18.3 — *never silently
substitute; absence is reported, never defaulted* — on the drop-in path itself.

The three are not one job:

| field | cost | evidence |
|---|---|---|
| `top_k` | **two lines** | `sovereign_contracts::CompletionRequest` already carries `top_k` (`quality/baselines/api/sovereign-contracts.txt:3724`) and `sovereign-inference/src/embedded/sampler.rs:394` documents that `request.top_k` overrides the picked profile. It is absent only from the HTTP struct — so the Ollama shim *builds* it at `routes_ollama.rs:190` and serde discards it at `:220`. A client setting `options.top_k` gets a silent no-op today. |
| `frequency_penalty` / `presence_penalty` | small | The sampler already carries a presence term (`sampler.rs:431`), driven by per-mode quirks rather than by the request. Needs a contract field plus a mapping line. |
| `stop` | **real work** | `grep -r "stop_token\|stop_sequence\|antiprompt\|stop_words" sovereign/crates/sovereign-inference/src/` returns nothing. The embedded engine has no stop-sequence facility at all. This is decode-loop work, not plumbing, and should not be sized with the other two. |

**Whatever is not fixed should be logged.** A `debug!` naming each dropped
field converts a §18.3 violation into an honest degrade, at the cost of one
line per field.

### 4.2 Endpoint parity: 3 of 21

Present: `POST /v1/chat/completions` (`server.rs:51-54`), `GET /v1/models`
(`:83`), `POST /v1/embeddings` (`:82`).

`POST /v1/completions` (`:62`) is present but is a **FIM endpoint**, not
OpenAI text completion: it 503s `fim_unavailable` whenever the edit slot's
FIM lane is absent (`routes_completions.rs:87-102`) — either no
`[models.edit]` is configured (the key was `[models.fim]` when this review
was written; still accepted as a deprecated alias), or the configured
model's vocab carries no FIM markers.

Absent: `/health`, `/props`, `/tokenize`, `/detokenize`, `/apply-template`,
`/completion`, `/infill`, `/slots`, `/metrics`, `/reranking`,
`/lora-adapters`. Verified by grepping each path string against `server.rs`;
the only `/health` literal in the tree is a test double
(`commonwealth-test-harness/src/mock_llama.rs:53`).

**`/health` is the one that costs adoption.** Supervisors, load balancers and
most local-model UIs probe it before sending traffic. The daemon's own
`probe_inference_capability` (`commonwealth-daemon/src/main.rs:978`) probes
`/health` on *other people's* servers while not serving one itself.

### 4.3 Error shapes: six variants, and the two most-hit are the wrong two

| shape | where |
|---|---|
| `{"error":{"message","type","code"}}` — OpenAI-like but **missing `param`** | `openai_types.rs:591-613` |
| `{"error": "<string>"}` — a bare string, not an object | `client_auth.rs:103`, `:134`, `:165` (401/403/500) |
| `{"error","reason","retry_after_secs"}` + `Retry-After` | `admission.rs:64-71` |
| `{"error": "<stringified inner JSON>"}` — double-encoded | `routes_ollama.rs:62-64` |
| `{"error":{"message","type"}}` | `routes_responses.rs:1548-1568` |
| `{"error": "<string>"}` | `routes_internal/mod.rs:122-125` |

There is no `JsonRejection` handler and no router fallback, so axum's defaults
apply: a malformed body returns **plain-text 422**, an unknown path returns an
**empty-bodied 404**.

First contact with a new integration hits exactly two failure modes — bad auth
and malformed JSON — and those are precisely the two that return non-OpenAI
shapes. A client doing `err.error.message` throws on every 401.

### 4.4 Other integration friction

- **No CORS layer anywhere** (deliberate, `routes_ollama.rs:39-43`).
  Browser-resident clients fail preflight.
- **`chat_template_kwargs` is partial**: only `enable_thinking` is read
  (`inference_adapter.rs:510`); every other key is dropped. **(single-source)**
- **The admission gate covers 2 of 5 client inference routes.** It is layered
  on `/v1/chat/completions` (`server.rs:53`) and `/v1/edit_predictions`
  (`:80`) but not `/v1/responses`, `/v1/completions`, or `/v1/embeddings`. It
  only triggers when `X-Node-Id` is present (`admission.rs:125`), so local
  callers are unaffected — but the operator-facing "pause contribution" switch
  is enforced on less than half the inference surface.

## 5. Q2 — open for extension, closed for modification

Three distinct failure modes, and the third makes the first two moot.

### 5.1 Runtime seams — genuinely open

| seam | shape | assessment |
|---|---|---|
| `PeerTransport` (`commonwealth-transport/src/lib.rs:155`) | 2 methods, 5 implementors, `install_peer_transport` (`state.rs:1009`), hot-swappable via `RwLock`, composed per-`TrafficClass` by `RoutedTransport` | **the best seam in the repo** |
| `AppRegistry` (`commonwealth-app/src/registry.rs:25`) | `POST /v1/apps/{id}/install` (`server.rs:147`) launches an arbitrary external process; gossiped under a string key (`gossip.rs:33,36`) | **the one true runtime extension point** — a third party extends the mesh without touching this repo |
| `LocalInferenceService` (`state.rs:181-317`) | **4 required methods**, 9 defaulted, each default naming its own absence (*"only the embedded llama.cpp service does"*) | correct shape; see §5.4 for why nobody can reach it |
| `Clock` (`commonwealth-core/src/clock.rs:28`) | `install_clock` (`state.rs:1032`) | fine |
| `RpcShardWarmer` (`state.rs:336`) | 1 method, opaque-JSON payload so no type dependency is needed | cleanest small seam |

`LocalInferenceService` deserves specific praise: 4 required methods with 9
capability defaults that each explain their own absence is exactly the
degrades-honestly discipline the OICP spec preaches, expressed in code.
Contrast `sovereign_contracts::InferenceProvider`, which a new backend must
also satisfy: **21 methods**, including six `complete_stream*` variants and
`load_extra_slot(String, PathBuf, u32)` — a filesystem path on a trait a
remote provider is expected to implement.

### 5.2 Build-time seams — collapsed

`MiddlewareRegistry` (`middleware/mod.rs:323-350`) has the textbook shape:
`HashMap<String, Arc<dyn Middleware>>`, `register()` at `:332`,
`build_pipeline(&[String])` at `:345` resolving ids from TOML. It is then
sealed inside `AppState::new_with_platform_and_engine` (`state.rs:1110-1147`)
with **no installer** — there is no `with_middleware`. A third party must fork
`state.rs`.

`PipelineAliasTable` (`pipeline_aliases.rs:60`) and `ModelAliasTable`
(`model_aliases.rs:34`) both expose `pub` TOML parsers whose only callers are
their own unit tests; the production loader is `include_str!`
(`pipeline_aliases.rs:101`). Registry shape, constant contents.

### 5.3 Absent seams

- `MeshStore` wraps a concrete `Arc<SqliteBackend>`; `mod backend` is private
  (`commonwealth-state/src/lib.rs:8`) and there is no storage trait.
- The knowledge plane binds `corpus_engine::CorpusEngine` concretely.
- `ProcessKind` (`commonwealth-core/src/capabilities.rs:175`) has two arms —
  `LlamaServer`, `RpcServer`. The engine-kind set is a closed enum.
- `GossipTransport` (`commonwealth-discovery/src/gossip_service.rs:139`) is
  **declared with zero implementors** and is referenced only by its own doc
  comment at `:47`. Production gossip runs over `PeerTransport` + HTTP.

### 5.4 The wire contract is the sharpest gap

**Zero `#[non_exhaustive]`** across `oicp-types/`, `oicp-client/` and
`commonwealth/crates/` — verified, count is 0. **No `flatten`, no
`deny_unknown_fields`, no catch-all map**; `serde(other)` appears exactly once
(`capability.rs:42`, `Capability::Unknown`).

Consequence: OICP is forward-*tolerant* but not round-trip-*preserving*.
Unknown fields parse successfully and are then **silently dropped**. A v0.4
proxy relaying a v0.5 manifest strips every unrecognised field.

Nine enums break downstream on a new variant. Two are load-bearing:
`PoolingStrategy` (`manifest.rs:209`) and `NormalizationStrategy` (`:221`) sit
inside `EmbedModelInfo`, whose **exact equality gates collaborative
ingestion** (`manifest.rs:236-241`) — so a new pooling mode partitions the
mesh rather than degrading.

**The counter-example is in the same crate.** `CapabilityHint`
(`capability.rs:118-209`) is not an enum: it is a `String` newtype with a
governed `STANDARDIZED` list (`:138`), an `x:` extension track (`:134`, `:155`),
and an explicit `is_unknown_bare()` (`:209`) for "a future spec standardized
this and I predate it." `features` follows the same pattern
(`manifest.rs:21-92`) and already ships a live out-of-spec capability
(`X_FORCED_CHOICE`, `:62`). **Nine enums lack the discipline that two strings
have** — and the discipline is already written, tested and in production.

### 5.5 Distribution closes everything anyway

Root `Cargo.toml:112-116` states plainly that every crate is unpublished, and
all inter-crate deps are `path =` (`:166-207`).

- The **spec** declares itself **CC0** (`commonwealth/docs/oicp-v0.4.md:5`).
- The **only implementation** is **AGPL-3.0-or-later** (`Cargo.toml:122`),
  which `commonwealth/ARCHITECTURE.md:32` still describes as Apache 2.0.
- `oicp-conformance` — whose own manifest says *"A conforming third-party host
  can copy this crate wholesale"* — is `publish = false`, lives inside
  `commonwealth/crates/`, and is referenced by nothing in CI.
- `oicp-client`, billed as the thin pure-HTTP client, depends on
  `sovereign-contracts`: **63 traits / 221 structs / 102 enums**
  (`quality/baselines/api/sovereign-contracts.txt`). Adopting it means
  adopting Sovereign's whole agent-runtime vocabulary.

`oicp-types` itself is clean — `serde` + `serde_json`, zero internal deps,
`#![warn(missing_docs)]`.

### 5.6 Version negotiation does not exist

`oicp-types/src/version.rs` is 19 lines: one `pub const OICP_VERSION` plus a
test asserting it equals itself. `oicp-client` never reads
`manifest.oicp_version` **(single-source)**. Negotiation is entirely
structural — `fetch_manifest` returns `Option` and `None` means "degrade to
v0.3 defaults" (`oicp-client/src/lib.rs:498-504`), so a 500 from a v0.4 host
is indistinguishable from a v0.3 host.

This is a deliberate and defensible reading of the spec's *"feature presence,
not the version string, gates behaviour"* (`oicp-v0.4.md:26-28`). It is worth
recording that the corollary — there is no mechanism to detect or refuse an
incompatible peer — is accepted, not overlooked.

## 6. Q3 — the simplicity ladder

### 6.1 The bottom rung is a trap

`commonwealth daemon start` needs zero arguments, creates its own data
directory, auto-generates its bearer token, and boots clean on a bare machine.
It then returns 503 to every inference request, forever, with no local path to
fix it.

Verified chain:

- `AppState::with_local_inference` has **one production caller in the entire
  tree** — `sovereign-mesh/src/daemon.rs:2377`, the embedded path. Every other
  call site is a test.
- `commonwealth-daemon/src/main.rs:828` is the **sole production constructor**
  that leaves `local_inference == None`.
- `inference_store.set_plan` has **one production caller** —
  `routes_internal/gossip.rs:130`, the inbound-gossip handler.
- There is no `join` subcommand and no `model pull` (`main.rs:39-79`), so a
  plan can never arrive.

Supporting drift, all still on disk: `contrib/install.sh:9` points at
`github.com/commonwealth-rs/commonwealth`;
`contrib/systemd/commonwealth.service:9` invokes `commonwealth daemon stop`,
which does not exist (`DaemonCommands` has one variant, `Start`,
`main.rs:121-124`); `ARCHITECTURE.md:962` documents `start/stop/status`; and
`README.md:5` cites `CLOUD_PEER_DEPLOY.md` as the binary's use case — a doc
that actually runs `sovereign mesh join` (`:303`).

### 6.2 Which makes four of five branches in `chat_completions` dead

Priorities 1–4 — `route_with_oicp` (`routes_inference.rs:353`),
`forward_to_model` (`:431`), `forward_to_llama_server` (`:491`), and the
llama-server address bookkeeping behind them (`state.rs:1406-1418`) — are
reachable only when `local_inference == None`, which only the standalone
binary produces. Priority 0 fires first and unconditionally on every real
request (`routes_inference.rs:165`).

**The path is well-tested but unreachable.** `commonwealth-test-harness`
constructs `AppState` without `local_inference` and drives Priorities 1–4
against `MockLlamaServer` (`simulated_node.rs:205`), and `route_with_oicp`
carries full tracing from the OICP Phase-B fix (`:374`, `:392`, `:399`). Green
tests and good glassbox on a path production never enters.

This is the same failure class `sovereign/docs/specs/OICP_RATIONALIZATION.md`
F2 named — *"a dead twin scheduler shadowing the live one"*. Phase A deleted
the **scheduler** twin (`ModelPortfolio`, `adaptive.rs`, `plan_builder`,
`layer_assignment`, orchestrator spawn). The **routing** twin survived because
it hides behind an `Option` at runtime rather than behind an uncalled function
— which is precisely why a `callers`-based sweep did not catch it in June.

### 6.3 The middle rung does not exist

Between "point an OpenAI client at the daemon" and "fork the monorepo" there
is nothing. Scaling up means *leaving* Commonwealth: real serving, iroh,
corpora and ATOS all live in `sovereign-mesh::EmbeddedDaemon`. Commonwealth
scales up by being consumed in-process, not by being configured.

**This violates a rule this repo already wrote for itself.** The code-intel
package-boundary decision (2026-07-27, in the notes store) states: *"the
daemon consumes the package the way a stranger does — no private back-channel,
no `cfg(sovereign)`. A path that only works against our daemon is a bug in the
package."* Sovereign does not consume Commonwealth the way a stranger does —
it reaches in via `with_local_inference` in-process.

`studio/BOUNDARY.md` + `cargo run -p xtask -- boundary-gate` is the
enforcement pattern for exactly this, already proven twice in this repo
(studio, code-intel) and **never applied to Commonwealth**.

## 7. The harness question — how opinionated is the IDE path?

The operator asked whether driving the daemon from an IDE or agent harness
(`pi`, `codex`, …) works generically or only in an opinionated way.

**It is opinionated, in three specific and fixable ways.**

**1. `Harness` is a four-arm closed enum** (`frontdoor.rs:64-81`): `Codex`,
`Opencode`, `Generic`, `Bare` — two named products plus two fallbacks. Five
behaviour predicates branch on it:

| predicate | applies to | `frontdoor.rs` |
|---|---|---|
| `runs_distiller` | Opencode only | `:105` |
| `runs_catalog_filter` | Codex \| Opencode | `:116` |
| `runs_synthetic_tools` | Opencode only | `:130` |
| `runs_grammar_lock` | Codex \| Opencode \| Generic | `:148` |
| `runs_coherence_baseline` | everything except Bare | `:154` |

**2. Detection is User-Agent substring sniffing.** `detect_harness`
(`frontdoor.rs:161-197`) matches `codex_cli`/`codex-cli` → `Codex` and
`opencode` → `Opencode`. **A harness cannot declare itself.** The only
override, `SOVEREIGN_HARNESS`, is a **process-global env var** (`:162`) — so a
daemon serving two IDEs at once cannot set it per-client. Per-request
behaviour is selected by a process-global knob or a substring match.

**3. `CODEX_TOOL_KEEPLIST = ["exec_command", "web_search"]`**
(`frontdoor.rs:219`) is a two-entry hardcoded allowlist that drops every other
tool **before the model ever sees it**. For codex that is a validated 9-of-11
drop with a documented rationale. For a harness with a different tool
vocabulary it is meaningless — but it is the only list there is.

So `pi` (or Cline, Aider, Zed, Continue, Cursor) lands on `Generic`: grammar
lock and coherence baseline, no catalog filter, no synthetic tools, no
distiller. It works, conservatively. The tuning that makes codex and opencode
good is unreachable and undiscoverable.

**The fix already half-exists.** `X-Sovereign-Tool-Profile`
(`routes_inference.rs:57-64`) is a per-request header that sets
`request.tool_profile` — exactly the right shape: a per-request,
client-declared capability channel. It governs one narrow thing (tool
filtering) instead of the whole reshape profile.

The generalisation is the same move OICP already made for capability hints,
and the same rule the code-intel boundary decision states — *declare
capability, never name a vendor*:

- turn `Harness` from a vendor enum into a **named profile** resolved from a
  registry;
- load profiles from TOML rather than `match` arms, with the five predicates
  becoming profile fields;
- select per request by header, keeping UA sniffing as the fallback default so
  today's codex and opencode behaviour is preserved byte-for-byte;
- advertise the available profile names in the OICP manifest, so a harness can
  *discover* what it may ask for instead of guessing.

That converts frontdoor from "we know about codex and opencode" into "any
harness declares what it wants and can see what it got" — which is the
acceptance condition in §7.1 below, expressed structurally.

## 7.1 Decision recorded — the standalone binary is retired, conditionally

**Operator decision, 2026-08-05:** the standalone `commonwealth` binary is
unnecessary. `sovereign-cli` and the desktop app are the only intended
interfaces.

**The condition attached, stated so it is falsifiable:** an end user must be
able to get a satisfying, relatively bare-metal *local model serving*
experience — extensible with skills — driven from an ordinary IDE or agent
harness. §7 above is the measurement of how close that is today: it works, but
only two harnesses get the tuned path, and no harness can ask for a profile by
name.

**This does not authorise deletion by itself.** What follows from it:

- Priorities 1–4 of `chat_completions`, `forward_to_model`,
  `forward_to_llama_server`, and the orchestrator llama-server spawn
  bookkeeping become removable — collapsing the handler from five branches to
  one and removing the `stop`/penalty behavioural fork described in §4.1 as a
  side effect.
- The drift in `contrib/`, `ARCHITECTURE.md:962` and `README.md:5` (§6.1) must
  be resolved in the same change, not left describing a binary that no longer
  exists.
- If the binary is kept as a *test* fixture, it needs a module banner saying
  so — per `OICP_RATIONALIZATION.md` Phase A's rule: *"no dead module without a
  banner."*

## 8. Findings, ranked by impact per unit of work

| # | Finding | Size | Why it ranks here |
|---|---|---|---|
| 1 | Publish `oicp-types` + `oicp-conformance` under a permissive licence; move the conformance crate to a top-level sibling and wire it into CI | policy + 2 manifest keys, no code | A CC0 spec whose only implementation is AGPL and unpublished is a single-implementation protocol with extra steps. Both crates are already clean enough to lift. Highest leverage on the list. |
| 2 | Decide the standalone binary's fate and act on it (§7.1) | ~250 lines removed | Four of five branches in the hottest handler; a binary that traps anyone who finds it. Decision taken; execution pending. |
| 3 | `top_k` two-line fix, then `/health`, then unify the three bare-string auth error bodies | ~2 / ~10 / ~3 sites | Each closes a bug that every affected client hits on first contact. `top_k` closes one the Ollama shim actively creates. |
| 4 | Log every dropped request field | 1 line per field | Converts a §18.3 violation into an honest degrade even where the field cannot yet be honoured. |
| 5 | `#[non_exhaustive]` across `oicp-types`; give `PoolingStrategy`/`NormalizationStrategy` the `CapabilityHint` treatment | ~30 annotations + 1 api-gate baseline | Those two enums gate collaborative ingestion by exact equality — a new variant partitions the mesh. The pattern to copy is in the same crate. |
| 6 | Harness profiles: registry + per-request header + manifest advertisement (§7) | moderate | Turns the IDE path from vendor-recognition into capability-declaration. Directly serves the §7.1 condition. |
| 7 | Give Commonwealth a `BOUNDARY.md` + `boundary-gate` package | moderate | Would have caught `commonwealth-api → sovereign-core`/`sovereign-tools` structurally instead of as grandfathered exceptions (`quality/ARCH_LAYERS.toml:185-201`, remediation R6). Principle #10: structural, not remembered. |

## 9. Deliberately not proposed

Per the reporting rule that a gap without quantified demand is a lead and not
a finding, the following are recorded as real closures with **no proposed
work**:

- **Opening `MiddlewareRegistry`.** Genuinely closed (§5.2), but no named
  party wants to add a middleware from outside. If a second host or plug-in
  author appears, this is the first one to open — the registry is already
  built and only needs an installer.
- **A storage trait behind `MeshStore`.** No demand evidence.
- **A `Scheduler` trait replacing `SchedulingStrategy`** (`plan.rs:40`). No
  demand evidence, and the enum is honest about being closed.
- **`GossipTransport`.** Zero implementors (§5.3). Either delete it or banner
  it; do not build against it.

## 10. Doc drift found while reviewing

To be fixed by whoever next touches these files:

| claim | reality |
|---|---|
| `ARCHITECTURE.md:962` — `commonwealth daemon start/stop/status` | one variant, `Start` (`main.rs:121-124`) |
| `ARCHITECTURE.md:900` — a `[fairness]` config table | deleted from the struct (`commonwealth-core/src/config.rs:6-11`) |
| `sovereign/SYSTEM_OVERVIEW.md:201` — "`docs/oicp-v0.3.md` is the canonical OICP spec" | `oicp-types/src/lib.rs:5` — v0.4 is canonical. **Fixed 2026-08-05 as part of this review.** |
| `oicp-types/Cargo.toml` description — "OICP v0.2" | implements v0.4.0 (`version.rs:5`) |
| `commonwealth/docs/` carries four overlapping OICP specs (v0.2, v0.3, v0.4, unversioned) totalling 1,781 lines | no "read this one" pointer |
| `contrib/systemd/commonwealth.service:9` — `ExecStop=… daemon stop` | no such subcommand |
| `contrib/install.sh:9` — `github.com/commonwealth-rs/commonwealth` | not this repository |

## 11. One open question, not a drift item

`ARCHITECTURE.md:32` states, as one of three *"constitutional constraints"*,
that the project is **Apache 2.0**. Today it is AGPL-3.0-or-later
(`Cargo.toml:122`).

This is deliberately **not** listed as drift above. `ARCHITECTURE.md`'s own
header marks it a historical record, and `Cargo.toml:120-121` confirms
commonwealth genuinely *was* Apache-2.0 before the open-source-launch
consolidation — so the sentence is historically accurate.

The tension is that the same header says the design-philosophy section *"still
governs the project."* That section's stated reason for Apache 2.0 is
anti-capture — *"forking it is trivial and there is no moat to build around
it."* AGPL and Apache both permit forking; they differ on what a *network
service operator* must return. Whether the relicence is consistent with the
stated constraint, and whether §5.5's recommendation to carve `oicp-types` and
`oicp-conformance` back out to a permissive licence is a partial restoration
of it or a separate decision, is an **operator question this review does not
answer.** It is recorded here so it is decided rather than inherited.
