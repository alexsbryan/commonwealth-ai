# Main-session protocol — moved from .claude/CLAUDE.md

Everything below was relocated VERBATIM from `.claude/CLAUDE.md` on
2026-08-07 (order claude-md-slim; disposition ledger:
`CLAUDE_MD_DISPOSITION_2026-08-07.md`). The slim core points here at
each trigger. Because sections moved whole, internal cross-references
("above", "below", named sections) may point at sections that remain
in the core `.claude/CLAUDE.md`.

## Original preamble (paragraphs 2–3 of the pre-consolidation core)

When developing features you have a high amount of empathy for the end user and the other developers using the system. You write code that is traceable and you build "glassbox" systems that allow those who run them to understand the internals of the working process. Transparency and observability are also core principles to your coding work.


Within this project you consult with SYSTEM_OVERVIEW.md to understand the system at a glance and you keep it up to date when you make what feel like major changes to any of the systems in this project. You use ARCH_PRINCIPLES.md as your compass for evaluating technical design tradeoffs and approaches for implementation.

## Reporting to the operator — tech lead briefing a product lead

Everything you report — turn summaries, findings, session wrap-ups, notes — is input to a product decision. Write it the way a tech lead briefs a product lead:

- **Bottom line up front.** First sentence states the outcome or the recommendation; detail follows for whoever wants it. Never open with process narration, and never build a report as a hedge-chain ("this, but that, then this, so that…"). State the conclusion once and qualify your confidence once ("verified by tests" / "inferred, untested") — not in every sentence.
- **Magnitude, or it's a lead — not a finding.** A gap without quantified impact is not reportable as a result. "The retry path lacks backoff" is a lead; "the retry path lacks backoff — every mesh-join under load hits it, ~40s stall" is a finding. If you can't quantify it, say what measurement would, and get that number before proposing the work.
- **End-user impact is the lens.** Every feature report answers: what does a user do or experience differently now? If the honest answer is "nothing observable", say exactly that — it's a signal to stop, not something to reframe as progress.
- **No unquantified gap-filling.** Finding gaps and filling them because they're findable is the failure mode this section exists to stop. Proposing work requires an impact estimate first: who hits this, how often, how bad. Complexity that doesn't move a named metric gets reverted, not defended.

## MCP surface — inventory, aliases, CLI-only tools

A Sovereign code intelligence server runs at `http://localhost:9741/mcp`. The MCP transport exposes **38 tools** — 32 canonical plus 6 deprecated aliases (see below). That covers code intelligence (`symbols`, `callers`, `callees`, `blast`, `code_search`, `facts`, `capability_map`, `arch_report`, `arch_posture`), notes (`note`, `notes`, `retire_note`, `briefing`, `session_state`), coordination (`work_in_flight`, `declare_scope`, `release_scope`), drift (`drift_findings`, `drift_posture`, `atos_verify`), build feedback (`lint_status`, `get_lint_output`, `build`), and `solve`.

The build-feedback three are **dormant in this repo** — the lint/test watchers are off by design, so they have nothing to report. That is a supported posture, not a fault; see "Compilation and test feedback" for the gate that replaces them.

A handful of tools are **CLI-only** — `sovereign tools list` shows them but they are NOT on the MCP surface, and calling one over MCP returns tool-not-found: `test_status`, `run_tests`, `get_run_output`, `recent_changes`, `project_context`, `session_reflection`. Reach those via `sovereign tools call <name>`.

Don't trust this paragraph over the wire: `tools/list` is the authoritative answer, and the served set is `registry ∩ allowlist`, so it varies by which server you're talking to (`svrn daemon` serves the largest set).

Every tool declares behavioural properties (Effect · Scope · Latency) and an output_schema you can see via `sovereign tools describe <id>`. Use these to compose multi-step plans confidently — the schema tells you which `{N.key}` references are valid when piping step N's output into step N+1's params.

**Tool-name renames (March 2026 CLI refactor).** The MCP server still accepts the old names as deprecated aliases, but new code should use the short names:

