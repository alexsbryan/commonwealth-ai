# Guest QA — the first-hour hospitality harness (measure-first spec)

**Status:** DRAFT (2026-07-27) — nothing built. Per the
`CHAOS_HARNESS.md` discipline: build the harness, expose the real
failure modes with receipts, THEN build the gate the receipts justify.
This doc is step 1.

Companion harnesses, each of which this one deliberately does not
duplicate:

| Harness | Adversary | Trust-breaker it measures | Where it starts |
|---|---|---|---|
| `CHAOS_QA_METHODOLOGY.md` (chaos v1) | the hardest examiner | confabulation | a question |
| `PERSONA_QA_DESIGN.md` (persona) | a real user's goal | ungraceful failure | a goal |
| `bench/inner_work/CHAOS_HARNESS.md` | an adversarial discloser | harm | a journal entry |
| **this doc (guest)** | **the machine and the clock** | **abandonment** | **a double-click on a cold install** |

---

## 1. Why the existing three don't cover this

Every instrument we own starts the clock after the guest is already
seated. Chaos v1 samples a corpus chunk, so a corpus exists. Persona QA
boots to the chat box — all seven personas (`drive_by`, `thread_haver`,
`paster`, `omniscience_expecter`, `skeptic`, `impatient_rephraser`,
`vibe_typer`) arrive at a working app. Inner-chaos seeds resident
memories. `TTFI.md` measures within-session first-token latency and has
no cold-start scenario.

The one instrument that does cover first launch,
`real/journeys/first-launch-setup.journey.spec.ts`, calls itself "J5
(Tier 1, CRITICAL) … the highest-stakes journey" — and sets
`SOVEREIGN_FORCE_LOCAL: "1"` at line 77 for a legitimate harness reason
(the shared daemon on `:9741` would otherwise be attached to, skipping
the wizard). But that env var is exactly `supervisor_setup::is_enabled()
== false`. **The highest-stakes journey exercises the one branch real
users never take**, and its assertion absorbed the consequence:
`expect(gate.or(chat))` at line 160, with the consent gate clicked only
`if (await gate.isVisible())`.

That single file is the argument for this harness. A *mechanics* test
asks "did the button work on the path I could set up easily." A
*posture* gate asks "was the guest looked after on the path they
actually walk."

### The failure class, stated once

`persona-gates.toml` already carries `grace_mean` — "admits plainly /
offers agency / no internal jargon" — measured 1.0–1.5 against a 1.5
floor, with the rationale field naming the deficit precisely:
**"agency is the deficit."** Guest QA is that same rubric, lifted off
the chat gap-turn and applied to every state a newcomer can reach.

---

## 2. The quality bar

> Is the guest ever **left standing**? A guest is stranded when the
> system is in a state they did not choose, cannot understand, and
> cannot leave. Below that floor, hospitality quality is the trust
> signal: was the next thing offered before it was asked for, and did
> the host absorb the work rather than hand it over?

Two tiers, scored separately, **never averaged** — a warm sentence
cannot buy back a dead end.

### Tier 0 — STRANDING RED LINES (any breach = hard FAIL for the state)

| Red line | Breach looks like | Deterministic? |
|---|---|---|
| `broken_promise` | The guest was told something would happen and it did not, with no error surfaced. **The worst bucket** — see §5. | yes |
| `dead_end` | The state reports a problem and exposes no enabled control that changes it. Includes a terminal view whose only controls all invoke the same failing command. | yes |
| `silent_failure` | An operation failed and the UI shows nothing, or the affected control self-hides so absence is indistinguishable from success. | yes |
| `naked_internal` | A first-hour surface renders an unmapped internal string — a raw `Err` chain, an unmapped phase id, a bare corpus slug, an `os error N`. | yes |
| `unexplained_wait` | Elapsed past the state's budget with no phase text, no percentage, and no ETA. | yes |
| `blamed_guest` | Copy implies user error for a system condition ("invalid", "you must", "failed to") where the guest did nothing wrong. | judge |

`broken_promise`, `dead_end`, and `silent_failure` are zero-tolerance
from day one — any instance is a defect by definition, so no baseline is
needed to set the floor at 0. The rest get floors from the first
measured run (§8).

### Tier 1 — HOSTING QUALITY (composite among non-breaching states)

