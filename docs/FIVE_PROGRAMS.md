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

That sentence is served by six programs joined by two wires everyone already
speaks: OpenAI-compatible HTTP for turns and completions, MCP for tools.
Programs compose by process or through a distribution. One program dials
another and never links another's internals; a *distribution* (§2c) may compose
programs' declared library faces in one process, and it holds wiring only (the
stock install is one process, phase-b-29). Each program owns its own config
file, data directory and log. Every program builds and
runs alone, and its lift sandbox proves it. The crate graph is a build detail,
not architecture.

The test for every boundary is the next developer who wants THIS program but
not THAT one (operator, 2026-09-25, `ralph/decisions/phase-b-1.md`).

## 2. The programs

There were five until 2026-09-25. That day `serve` was split out of `cmnwlth`,
because a local model server must not need a mesh and a mesh must be able to
front someone else's model server (phase-b-1).

| Program | Shape | Wire it serves | Owns on disk | Exists today as |
|---|---|---|---|---|
| `svrn` | knowledge server | MCP `ask`, `search`, `atoms_lookup`; HTTP `/v1/chat/completions` | corpus indexes, conversations, its own memory (lessons, commitments, dossier) | `corpus-mcp/` (three verbs, lifted) + the turn path in `sovereign-core` |
| `svrn ingest` | recipe pipeline | CLI over a library; the work plane is one optional caller, never a requirement | the index directory it writes | `sovereign-recipes/` + `corpus-engine` extractors, chunkers, index |
| `cmnwlth` | endpoint router with a roster | HTTP `/v1/*` proxied, OICP manifest; adverts any origin, inference included, and never ranks | roster, adverts, decision log, the node key | `commonwealth/` package (lifted, `scripts/cw-rails-lift.sh`) |
| `serve` | model server | HTTP `/v1/chat/completions`, `/v1/embeddings`, `/v1/rerank`, `/v1/ner`, `/v1/models` and its own OICP manifest | weights, its placement config | `sovereign-serve` (the binary, lifted: `scripts/program-lift.sh --sandbox serve`), `sovereign-inference`, `sovereign-compute`, `sovereign-serving-host`, `sovereign-gliner` (the NER kind's loader, served on `/v1/ner`, which the svrn daemon dials; in the `cmnwlth` package until pb-serve-package) (one engine assembly; model kinds by registration; placement per kind: in-process, child or dial) |
| `svrn code` | LSP for agents | MCP `symbols`, `callers`, `blast`, `solve`, …; HTTP `/v1/completions` (FIM), `/v1/solve/jobs` | SCIP index, decision notes, the work atlas (optional bundle) | `sovereign/crates/sovereign-tools/src/code/`, `corpus-engine-scip`, `corpus-engine-notes`, `packages/vscode-sovereign/`, and the TDD solver whose chat dials serve: `sovereign-tdd`, `sovereign-agent-tools`, `sovereign-agent-bench` (the agent-coding lane runs it in-process; moved from `bench` at pb-meshapp-solve) |
| `svrn bench` | evaluator | none; dials three URLs: the model, the subject (svrn) and a separate judge | banks, baselines, verdict tables | `sovereign/bench/` lanes, `sovereign-eval` minus its product links (the agent-coding lane's crates are `code`'s since pb-meshapp-solve) |

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

## 2c. Distributions, the host kit, and the compose rule (phase-b-1)

**A distribution is a composition root that holds wiring only.** Examples: the
`svrn` dispatcher, the setup wizard, service install, the container image and
a native shell. A distribution may:

- exec or install program binaries;
- link the wire leaves, the host kit and `sovereign-turn-client`;
- link a program's declared library face, never its internals.

It owns only the bundle's concerns: the composed setup flow, the verb map, and
the placement and lifecycle of each program it bundles. The stock install is
ONE process: it builds `serve`'s provider and hands it to svrn through the
`InferenceProvider` port, and binds serve's router on serve's port so other
programs still dial it. A standalone svrn dials a configured serve and brings
nothing up. There is no phone host build; the phone is a client (phase-b-29).
The same root composes the other programs a stock node runs: ingest's engine
reaches svrn through ingest's ports, one narrow port per tool family
(phase-b-33), and code's MCP bundles mount on svrn's port under their own
mount (phase-b-30). Where a program's face needs a registry another program owns, the root
hands it in as a value: code's next-edit door takes the extension → grammar
lookup as `CodeParts::grammar`, the stock binary supplies ingest's registry
through a declared face item, and standalone `svrn code` passes `None`, so
the lanes that need a grammar report it absent (phase-b-68). A port lives in the leaf that already owns every type its
methods name: `corpus-index` beside `CorpusReadPort` (`ingest_port.rs`), or
`corpus-engine-atlas-reader` for a port that names atlas types.
`sovereign-contracts` cannot host them, because `corpus-index` depends on it.
No engine-internal type moves to a leaf to make a port nameable (§12 3a rung
2, last bullet). When a svrn pipeline passes one, the code that names it moves
into ingest's crate as the port's implementation, and svrn keeps the tool
shell and whatever names `sovereign-core` (phase-b-43). The inbound
direction is the mirror: where the engine declares a trait that svrn
implements (the tiered-enrichment hooks), the trait and the values its methods
name are that port's vocabulary, spoken by both programs, and they move to
the same leaf beside the other ingest ports (`corpus-index`
`ingest_port/tiered.rs`) with the engine re-exporting them. A type qualifies
only if it is pure data under the leaf test and a port method names it
(phase-b-49). "Engine-internal" means spoken only by
ingest's own code; a type a cross-program port's method names is that
port's vocabulary, whichever program declared the port (phase-b-51). A port between two programs is legitimate because the root
that plugs it sits outside every package.
Distributions are declared as `[[distribution]]` rows in
`quality/ARCH_LAYERS.toml`, extending `[thin_surfaces]`: the crate may reach
its own crates, the shared leaves and each program's declared face, judged on
its direct edges, under a fixed `max_code_lines` that is never ratcheted.
A distribution's tests count as its edges, so a composition test that must
name a program's library reaches it through a declared face with empty
`items`: the face-item scan reads `src/` only, so the root's wiring still
names none of it (phase-b-72, stock's face on `sovereign-serving-host`).
Phase B enrols one, the stock binary (cap 300); the dispatcher, the setup
verbs and service install stay `svrn` members until the follow-on queue carves
their wiring out, so their edges keep counting (phase-b-30). A distribution
never excuses a program from building and running alone.

**The host kit holds what each program's binary owns about itself:**

- its data-root lock;
- its data root (the path is always supplied by the caller);
- its server shell: bind, loopback guard, peer address, body limits, shutdown
  as a value, mount tracing;
- MCP dispatch and its framings, the call-log port, and tool exposure as
  manifest data.

It is a neutrally named leaf, so `cw-rails` can take it. It owns no store and
names no program's vocabulary. Reaching a program is the client's half and
stays in `sovereign-turn-client`: probing it, bringing it up through the one
`bring_up_decider`, and locating its binary. Principle 12 splits the two.

**Compose, never re-own.** Before building, a change names the existing owner
it extends and the registry it plugs into (principle 11). A change that adds a
second implementation of a drive or a second owner of a capability stops. The
drives are: bring-up, root lock, engine assembly, MCP dispatch, tool-set build,
route mounting and job execution. A new model kind, tool or route is a
registration, never a new binary (principles 8, 9). The duplicates are
collapsed before a process is split, the way principle 8 collapses the drive
before the file is split.

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
2. Dial, never embed. A program never embeds another program. A distribution
   composing declared library faces in one process is wiring, not embedding
   (§2c, phase-b-29). `EmbeddedDaemon` (constructed at
   `sovereign/crates/sovereign-daemon/src/daemon_cmd/boot.rs:996` and, for the
   one-shot `admin join`, `daemon_cmd/admin_join.rs:66`) is svrn's own state;
   the stock distribution composes it with serve in one process, and nothing
   else constructs it.
3. An unreachable peer program is `CouldNotJudge` or absent, never empty.
4. Every decision visible at `tracing=debug`.
5. Config that can change without a code change is a file, not a constant.
6. A program never links another program's crates. Shared vocabulary lives in
   `oicp-types`, `kernel-types` and `sovereign-contracts` (§12 3a), and the one
   shared mechanism is the host kit (§2c). A module only one program uses
   belongs to that program, even when it sits in a shared crate today.
7. Each program's config holds its own sections only. After Phase B, svrn
   and `serve` each read only their own sections of the shared 13-section
   `SetupConfig` file (phase-b-2). Splitting it into one file per program,
   with the migration shipping in the same commit as the switch, belongs to
   the follow-on queue (`ralph/next/phase-c/`).
8. The mesh is a layer, never a host (operator, 2026-09-26, phase-b-18:
   "daemons serve, mesh added by cw-rails — it should all gracefully LAYER
   rather than enmesh and embed").
   - Each program serves its own surface on loopback and passes its
     journeys with cw-rails absent. The mesh's absence costs reach, never an
     answer the program can give alone. That is TOPOLOGY §3.5's ring rule,
     which places a capability by what its absence costs.
   - cw-rails holds what makes a node a member: the node key, the one
     endpoint, the roster, peer admission and advertisement. It adds these
     to programs from outside:
     - it forwards each traffic class to the loopback origin that class's
       owner registered;
     - it carries the peer's identity in `X-Mesh-*` headers;
     - it advertises what the origins declare, as of each origin's latest
       register or renew (phase-b-76);
     - it measures the node's hardware and live load itself, through the
       one detector (`commonwealth_discovery::hardware`): the machine is the
       node's, not any origin's, and an origin declares only what it owns
       (svrn's storage budget clamps the advertised free storage; serve
       declares VRAM) (phase-b-83);
     - it hands a local caller a loopback bridge to a peer's origin.
   - Who a peer is, is cw-rails' question. What that principal may see is
     the owning program's (DAEMON_CORE §1's `principal → Scope` table stays
     svrn's).
   - A program never holds a key, an endpoint, a roster or a peer dial of
     its own, and cw-rails never links a program's crates to host its
     feature. For anything mesh-facing, the placement test has one answer:
     the program that owns the capability (§2) serves it on loopback, and
     cw-rails forwards to it. Node compositions nest the way TOPOLOGY's
     construction variants do: `serve` ⊂ `serve` + `svrn` ⊂ … + `cw-rails`.
     Each layer adds reach and changes nothing below it.
   - The vocabulary both sides of a peer dial speak (`PeerTransport`,
     `PeerContact`, `TrafficClass`, `PeerEndpoint`) and `RailsTransport`, the
     client over cw-rails' reach door, live in the neutral leaf `mesh-reach`
     (phase-b-30). A NON-member borrowing from someone else's mesh is a
     client of that mesh, like the phone (§2a), so its guest dial lives in the
     same leaf.

## 5. What the design does not contain

Deleted, not migrated: the in-process daemon; the two-name state database;
`sovereign/SYSTEM_OVERVIEW.md` and the drift machinery that keeps it honest;
orders, campaigns, cursors, journals and demos under `.sovereign/features/`
(the work atlas and its claims STAY: operator, 2026-09-25, as an optional
code-program bundle); the canon store; notes injection; the ten-context
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

**Step 10 — de-embed.** Construction sites of `EmbeddedDaemon`
outside `sovereign-daemon` become dials through `sovereign-turn-client`.
Done when the construction census in §12 "Done" passes (phase-b-30; see the
§11 correction).

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
| bench (`eval`, `tdd`) + ingest (`authoring-harness`, moved at fp-57) | 4 | daemon, cli-llm |

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

- [ ] `sovereign-recipes/` becomes an `[ingest]` data crate that `include_str!`s
      its own files from a static list, with a list-matches-tree test; corpus-engine
      reads definitions only through its own recipe-source and asset-source ports,
      with one default-source module (five-programs-52, which replaces the earlier
      move into `corpus-engine/recipes/`; Phase B lifts the default source out).
- [ ] Delete `corpus-engine/build.rs`.
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
- [ ] Done when the construction census in §12 "Done" passes (phase-b-30:
      the grep this line named counts comments and could never pass; NOT
      cmnwlth's main — see the correction).
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
the authoring-harness drive (`run_over_frozen_sample` — an ingest dial since fp-57, row fp-43).

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
      worker_pod_provider, alignment_cmd), **svrn 16,975**
      (chat_cmd, awareness_cmd, mcp_cmd, mcp_demo_server, govern_cmd, turn_sink,
      newsworthy, mobile, reading_diag, proxy, portfolio, router_*, lib/main).
- [ ] Amended 2026-09-29 (phase-b-54): inner_chaos and voice_eval are svrn's,
      not bench's. They give each thread a fresh svrn store and plant memories
      into it, so they are svrn's white-box tests (ARCH principle 12), and they
      name no `sovereign_eval` and nothing in the bench tree names them. The
      same holds for eval_cmd's probe modes (routing classify,
      `--prod-pipeline`, rerank config): they stay in cli-llm's remainder,
      spelling unchanged (pb-bench-dials-whitebox). Only black-box turn lanes
      dial svrn, with sampling pins on the wire (pb-bench-dials-wire).
      Amended 2026-09-29 (phase-b-59, -60): promote's rerank arms are
      black-box turns, not probes; they dial with a per-turn rerank override
      on the wire, after `sampling` (pb-bench-dials-rerank), and the dial is
      three rows by proof: plain turns, that wire, document turns and stores
      (pb-bench-dials-turns, -rerank, -docs). svrn's grounding-gate
      primitives that bench uses as scorers are not turns; the ladder has no
      rung for them, so their placement is pb-cli-llm-bench-move's census,
      and a leaf for them is the operator's.
      Amended 2026-09-29 (phase-b-62): the document lanes do not dial.
      svrn serves no route that attaches an asset to a conversation, and
      `/v1/documents/{id}/ask` runs the route→execute pipeline decision
      7693f16b moved the book-report lane off, so dialing it would change
      the subject. book_report, chaos_monkey's attached transport,
      vault_report and faithfulness exec `svrn __probe` instead, with new
      `attached`, `vault-build` and `raptor-nodes` modes: svrn runs its own
      in-process build and turn and writes raw answers, chunks and a
      resource ledger; bench scores them (pb-bench-dials-docs, -vault).
- [x] Landed 2026-09-29 (pb-cli-llm-bench-move): bench's CLI is its own
      crate, `sovereign-cli-bench` (`[lib] + [[bin]]`, in `[[package]] bench`),
      holding bench_cmd, eval_cmd and quality_lane_cmd; sovereign-cli-llm links
      no `sovereign-eval`. The dispatcher's `bench_bin` execs it for `svrn
      bench|eval` and `svrn quality lane`. The bench list above is amended, by
      phase-b-60/-64's rule: a lane that exercises a program's own internals and
      names no `sovereign_eval` stays with that program. So search_gym_cmd,
      knowledge_gym_cmd and gym_judge stay in cli-llm, and so do the white-box
      lanes judge_replay, resolver_precision and `bench atlas` (src/bench_atlas.rs).
      Their `bench …` spellings are routed to cli-llm by the dispatcher. svrn's
      grounding primitives are the `assess` and `judge` modes of `svrn __probe`
      (phase-b-63), and what bench runs of ingest it execs as `svrn
      enrich|corpus …`.
- [x] Landed 2026-09-30 (pb-cli-llm-ingest-move): ingest's verbs are
      `svrn-ingest`'s, sovereign-pipeline's `[[bin]]`, with the moved modules
      at its lib root (`run_cli_verb`). The dispatcher's `ingest_bin::owns`
      table routes them there: `enrich`, `corpus`, `atlas`, `meta-atlas`,
      `recipe`, `pipeline`, `alignment` and `bench atlas`. The ingest list above
      is amended by phase-b-70 (2): a module that opens svrn's store, calls
      svrn's routes or uses svrn's tools stays in cli-llm under its unchanged
      spelling. That keeps recipe_agent_cmd, recipe_agent_live_trial,
      corpus_watch_cmd, corpus_catalog_cmd, corpus_extract_entities_cmd,
      `corpus ingest|share|pull`, `enrich raptor|raptor-index|summary-atoms`
      and `atlas budget|status|list-corpora|list-atoms|show-atom|typed-extension`
      in cli-llm. `workflow_cmd` stays too: it is a client of svrn's
      `/internal/workflows` (seat, phase-b-33). sovereign-cli-llm names no
      ingest crate, not even as a dev-dependency: of the two examples that
      built an engine, coverage_layers_probe measures ingest's layers and is
      sovereign-pipeline's, and epistemic_demo is `svrn __probe`'s
      `epistemic` mode (seat, phase-b-75).
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
      `sovereign-work-atlas`) — drop them first, that is free. (Spent: they
      left at 51cd76669.)
- [ ] PRICED 2026-09-24 (fw-4, struck; five-programs-11): the split as a
      straight move is NET-INCREASING at boundary-gate 62 — it closes ≤ 5 of
      cli-llm's 14 edges and each new crate opens edges into non-leaf [svrn]
      members (core, cli-shared, tools, store, chat_cmd's Runtime bootstrap),
      ≥ 10 together. The dials that remove the halves' svrn reach come first;
      `REVIEW-mint-fp-cli-llm-split` prices them.
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

**Where a cross-program test lives (phase-b-47).** Because the dev edge counts,
a test that drives a svrn crate AND ingest's real implementor has no home in
either package. It splits at the port. The svrn side drives one test double
per port, kept beside the port's trait in its leaf behind a dependency-free
`test-doubles` feature (one double per port, reused by every svrn crate). The
engine side re-asserts the same behaviour on the implementor, in the
implementor's crate, over the same fixtures. The composed path is proven on
the stock binary. A no-package test crate or a gate that exempts test targets
would also work, but both are operator decisions and neither is needed. The
pattern is pb-grants-merge's, and pb-ingest-dial-tools-doubles adopts it for
sovereign-tools (65 test-module sites, 10 test files, 5,143 lines at 451cc7ee2).
Phase-b-48 adds two cases. First, a test whose subject is an ingest crate
moves into that crate's own tests, where the engine is an intra-package
dev-dependency. The recipe-author tests are the example. Second, when a
program's code reads an ingest-written ON-DISK artifact directly rather than
through a port (svrn's `AtlasContextManager` opens atlas stores through the
leaf's openers), the artifact becomes a checked-in fixture. It lives in the
leaf that reads its format, behind the same `test-doubles` accessor, and has
one writer: an engine test regenerates it and asserts the fresh store and
the checked-in one read the same.
Phase-b-52 adds a third. Where the implementor's read is itself a delegation
to the leaf's reader (`CorpusEngine` lists indexes through
`corpus_index::FsIndexSource`), the double delegates to that same reader
over the test's fixture dir rather than re-implementing it, so the double
holds no second copy of the decider. The engine side then re-asserts only
the behaviour the engine adds past the delegation.

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

### Probe REFUSED 2026-09-21 (80 at a546a456b)

- **`sovereign-cli-dev -> sovereign-daemon`** (3 refs, `project_cmd/serve.rs:670,679,700`):
  `svrn project serve` stands up a SECOND in-process MCP server
  (`mcp_router::{FeatureRoot, McpNotifier, mcp_router}`) scoped to the project
  root, with a `SpecWatcher` firing `notifications/tools/list_changed`. The
  daemon already IS an MCP server (`/mcp`, mounted on its client router,
  daemon.rs:3753) but its `FeatureRoot` is not project-scoped and it has no
  spec-watcher notifications. Missing capability before this edge closes: the
  daemon's `/mcp` must take a project/feature root (and expose the list-changed
  fan-out), or `project serve` becomes a thin proxy onto it.
- Orphan from the harness fix (a546a456b): `sovereign-mesh-test-harness`'s
  `MockLlamaServer` has no consumer left after the 15 host-route tests were
  dropped; it should move to the daemon's suite or be deleted.
- **`sovereign-cli-daemon -> sovereign-inference`** (29 refs): `setup_planner`
  (11 refs) is fully portable — its `crate::hardware::ProfileName` and
  `crate::{validate_gguf, GgufExpectation}` are contract re-exports — but moving
  it clears zero gate count. The edge is carried by 18 others: `rpc_worker_main`
  (lib.rs:162), `llama_logs` (lib.rs:130), `hardware::detect_hardware`
  (fim.rs:45, needs llama.cpp), `capacity` (vram_plan.rs:22), `smoketest`.
  The fp-25 row's proposed daemon dial for hardware detection is FALSE for
  first run: `setup_cmd/mod.rs:120-130` serves `setup --plan --json` before a
  daemon or config exists; `daemon_cmd/mod.rs:201-239` gates daemon startup
  on that setup. The post-setup hardware route already exists in
  `sovereign-daemon/src/assets_http.rs:52,77-91`; the daemon binary already
  owns an RPC-worker entry (`sovereign-daemon/src/bin/sovereign-daemon.rs:64`). Preserve the
  first-run standalone setup surface, then separate any post-setup dials from
  binary ownership before removing the cli-daemon inference link. A daemon
  route cannot serve a setup command that runs before the daemon.

- **`sovereign-tools -> sovereign-enrichment-catalog`** (fp-28): the TSV's
  `list_enriched_corpora_in` reader-port premise has zero callers in tools;
  `sovereign-daemon/src/enrich_http.rs:61` is its caller. Tools instead load
  full configs (`atlas_context_manager.rs:57`,
  `local_corpus/atlas_dispatch.rs:83`) and constructs one for watched folders
  (`local_corpus/watched/enrich.rs:50,98-125`). A list-only trait would leave
  the dependency red. Reclassify those reads and the writer before cutting.

### Premise checks REFUSED 2026-09-23 (68 at faddc360d) — three more, all recorded

Found by the director's resolution session while verifying fp-25's package.
Dispositions and full evidence: `ralph/DECISIONS.md` entry `five-programs-2`.

- **`sovereign-cli-llm -> sovereign-cli-mesh`** (fp-26, 1 ref): the row's
  GRANDFATHER verb came from TSV `missing_capability = keep`, which §12
  decision 6 already reversed — `guest_route::open_route` is "wire, not keep",
  and the same TSV row's `fix_shape` cell says `route`. The ONE ref is
  `chat_cmd/config.rs:315`. The dial cannot land yet: the daemon's guest
  surface (`routes_guest_ask.rs:50`, `routes_guest_session.rs:52`) has no
  tunnel-open route. REFUSED alternative — moving `open_route` into cli-llm
  needs `sovereign_mesh::guest_tunnel` and duplicates the one security decider
  (principle 8; D6's own last line). Ordered behind fp-8.
- **`sovereign-tools -> sovereign-recipe-author`** (fp-29, 2 refs): there is no
  `FeatureStore` in either crate or in contracts — it is an ATOS concept that
  left corpus-engine (`corpus-engine/src/lib.rs:220`), so the TSV's
  `missing_capability` misnames the subject. The refs are to a concrete sqlite
  `RecipeProjectStore` (`sovereign-tools/src/bundles.rs:445,486`). "Drop the
  pub use shim" is NET +1 and was reproduced: `sovereign-tools` is the only
  Cargo.toml in the workspace naming `sovereign-recipe-author`, while 7+ sites
  in sovereign-cli-llm and sovereign-daemon ride the `pub use` at
  `sovereign-tools/src/lib.rs:68`. A port cannot close it either — the
  constructor is svrn's (`sovereign-daemon/src/daemon_cmd/boot.rs:790`) and a
  program-owned store is never a leaf. §12 decision 2's dial is what remains;
  ordered behind fp-7.
- **`sovereign-cli-dev -> sovereign-store`** (fp-32, 2 refs): the port half had
  ALREADY landed — `ConversationStore` is in `sovereign-contracts`
  (`traits.rs:1030`) — and cli-dev is the composition root of its own
  `cmd_audit_recover`, so the TSV's "construction at composition root" is
  vacuous. §4 rule 1 decides the shape instead (`state.db` is svrn's, resolved
  under `sovereign_cli_shared::dirs::sovereign_root()`,
  `audit_recover.rs:364-378`). Not a refusal in the end: the row is buildable
  with NO new route — `recover_inferred_with_store` (`audit_recover.rs:273`) is
  already the pure loop over `&dyn ConversationStore` and uses exactly
  `list_conversations` and `get_conversation`, both served at
  `turn_http.rs:123,125`.
- **`sovereign-cli-dev -> corpus-engine`** (fp-34, 16 refs, the same day's
  second package — entry `five-programs-4`): the fix cell "daemon project
  index route" names a capability that already exists (`POST
  /internal/corpus/{corpus}/index/build`, `corpus_catalog_http.rs:176`,
  already desktop-dialed) and a caller that does not — cli-dev has NO
  index-build site; its build verbs are cli-shared's `code index` (fp-5's
  recorded D5 keep) and sovereign-cli's `project init` (§11:1197-1203's
  closing condition, the NEEDS-OPERATOR line). Build arm struck; the row
  rescoped to the read-mount proxy client, `depends [fp-11]`;
  `code finalize`/`code watch` need NEW daemon surfaces and stay
  NEEDS-OPERATOR (new capability is the operator's, principle 11).

Also corrected 2026-09-23: **fp-8's row named the wrong TSV pairs** for its
guest_route half. `sovereign-cli-llm -> commonwealth-state` (2 refs) is
`MeshStore` in newsworthy/portfolio and `-> sovereign-mesh` (12 refs) is
pinned-pod/persist/capabilities/canonical_pull — neither carries a guest_route
ref. fp-8's VERB (the daemon's guest tunnel source) is still the right owner;
the cli-llm client side is fp-26's pair.

Corrected 2026-09-24 (director, five-programs-19): **fp-8's pump half was
already done** — fw-1/fp-54 put the rail journal behind `RailsRingRail`, and
the pump's local `outbox_take` is D4's store flip (fp-42). What fp-8 keeps is
D6's serve half: one loopback daemon door that returns the base URL of the
tunnel `StoredGuestLink` opens for the stored link. fp-26 dials it. The cost
is that a guest with a stored link now needs a local daemon to chat, which D6
implies and no row had priced.

## 12. The decision sheet — front-loaded so implementation is mechanical

`docs/FIVE_PROGRAMS_DECISIONS.tsv` records the §12 edge inventory, one row per edge:
source, target, refs, use shape, fix shape, missing capability, prerequisite,
behaviour delta, the ONE decision, effort. It was produced by three read-only
scouts over the gate output; it is data, not prose, and it replaces re-probing.
Six premise checks (§11) have now found six misclassified cells — fp-25,
fp-26, fp-28, fp-29, fp-32, fp-34 — five of them dispositioned by the director
2026-09-23 (ralph/DECISIONS.md `five-programs-2`, `five-programs-4`); only
fp-28's writer/reader boundary is still an open decision, and it is the
operator's. **Check every
row's premise against its live callers before using it as a mechanical task.**
That step is load-bearing, not a formality: the misclassifications cluster in
`missing_capability` and `fix_shape`, the two cells a scout wrote from the gate
output rather than from a caller graph.

### The classes (by fix shape, not by source crate)

| class | rows | what it is | how it lands |
|---|---|---|---|
| **Atlas read surface** | 8 (tools 286, cli-llm 355, core 65, corpus-mcp 32, daemon 179, meshapp 11, mesh 6, cli-dev 16) | `corpus_engine::enrichment` read refs (~950) | ONE leaf decision + a module carve |
| **Serving-cluster dial** | ~20 (all `sovereign-daemon` -> cmnwlth/ingest/code crates) | the daemon calls fabric/engine/queue/watcher/registry code in-process | the process-boundary decision, then route+client+repoint per edge |
| **Vocabulary leaf promotions** | ~10 (rail-core, core-subset, transport, work-model, serving-policy, scheduler, peer-wire dep, watcher schema, cli-shared thin half, notes types) | pure vocabulary reached across programs | `[[package_leaf]]` rows + allow + repoints (the scip pattern: 7 edges in one manifest edit) |
| **Ports** | ~8 (state, meshapp, meshapp-registry, runtime-commission, recipe FeatureStore, gliner, watchers, notes) | a trait in contracts + impl in owner + injection | only closes when the consumer stops constructing; construction stays with the owner. For every `sovereign-daemon` pair this WAS vacuous while the only composition root, `sovereign-cli-daemon`, was itself [svrn], so the constructing crate kept the edge (delta 0 — fw-2, five-programs-10, 62 at 0aacc0818). The stock `[[distribution]]` root sits outside every package (phase-b-29, -30), so a port now closes the edge: serve's provider and ingest's engine are composed there |
| **Placement** | 5 (corpus-mcp, work-atlas, runtime-recipe, cli-dev's notes/tools, cli-llm split) | which program owns a crate | a membership row, then the crate's edges follow |
| **Structural / dial-only** | ~8 (daemon->mesh 250, ->inference 42, ->compute 25, ->code 34, cli-dev->daemon, cli-llm->pods, grants->corpus-engine, mesh->corpus-engine) | in-process construction of another program's runtime | a process or a wire; the largest single-effort rows |
| **Keep** | 4 (guest_route, project init's zero-vector index, the 3 notes tests, cli->cli-mesh) | deliberate non-changes with reasons | document, do not cut |

### The decisions, taken — each against ARCH_PRINCIPLES.md

**1. The atlas reader: a NEW thin reader leaf, read-only; the vocabulary door
stays in `understanding-vocab`.** Principle 11 forbids building new before
citing why the existing surface cannot serve, and here the citation is exact:
`understanding-vocab` is depended on by `oicp-types`, the wire floor. The
resolved-atlas reads (`summary`, `store`, `ann_store`, `context`, `inventory`,
`provider`) need arrow/lancedb/memmap2, and pulling a store under the wire floor
is the line drawn wrong (12: "look where the ability is granted — the
dependency"). The leaf holds RAW reads (`corpus-index` precedent, §11's "thin
reader"); `ground`'s selection POLICY (`candidate_atlas_ids`, walk choice) is
the consumer's decision and moves to the svrn side (12: "a gap in one thing is
not a job for another" — the ingest program owns the atlas, svrn owns what it
grounds on). Carve one module set at a time (2).

**2. The serving cluster is cmnwlth's own process; the daemon DIALS it — no
leaf promotions for program substrate.** Principle 12: a daemon owns its data
root and its conversations; the mesh owns the roster, the model server owns its
weights, idle policy and restart. The daemon currently supervises/embeds all
three, which is "a component holding another's lifecycle" — the line is wrong.
Principle 11: `cw-rails` already exists and already serves `/v1/mesh/media`,
`/v1/mesh/publish`, `/v1/mesh/{status,app,fanout}` — extend it, do not build a
new binary. Principle 6: with the serving process absent the route reports
absence ("a daemon alone serves no model"), never a silent fallback. Principle
10: the boundary becomes the import/wire, not a remembered rule. This rejects
the alternative reading — promoting `commonwealth-core`/`-transport`/`-work` to
leaves would make every program co-own cmnwlth's substrate, which is the
"ability granted" test failing.

**3. Leaf widening: only wire FORMAT/VOCABULARY moves, into the two owners
`§4 rule 6` already names (`oicp-types`, `sovereign-contracts`) — no program
crate is promoted.** Principle 8: a schema or wire constant must have ONE
definition; moving it into the shared leaf is how that becomes structural.
Principle 12: a wire type is shared vocabulary (both ends own it); a program's
substrate is not. So: rail wire types (`RailAct`/`Admission`/`Roster`/`Payload`),
`sovereign-peer-wire`'s ring-sync types, the watcher `projects` schema, and the
notes DTO set move to contracts; the crates that produce them stay where they
are. The scip promotion already proved the mechanism (7 edges, one row) — but it
was legitimate there because scip is a format/read port, not a store (11).

**3a. The third vocabulary owner, and the N+1 ladder (operator-approved
2026-09-23).** Decision 3 named two vocabulary owners; `mesh-join-vocab` is the
third, admitted because the join-key + deep-link format must be named by
`commonwealth-discovery` and `commonwealth-rails`, and the
`commonwealth-discovery/rails → sovereign-*` forbid rows (no except) wall every
sovereign-named home off from them. This is the ladder an N+1 feature follows —
first match wins:

1. One program uses it → that program's crates (§2's table; `boundary-gate`
   enforces the map).
2. Two or more programs speak it on a wire → shared vocabulary:
   - federation wire (what a node advertises to strangers) → `oicp-types`;
   - content identity/provenance, brand-free atoms → `kernel-types`;
   - svrn serving contract (turn/wire DTOs, ports) → `sovereign-contracts`;
   - a format both families speak that contracts CANNOT host → a new neutral
     leaf (`mesh-join-vocab` precedent). Operator decision, requiring all
     three: a named refusal of each existing home, the two programs that share
     the vocabulary, and the leaf count shown in the burn-down;
   - none of these → it is not vocabulary: dial, port trait, or split (the
     class table below). Never a leaf. The leaf test: no fs, no store, and a
     dep budget a third-party lifter would pay anyway.
3. A leaf stays honest by the same test re-applied at every later touch.
   The follow-on queue (`ralph/next/phase-c/`) re-applies it to
   `sovereign-contracts` itself, a 42k-line crate that 51 manifests name. Its
   single-program modules move to their owners, and traits whose implementer
   and consumer are the same program move into that program. Phase B deletes
   only what nothing reaches (phase-b-2).
4. **The one mechanism rung (operator, 2026-09-25, phase-b-1).** A mechanism
   that every program's binary needs about ITSELF goes to the host kit (§2c).
   Examples: its lock, its data root, its server shell and MCP dispatch. It
   qualifies only if every path is supplied by the caller, it owns no store,
   and it names no program's vocabulary. The kit is the only leaf allowed to
   do fs and process work. It has a size cap, and the cap is not ratcheted.

**4. The replicated store: cmnwlth owns the disk; the daemon keeps a read-through
cache and dials.** Principle 12, second clause verbatim: a gap in one thing is
not a job for another — the in-memory `MeshStore` is the mesh's missing durable
owner, not a licence for the daemon to hold it. Principle 6: the reverted wire
was reverted BECAUSE it silently stopped persisting; the rule is that an absence
is reported (a named refusal), never defaulted. Until the durable owner exists,
`portfolio`/`newsworthy` keep their file store and their edge stays red — that is
the honest state, not a fix.

**5. Placement — read off §2's own table (principle 11: the inventory is the
authority).**
- `corpus-mcp` stays `[svrn]`: §2 lists it as svrn's own ("exists today as
  `corpus-mcp/` + the turn path"). Its `corpus-engine` refs are the atlas-read
  (row 1) and an ingest dial — not a re-home.
- `sovereign-work-atlas` -> `[code]`. It owns agents' claims about their own
  work; §2 gives code the agents' metadata (SCIP index, notes), and the three
  tools are mandated by AGENTS.md, so 11 says place a used capability rather
  than delete it. The daemon dials the three MCP tools.
- `sovereign-runtime-recipe` -> SPLIT (12: the runtime assembly is svrn's own
  lifetime; the ingest lane is ingest's). Assembly to `[svrn]`, lane stays.
- `sovereign-tools::notes` (`patterns`, `diff_extract`, `response_mine`) and
  `sovereign-cli-shared::{code_index, scip, observation, rail}` move to their
  owning programs — `notes` and `code_index` are §2's code program's own state
  ("owns SCIP index, notes"). cli-shared keeps only the thin dispatcher helpers.

**6. The keep/turn list — every abstention has the run that demanded it (5).**
- `project init`'s FTS-only zero-vector index: **keep.** The alternative silently
  changes what lands on disk at install time (6), and routing init through the
  daemon makes the daemon a prerequisite of installation — a lifecycle inversion
  (12). §11:961 already records it as deliberate; this confirms it.
- The three real-SQL tests beside `corpus-engine-notes`: **move them to the
  owner's test tree, not grandfather.** Principle 12 (the gap is the owner's)
  and 5 (faking them swaps real-SQL proof for a green gate — so do not fake;
  relocate).
- `guest_route::open_route`: **wire, not keep.** It parks a tunnel handle in a
  process `OnceLock` — holding a handle is ownership (12), and the mesh owns the
  tunnel; cli-llm dials the daemon's guest surface. Duplicating it is forbidden
  by 8 (one security decider).

### Front-loading procedure

1. Operator answers 1-6 (the sheet's `decision_needed` column is the input).
2. **Phase A — atlas carve** (after 1): 3-4 waves; closes 8 edges / ~950 refs.
3. **Phase B — the serving-cluster dial** (after 2, 3): make the serving binary
   own the verbs `cw-rails` already exposes, promote the vocabulary leaves, then
   route+client+repoint each daemon edge. ~20 edges; the largest single phase.
4. **Phase C — the state wire** (after 4) + **Phase D — the mechanical batch**
   (leaf rows, ports; parallelisable) + **Phase E — placement moves** (after 5).
5. Each row lands as a cutter task with NO decision left; 3 cutters/wave; the
   gate's raw count in each commit body.

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
      ("is anyone else on the mesh touching this?"). CORRECTED 2026-09-25:
      `work_in_flight` returns live file-level Observations, and the session
      boot brief renders them (`sovereign-code/src/brief.rs:298`). DECIDED
      2026-09-25 by the operator: KEPT, as an optional code-program bundle that
      dials cw-rails' KV directly (phase-b-1). Consumers: `sovereign-cli-dev` 4 files,
      `sovereign-daemon` 3, `sovereign-code` 1.
      **It is agent-facing, which makes it `code`'s** by the same argument that
      puts `symbols`/`callers` there. Blocked on its own deps: `sovereign-core`
      (svrn — may be re-export-only after phase 1) and `commonwealth-state`
      (dev, cmnwlth). Decide after phase 1 re-measures it. The live alternative
      is that nothing on this mesh has more than one agent at a time and the
      crate is inventory, in which case it joins atos.
- [x] **`corpus-mcp`'s membership.** Stays `[svrn]` (decision 5) as the
      `serve` verb: ask, search and atoms over MCP. Its `ingest` and `recipe`
      verbs are ingest's (rung 1) and move to ingest's one CLI, taking the
      `sovereign-enrichment-build` edge and the manifest's defence of it with
      them; `serve`'s install-if-absent is the ingest dial decision 5 names.
      21 non-test `corpus-engine` lines at f7238e6d3, not 32 (phase-b-30).
- [x] **`bench`'s leaf budget.** Expressed as `[[package]] bench`'s
      `leaf_budget` (quality/ARCH_LAYERS.toml:1437; evaluated in
      quality/arch-layers/src/packages.rs:231 and pinned by xtask
      boundary_gate.rs:651). `understanding-vocab` is in it: the operator
      admitted it (HUMAN-fp58 (a), 2026-09-24), which supersedes the last line
      below. The original question: the one `[[forbid]]` row §9 admits it cannot
      express — `bench -> *` except `oicp-types` and `sovereign-contracts` —
      needs a per-package leaf budget in `quality/arch-layers/src/packages.rs`,
      not a hand-copied membership list. Contents decided 2026-09-24
      (five-programs-12): the pair plus `kernel-types` (3a's identity home),
      `sovereign-time` (empty deps; clock-gate routes at it) and
      `workspace-hack` (hakari plumbing) — none adds a crate to the closure.
      `understanding-vocab` stays out: it is ingest's language and reads fs.
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

- [x] `sovereign-core/src/router_calibration.rs:1253` reads the one bank now
      inside `sovereign-core/data/calibration/` (fp-49). `router fit` and the
      fitter point at the new path; the `calibration-fit/` baseline key stays.
- [x] `sovereign-code → corpus-engine` (dev) — CLOSED fp-51 (2026-09-23):
      `tests/e2e_code_intel.rs` moved WHOLE to `sovereign-daemon/tests/main/`
      (the daemon already links both crates; fixtures are inline TempDir
      strings, nothing faked, no path rewrites); the dead
      `exercise_code_tools` example deleted with its deletion-manifest entry;
      sovereign-code drops the corpus-engine/arrow/parquet/filetime dev-deps.
      The `treesitter` cfg stays correct: the gate scripts resolve
      `sovereign-daemon/treesitter`. boundary-gate 70 → 69.
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
one now deleted with atos — plus two already in flight) **and to 16 on
2026-09-23** (`mesh-join-vocab`, decision 3a). Leaves are 21% of the
governed set, and every promotion widens all five packages at once. **A path to
zero that promotes another twenty leaves reaches a number that means nothing.**
The test, applied on the day and to be applied again: a leaf is shared
VOCABULARY or a thin reader with a one-or-two-crate in-repo budget, never a
store a program owns on disk. That is why `corpus-engine-notes` was NOT
promoted even though one row would have closed eight edges. The operator
admitted `mesh-reach` on 2026-09-27 (phase-b-30) after every existing home
refused the peer-dial vocabulary; its falsifier is any WORKSPACE dependency
beyond `kernel-types` and `workspace-hack` (phase-b-33). Third-party crates
come behind the feature that needs them, and the guest dialer's iroh endpoint
machinery is one such feature, `guest` (phase-b-35).

### Done is three conditions, not one

- [ ] `cd corpus-engine && cargo xtask boundary-gate` exits 0.
- [ ] `EmbeddedDaemon::new` is called in non-test code only by svrn's own
      process entry (boot and the `admin join` one-shot) and the stock
      `[[distribution]]` binary, pinned by a workspace construction census
      (phase-b-30; the grep this line used to name counts comments, so it
      could never pass). Owner: pb-distribution.
- [x] `bench`'s per-package leaf budget exists and the row is expressed
      (phase 7), so the evaluator cannot link the thing it measures
      (quality/ARCH_LAYERS.toml:1437).

Phase B adds the conditions that make "take THIS without THAT" true
(phase-b-1):

- [ ] No `[[exception]]` row with `package = "svrn"` remains (fp-9, fp-10,
      fp-68, fp-69 retired by building the owner, never by exception).
- [ ] Every program passes its own lift sandbox, meaning it builds and runs
      with only its shared leaves: `svrn`, `ingest`, `cmnwlth`, `serve`,
      `code` and `bench`.
- Moved to phase-c (operator, 2026-09-27, phase-b-32): each drive in §2c
  has one implementation. Phase B never ADDS a copy of a drive; collapsing
  the copies that exist is phase-c's pc-daemon-adopts and its siblings.