| Old name (still works as alias) | Use this instead |
|---|---|
| `symbol_lookup` | `symbols` |
| `find_callers` | `callers` |
| `find_callees` | `callees` |
| `blast_radius` | `blast` |
| `read_notes` | `notes` |
| `write_note` | `note` |

### Session start — do these before anything else

1. **The compass is already loaded** — "The architectural compass" section above carries the ten + `ARCH_PRINCIPLES.md §0` + §15 + the section index, so there is no day-one read to perform. What you owe at session start is *routing*: when the task names a design decision, open the numbered section it maps to (the "Which door to open" table). When the task lands you in an area you have no model of, read `docs/ARCHITECTURE_TOUR.md` (227 lines) — not `SYSTEM_OVERVIEW.md`, which is 265KB and is a lookup surface, not a read.
2. `recent_changes(hours: 24)` — see which subsystems are active
3. `project_context("<user's stated task>")` — pull relevant conventions and architecture docs
4. `notes(query: "<task area>")` — surface decisions and invariants from prior sessions
5. `drift_posture()` — answer "is the latest drift report still current against the narrative docs?" Returns top critical findings + age. If `status=stale`, the architecture docs have been edited since the last drift run; cite findings carefully. If `status=fresh`, the drift findings (and `drift_findings()` queries below) reflect current state.
6. `work_in_flight(scope="<task area>", match_mode="file")` — **when the task names a file or symbol**, check whether a peer agent or human on the mesh is already there. A non-empty result means another node is active; surface that to the user before proceeding rather than silently colliding. See the "Coordination — work atlas" section below for grades and what to do on overlap.
7. `arch_posture()` — **when the task moves boundaries** (new crate deps, splitting/merging modules, touching a hub crate): the architectural headlines (top god-crate, hubs, layer violations, hidden temporal coupling) + whether the persisted report is stale. Refresh with `sovereign code arch-report`; the layer map itself is `quality/ARCH_LAYERS.toml` (ARCH_PRINCIPLES §8.6).

### Session splitting — standing protocol (proven 2026-07-23)

Long sessions pay cache-read ≈ avg_ctx × turns, and splitting a genuinely fat
one recovers a large share of that. The statusline shows `ctx <N>k` (yellow
"split soon" ≥250k, red "SPLIT" ≥500k) and `frame ✓<age>`.

**Splitting is a FAT-context lever — do not split a thin session.** Thresholds
were raised from 90k/250k on 2026-08-02 (operator call). A split is not free:
the donor writes a frame, and the successor re-derives by hand whatever 2,150
tokens could not carry. Below ~250k that overhead exceeds the cache-read it
avoids, so an eager split makes the work more expensive, not less. Do not
propose one, and do not treat a long-but-sub-250k session as a problem to
manage. Everything below applies once you are actually past yellow.

- **As the donor:** keep your frame current AS YOU WORK — call the
  `session_state` MCP tool (or `sovereign tools call session_state`) at
  transitions: task start, plan step done, blocker hit. It upserts named
  sections of `~/.sovereign/sessions/<session-id>/frame.md`, preserves the
  rest, and rejects over-budget writes with per-section token counts. You
  hold the state; your encode-time writes are the strong path
  (auto-distilled frames recall ~17% and never authorize a split). A
  wrap-up request should be a final small upsert, not a from-scratch write.
- **`objective` is required, and you INHERIT it — you do not re-author it.**
  Any write touching `goal`/`state`/`next`/`decisions` is rejected while the
  frame's `## Objective` is blank. It holds the STANDING outcome the work
  serves — what a user gets when the initiative lands — plus `Done when:`
  (falsifiable at initiative altitude) and `Not worth continuing if:`. When
  continuing a predecessor, copy its `## Objective` verbatim from the frame
  the boot hook already injected; edit it only if the objective genuinely
  changed, and say so in `Decisions` when it does. **Restating it as a delta
  from the last frame ("item two's remaining half") is the specific failure
  this exists to stop** — audited 2026-07-29, 21 of 63 frames did exactly
  that, and one three-session chain lost the name of its own objective.
  Contract: `SESSION_CONTINUITY.md §2.1`.
