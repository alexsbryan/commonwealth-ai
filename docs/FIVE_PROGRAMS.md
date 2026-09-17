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
