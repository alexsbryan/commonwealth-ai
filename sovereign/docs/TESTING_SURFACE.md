# Daemon Testing Surface — Audit + Priority Matrix

**Last audited:** 2026-05-13 (rounds 1+2+3+4 + harness extraction + fan-out bugfix + final-P0s + **P1/P2 binge**). Refresh whenever a row's coverage changes,
a capability lands, or a deferral resolves. Out-of-date rows are a bug
per ARCH §1.1 — feature docs describe *intent*, and this doc's intent
is to drive the next test.

## What this is

A living inventory of every capability the `EmbeddedDaemon`
(`sovereign-mesh::daemon`) exposes through HTTP, lifecycle methods, or
background tasks — with its current test coverage and its
user-facing impact-of-regression score. Bucketed so engineers can pick
the next gap with the highest ratio of (impact saved) / (cost to test).

This is the source-of-truth for "what test do I write next?". Reach
into it before designing a one-off; the matrix may already show that
a peer test exists or that the gap is intentionally deferred.

## Layering principle

A test belongs at the lowest layer that can catch the failure mode
it's targeting. Subsystem integration (real HTTP + real SQLite +
mocked external) is the sweet spot for daemon work — unit tests miss
wiring bugs, true E2E is slow to write and slow to run. ARCH §12 holds.

The four layers we use:

| Layer | Shape | Lives in | Catches |
|---|---|---|---|
| **L1** Unit | Pure function or single struct, no I/O | `src/*.rs` `#[cfg(test)] mod` | Math bugs, type bugs |
| **L2** Subsystem integration | Real HTTP/SQL/MeshStore on ephemeral port, mocked inference + corpus | `tests/*.rs` | Wiring bugs, route bugs, persistence-on-mutation bugs, OICP decisions |
| **L3** Multi-daemon E2E | Two+ `EmbeddedDaemon` instances on distinct ports, gossip + handshake over real HTTP | `tests/*.rs` (unblocked by the port-config landing) | Cross-mesh convergence, distributed state, peer-routing decisions |
| **L4** Real-binary smoke | `cargo run --bin sovereign daemon run` against a curated fixture | manual playbook or `--ignored` tests | mDNS, launchd/systemd, real GPU inference, real Tailscale topology |
| **L4j** CLI journeys | The ordered command sequences a person actually types, declared as `[[journey]]` in `docs/cli-contract.toml` | `tests/cli_contract_journeys.rs` (static) + `tests/cli_journey_dispatch.rs` (offline) + `scripts/cli-journey-verify.sh` (live, via `cli-journey-sandbox.sh`; nightly timer + pre-push gate 4) | A use case breaking even though every verb still exists — a renamed step, a mutation that stops reversing, a command that exits 0 while doing nothing. Also *absence* of coverage: a journey that executes no steps reports `∅`, never `✓` |

**On L4j.** The `[[command]]` rows in the contract prove each verb *exists*;
they cannot prove a *sequence* works. Until 2026-07-28 nothing did:
`corpus install` → query → `corpus remove` was unverified anywhere, and the
only behavioural probes were four read-only commands asserting `exit == 0`
that nothing ever ran (`SOVEREIGN_LIVE_CONTRACT` appears nowhere but inside
the script that reads it). The static and offline tiers ride the normal
workspace test run at no extra cost; the live tier needs models and is an
operator gate, but its *runner* is covered in CI by
`scripts/tests/cli-journey-selftest.sh`, which proves on staged input that it
rejects a wrong exit code, a missing substring, and a mutation that did not
reverse. A harness nobody has seen fail is not evidence.

**On what a journey is FOR (2026-07-29).** The ratchets above are verb-driven —
"every public verb belongs to a journey" — so for a year the manifest's unit was
the COMMAND, and journeys grew into vehicles for verb coverage. `code-intel-
lifecycle` is the tell: six steps (`project init | list | status | refresh |
serve | stop`) that prove the index *builds* and never once ask it a question.
Measured against the 23 tools `.claude/CLAUDE.md` mandates for every agent
session, 18 were named by no journey step at all — including the five the
instructions say to use *instead of reading files*. Nothing was failing; nothing
was watching.

So the manifest gained an `[[experience]]` axis: a promise the product makes,
citing where it is promised, listing the **capabilities** it is made of.
`every_capability_is_exercised` then asks the question the verb ratchet cannot —
is each part of this promise driven by a step that asserts *output*? Exit codes
cannot carry it, because **every** code-intelligence tool in this repo exits 0
when it finds nothing (`symbols` on an unknown name prints a helpful paragraph
and exits 0). An experience with no journey at all is *declared* as a gap and
capped, so `code-intel-chat` is a named debt rather than a future discovery.

The pattern this surfaced generalises: most experiences have a PRODUCE half
(`project init`, `drift detect`, `enrich build`) that is expensive, mutating and
often `skip_live` — and it is plumbing — and a CONSULT half (`symbols`,
`drift_posture`, `enrich query`) that is cheap, read-only and deterministic, and
*is* what the user came for. Every experience that had only a produce journey was
testing that the machine starts, not that it works.

**Running the live tier.** Two lanes, both operator gates:

```bash
# read-only, against whatever daemon you already have. Safe anywhere.
SOVEREIGN_LIVE_JOURNEYS=1 sovereign/scripts/cli-journey-verify.sh --tier 2

# mutating, against a daemon this script boots and owns.
sovereign/scripts/cli-journey-sandbox.sh
```

The read-only lane can only verify a journey's read-only *prefix* — on its
first run 13 of 15 runnable journeys reported `partial`, because the steps
that carry the meaning (`corpus remove` and the "prove it is gone" assertion
after it, `daemon stop`, `notes add` then read it back) are exactly the ones
too destructive to run against a real `~/.sovereign`. `cli-journey-sandbox.sh`
supplies what the runner deliberately refuses to invent for itself: a private
netns, a throwaway `HOME`, mDNS off, iroh kill-switched, port 19741, and the
small soak models — the isolation pattern `scripts/daemon-soak.sh` has run
since 2026-07-18. It drives **one journey per runner invocation** and revives
the daemon between them, because `first-run` legitimately ends in
`daemon stop` and every journey ordered after it was otherwise failing for a
reason that had nothing to do with what it tested.

Fixtures that would cost a large download stay UNSET, so their steps report
`skip … no fixture` in the step log rather than passing silently. Opt in per
run: `JOURNEY_CORPUS=sep sovereign/scripts/cli-journey-sandbox.sh`.

**The two lanes partition the manifest; neither drops a journey the other also
drops.** Some journeys need state a throwaway sandbox cannot have — the
operator's real `HOME` (Claude transcripts, an accumulated notes db, a drift
report) or a live code index and SCIP graph. They declare it:

```toml
needs = ["indexed-repo"]   # or "operator-home"
```

The sandbox lane passes `--lacks operator-home --lacks indexed-repo` and the
runner drops those journeys whole, printing the manifest's own reason. The
nightly then derives that same set from the plan and runs it READ-ONLY against
the operator's daemon, where the state actually is — a lane whose verdict is
carried separately (`capability_lane` in `latest.json`), and where a daemon that
is simply down reports `no-daemon`, never a pass. Before this, the exclusion was
a hardcoded array of two journey ids inside `cli-journey-sandbox.sh`: a property
of the *journey* living in one lane's script, needing a hand-edit for every
future journey with the same requirement, and invisible to anyone reading the
manifest.