- **Re-rank `Next` against the objective before you continue it.** The same
  audit found a lineage recopying four backlog items verbatim across three
  frames — carried forever, never done, never dropped. Inheriting an item is
  a decision, not a default: drop what the objective does not need. The write
  response now tells you: `carried[]` lists `Next` items your ancestors were
  also carrying (with `depth`), and `objective_sessions` counts how many
  consecutive frames have stated this same objective. **Act on it in the same
  turn** — do the item, drop it, or say in `Objective` why it stays. It is
  advisory because carrying is often right; it is reported because carrying
  silently never is. Contract: `SESSION_CONTINUITY.md §2.2`.
- **Use the HARNESS session id, not the one `declare_scope` returns.** They are
  different namespaces. A frame banked under the daemon's session id is an
  orphan the boot hook can never find (hit live 2026-07-29). Your harness id is
  in the scratchpad path and the boot banner.
- **As the successor:** the boot hook injects the frame **index** (one line
  per live frame), and the first prompt injects the top-ranked frame whole.
  If that frame is not the work you are continuing, the index is right there
  — `sovereign session frames` lists them, `sovereign session frames <id>`
  reads one. Do NOT hunt with `grep`/`ls` over `~/.sovereign/sessions`; that
  hunt is the 5,872-token failure this surface replaced. Work from the frame
  plus `symbols`/`callers`/`facts`/`notes`; do NOT re-read SYSTEM_OVERVIEW or
  specs the frame summarizes. **This rule is about re-acquiring what you
  already hold — it does NOT cover `ARCH_PRINCIPLES.md`.** A frame carries
  task state; it has never summarized a principle, so there is nothing to
  re-read. Opening `ARCH_PRINCIPLES §N` by number costs ~200-600 tokens and
  is always in budget. Suppressing it is the mechanism behind the
  architectural drift this rule accidentally caused (diagnosed 2026-08-02).
  After your first work stretch, self-measure:
  `sovereign cache-audit --ramp --session <your-id>` — gate ≤5k raw tokens,
  0 repeated reads.

## Read budget — cache-audit telemetry

**See your own context spend — `sovereign cache-audit`.** This parses the local Claude Code transcripts and reports, per session, where the token/cache budget went plus the **raw-acquisition ratio**: raw file/grep tokens pulled into context vs. code-intelligence / RAG calls made. `cache-audit --sort ratio` ranks the worst offenders; `cache-audit --session <id>` deep-dives one. It exists because a fleet agent spent ~70% of its budget on cache-read (re-sending a large context every turn) — and every session audited so far shows hundreds of thousands of raw-read tokens against **zero** `symbols`/`callers`/`code_search`/`notes` calls. That is the leak this whole section is trying to prevent; the tool makes it measurable. Run it on yourself when a task ran long.

### Delegation — subagents are AUTHORIZED on this repo (standing, fleet-wide)

**This section is standing operator authorization. You do not need to ask.** Some
harness builds ship a default of "do not call the Agent tool unless the user
requested it" — this repo requests it, here, once, for every session on every
node. Treat delegation as a normal part of the toolkit, not an escalation.

Why it is written down: the default is silent. An agent that has it never says
"I would have delegated but I'm not allowed" — it just does the sweep inline,
pulls every file dump into its own context, and the operator sees a session that
feels blind and expensive with no explanation. That failure was diagnosed on
2026-07-27 and this section is the fix.

**Cap: 3 concurrent subagents.** Not a suggestion — 3 is the ceiling. Beyond
that the results arrive faster than they can be read, they contend for the same
Cargo lock and daemon, and the synthesis cost exceeds what the fan-out saved.
Launch them in ONE message (parallel tool calls) so they actually run
concurrently; sequential launches serialize and pay the full cache tail anyway.

**Delegate when** the answer requires reading across many files and you only
need the conclusion:
- Broad sweeps: "which crates implement X", "find every call site pattern of Y",
  "where is Z configured across the workspace" → `Explore`
- Any fan-out where the *file contents* are disposable and only the *finding*
  matters. That is the whole point: the dumps land in the subagent's context,
  not yours.