| Signal | Failure |
|---|---|
| `anticipation` | The obvious next move exists but the guest must go find it. An empty state with no action is the canonical miss. |
| `absorbed_labor` | The guest is asked to decide something the system could have decided, or to do work the system could have done. |
| `reversibility` | A default is applied with no visible, stated way back. |
| `disclosure_fit` | Internals are shown to the guest when they only needed the outcome — or hidden when they needed the reason. The glassbox floor is *available*, not *imposed*. |
| `brevity` | Correct and hospitable but too long to read at the moment it appears. |

Positive markers (raise the composite): a stated default with a stated
reason; a wait that names what it is waiting on; a failure that offers
the specific next move rather than a generic retry; provenance shown as
one line rather than a panel.

---

## 3. What it reuses (nearly everything)

- **Runner, Lane A** — the existing Playwright + mocked-Tauri-bridge
  suite (`fixtures/tauri-shim.js`, per-test `setHandler(cmd, fn)`).
  This is the injection point that already lets `onboarding.spec.ts`
  make the backend reject without emitting `Failed`.
- **Runner, Lane B** — `real/journeys/`'s `spawnDesktop` with real
  process boundaries and a real filesystem.
- **Judge + calibration discipline** — the `--calibrate` gate shape and
  the sensitivity floor from `CHAOS_HARNESS.md`.
- **Gate file discipline** — `persona-gates.toml`: floor/target pairs,
  rationale IN the file, no threshold change without a rationale edit
  in the same commit, anti-accretion.
- **Scenario harvesting** — the `ttfi-recorder` production module
  (`?ttfi=record`), which already exports replayable Scenario-shaped
  logs. §7.

### What is new

1. The unit of measurement is a **state**, not a turn (§4).
2. The adversary is the **environment**, not the question (§6).
3. The **promise ledger** (§5).
4. Scoring is **deterministic-first** (§9).

---

## 4. The unit: a state, not a turn

Chaos and persona score turns because their subject is an utterance.
Hospitality is a property of *screens over time*, so the harness
enumerates **reachable states** and scores each one.

A state is captured as:

```
{ id, route, fault, elapsed_ms,
  visible_text,            // innerText of the app root
  enabled_controls[],      // focusable, non-disabled, with accessible names
  progress: { determinate, percent, eta_text } | null,
  promises_open[],         // §5
  console_errors[] }
```

Enumeration is **fault-driven, not click-driven**: for each fault in the
bank (§6), drive the app to each of its documented first-hour routes and
capture the state. No exhaustive crawl — the route list is explicit and
reviewed, so coverage is legible and gaps are visible rather than
assumed.

First-hour routes (initial set): boot splash · welcome · setup plan ·
setup progress · setup failure · post-restart boot · consent gate ·
empty chat · first turn in flight · first turn failed · corpus install
banner · Library empty · Settings→Knowledge empty · folder-drop flow ·
mesh join from deep link · Explore/Atlas empty.

---

## 5. The promise ledger — the novel primitive

The worst verified breach on the current build is not an ugly error. It
is this: on a default fresh install the guest ticks "Install Wikipedia
Core" on the setup plan, and it never installs, silently.