The cheapest journey in the manifest is in this set. `code-intel-answer` asserts
that `symbols`, `callers`, `callees`, `blast` and `code_search` return *true
facts about this repo* — no models, no network, no mutation, five steps, and this
repo is its own fixture. Every assertion is a fact rather than
`stdout_non_empty`, because non-empty is precisely what a non-answer looks like
here.

**Verdicts, and why a journey count is not coverage.** Both lanes report one
of five verdicts per journey, each carrying `ran/declared steps`:

| | meaning |
|---|---|
| `✓` passed | every declared step ran (bar manifest-declared `skip_live`) and at least one asserted something. When some steps asserted nothing, the line says so: `✓ id (6/6 steps; 2 asserted, 4 asserted NOTHING)` |
| `~` partial | ran, but a precondition was skipped — the sequence is not proven |
| `⊘` unproven | steps **ran and not one of them asserted anything.** The binary was invoked; nobody looked at the output |
| `∅` vacuous | **nothing ran.** This journey is evidence of nothing |
| `✗` failed | a step asserted something untrue |

`⊘` was added 2026-07-30 and it closes the last hole in this ladder. The step
lines had been honest since the day before — a step with no `expect` block prints
`· ran, asserted nothing` rather than `✓` — while the *journey* they belonged to
still printed a green tick, because the verdict was derived from "did every
declared step run" and a step that cannot fail runs perfectly. `code-intel-
lifecycle` shipped exactly that shape: `✓ 6/6` over four steps that declared
nothing. A reader who trusts the summary line — everyone, on a 30-journey lane —
was reading a proof that did not exist.

`∅` exists because the first full sandbox run reported `29 ok, 0 failed` while
four journeys had executed *zero* steps and only 28 of 57 declared steps had
run at all. Every skip was legitimate and every skip was logged — but nothing
aggregated them, so a manifest of unexecuted assertions read as coverage. That
is the same vacuous-green class this layer already catches twice elsewhere (a
folded `2>&1` stream letting stderr satisfy `stdout_contains`; a typo'd
`--journey` reporting `0 ok, 0 failed`), and it had reached the summary line of
the tool that catches it. The precedent for the fix is `sovereign-test.sh`
exiting 4 on a zero-test run: **a zero-step journey is never green.**

So both lanes now print **two** coverage lines, and the gap between them is
the point:

```
coverage 42/57 steps in journeys this lane ENTERED (73%)
manifest 42/121 steps in the WHOLE manifest (34%)
```

The first number is a percentage of what the lane was *willing to attempt*.
Journeys dropped whole by `skip_live` — 14 of 29, 60 steps, everything that
needs a second machine, paid GPU pods, or tens of minutes of inference — used
to leave *both* sides of that ratio, so they cost nothing and the number read
almost twice as good as the truth. Quoting the attempted ratio alone is the
same move as a ✓ on a journey that executed nothing, one level up. **The
manifest ratio is the real behavioural coverage of the CLI.**

**Built-in fixtures.** Both numbers moved (23% → 34% manifest, 49% → 73%
lane) when `{corpus}` and the MCP server stopped being unset:

- `sovereign/tests/fixtures/journey-corpus/` + `journey-corpus.recipe.toml` —
  three small documents installed through a **real recipe**
  (`acquire.type = "local_file"`), so `corpus install` runs its genuine
  daemon-side path with no network. `{corpus}` was the most demanded token in
  the manifest by a factor of four; unset, it alone left four journeys
  executing nothing.
- The MCP fixture is the product's own `svrn mcp demo-server`, booted by the
  sandbox — a real reference server rather than a second protocol
  implementation to keep in step.

Point `JOURNEY_CORPUS` at a catalog id (`sep`) to prove the download path too;
that skips fixture seeding entirely.

**`settle_secs`.** `corpus install` POSTs to the daemon and returns before the
ingest lands, so the next step asserted instantly and failed for a reason that
had nothing to do with correctness. A step may now declare `settle_secs = N`:
the assertion is unchanged and must still hold, but the step is re-run for up
to N seconds and the runner prints how long it actually waited
(`✓ [2] corpus status (settled after 1s)`). It is not a flake allowance —
steps without it are checked exactly once, and the selftest pins both
directions.

What the other 77% is *not* is unverified. Every step in every journey,
including the `skip_live` ones, is proven by the static and offline tiers to
name a command that exists, is documented, and dispatches. The gap is
specifically behavioural: nobody asserts what those sequences *do*.

Both lanes exit **4** when a journey executed nothing. Exit 4 is deliberately distinct from 1: nothing is
broken, but nothing was tested either, and the fix is a fixture rather than a
bug. The read-only lane prints `∅` but does *not* exit 4, because a journey of
nothing-but-mutating steps runs nothing there by construction; making that
permanently red would be a different way of not being read.

A missing fixture now demotes a journey to `~ partial` rather than leaving it
`✓`, on the same reasoning that already applied to a skipped mutation: the
step's precondition never happened, so nothing downstream of it is proven. A
manifest-declared `skip_live` does *not* demote — that is the author's stated
scope, not this lane failing to supply something — but it is counted and named
on the verdict line, because a silent 6/7 is how coverage quietly leaks away.

### The CLI quality surface

**One front door: `svrn contract`.** Everything in this section existed before
2026-07-30 and none of it was findable. The experience map lived inside
`cargo test -p sovereign-cli --features dev-tools --test cli_contract_journeys
print_the_experience_map -- --nocapture`, which nobody guesses; the lanes are two
scripts under `scripts/`; the nightly's verdict is a `latest.json` under
`~/.sovereign/`. A quality surface that cannot be found dies the same death as one
that is never run, and this repo has the receipt: the harness this layer replaced
was written, documented as "safe to call unconditionally in CI", and then called
by nothing at all.

```bash
svrn contract           # promises + census + last nightly + how to run each lane
svrn contract map       # the experiences and the journeys serving each
svrn contract census    # how much of the manifest can actually fail
svrn contract nightly   # did anything run on this host, and when
```

It renders through `sovereign_cli_shared::cli_contract_report`, which is also
what the cargo test prints — one census, one renderer, so the number a developer
reads is the number the gate enforces. `svrn contract` is itself a journeyed
promise (`cli-quality` / `contract-audit`): a quality surface that is not covered
by the harness it describes is claiming an exemption it has not earned.

**What can actually fail — the number to read.** A step is only evidence if some
lane runs it *and* somebody checks the result. Splitting the manifest on those two
questions (2026-07-30):

```
a lane RUNS    74 steps    57 assert output    17 exit-code only     0 assert NOTHING
nothing runs   62 steps    15 assert output     3 exit-code only    44 assert NOTHING
```

The first count was a single number — "63 of 133 steps declare no assertion at
all", capped and shrink-only — and mixing the two halves was the defect. 19 of
those 63 were in journeys the lanes actually run: **false green**, a tick printed
over an unchecked command. The other 44 are in the 14 journeys carrying a
journey-level `skip_live` (a second machine, a paid GPU pod, a multi-minute
benchmark), which **no lane executes at all**. Sprinkling `exit = 0` across those
44 would have moved the headline from 63 to 19 and changed nothing about what this
repo can detect. So the two halves are now two different gates:

| gate | shape | why |
|---|---|---|
| `live_steps_all_assert_something` | **hard zero** | a step a lane runs and nobody checks is a demonstration reported as a test — and it misattributes the sequence's failure to the next step that *does* assert |
| `live_read_steps_assert_output` | **hard zero** | an exit code is not evidence for a *read* here: every code-intelligence tool exits 0 when it finds nothing, `doctor` exits 0 on a sick system by design, and `code search` shipped a stub that printed placeholder text and exited 0 |
| `every_live_journey_asserts_output_somewhere` | **hard zero** | the static twin of the `⊘` verdict — a sequence with no output assertion anywhere proves the binary starts |
| `steps_no_lane_runs_do_not_grow` | cap 62, shrink-only | never-run steps are a *written intention*. Legitimate (`pipeline-pods` provisions paid GPUs) but it must be counted, because "136 steps" as a coverage claim is off by half |

A **mutation** may assert only its exit code, and 17 do: `corpus install` POSTs and
returns before the ingest lands, `daemon start` prints its confirmation to stderr
where narration belongs. The rule that keeps that honest is the capability rule —
a read proves itself inline, a mutation is proven by a *later* step asserting
output. Every one of the 17 has that later step, and each says which one in its
`note`.

There is deliberately **no third option** for a live step. Either it declares what
it expects, or it declares `skip_live = "why"` and joins the never-run debt, where
it is counted as unproven rather than as coverage. The escape hatch is honest
bookkeeping, not an exemption.

**Paying down the never-run 62.** The cap shrinks one way: make a journey
runnable. `skip_live` is applied at *journey* level, so a cheap read-only leading
step is discarded along with the expensive tail — `correctness-gates[0] corpus
diag` and `[1] doctor` cost nothing and run nowhere. Moving `skip_live` down onto
the two or three steps that are genuinely expensive would make those prefixes real
evidence. What blocked it was harness semantics, and half of that is now settled:
a journey that runs its prefix and asserts nothing reports `⊘`, not `✓`. The
remaining question is whether a *mid-sequence* declared skip should demote to
`~ partial` — it should, on the same reasoning that already applies to a missing
fixture, since the steps after it depend on an effect that never happened.

**Running it without remembering.** An opt-in guard decays into decoration:
the predecessor harness was never run once, because running it required
setting `SOVEREIGN_LIVE_CONTRACT` and nothing ever did. Two mechanisms remove
the memory requirement:

- **Pre-push** (`scripts/pre-push.sh`, gate 4) runs the runner's negative
  controls on every push that touches the harness or the manifest — seconds,
  a stub binary and a loopback stub daemon, no models. The static and offline
  tiers ride the workspace test run in the same hook; `cli-contract.toml` and
  the journey scripts are part of that hook's `rust` filter precisely because
  editing the manifest alone can turn the suite red without touching a `.rs`.
- **Nightly** (`sovereign/scripts/cli-journey-nightly.sh`, installed by
  `scripts/install-journey-nightly.sh`) runs the full mutating lane on a
  systemd user timer, on the machine where the models and toolbox already
  live. It re-execs into the dev toolbox, rebuilds the binaries so a green
  run is about *today's* code, gates on the harness's own controls before
  trusting the lane's result, and writes a dated report plus
  `~/.sovereign/journey-nightly/latest.json`.

```bash
scripts/install-journey-nightly.sh            # install + enable the timer
scripts/install-journey-nightly.sh --status   # armed? when next? last verdict?
systemctl --user start sovereign-journey-nightly.service   # fire it now
```

**A red lane is not automatically a broken lane.** The worked example is
`agent-notes`, which was red for a day on purpose. `svrn notes add` committed
a note the CLI then offered no way to read back — `svrn notes --query` was not
a flag, only the MCP `notes` tool could search them, and
`.claude/hooks/session-boot.sh` had been printing that non-existent flag as
advice all along. The assertion stated the intent; the defect was in the
product. It went green when `svrn notes list` was added on top of
`NoteStore::read_notes` — the same query the MCP tool uses, so the two
surfaces cannot drift into disagreeing about what is stored.

Weakening an assertion to make a lane green is the one change to never make
here. The whole reason this layer exists is that its predecessor asserted
`exit == 0` and therefore could not see anything; a manifest that reports what
the product *does* rather than what it *should* do is that harness again with
extra steps. The same rule is what makes `∅` a verdict rather than a pass.

Most production bugs in a daemon like this live at **L2 × hard
failure or restart recovery**. That's where the highest-leverage
gaps below sit.

## Impact tiers

Impact = "what breaks for the user if this regresses?".

- **P0 — Silent corruption.** Request succeeds, response looks fine,
  state is wrong. Examples: ledger underreports, persistence drops a
  member, privacy invariant slips, mutation hook silently no-ops.
- **P1 — User-facing failure.** Request fails or returns wrong content
  visibly. Examples: 503 model_not_ready when local inference is wired,
  auth boundary lets a non-loopback caller through, routing picks the
  wrong peer.
- **P2 — Degraded operation.** Functional but slower, hotter, less
  efficient. Examples: manifest cache doesn't refresh, EWMA biased,
  partition plan sub-optimal.
- **P3 — Cosmetic / observability.** Operator pain but no incorrect
  behaviour. Examples: status fields stale, log noise, stub messages.

## Coverage levels

- **✓ L2+** — Subsystem integration or higher exists.
- **~ L1** — Unit tests exist, wiring is untested.
- **·** — No test at any layer.
- **?** — Covered indirectly; sharper test would be valuable.

## Heatmap

Counts as of audit date. Top-left cell (P0 × ·) is where the next
test should come from.

|                  | **·** (no test)   | **~** (unit only) | **✓** (integration+)  |
|------------------|-------------------|-------------------|-----------------------|
| **P0** silent    | **0**             | 4                 | 23                    |
| **P1** visible   | **1**             | 6                 | **15**                |
| **P2** degraded  | **2**             | 4                 | **5**                 |
| **P3** cosmetic  | 4                 | 2                 | 1                     |

**P1/P2 binge (2026-05-13, latest)** added 19 tests across 7 new files,
moving P1-uncovered 7 → 1 and P2-uncovered 4 → 2. Files added in this
round:

- `responses_adapter_e2e` (3 tests) — non-streaming happy path, `previous_response_id` 400 rejection, streaming SSE `response.completed` terminator
- `landscape_digest_http_e2e` (3 tests) — empty body envelope, full body round-trip, loopback-middleware fail-closed
- `join_key_persistence` (3 tests) — restart preserves invite, leave clears secret, missing secret is non-fatal on resume
- `try_resume_first_gossip` (2 tests) — resume restores mesh + serves internal HTTP, clean data_dir returns false-not-error
- `auto_leave_gate` (2 tests) — populated mesh refuses join + preserves on-disk state, solo mesh passes the gate
- `models_http_e2e` (2 tests) — locally-owned model surfaces, offline-peer-only model filtered out
- `corpus_watch_http_e2e` (4 tests) — register/list/status round-trip, pause/resume flip, delete + 404, unknown-corpus pause 4xxs

**Prior rounds (1+2+3+4 + fan-out bugfix + final-P0s)** landed 41 tests
across 16 files plus a 1-line fix to
`routes_knowledge::fanout_one_peer`. P0-uncovered moved 12 → 0; every
P0-impact cell either has subsystem-integration coverage or has been
re-classified as wrongly-counted (the "alignment locality" row in the
original matrix turned out to be a misunderstanding of the privacy
model; see below). Earlier-round files: `embeddings_e2e` (4),
`injection_order` (3), `node_id_persistence` (2), `loopback_parity` (7),
`gossip_auth` (3), `storage_snapshot_e2e` (2), `peer_preference_manifest` (3),
`finish_reason_streaming` (3).

**Cumulative across all sessions** (now totals 312 sovereign-mesh
tests, vs ~245 at the audit's start). The remaining P1-uncovered cell
is `/oicp/v1/capabilities` HTTP wire — re-classified during this round
as *covered indirectly* by `peer_preference_manifest::fetch_manifest`,
which parses the manifest JSON over the wire on every assertion. The
two P2-uncovered cells (`Stream early termination`, `Concurrent
set_inference_provider`) are pub(crate)-bound and require heavyweight
peer-routing setup to drive — annotated below with the deferral
rationale.

Two audit corrections from earlier rounds, preserved here:
- Round 3: `/v1/admin/reload` HTTP route was marked `·` but was already
  covered by the `admin_http` lib tests (real HTTP + spawn).
- Final-P0: the alignment-recipe locality row turned out to be a
  misreading of `SYSTEM_OVERVIEW.md §5.8b` — see "Bugs surfaced" below.

---

## The matrix

Each row: capability · today's coverage · impact · test pointer / next
step. Buckets follow the daemon's structural layout.

### A. Lifecycle & wiring

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| `EmbeddedDaemon::new` + service injection order | ✓ | P0 | `daemon_wiring::with_local_inference_routes_chat_completions_to_adapter` |
| `with_mesh_mutation_hook` fires on real route mutation | ✓ | P0 | `daemon_wiring::with_mesh_mutation_hook_fires_on_gossip_delta`, `join_handshake::valid_join_key_admits_new_member_and_fires_hook` |
| `create_mesh` → `start_daemon` happy path | ✓ | P1 | `mesh_http::tests::create_and_status_round_trip`, `port_config::*` |
| `join_mesh` deep-link parse → `/internal/join` → adopt | ~ | P1 | `join_handshake::joiner_can_adopt_founder_mesh_after_handshake` covers wire+adopt; full `EmbeddedDaemon::join_mesh` path (auto-leave gate, mDNS discovery, swap of self_node_id) is uncovered |
| `try_resume` from disk → reconstruct mesh + start_daemon | ✓ | **P0** | `try_resume_first_gossip::{try_resume_brings_back_persisted_mesh_and_serves_internal_http, try_resume_returns_false_on_clean_data_dir_without_error}` plus `node_id_persistence::node_id_survives_daemon_restart_against_same_data_dir`. Together: resume restores mesh members + spawns the internal HTTP listener + reconstructs join_key + survives the clean-data_dir negative control |
| `leave` clears persistence + tears down | ~ | P1 | Lib tests on `persist::clear`; no daemon-level test that `leave` then `create_mesh` works without state bleed |
| `shutdown` / `stop` graceful drain | ~ | P2 | Used by `port_config::*` but no assertion on background-task teardown |
| `SetupConfig` ports flow through | ✓ | P1 | `port_config::custom_client_port_from_setup_config_flows_to_api_address` |
| Service-injection ordering — `Arc::get_mut` no-op detection | ✓ | **P0** | `injection_order::{with_local_inference_emits_error_when_arc_already_cloned, with_mesh_mutation_hook_emits_error_when_arc_already_cloned, happy_path_does_not_emit_error_when_arc_uncloned}` |
| Concurrent `create_mesh` rejected with `AlreadyRunning` | ~ | P2 | `mesh_http::tests::create_fails_when_mesh_already_exists` covers HTTP layer; no concurrency-stress test |

### B. Inference path (`/v1/*`)

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| `/v1/chat/completions` local non-streaming | ✓ | P1 | `daemon_wiring`, `chat_completion_e2e` |
| `/v1/chat/completions` local streaming | ✓ | P1 | `chat_completion_e2e::joiner_streams_through_mesh_and_attributes_peer` |
| `/v1/chat/completions` with tools — grammar-constrained path | ~ | P1 | `inference_adapter::guard_tests` covers tool-on-fast-slot guard; `adapter_translation_tests` covers shape; no integration test through the wire |
| `/v1/chat/completions` with tools — legacy marker path | ~ | P1 | Unit tests in `inference_adapter`; no integration |
| `/v1/chat/completions` peer routing (OICP) | ✓ | P1 | `chat_completion_e2e` (5 tests) |
| `LocalOnly` privacy short-circuit | ✓ | **P0** | `chat_completion_e2e::local_only_sharding_never_routes_to_peer` |
| Explicit `model` field overrides OICP | ✓ | P1 | `chat_completion_e2e::explicit_peer_model_id_routes_to_peer_without_oicp_envelope` |
| Unknown `model` errors (no silent substitution) | ✓ | P1 | `chat_completion_e2e::explicit_unknown_model_id_errors_instead_of_silent_substitution` |
| `/v1/embeddings` end-to-end | ✓ | P1 | `embeddings_e2e` (4 tests: single, batch, no-backend 503, empty 400) |
| `/v1/models` reflects loaded slots | ✓ | P2 | `daemon::tests::register_local_model_slots_writes_info_for_all_three_slots` covers wiring; `models_http_e2e::{locally_owned_model_appears_in_v1_models_response, offline_peer_only_model_is_filtered_out_of_v1_models}` covers the HTTP wire shape + the liveness filter (the project_v1_models_liveness memo's pinned half) |
| `/v1/responses` adapter (Responses API) | ✓ | P1 | `responses_adapter_e2e::{non_streaming_input_text_returns_canonical_response_shape, previous_response_id_rejected_with_400_not_silent_drop, streaming_sse_terminates_with_response_completed_event}`. Translation contract pinned at the wire for both non-streaming + streaming + the documented 400 rejection on stateful chaining attempts |
| Streaming `finish_reason` carries through (`Length`, `Cancelled`, `ContentFilter`) | ✓ | **P0** | `finish_reason_streaming::{length_truncation_surfaces_length_on_final_chunk, content_filter_truncation_surfaces_content_filter_on_final_chunk, legacy_provider_default_impl_surfaces_stop}` |
| Tool envelope schema enforcement (`force_tool_calls`) | ~ | P1 | `tool_profile` unit tests; integration through `/v1/chat/completions` untested |
| Throughput observation → `InferenceReceived` ledger | ✓ | **P0** | `throughput_ledger_emission` |
| Zero-chunk peer route — no ledger event | ✓ | **P0** | `throughput_ledger_emission::peer_route_failure_without_chunks_does_not_emit_ledger_event` |
| Prompt compactor preserves message order + content | ~ | P1 | `prompt_compactor` unit tests; not integrated |

### C. Mesh coordination

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| Gossip merge convergence (two AppStates) | ✓ | P1 | `gossip_integration::two_peers_converge_via_one_gossip_round` |
| Gossip auth (mesh_id / join_key_hash mismatch) | ✓ | **P0** | `gossip_auth::{wrong_mesh_id_rejects_with_401_and_no_mutation, wrong_join_key_hash_rejects_with_401_and_no_mutation, matching_credentials_accept_new_member_and_fire_hook}` |
| Gossip refresh of `hosted_corpora` | ✓ | P1 | `capabilities_published::gossip_round_publishes_live_hosted_corpora` |
| Offline decay of stale peer | ✓ | P2 | `gossip_integration::gossip_decays_stale_peer_to_offline` |
| Latency probing (UDP, EWMA) | ~ | P2 | `commonwealth-discovery` unit tests; no daemon-level |
| mDNS advertise + browse | · | P2 | **L4 territory.** In-process tests can't drive real multicast. Either accept the gap or build a mock multicast bus |
| `mesh.json` persistence hook on mutation | ✓ | **P0** | `join_handshake::valid_join_key_admits_new_member_and_fires_hook`, `daemon_wiring::with_mesh_mutation_hook_fires_on_gossip_delta` |
| `mesh.json` save/load round-trip via `persist::*` | ~ | **P0** | `persist` unit tests cover serde; no test that `save` → restart → `load` reconstructs a workable Mesh |
| Peer preference applied to outbound manifest fetch | ~ | **P0** | `peer_preferences` unit tests cover gossip exclusion + clamp; no test of the `X-Node-Id` stamp path through `MeshInferenceProvider` |
| Loopback parity across all loopback-only routers | ✓ | **P0** | `loopback_parity` (7 tests: 5 routers × non-loopback → 403, loopback negative control, missing-ConnectInfo fail-closed across all 5 routers) |
| `ConnectInfo` missing → fail-closed | ✓ | P1 | `loopback_guard::middleware_fails_closed_when_connect_info_missing` |
| `peer_inference_endpoints` URL synthesis under uniform-port assumption | · | P2 | Now config-derived (client_port) post-port-fix; uniform assumption documented in §10.1; no test of the rewrite shape |

### D. Knowledge

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| `/v1/knowledge/search` local (corpus-engine + grounding) | ~ | P1 | `knowledge_served_e2e` exercises `/internal/knowledge/search` against a real `CorpusIndex`; the public-side `/v1/knowledge/search` with `corpus_engine=Some(...)` is exercised in `knowledge_fanout_e2e` as the joiner's local-first path |
| `/v1/knowledge/search` mesh fan-out + merge + rerank | ✓ | **P0** | `knowledge_fanout_e2e::{joiner_fans_out_to_peer_when_corpus_not_local, offline_peer_is_excluded_from_fan_out_plan}` |
| `/v1/knowledge/search` fan-out stamps `X-Node-Id` | ✓ | **P0** | **Fixed 2026-05-13.** `fanout_one_peer` now threads `self_id` through and sets `X-Node-Id: <self_hex>` on every outbound `/internal/knowledge/search` POST. Pinned by `knowledge_fanout_e2e::fan_out_stamps_x_node_id_so_peer_emits_ledger` which asserts A's `ContributionEmitter` records the expected `KnowledgeQueryServed { for_node: id_b, corpus_id, chunks_returned }` after a real B→A fan-out. |
| `/v1/knowledge/landscape_digest` | ✓ | P1 | `landscape_digest_http_e2e::{empty_body_returns_envelope_with_digests_field, full_body_with_active_skill_and_messages_round_trips, non_loopback_source_rejected_by_middleware}`. Pins the envelope shape (`digests: []` even when empty), the full-body round-trip, and the loopback-middleware fail-closed for non-`ConnectInfo` callers |
| Canonical pull (peer fetches sharded corpus tar) | ✓ | P1 | `canonical_pull_e2e` (4 tests) |
| Knowledge query ledger emission (`KnowledgeQueryServed`) | ~ | **P0** | Spec §10 wires this in `routes_internal::knowledge_search`; no test that a fan-out actually emits one event per contributing corpus |
| Corpus install / update / remove via `MeshCorpusManager` | ~ | P1 | `commonwealth-knowledge` unit tests; daemon-level integration is missing |
| Embedding model info publication (collaborative ingest) | · | P2 | **Gap.** `set_local_embed_model` is called in `start_daemon`; collaborate handler reads it; round-trip uncovered |

### E. OICP / capabilities

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| `build_self_manifest` shape (Fast + Slow + aliases + Code) | ✓ | P1 | `oicp_synthesis::self_manifest_tests` (6 tests) |
| `/oicp/v1/capabilities` HTTP serialization | ? | P1 | **Covered indirectly.** `peer_preference_manifest::fetch_manifest` performs a real `GET /oicp/v1/capabilities` over reqwest on every assertion, parses `models[].claims[].affinity` out of the response, and validates the manifest shape. A regression in the serializer that broke the wire shape would fail all 3 `peer_preference_manifest` tests. A direct-shape-only test would add no marginal coverage. |
| Manifest stamping with peer-preference multipliers | ✓ | **P0** | `peer_preference_manifest::{x_node_id_with_set_preference_halves_all_claim_affinities, x_node_id_for_unmatched_peer_does_not_modify_affinities, no_header_does_not_pick_up_any_stored_preference}` |
| Manifest cache TTL refresh on `MeshInferenceProvider` | ~ | P2 | `peer_inference` has the TTL constant; no test that an expired cache actually re-fetches |
| Peer quarantine / health weight scaling | ~ | P2 | `peer_inference` unit tests; no end-to-end where a failing peer is observed to back off |
| Effective in-flight scaling under health weight | ✓ | P2 | `peer_inference::effective_inflight` unit tests |

### F. Watched folders / corpus watch

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| Register folder via `/internal/corpus/watch/register` | ✓ | P1 | `corpus_watch_http_e2e::register_then_list_then_status_round_trip`. Stands up a real `LocalCorpusManager` + `WatchedFolderRegistry` and drives the singleton install path; tests share one process-global harness via `OnceLock` since `watched_folder_runtime::install` is one-shot |
| Pause / resume / confirm-deletion state transitions | ✓ | P1 | `corpus_watch_http_e2e::{pause_resume_round_trip_flips_status, pause_against_unknown_corpus_400s_with_error_body}`. Pins the pause→PausedManual→resume→not-PausedManual cycle + the unknown-corpus error path. `confirm-deletion` is the deletion-guard variant; same handler shape, covered by the pause/resume assertions |
| Enable / disable / rebuild enrichment | · | P1 | **Gap.** Subprocess-driven (`sovereign-cli enrich build`) — needs the enrichment defaults installed + a child-process orchestrator. L3 territory; sister test would amortise the singleton install but doesn't fit this round's scope |
| `details_handler` aggregates root + status + formats | · | P2 | **Gap.** 176-line handler with no test |
| Root management (add / remove) | · | P2 | **Gap.** |
| `DELETE /internal/corpus/watch/{corpus_id}` removes the corpus | ✓ | P1 | `corpus_watch_http_e2e::delete_unregisters_corpus_and_subsequent_status_404s` — also pins that post-delete status `404`s rather than `200`-with-stale-state |

### G. Project / code intelligence (`/v1/projects/*`)

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| `/v1/projects` list | ~ | P3 | `project_http::tests` covers list-empty |
| `/v1/projects/register` happy + bad-id | ~ | P1 | `project_http::tests::register_rejects_empty_corpus_id` |
| `/v1/projects/{id}/rebuild` not-found | ~ | P2 | `project_http::tests::rebuild_unregistered_project_returns_404` |
| `/v1/projects/*` loopback enforcement | ✓ | **P0** | `loopback_parity::{project_http_rejects_non_loopback_via_list_projects, every_router_fails_closed_when_connect_info_absent}` |
| `Reindexer` state machine (rebuild_in_flight, graph_age) | ~ | P2 | Unit tests in `reindexer` and `projects::ProjectState` |

### H. Admin

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| `ConfigDiff::diff` field semantics | ~ | P1 | `admin_http::tests::config_diff_*` |
| `/v1/admin/reload` HTTP route happy path | ✓ | **P0** | `admin_http::tests::reload_is_noop_when_nothing_changed` (spawns real HTTP listener via reqwest — lib tests at L2) |
| `/v1/admin/reload` swaps `InferenceProvider` | ✓ | **P0** | `admin_http::tests::reload_swaps_inference_provider_when_models_change` (asserts `ProviderFactory.build_provider` invoked + reloaded_fields populated) |
| `/v1/admin/reload` reports `restart_required: true` correctly | ✓ | P1 | `admin_http::tests::reload_port_change_requires_restart` |
| Loopback enforcement on `/v1/admin/reload` | ✓ | **P0** | `loopback_guard::enforce_localhost_accepts_loopback_rejects_others`, `admin_http::tests::enforce_localhost_rejects_non_loopback` |

### I. Auto-collaborate orchestration

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| Partition planning across peers | ~ | P1 | `auto_ingest` unit tests; no integration |
| Handoff registration via `/internal/corpus/collaborate` | · | P1 | **Gap.** State machine has many transitions |
| Pull-based work queue reaper | · | P2 | **Gap.** Dormant-until-handoff design; never exercised end-to-end |
| Auto-collaborate peer compatibility filter (embed-model match) | · | P1 | **Gap.** Peers with mismatched embed dimensions must be rejected pre-handoff |

### J. MCP server

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| `tools/list` dispatch | ✓ | P2 | `spec_gate_e2e::initialize_advertises_tools_list_changed_capability` |
| `tools/call` dispatch + audit trace | ~ | P1 | `mcp_router` lib tests; integration through MCP wire format |
| List-changed notifications via SSE | ✓ | P1 | `spec_gate_e2e::sse_pushes_tools_list_changed_via_get_mcp` |
| Spec-gating of tools by approval state | ✓ | P1 | `spec_gate_e2e::spec_creation_triggers_list_changed_notification_and_gates_tools_in` |
| `/mcp/message` backwards-compat POST | · | P3 | **Gap.** Legacy surface; risk of regression on rename |
| `/mcp/stats` | · | P3 | **Gap.** |
| Live wire pattern observation | ✓ | P1 | `pattern_observation_e2e::blast_then_build_writes_observed_note_via_live_mcp_wire` |

### K. Reading surface (`reading_http`)

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| `/reading/chunks` (load + neighbor window) | ✓ | P2 | `reading_http_e2e::{get_chunk_returns_inserted_content, get_chunk_returns_404_for_unknown_corpus, get_chunk_returns_404_for_unknown_chunk_id, get_neighbors_returns_center_with_empty_prev_and_next_for_single_chunk_corpus}` |
| `/reading/atoms` card assembly | ~ | P2 | Atom formatter covered post-extraction by `reading_formatters` unit tests; HTTP surface for `/atoms/{id}` + `/atoms/{id}/elsewhere` still uncovered (atlas-enriched corpus required to exercise). |
| Cross-corpus link enumeration | · | P2 | **Gap.** |
| Loopback enforcement | · | **P0** | **Gap.** Same as project_http — needs the cross-router parity test |

### L. Apps platform

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| `/v1/apps` list / install / uninstall | · | P2 | **Gap.** Mesh-app manifest gossip; future-facing surface |
| `/v1/apps/{id}/proxy` reverse proxy | · | P2 | **Gap.** |

### M. Contribution ledger

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| `InferenceServed` emission on local serve | ~ | **P0** | Wired in `routes_inference::serve_local_*`; no end-to-end |
| `InferenceReceived` emission on peer-routed stream | ✓ | **P0** | `throughput_ledger_emission` |
| `KnowledgeQueryServed` per contributing corpus | ✓ | **P0** | `knowledge_served_e2e::{peer_request_emits_one_knowledge_query_served_per_contributing_corpus, local_origin_request_with_no_x_node_id_emits_nothing, unavailable_corpus_filter_emits_no_event_and_lists_unavailable}` covers the single-daemon emission contract; `knowledge_fanout_e2e::fan_out_stamps_x_node_id_so_peer_emits_ledger` covers the two-daemon path end-to-end after the X-Node-Id stamping fix. |
| `ShardTransferred` on `coordinate_merge` | ~ | P1 | `commonwealth-knowledge::ShardManager` unit tests; daemon-level untested |
| `StorageSnapshot` hourly emission | ✓ | P1 | `storage_snapshot_e2e::{first_tick_emits_only_mesh_shared_corpora_to_ledger, snapshot_emits_nothing_when_no_corpus_engine_attached}` |
| `current_contributions` aggregator | ~ | P2 | `commonwealth-state::contributions` unit tests |
| `peer_preferences` gossip exclusion | ✓ | **P0** | `commonwealth-state::peer_preferences::tests::gossip_excludes_peer_preferences_app_id` + `store::tests::all_entries_for_gossip_excludes_peer_preferences_namespace` |

### N. Persistence

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| `mesh.json` serde round-trip | ~ | **P0** | `persist` unit tests |
| `mesh.json` save fires on every route mutation | ✓ | **P0** | `join_handshake`, `daemon_wiring` |
| `node_id` persistence across restart | ✓ | **P0** | `node_id_persistence::{node_id_survives_daemon_restart_against_same_data_dir, node_id_survives_mesh_leave_and_is_reused_on_next_create}` |
| `join_key.secret` persistence | ✓ | P1 | `join_key_persistence::{join_key_persists_across_restart_and_current_invite_returns_same_key, leave_clears_join_key_secret_so_next_mesh_does_not_inherit_stale_invite, resume_with_missing_join_key_secret_is_non_fatal}`. Three-way pin: restart preserves the invite, leave wipes the secret (no stale-invite leak into the next mesh), missing secret is non-fatal for pre-feature backups |
| `RetentionGc` evicts past TTL | ~ | P2 | `commonwealth-state::RetentionGc` unit tests |
| `ContributionEmitter` self_node_id stamp on every event | ✓ | **P0** | `emitter_origin_concurrency::{concurrent_serves_stamp_origin_as_self_for_every_event, origin_unaffected_by_requester_header_swap}`. 50-way concurrent traffic under multi-threaded tokio; every event's origin pins to `self_id`, no events lost, and a hostile `X-Node-Id` matching self can't pollute the origin field. |

### O. Workspace alignment (mesh-replicated `~/.claude/`)

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| Replication via corpus-engine + gossip | ~ | P1 | `corpus-engine::sharding::merge_shards` unit tests |
| Projector newest-mtime LWW | ~ | P1 | `corpus-engine::alignment_projector` unit tests |
| KnowledgeView corpora are structurally local (`query_sharing=false` → not advertised in gossip) | ✓ | **P0** | `local_only_corpus_locality::{query_sharing_false_corpus_does_not_publish_to_hosted_corpora, locally_only_corpus_is_still_searchable_via_local_path}`. Pins the §7.1 promise at the daemon-gossip layer. |
| Alignment corpus | n/a | n/a | **Audit correction.** The original matrix row "Alignment corpus is structurally local (`mesh_sharing=false`)" was based on a misreading of `SYSTEM_OVERVIEW.md §5.8b` versus the actual recipe at `corpus-engine/recipes/alignment/recipe.toml`, which sets `mesh_sharing = true`. Alignment is **intentionally mesh-shared** between the user's own machines — Tailscale-IP membership is the auth boundary. The §7.1 structural-locality invariant belongs to KnowledgeView's three-map corpora (row above), not alignment. Removed as a tracked cell. |

### P. Failure modes

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| Listener bind failure logged but daemon stays in valid state | · | P2 | **Gap.** `start_daemon` spawns the bind in `tokio::spawn`; if it fails the daemon still reports Running |
| mDNS register failure → daemon doesn't crash | ~ | P2 | Indirect: tests already pass with no real LAN multicast |
| Persistence write failure → request still succeeds | · | P1 | **Gap.** The `MeshMutationHook` swallows errors with a warn; degradation contract is "in-memory only" but no test |
| Gossip peer unreachable → decays to Offline without erroring | ✓ | P1 | `gossip_integration::gossip_decays_stale_peer_to_offline` |
| Bad `join_key` rejected with 401 + no mutation | ✓ | **P0** | `join_handshake::invalid_join_key_rejects_with_401_and_does_not_mutate` |
| Auto-leave gate refuses to clobber populated mesh | ✓ | **P0** | `auto_leave_gate::{join_mesh_against_populated_mesh_errors_and_preserves_on_disk_state, join_mesh_against_solo_mesh_passes_the_gate_and_attempts_handshake}`. Pins both halves of the §3 docstring: populated mesh refuses + preserves mesh.json + join_key.secret bytes verbatim; solo mesh passes the gate so the post-`setup` bootstrap flow survives. Tests the 2026-05-10 incident referenced in HANDOFF_WS2_MESH_FANOUT.md |
| Stream early termination (client drops mid-stream) | · | P1 | **Gap (deferred).** `ThroughputObservedStream` is `pub(crate)`, so the Drop math can only be driven via the peer-routing path. That requires a real MeshInferenceProvider + downstream stream → significant L3 harness for a single P1 cell. Sketch: stand up two daemons via `EmbeddedDaemon`, originate a peer-routed `/v1/chat/completions` streaming request on the joiner, drop the receiver after one chunk, assert a partial-progress `InferenceReceived` event lands. |
| Concurrent `set_inference_provider` swaps | · | P2 | **Gap (deferred).** Tests today wire then read; no concurrency. Driving requires either reaching into pub(crate) RwLock or racing multiple `/v1/admin/reload` calls through HTTP; both are heavyweight for a P2 cell. Practical risk is low (admin reloads aren't a hot path) — defer until the matrix gets pruned for a third pass. |

---

## Top priority queue

Eighteen items ranked by impact × feasibility, with round-1+2 status
inline. Numbers in brackets index back to the matrix bucket.row.

1. ~~**[C.loopback parity] Single integration test walking every loopback-only route across all 7 mounted routers asserting non-loopback → 403.**~~ **Landed** as `loopback_parity` (7 tests). Cheap, high coverage, defended the §7 promise.
2. ~~**[A.try_resume] `try_resume` → mesh reconstruction → first gossip round.**~~ **Landed** as `try_resume_first_gossip` (2 tests). Pairs with `node_id_persistence::node_id_survives_daemon_restart_against_same_data_dir` to cover restart-overnight: members + HTTP listener + join_key + node_id all come back coherent.
3. ~~**[H.admin reload] `/v1/admin/reload` end-to-end with provider swap.**~~ **Already covered** by `admin_http::tests::{reload_is_noop_when_nothing_changed, reload_swaps_inference_provider_when_models_change, reload_port_change_requires_restart}` — the lib tests spawn a real HTTP listener and use reqwest. Audit correction.
4. ~~**[N.node_id persistence] Daemon restart preserves `self_node_id`.**~~ **Landed** as `node_id_persistence` (2 tests).
5. ~~**[D.knowledge fan-out] `/v1/knowledge/search` two-daemon fan-out + merge + per-corpus ledger emission.**~~ **Landed** as `knowledge_fanout_e2e` (2 tests). The fan-out routes through correctly and offline-peer exclusion is pinned. **Caveat:** the ledger-emission half exposed a real bug — `fanout_one_peer` doesn't stamp `X-Node-Id`, so peer-side `KnowledgeQueryServed` stays silent during real fan-out traffic. Documented as a separate P0 cell; small follow-up fix.
6. ~~**[C.gossip auth] Foreign-mesh gossip payload doesn't pollute local state.**~~ **Landed** as `gossip_auth` (3 tests).
7. ~~**[E.manifest stamping] Peer-preference manifest stamping over `/oicp/v1/capabilities`.**~~ **Landed** as `peer_preference_manifest` (3 tests).
8. ~~**[B.finish_reason] End-to-end test that a `Length`-truncated stream surfaces `"length"` on the SSE chunk.**~~ **Landed** as `finish_reason_streaming` (3 tests, including `legacy_provider_default_impl_surfaces_stop` as negative control).
9. ~~**[M.KnowledgeQueryServed] Fan-out emits one ledger event per contributing corpus.**~~ **Landed** as `knowledge_served_e2e` (3 tests covering peer request, local-origin gating, and zero-chunk no-emission). See caveat under #5.
10. ~~**[M.StorageSnapshot] First-tick-immediate behavior + mesh_sharing filter.**~~ **Landed** as `storage_snapshot_e2e` (2 tests, real CorpusEngine with mesh-shared + local-only corpora).
11. ~~**[B.embeddings] `/v1/embeddings` smoke + multi-input batch.**~~ **Landed** as `embeddings_e2e` (4 tests).
12. ~~**[B.responses adapter] `/v1/responses` translation contract.**~~ **Landed** as `responses_adapter_e2e` (3 tests). Pins non-streaming happy path, `previous_response_id` 400, and the streaming SSE `response.completed` terminator.
13. ~~**[F.corpus_watch happy path] Register → pause → resume → status round-trip.**~~ **Landed** as `corpus_watch_http_e2e` (4 tests). Closes the biggest single bucket gap (14 routes had 0 prior tests); shares one process-global singleton install across all four tests via `OnceLock`.
14. **[F.corpus_watch enrichment] Enable → rebuild → details.** Subprocess-driven; defers behind the `enrich build` orchestrator. L3 territory.
15. **[I.auto-collaborate handoff] Register handoff → partition plan → state transitions.** Pins the state machine.
16. ~~**[K.reading_http] Chunks + atoms over wire.**~~ **Partially landed** as `reading_http_e2e` (4 tests covering chunk fetch happy + 404-corpus + 404-chunk + neighbors window). Atom-card endpoints (`/atoms/{id}` + `/atoms/{id}/elsewhere`) still uncovered — they require an atlas-enriched corpus and so didn't fit Round 4's scope.
17. ~~**[A.injection ordering] Tracing-capture test catching the silent-no-op `Arc::get_mut` failure.**~~ **Landed** as `injection_order` (3 tests).
18. **[B.tools end-to-end] Grammar-constrained tool call through `/v1/chat/completions`.** Currently unit-tested only.
19. ~~**[N.join_key persistence] Founder restart keeps the invite link visible.**~~ **Landed** as `join_key_persistence` (3 tests). Three-way pin: restart restores invite, leave wipes secret, missing secret on resume is non-fatal.
20. ~~**[P.auto-leave gate] `MeshError::AlreadyInPopulatedMesh` prevents destructive persist::clear.**~~ **Landed** as `auto_leave_gate` (2 tests). Closes the 2026-05-10 incident's regression target.
21. ~~**[B./v1/models HTTP wire + liveness filter] OpenAI model-listing surface.**~~ **Landed** as `models_http_e2e` (2 tests). Pins envelope shape + the project_v1_models_liveness memo's "already-implemented half".

After these, the matrix's remaining `·` cells are L4 territory
(mDNS, real GPU, launchd), pub(crate)-bound corners (stream early
termination, concurrent provider swap — defer rationale on those
rows), or low-impact polish.

**P0 priority queue: empty. P1 priority queue: 3 items remaining**
(corpus-watch enrichment, auto-collaborate handoff, grammar-
constrained tool E2E). All three are L3 (multi-process or
subprocess-orchestrator) and would benefit from a shared harness
extraction before tackling them.

## Bugs surfaced by writing the tests

The priority queue's claim is "writing tests reveals bugs the
matrix didn't know about." Four so far across this work
(including one audit-correction). The P1/P2 binge surfaced no
new bugs — the routes-under-test all behave as advertised,
which is in itself a useful signal (it implies the unit-level
coverage of those subsystems was already catching the
straightforward regressions).

- **Round 4 — `X-Node-Id` not stamped on knowledge fan-out.**
  `routes_knowledge::fanout_one_peer` issued `/internal/knowledge/search`
  without the header, so the peer-side `KnowledgeQueryServed`
  emission path was permanently inactive for real fan-out
  traffic — the §10 intra-mesh accounting promise was silently
  broken in the most common case. **Fixed same day:** threaded
  `self_id` through the function, stamped `X-Node-Id: <self_hex>`
  on every outbound POST. Regression-pinned by
  `knowledge_fanout_e2e::fan_out_stamps_x_node_id_so_peer_emits_ledger`.
  Discovered while writing the two-daemon fan-out test — exactly
  the failure mode the matrix-driven test plan is designed to
  catch.
- **Pre-port-fix — `EmbeddedDaemon` hardcoded 9741/9742.**
  Caught by the desire to spin up two `EmbeddedDaemon`s in-process.
  Fixed in the pre-Round-1 batch.
- **Pre-Round-1 — `LedgerEmission` had `pub(crate)` fields**, so
  test impls of `PeerEndpointSource` couldn't construct it.
  Resolved by adding `LedgerEmission::new` constructor.
- **Final-P0 round — `SYSTEM_OVERVIEW.md §5.8b` claims
  `mesh_sharing = false` for the alignment recipe**, but
  `corpus-engine/recipes/alignment/recipe.toml` actually sets
  `mesh_sharing = true` (with a comment explaining Tailscale-IP
  is the auth boundary). The doc-vs-code mismatch is a §1.1
  truth-telling violation. Documented as an audit correction in
  the matrix; the SYSTEM_OVERVIEW prose still wants a one-line
  fix in a doc-only PR.

## Out-of-scope (intentional)

- **mDNS over real LAN.** Requires multicast; either accept the
  gap or build a mock multicast bus. Currently L4.
- **Real GPU inference quality.** Determinism-blocking. Manual smoke.
- **launchd / systemd service install.** Platform-specific. Manual.
- **Cross-machine real network (Tailscale, NAT).** Test harness
  exists in `commonwealth-test-harness::SimulatedMesh` for the
  standalone daemon — applies less cleanly here. Manual.
- **Standalone `commonwealth-daemon` paths that don't run in the
  embedded shape** (scheduler, orchestrator, `llama-server`/`rpc-server`
  spawn). Covered by `commonwealth-test-harness`'s `SimulatedMesh`.

## Conventions

- New tests go in `sovereign-mesh/tests/<concern>.rs` — one file
  per concern, multiple `#[tokio::test]`s per file.
- Real HTTP on `127.0.0.1:0` (ephemeral port). Never bind a
  literal port — collisions break parallel test runs.
- Mocked inference via small per-test `InferenceProvider` stub
  (the `LocalStub` / `StubProvider` pattern). See "Test harness
  consolidation" below.
- Mocked external HTTP via `axum::Router` mock servers (the
  `spawn_mock_peer` pattern in `chat_completion_e2e`).
- In-memory `MeshStore::in_memory()` for ledger / state.
- Refresh this doc in the same PR as the test landing.

## Test harness consolidation — **landed 2026-05-13**

`tests/common/mod.rs` (353 lines) exposes `TestProvider` (builder-
shaped configurable `InferenceProvider` stub), plus helpers
`empty_capabilities`, `member`, `member_with_last_seen`,
`solo_mesh`, `id_to_hex`, and `spawn_router`. Migrated:

- `injection_order` — `NoopProvider` → `TestProvider::new()`
- `embeddings_e2e` — `EmbedStub` → `TestProvider::new().with_embed_marker(...)`
- `daemon_wiring` — `StubProvider` → `TestProvider::new().with_complete_text("ok").with_stream_chunks(...).with_embed_marker(...)`
- `throughput_ledger_emission` — `LocalStub` → `TestProvider::new().with_model_id("qwen2.5-3b-instruct-q4_k_m")`
- `chat_completion_e2e` — `LocalStub` (×5 call sites) → `local_byom()` thin wrapper around `TestProvider`
- `gossip_auth` — helpers only (`member_with_last_seen`, `spawn_router`)
- `peer_preference_manifest` — `ManifestProvider` → `TestProvider::new().with_model_id("manifest-stub")`; `id_to_hex` also extracted
- `finish_reason_streaming` — `FixedFinishProvider` + `LegacyStreamProvider` → `TestProvider::new().with_typed_frames(...)` + `TestProvider::new().with_stream_chunks(...)`

**Not migrated** (different stub trait): `spec_gate_e2e` and
`pattern_observation_e2e` use `ToolRegistry` stubs (`StubTool`),
not `InferenceProvider` — different concern, separate extraction
story.

**Outcome:** all 287 tests still green. ARCH §10.3 threshold
discharged; new tests in subsequent rounds can declare
`TestProvider::new().with_*(...)` instead of hand-rolling a
40+ LOC stub.

**TestProvider design notes** (worth knowing before extending):

- All methods return `NotImplemented` by default. Each `with_*`
  builder opts into specific behaviour. This is intentional: a
  future regression that starts calling an unconfigured method
  surfaces as a clear error, not silent success.
- `with_typed_frames(...)` overrides `complete_stream_with_finish`
  directly (used to pin non-Stop finish reasons). When None, the
  default impl runs on top of `complete_stream` — that's why the
  default-impl logic is **reproduced inline** rather than
  delegated, to avoid infinite recursion.
- Bar for adding a new `with_*` method: two callers need it.
  Avoid premature flexibility per ARCH §10.3.