- Independent parallel work: three unrelated investigations that don't feed each
  other.
- **Especially when code intel is down.** With `symbols`/`callers` dark, a sweep
  that would have been one exact query becomes dozens of raw reads — that is
  exactly the case that should leave your context. The `prefer-code-intel` hook
  will say so.

**Do NOT delegate when:**
- You already know the file and symbol — one `symbols` call or one tight Read is
  cheaper than briefing an agent.
- Code intel is UP and the question is a single exact lookup. `symbols("Name")`
  beats a subagent every time; delegation is for breadth, not precision.
- The work needs your accumulated session context to be judged correctly — a
  subagent starts fresh and cannot see the conversation.
- You would have to spawn and also run the same search yourself. Pick one. Once
  delegated, wait for the result.

**Rules of engagement.**
- The subagent's report is NOT shown to the operator. Relay what matters, in
  your own words, with `file:line` refs. Never say "the agent found…" and stop.
- Do not fabricate or pre-announce a pending subagent's findings. If asked
  before it returns, say it is still running.
- Subagents do not `declare_scope`. Coordination is per-session — you hold the
  claim; they inherit your scope. Do not have three agents each claim the same
  symbol.
- Prefer `Explore` for read-only search (it excerpts rather than dumping whole
  files). Reach for `general-purpose` only when the task must also write or run
  things.
- Say out loud that you are delegating and why. Glassbox applies to your own
  process, not just the code you write — a silent fan-out is as opaque to the
  operator as a silent refusal to fan out.

**Gating a fan-out: workers run TARGETED tests, the seat runs ONE
definition-of-done sweep when they all return.** Operator direction 2026-08-20.

This is a CORRECTNESS rule, not an efficiency one, which is why it sits here
rather than under "Gate details". `scripts/sovereign-test.sh` exits **5 on
unattributable results** by design: concurrent nextest runs overwrite the shared
JUnit report, so the counts are not yours. N workers each running the unscoped
suite either collide into exit 5, or hit the worse case — one reads a green
summary that belongs to a peer and reports it as its own verdict. **A fan-out
where every worker runs the full suite cannot produce an honest verdict.** The
saving (N × ~14 min on a memory-capped box) is real but secondary.

- **Workers** get `./scripts/sovereign-lint.sh --human` (scoped) and
  `./scripts/sovereign-test.sh --human` with `--package <crate>` / `--changed` /
  `--filter <test-name>`. Cheap structural gates (`cargo xtask layer-gate`,
  a boundary script) stay with the worker — they prove that worker's own move.
- **Worker verdicts report the full suite as "not-run-by-design, deferred to the
  seat's sweep"** — never passed, never failed. That is a fifth honest state
  beside §18.1's four, and it exists because the run was deliberately not theirs.
- **The seat** runs ONE `--full` lint and ONE unscoped suite after every worker
  is back. That is the definition of done for the whole wave.
- Already dispatched with full gates? Amend mid-flight — `SendMessage` reaches a
  running agent at its next tool round.
- **If the sweep goes red, attribution is the SEAT's job**, not a worker's. Diff
  by worker before assigning blame, and check first whether the failure predates
  the fan-out at all.

Unchanged inside the targeted forms: gate on exit codes, never a summary line; a
zero-test run exits 4 and is NOT green (`--filter` matches the TEST NAME, not the
file, so a typo verifies nothing).

## Work atlas — privacy

**Privacy.** Sessions inherit `node.default_privacy` from `~/.sovereign/work-atlas.toml` (default `public`). Private claims/observations are written to `work-atlas-private` and structurally never gossip — peers never see them. The daemon enforces this at three layers (store, gossip, read). Toggling to private mid-session does NOT retroactively unpublish prior records.

### Writing notes — mandatory triggers

Use `note` to leave durable context for future sessions. **Do not wait until the end of a session** — write notes at the moment of the decision.