The mechanism (verified 2026-07-27): `supervisor_setup::is_enabled()`
defaults true, so `setup_flow.rs:405` spawns a new process and
`app_handle.exit(0)`s at `supervisor_setup.rs:271`. The
`completeSetupAuto` promise never resolves, so `handleSetupComplete()`
at `App.svelte:359-394` never runs — and it is the sole call site for
`ensureSeededConversations()`, the ConsentGate routing, and
`startDefaultCorpusInstall()`. On relaunch `App.svelte:287` sees setup
complete and routes straight to chat, never re-checking consent. (The
comment at `App.svelte:376` claiming "the gate will re-appear on the
next launch" is false.) A second, independent silent path covers the
same promise: `App.svelte:392` is
`void startDefaultCorpusInstall().catch(() => {})`, and
`corpusProgress.failures` — documented in-source as "a standing
condition the user still has to resolve" — has zero consumers.

Generalize it. A **promise** is anything the guest was told would
happen:

```
{ id, made_at_state, description, source, resolution }
resolution ∈ { fulfilled | failed_and_surfaced | pending | ABSENT }
```

Sources of promises in the first hour: a checked box on the setup plan;
a recorded consent; a queued background install; a "we'll keep watching
this folder" registration; a peer-assist grant.

**Tier-0 rule: a promise resolving to `ABSENT` is a `broken_promise`
breach.** `pending` past the state's budget is an `unexplained_wait`.
`failed_and_surfaced` is *not* a breach — an honest failure keeps the
guest informed, which is the whole bar.

This is deterministically checkable and it generalizes past the current
bug: any consent-to-outcome path that lives only in a resolved JS
promise, on a process that deliberately exits, is structurally unsafe.
The durable fix (ARCH_PRINCIPLES §7.1) is a persisted intent record the
boot path drains, which makes the violation unrepresentable rather than
merely tested — but the harness is what proves it stays fixed.

---

## 6. The fault bank — the environment as adversary

Each fault is a named, deterministic condition. **Lane assignment is
load-bearing**: a mocked shim cannot fake a process boundary, and the
bug in §5 would be invisible to Lane A. Say so in the file rather than
discovering it later.

### Lane A — mocked bridge (breadth, fast, hard CI gate)

| id | condition |
|---|---|
| `daemon_down` | every command rejects with a connection error |
| `daemon_flaps` | commands fail, then succeed, then fail |
| `download_404` | model fetch rejects mid-phase |
| `download_stalls` | progress events stop at 60% and never resume |
| `manifest_gap` | `bundled manifest is missing a fast slot for this hardware` |
| `config_readonly` | every config write rejects with `os error 13` |
| `catalog_empty` | model catalog resolves to `[]` |
| `hardware_unknown` | hardware probe rejects |
| `corpus_install_fails` | starter corpus install rejects after 20% |
| `zero_everything` | no corpora, no conversations, no peers, no models |
| `slow_first_turn` | first inference takes 90s |
| `first_turn_errors` | first send rejects with a raw anyhow chain |
| `join_link_invalid` | deep-link preview rejects |
| `mesh_empty` | mesh online, zero peers |

### Lane B — real process (narrow, slow, nightly)

| id | condition |
|---|---|
| `supervised_restart` | **the default path** — supervisor enabled, wizard completes, process respawns. Asserts the §5 promise ledger across the restart. |
| `disk_full_midway` | a size-capped volume that exhausts during model download |
| `readonly_home` | config dir mounted read-only — asserts the consent gate is escapable |
| `killed_during_ingest` | SIGKILL mid-corpus-build, then relaunch |

`supervised_restart` is the fault that J5 disables. It is the single
most important scenario in this document.

---

## 7. Seeding the bank from what actually shipped

Calibration examples must be real, not invented. Seven verified
breaches from the 2026-07-27 audit, each a labeled bank entry:

| Label | Breach | Evidence |
|---|---|---|
| `broken_promise` | consented Wikipedia install never starts | §5 |
| `broken_promise` | consent gate and seed conversations never run | `App.svelte:359-394` |
| `dead_end` | boot splash suppresses Retry exactly when `backendError` is set | `App.svelte:530-542` |
| `dead_end` | consent gate is terminal; both buttons invoke the same failing command | `ConsentGate.svelte:43-63` |
| `silent_failure` | `PeerAssistOffer` self-hides at 2 of 4 ingest call sites | `KnowledgeStatus.svelte:463`, `WatchedFolderDetail.svelte:677` |
| `silent_failure` | `corpusProgress.failures` has zero consumers | `corpusProgress.svelte.ts:177` |
| `naked_internal` | setup failure renders `bundled manifest is missing a fast slot` as the only text on screen | `SetupScreen.svelte:65` ← `setup_flow.rs:121` |

Two non-breach controls are mandatory so the judge cannot pass by
suspicion alone: the setup plan's provenance card
(`SetupPlanView.svelte:195-217`) and `LibraryView.svelte:130-144`
("No notebooks yet" + explanation + **Add your first notebook**) are
both hospitable and must score clean.

**Ongoing supply.** Point the existing `ttfi-recorder` at first runs.
Its round-trip contract is already the thing "that lets us trust
scenarios harvested from real usage" — a recorded first run becomes a
replayable guest scenario, which is the only way to keep the bank
honest about a moment that happens once per user.

---

## 8. Metrics and gates

New rows for `persona-gates.toml` (or a sibling `guest-gates.toml`),
same discipline: rationale in the file, no threshold edit without a
rationale edit in the same commit.

| metric | dir | floor | target | note |
|---|---|---|---|---|
| `promise_kept_rate` | min | **1.0** | 1.0 | Zero-tolerance. A count, not a rate — one broken promise fails the run. |
| `stranding_rate` | max | **0** | 0 | States with any `dead_end` or `silent_failure` breach. |
| `naked_internal_rate` | max | TBD | 0 | Set from first baseline; direction is not in doubt. |
| `first_run_grace_mean` | min | TBD | 2.5 | The `grace_mean` rubric over first-hour states. Target matches the chat gate so one bar governs both. |
| `time_to_first_ask_s` | max | TBD | — | **The newcomer headline.** Launch → guest can send a message. Currently gated on a 19–23 GB download on a typical M-series machine, so the first measurement is expected to be very large; that number is the point. |

`time_to_first_ask` is the metric most likely to change the product
rather than the tests. It is the guest-lane analogue of TTV, and if it
is download-bound then no amount of copy fixes it — the answer is to
make the app useful before the download finishes.

**Do not set the TBD floors in this commit.** Increment 1 measures;
Increment 3 gates. Setting a floor before a baseline is the mistake
`grounded_rate` already paid for once (floor lowered 0.25 → 0.20 on
2026-07-11 when the pooled honest rate landed at 0.19).

---

## 9. Deterministic-first — the load-bearing design decision

`CHAOS_HARNESS.md`'s calibration receipt is the reason this harness is
shaped differently from its siblings:

> Category agreement plateaus at 0.59: the 35B systematically
> over-lists Tier-1 signals … Prompt-language fixes did not move it …
> the candidate fix is a deterministic signal-verification layer, not
> more rubric prose.

Take that lesson as a premise rather than re-learning it. Five of six
Tier-0 red lines are DOM predicates, not judgments:

- `dead_end` — `enabled_controls.length === 0` while an error is
  present, or all controls invoke the same failed command
- `silent_failure` — a `console_errors[]` entry or a rejected command
  with no corresponding visible text delta
- `naked_internal` — `visible_text` matches the shared jargon list, an
  `os error \d+`, or an unmapped phase id
- `unexplained_wait` — `elapsed_ms > budget && progress === null`
- `broken_promise` — ledger resolution is `ABSENT`

Only `blamed_guest` and the Tier-1 composite need a model. That inverts
the cost profile against the other harnesses and is what lets Lane A be
a **hard CI gate** rather than a tracked lane.

**One shared jargon list.** Lift the wordlist out of the persona
posture rubric ("doesn't mention corpora, mesh, retrieval, atlases, or
chunks") into a single exported const consumed by the persona judge,
this harness, and a static string check. Three separate de-jargon
commits have already landed by hand — `720c75c7 fix(grace): de-jargon
the rewrite prefix`, `c9fdcd27 de-jargon the copy`, `3cc4ef76 clean
rubric — machinery jargon != provenance language`. One list is the fix
for a recurring class.

---

## 10. Increments

**I1 — capture, no scoring.** State capture + the Lane A fault bank +
an HTML gallery of every captured state. Ships as a `study` artifact.
The gallery alone is expected to be the most persuasive output in this
document: fourteen faults across sixteen routes is a wall of screens
nobody has ever looked at side by side.

**I2 — deterministic Tier 0.** The five DOM predicates + the promise
ledger. Run against the seeded bank (§7); it must reproduce all seven
known breaches and clear both controls, or the predicates are wrong.

**I3 — gate.** Lane A becomes a hard CI gate on the zero-tolerance
rows. TBD floors set from the I1/I2 baseline with rationale.

**I4 — Lane B.** `supervised_restart` first, since it is the scenario
that covers the live defect. Nightly, not on the PR critical path.

**I5 — judge.** `blamed_guest` + the Tier-1 composite, with a
`--calibrate` sensitivity gate ≥0.9 against the §7 bank before it is
allowed to influence any number.

I1 and I2 are the whole value if I3–I5 never land. Do not build the
judge first.

---

## 11. Open questions

- **State identity across faults.** Is "boot splash under `daemon_down`"
  the same state as "boot splash under `download_stalls`" for
  deduplication? Proposal: key on `(route, fault)` and accept the
  duplication; the gallery is more useful than a minimal set.
- **Wait budgets per state.** `unexplained_wait` needs a per-state
  budget. The boot splash currently waits 45s before offering Retry
  (`App.svelte:324`); is 45s the budget, or is 45s the bug? I1 should
  report the distribution and let the receipts answer.
- **Does Lane A's mocked bridge diverge from the real app enough to
  produce false clean states?** The mock is a fidelity risk in exactly
  the direction that matters (it cannot exit the process). I4 is the
  mitigation; the residual should be measured by running the same
  routes in both lanes and diffing.
- **Should `disclosure_fit` be Tier 0?** Showing internals to a
  newcomer is currently Tier 1, but `naked_internal` already covers the
  acute case. Revisit after I1.