- **`decision`** — when you choose one approach over alternatives (e.g., "chose FTS5 over LanceDB because zero-vector embeddings make cosine similarity useless")
- **`invariant`** — when you discover a constraint that must never be violated (e.g., "collect MappedRows inside the same scope as stmt and conn — cannot return across a block boundary")
- **`todo`** — when you identify follow-up work that won't be done in this session
- **`attempt`** — when an approach was tried and failed, so future-you doesn't repeat it

**Shipping anything default-off or dark additionally requires a row in `sovereign/DEFAULTS_LEDGER.md` — in the same commit.** The row names the falsifiable flip condition, which plan item settles it, and a review-by date. Flipping or rejecting a default moves its row (Graduated/Rejected), never deletes it. If you touch an area whose ledger row is past its review-by date, raise it to the operator: flip it, kill it, or re-date it with a reason — "still waiting" without a named blocker is not a valid state. This exists because proven-but-dark capabilities were withering: the flip condition lived only in a session summary nobody re-read (operator directive 2026-07-31).

### Session reflection — at task end

Use `session_reflection` when a significant task is complete. This improves the system over time.

```
session_reflection(
  task_summary: "Refactored EmbedFn across 12 call sites",
  tool_name: "blast",           // primary tool this feedback concerns
  tools_that_helped: ["blast", "lint_status"],
  manual_work_that_should_be_a_tool: "Had to grep for macro invocations blast missed",
  wished_i_had_known: "EmbedFn is wrapped in a macro in commonwealth-inference"
)
```

All fields except `task_summary` are optional. Be specific — vague reflections are not useful.

**Also at task end: release any claims you declared.** If you called `declare_scope` during the work, call `release_scope(claim_id)` now. The TTL would drop it eventually, but peers querying `work_in_flight` in the meantime would still see a stale claim. Use the `claim_id` returned by the original `declare_scope` call (or list them with `sovereign claim list --mine`).

**Before using `blast` or `project_context` on a large task**, first check for known limitations:
```
notes(kinds=["reflection"], query="blast")
```
Active reflections from prior sessions surface automatically. Once a limitation is fixed, the developer retires the reflection via `sovereign reflect --retire` and it disappears from future results.

### Drift tool feedback — mandatory when results disappoint

The drift toolchain (`drift_posture`, `drift_findings`) is recent and known to be incomplete. **When the tool returns unhelpful results during a real workflow, call `session_reflection` immediately.** Specifically:

- **`drift_findings` returned `no_matches`** for a query you know is anchored in the narrative — the anchor extraction or matching pipeline missed it. Reflect with `tool_name: "drift_findings"`, `wished_i_had_known`: the symbol you searched for, the narrative section that mentions it, and what the match SHOULD have been.
- **`drift_findings` returned matches with prose-truncated anchors** (e.g. `"The daemon does not auto-resolve..."` as the anchor instead of a code symbol) — the Phase 1 prompt drifted. Reflect with `tool_name: "drift_findings"`, `manual_work_that_should_be_a_tool`: "had to grep manually because the anchor was prose, not a symbol."
- **`drift_posture` returned `never_run`** despite a known recent drift run — the canonical-path mirror (`~/.sovereign/drift/latest.md.json`) didn't land. Reflect with `tool_name: "drift_posture"`, `manual_work_that_should_be_a_tool`: "had to read the markdown report directly because the JSON sidecar wasn't at the expected path."
- **The action text on a finding was too vague to act on** — log this so the renderer's `action` template can grow more specific guidance per `FindingKind`.

The bar for reflecting on drift tools is *lower* than for code-intelligence tools: it's a young surface, and silence is the failure mode that's hardest to detect later.

## Gate details — throttling, doctests, watcher restore

**Both gates now throttle themselves, and the banner says by how much.** Since 2026-08-07 neither script runs unbounded: the `jobs:` line names the concurrency it chose and which term bound it (`16 — half of 32 cores (99GB available)`, or `7 — memory-capped: 30GB available, 4GB/job`). The default is half the cores, further capped by free memory at 4GB per job, decided in `scripts/lib/cargo-jobs.sh` — one rule, both scripts. It exists because an unbounded run wedged a workstation: 32 rustc processes and then 32 test binaries against RAM a resident model already held, and on the Halo the GPU's memory IS system memory. Measured cost of the cap on an idle box: none (test phase ~24s either way). Override per run with `--jobs N`, per machine with `SOVEREIGN_TEST_JOBS` / `SOVEREIGN_LINT_JOBS`; `--jobs 0` restores the old unbounded behaviour. **If a gate run ever locks the machine again, `--jobs 4` first, then say so** — the default's memory probe reads `MemAvailable`, which does not know what the GPU is about to allocate.

**Doctests are OFF by default in the test script** and the banner says so. `cargo test --doc` costs 17.4s of a 63s warm run and this workspace collects zero runnable doctests (measured 2026-07-28); nextest cannot run doctests at all, so CI passes `--doctests` to keep the insurance. Add `--doctests` locally if you wrote one.

**If you want the watchers back** (`lint_status`, `test_status`, `run_tests`, `build`, and per-file `--changed` queries): set `[watchers] enabled = true` in `.sovereign/sovereign.toml`, uncomment the `[lint_runner]`/`[test_runner]` sections, and restart the daemon. Then read the `watcher` health object on every response *before* `status` — `watcher.live == false` means the results are orphaned and `status` reports `watcher_down` rather than a stale `fresh_*`. Note the surfaces differ: `lint_status` and `get_lint_output` are on MCP and can be called directly, while `test_status` and `run_tests` are CLI-only (`sovereign tools call test_status`). Nothing else in this file depends on that path.

### Measuring quality — the bench suite

**Lint and test are the BUILD gate. Neither says anything about whether the system still answers well.** A retrieval, routing, synthesis, enrichment or inference change can leave both scripts green and still regress answer quality — neither script ever runs a model against a question bank. This section exists because a session in 2026-08 did exactly that, then built its own ad-hoc bench without knowing the suite below already existed.

**There is ONE comprehensive bench: `scripts/sovereign-ci-bench.sh`.** It does not reinvent measurement — it *composes* the ~20 `svrn bench` subcommands, the two gyms and `sovereign-agent-bench` into lanes with a single PASS/FAIL. Three tiers:

```bash
./scripts/sovereign-ci-bench.sh --quick      # ~35-40m — the pre-push tier
./scripts/sovereign-ci-bench.sh --no-synth   # HARD lanes only; skips ~55m of judge lanes
./scripts/sovereign-ci-bench.sh              # full run, 4h budget
```

**Read the lane KIND before you read the verdict** (gate policy at `scripts/sovereign-ci-bench.sh:10-30`):

- **HARD** — deterministic, baseline-diffed: enrichment atom-F1, retrieval recall, `retrieval-prod`, routing, plus the paired `*-gate` lanes. These break the build.
- **SOFT** — the synthesis answer-equiv judge lane. Tracked with a band, *never* gated, because judge variance must not cause flaky red builds.
- **TRACKED** — chaos-monkey, mechanism-fidelity, faithfulness, governance, the gyms. Their absolute verdicts are true findings about the present system, not regression signals, so each pairs with a HARD `*-gate` twin that re-scores the same artifact against a committed baseline.

**Pick the lane that matches your change.** A retrieval or pipeline change is measured by Lane 2b `retrieval-prod` (`--prod-pipeline --isolate`, `sovereign-ci-bench.sh:321-325`) — HARD, deterministic, diffs the composed evidence pool with no synthesis and no judge. Reaching for `--synth` puts an LLM judge between you and the answer, on the one axis that is explicitly non-gating.

**Confirm a bank's baseline exists BEFORE arming a long run.** `bench all` reports `first-run` for a bank that has none — and *writes* one from the run you just did. A first-run tally is a could-not-judge, not a pass, and the baseline it leaves behind will bless whatever you just changed. `svrn posture` (`bench-baselines` row) is the coverage check.

**Where the docs are.** They exist and they are good; finding them has been the whole problem:

| Question | Doc |
|---|---|
| How do I run the gate, or drill into a flagged lane? | `sovereign/bench/README.md` — the canonical entry point |
| A bench says regressed — is it real? Noise bands, baseline age, the legitimate re-mint path | `sovereign/docs/RUNBOOK.md` §6 |
| How do I re-baseline every CI lane from scratch? | `sovereign/bench/CI_GATE_HANDOFF.md` |
| How do I iterate a prompt or scorer without overfitting the golden? | `sovereign/bench/BENCH_LOOP.md` |
| What does lane X actually measure? | `sovereign/bench/<lane>/README.md` (24 of 42 banks carry one) |

`sovereign/docs/BENCHMARKING.md` is **throughput** (embed/decode across Metal/Vulkan/ROCm), not answer quality. It is the top grep hit for "benchmarking" and is not what you want here.

**On this host:** neither `timeout` nor `gtimeout` is installed, so the per-lane caps at `sovereign-ci-bench.sh:167-170` are inert — only the coarse inter-lane budget guard bounds a run. `brew install coreutils` restores them.

## Definition of done — iteration detail

Scoping flags for iteration, not for the final gate:

```bash
./scripts/sovereign-test.sh --human --package sovereign-cli    # one crate
./scripts/sovereign-test.sh --human --filter <test-name>       # TEST name, not file name
./scripts/sovereign-test.sh                                    # raw Tier 2 JSONL (daemon mode)
```

`--package sovereign-compute` fails on feature scoping (`does not contain this feature: corpus-engine/treesitter`) — use plain `cargo test -p sovereign-compute` there.

Both scripts write adapter logs (`target/sovereign-test/latest/`, `target/sovereign-lint/latest/`) including the raw cargo output, so triaging a failure never requires re-running cargo.

The two runners are meant to exercise the same `cargo --workspace` surface — when one passes and the other fails, the discrepancy is the bug, not the runner. One known, deliberate exception: lint checks `sovereign-mesh/mesh-sim` and the test run does not, so the scheduler simulator is compiled but never exercised.

### Index freshness

The daemon owns freshness via per-project watchers (`sovereign project list` shows their status). `sovereign project refresh` nudges a manual SCIP rebuild. If `symbols` returns "no symbol named X found in any installed code corpus" but you know it exists, the LanceDB chunk index for that project may be missing — check `sovereign project status` and re-index with `sovereign code index <path> --corpus-id=<id>` if the SCIP graph is healthy but the chunk corpus is gone.

**When code intelligence looks dead, run `sovereign doctor` FIRST — do not debug it by hand.** Two checks answer the whole question in one command, and both were added because this cost a full session to rediscover manually on 2026-07-24:

- **`watcher_freshness` = Failed, "NO projects registered"** — the registry (`~/.sovereign/projects.json`) is empty, so the Reindexer built zero `ProjectHandle`s and **nothing is watching anything**: no FS watcher, no git-HEAD poll, no rebuild queue. Every other surface still reports green, because they stat the files: `svrn status` prints `Index ✓ / Call graph ✓` off artifacts last built by hand, and doctor's own `scip_indexed` / `code_indexed` pass for the same reason. The repair is one command per orphan, printed by doctor: `svrn project register --corpus-id <id>`.
- **`code_tools_visibility` = Failed** — a code corpus exists on disk but the code tools screen it out, so `symbols` / `code_search` / `recent_changes` return empty against a perfectly healthy index. Visibility is `CorpusKind::Code` **OR** an on-disk `scip_graph.db` (`sovereign_tools::code::has_code_graph`).

`svrn status` also now carries a **`Watched`** line — the answer to "is anything maintaining this?", which the `Index`/`Call graph` ✓ marks do not tell you.

**Do not "fix" a repo corpus that reports `kind: "knowledge"`.** That is deliberate, not a mis-tag. Chat retrieval admits only `Knowledge | Catalog` (`runtime/retrieval/corpus_search.rs:95`) and CODE_INTEL_CHAT.md routes code questions *through* the knowledge path, so promoting a repo to `CorpusKind::Code` silently deletes it from chat. Code-ness is detected from the SCIP graph, never from the tag.

### When MCP tools add less value

For greenfield additions (new types, new files), MCP doesn't write the code — but `symbols` still validates the patterns you're matching. The writing is new; the patterns are not.
