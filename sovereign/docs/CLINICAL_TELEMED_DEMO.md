# Clinical Telemed Demo — system mapping

The setup: Sarah, an addiction-medicine MD and researcher at a
university medical center, calls her old college roommate. She just
got a NIDA R34 planning grant — $250K total, $50K earmarked for
software — to pilot a telemedicine app for medication-assisted
treatment (MAT) of opioid use disorder at three rural recovery
centers in Appalachia. Buprenorphine providers are scarce there;
patients drive two hours for a 15-minute check-in. Eighteen-month
timeline. IRB submission in week six.

She's a clinician, not a developer. Her friend is a developer. The
ceiling is hard: $50K means no AWS bill, no per-token API spend, no
contractor team. One person and a 64 GB Mac. Two-week prototype,
two-week clinical pilot with two consenting volunteers, then plan
the rest.

This doc traces that arc against the Phase 1–7 refactor work that
makes it survivable.

> The audit isn't a side artifact here — **it's Section 7 of the IRB
> submission**. Every decision they make about PHI handling, prescriber
> verification, crisis escalation, telehealth-license-state-matrix
> needs to be defensible to an IRB reviewer who'll never read the code.
> The audit's "non-empty floor" contract is the IRB's "engineering
> rationale" floor.

---

## Day 0 evening · The charter call

Sarah is on Zoom describing the regulatory landscape:

- **HIPAA** — every byte of PHI in transit and at rest, BAAs with every
  vendor, audit logs that survive a subpoena.
- **42 CFR Part 2** — federal SUD privacy. Treatment records can't be
  redisclosed without specific patient consent, even within the same
  hospital system. This is much stricter than vanilla HIPAA.
- **Ryan Haight Act / DEA EPCS** — buprenorphine is Schedule III. The
  initial induction visit either needs in-person OR a qualifying
  telemedicine exception (one was created in 2020, narrowed in 2023,
  re-broadened temporarily; check current rules).
- **State licensure** — the prescriber must be licensed in the state
  the patient is physically located in at the time of the visit.

The developer types as she talks:

```
sovereign init
# … 80 seconds later, MCP server up at :9741 with 5 always-on tools …

sovereign charter
```

`sovereign charter` (Phase 1: collapsed `sovereign project charter`)
runs the founding-conversation flow over the things Sarah just said.
The output is `.sovereign/CHARTER.md` — the project's constitution.

| What the charter captures | Why it's load-bearing |
|---|---|
| Regulatory framework | Every PHI-touching decision will be checked against this in real time via the structural nudge (Phase 7.1). |
| Clinical scope (OUD/MAT only — not co-occurring full SUD spectrum, not psychotherapy alone) | Bounds what the agent will scaffold. Out-of-scope features get refused with a pointer back to the charter. |
| User taxonomy (prescriber vs. peer recovery specialist vs. patient vs. compliance auditor) | Each role gets its own approval-gate identity. Phase 6's flow + Phase 5b's spec-presence gating make these per-feature. |
| Privacy posture ("PHI is in the EHR backend; the app is a thin client; the audit log is segregated") | Becomes the architecture invariants in the next step. |

The developer commits the charter. `git commit -m "charter: MAT
telemed pilot"`. From this point every chat completion includes the
charter in the context preamble (the existing `ContextInjector`
middleware reads `.sovereign/CHARTER.md`).

---

## Day 0 late night · `ARCHITECTURE.md` as the privacy contract

The developer drops a top-level `ARCHITECTURE.md` with the privacy
invariants written as `**bold**` spans and `# Heading` lines so the
nudge keyword extractor picks them up:

```markdown
# MAT Telemed Architecture

## Modules
- `auth/` — prescriber identity, EPCS hardware token integration
- `encounter/` — scheduled video visit, async messaging
- `prescription/` — buprenorphine workflow, controlled-substance audit
- `crisis/` — patient self-report → on-call escalation tree
- `compliance/` — 42 CFR Part 2 audit log, segregated storage

## Invariants
- **PHI never logged** at any level. Logging passes through
  `redact::safe_log` — no exceptions.
- **Diagnosis codes are 42 CFR Part 2-protected**. Never returned
  in HTTP error responses, stack traces, or telemetry.
- **Prescriber state-licensure** must be checked at every Rx event,
  not just at login. State of practice = state where patient is
  physically located at visit start.
- **Crisis path is fail-open**: if the audit log is degraded,
  patient escalation MUST still complete.
```

Two side effects of this commit:

1. **Spec gate stays on**. `mcp_surface::spec_present_in_dir` finds
   `ARCHITECTURE.md` at top level (Phase 5b). `tools/list` now
   includes `spec`/`drift`/`note`/`notes`. opencode receives a
   `notifications/tools/list_changed` SSE frame and refetches.

2. **Spec invariant keywords extracted**. Phase 7.1's
   `extract_spec_invariant_keywords` parses headings + bold spans:
   `["MAT Telemed Architecture", "Modules", "Invariants", "PHI never
   logged", "Diagnosis codes are 42 CFR Part 2-protected", "Prescriber
   state-licensure", "Crisis path is fail-open"]`. These become
   nudge signal 5's keyword set.

When the developer (or the agent) later touches code that mentions
`phi`, `diagnosis`, `licensure`, or `crisis_path`, the structural
nudge fires:

```
[note worth recording? You modified a struct/trait/impl definition;
 touched code matching a spec invariant. Call note(decision, …).]
```

The keyword normaliser (`fn normalise` in `nudge.rs`) bridges
separator differences — "PHI never logged" matches `phi_never_logged`
in code.

---

## Day 1 · Workflows as feature specs

Sarah lists the workflows. Each becomes a spec:

```
.sovereign/features/
├── induction-visit/spec.md           # Initial buprenorphine induction
├── maintenance-checkin/spec.md       # Weekly/biweekly visit
├── prescription-rx/spec.md           # EPCS workflow for Schedule III
├── peer-support-group/spec.md        # Async chat + scheduled video group
├── crisis-escalation/spec.md         # Severe withdrawal / OD risk path
└── compliance-audit-log/spec.md      # 42 CFR Part 2 access trail
```

Each spec is co-written: Sarah writes the **Goal**, **Invariants**,
and **Stop conditions** (the clinical contract); the developer fills
in the engineering specifics on subsequent commits.

A representative spec — `prescription-rx/spec.md`:

```markdown
# Feature: Buprenorphine prescription (EPCS)

## Goal
A board-certified addiction-medicine MD writes a Schedule III
prescription for buprenorphine/naloxone via the app, signed
electronically with their DEA-approved hardware token, transmitted
to the patient's chosen pharmacy via the SureScripts EPCS pipeline.

## Invariants
- **Identity proofing IDP must complete BEFORE any Rx event**.
  IDP is once-per-prescriber, not per-Rx.
- **Two-factor authentication** at the moment of signing — the
  hardware token IS the second factor; password is the first.
  No exceptions.
- **State licensure check** runs at signing, against the patient's
  current physical location. Mismatch = refuse, log, surface a
  human-readable error to the prescriber.
- **No diagnosis code in transmission**. The Rx carries NDC + dose
  + sig only. Diagnosis is in the chart, never in the Rx packet.

## Milestones
### 1. IDP integration (passing)
**Stop condition:** End-to-end IDP test against the sandbox harness
in `prescription-rx/test_idp.rs` returns `IdpStatus::Verified` for
the test prescriber.

### 2. Prescriber-side signing UI
**Stop condition:** Manual run produces a transmissible NCPDP SCRIPT
20231 message with valid signature.

### 3. SureScripts sandbox round-trip
**Stop condition:** The pipeline returns SUCCESS for a synthetic
patient and the resulting `script_id` is logged to the segregated
EPCS audit log.

### 4. State-licensure refusal path
**Stop condition:** Test seeds a prescriber licensed in OH, patient
located in TN, runs the signing flow, asserts a refused-and-logged
outcome.
```

Sarah commits these specs. Each commit lands in
`find_approval_via_git`'s reach (Phase 6). The agent gets write-tool
permission scoped to that feature_id. No `sovereign provision`
ceremony.

---

## Day 2–3 · Domain scaffolding with the call graph in reverse

The codebase doesn't exist yet — the call graph is empty. But the
agent can scaffold module skeletons from the specs and use
`symbols`/`callers`/`callees` as soon as a few definitions land.

The developer drives:

```
> Read ARCHITECTURE.md and every feature spec. Scaffold the Rust
> backend (axum + sqlx + tokio) and React Native frontend with
> the module structure I documented. For each feature, generate
> the empty handler skeleton with TODO markers tied to the spec
> milestones. Use `note` to record any architectural decisions
> you make beyond what the spec dictates.
```

The agent generates ~30 files. Several decisions emerge that aren't
in the spec, and they all get recorded — some explicitly via `note`,
some via Phase 7.2's per-turn extractor:

- Agent says "I'll use sqlx with offline mode for the encounter
  schema migrations because we want CI to pass without a live DB."
  → `response_mine::mine` catches this as a `Commitment` match →
  `pending_decision = Some(...)` on the session →
  next turn, no correction → `source='extracted'` note persisted +
  `[Noted: ...]` injected into the system prompt.

- Agent calls `note(decision, "chose libsodium-rs over ring for chart
  encryption — FIPS-validated build incoming")` → `source='agent'`.

- The structural nudge fires when the agent touches logging code
  (signal 5 — `PHI` keyword from the architecture doc). Agent acts:
  writes a `redact::safe_log` wrapper module + a unit test that asserts
  no diagnosis-shaped strings can pass through. Records via `note`.

End of day 3 the audit looks like:

```
$ sovereign audit

## Decisions
- Chose libsodium-rs over ring for chart encryption …  _[agent]_
- I'll use sqlx with offline mode for encounter migrations …  _[extracted]_
- All PHI logging routes through redact::safe_log; …  _[agent]_
…

## Notes by kind
| Kind | Count |
| decision | 11 |
| invariant | 4 |
| reflection | 7 |
```

Reflection = the Phase 7.1 `ToolPatternMatcher` rows ("investigated
impact (callers, callees) before running build", etc.).

---

## Day 4–7 · Implementation, feature by feature

Workflow per feature:

1. Sarah writes the spec body (clinical contract).
2. Developer commits → write-tool approval flips on for that
   feature_id (Phase 6).
3. Agent reads spec via the `spec` tool (Phase 2 renamed from
   `feature_spec`), sees Sarah's invariants in its context.
4. Agent traces the existing module via `callers`/`callees`/`blast`
   (Phase 2 short-form ids) before modifying anything.
5. After each round of edits, `build` runs through the watcher
   pipeline; `lint_status` + `test_status` aren't direct calls — the
   agent reads `build` and gets fresh-or-stale status.
6. Once the feature's stop condition passes, `sovereign milestone
   <feature-id> <N>` (Phase 1: collapsed `atos start-milestone`/
   `end-milestone`/`phase pass`) records the milestone artefact.

The `decision_extractor` middleware (Phase 7.2) runs every turn.
Sarah's invariants ("two-factor at the moment of signing — no
exceptions") get re-grounded as decisions in the audit each time the
agent makes an implementation choice that touches them:

- "I'll use the WebAuthn challenge-response on the hardware token
  rather than a TOTP fallback because the spec calls out 'no
  exceptions' on 2FA at signing." → extracted-source note.

The audit accumulates without anyone manually writing it.

### Reversal — IRB feedback mid-build

End of day 5, Sarah pings: "I talked to my IRB liaison. We can't
rely on patient-stated location for the licensure check; we need
either an attested check-in form OR cross-reference with the IP
geolocation as a sanity bound."

Developer:

```
> The licensure check can no longer trust patient-stated location
> alone. The IRB requires an attested check-in form plus an IP
> geolocation cross-check as a sanity bound. The decision in
> note:abc-123 is reversed. Implement the new flow per
> features/prescription-rx/spec.md (Sarah is updating the spec now).
```

Agent makes the change, calls `note(decision, "Geolocation cross-
check added as sanity bound on patient-stated location, per IRB
review.", supersedes="abc-123")`. The next `sovereign audit` renders:

```
## Decisions
- Trusted patient-stated location for state-licensure check …  _[agent]_
  ↳ REVERSED 2026-04-30: Geolocation cross-check added as sanity
    bound, per IRB review.  _[agent]_
- Two-factor at signing — hardware token only, no TOTP fallback …  _[extracted]_
…
```

Both versions visible. The IRB reviewer reading Section 7 of the
submission sees the original choice, the reason for reversal, and
the new design — the audit's reversal display (Phase 7.3's
`render_decisions` walking the `supersedes` map) makes this legible
without anyone re-writing the doc.

---

## End of week 1 · `sovereign audit` is the IRB submission

```
$ sovereign audit > docs/engineering-rationale-w1.md
```

The output is structured exactly the way an IRB engineering-section
reviewer wants:

```
## Spec / Charter
[lifecycle, charter version + drift, current phase]

## Decisions
[everything tagged decision/invariant, sorted by source priority,
 with reversals inlined]

## Deviations
[changes that diverged from a previously-approved spec hash —
 the approval_gate's drift-detection writes these]

## Open questions
[uncertainty notes, with low-confidence flagged on inferred-source]

## Observed patterns
[every blast→build / spec→build / notes→note sequence the
 ToolPatternMatcher caught]

## Features
[scope=feature merged with directory presence — every feature
 the agent worked on appears even without a features.db row]

## Notes by kind
[count summary]
```

Sarah reads it. She doesn't read code. She reads the Decisions
section and recognises every entry — they're the conversations
they've had over the past five days, transcribed and stamped with
date + source.

She forwards the markdown to her IRB liaison with one paragraph of
cover. The liaison replies the next morning: "This is the cleanest
engineering-rationale section I've seen from a developer in fifteen
years. Approved with no revisions."

The "non-empty floor" contract didn't matter for the panicked-engineer
demo because those bugs would have produced commits one way or
another. **It's load-bearing here**: Sarah and her developer are
moving fast, and neither of them has time to keep a separate
engineering log. The four extraction streams (`agent`, `committed`,
`extracted`, `observed`) are the engineering log.

---

## Week 2 · Clinical pilot + iteration

Sarah does mock visits with two consenting volunteer patients.
Issues surface fast:

| What Sarah reports | What happens in Sovereign |
|---|---|
| "Audio echoes after 8 minutes." | New spec at `features/audio-echo-fix/spec.md` with the failure mode and a stop condition (record a 12-minute test session, no echo). Agent traces the WebRTC config via `callers("RtcPeerConnection")`. Two-line config fix. Decision auto-recorded. |
| "When the prescriber's hardware token battery is dead, the error message says 'authentication failure' which doesn't help." | Spec for the error path. Agent edits the auth handler. Structural nudge fires (auth path = `Prescriber state-licensure` keyword neighbour). Note recorded: "Hardware-token-failure error now distinguishes battery-dead from PIN-incorrect from token-not-enrolled." |
| "I can hear the recovery specialist breathing in the background of the patient's video." | Domain-level architectural rethink. New spec for noise suppression. Agent investigates Twilio SDK options. Three decisions recorded across the implementation. |

Each issue closes the same way: spec → commit → approval → fix →
milestone → audit updates.

The commit harvester (Phase 7.1
`sovereign-mesh::commit_harvest::harvest_between`) catches every
substantive fix message — `fix:` / `feat:` / `refactor:` prefixes map
to `kind=decision`, others to the conservative default. The audit's
"committed" rows now include the fix history without anyone
duplicating it.

---

## End of month 1 · Deliverables

What Sarah ships to NIDA + her department chair:

1. **Working pilot app** on TestFlight + Google Play internal track.
   Two volunteer patients have completed three induction visits and
   one MAT check-in each.
2. **`sovereign audit`** rendered to `docs/engineering-rationale.md`,
   submitted as Section 7 of the IRB modification request.
3. **`.sovereign/CHARTER.md`** + every feature spec, committed —
   the project's "what and why" baseline.
4. **The agent's `tool_call_log`** ring buffer (10 K rows) — the
   forensic log of every code-intel call. Subpoena-grade if it ever
   matters.

Total developer time: 14 days. Total Sovereign API spend: $0 (every
inference call ran locally on the 64 GB Mac via the daemon's primary
slot — Qwen3.5-32B for reasoning, Qwen3-Coder-14B hot-swapped in for
mechanical refactoring runs). Total spend on the $50K software
budget: TestFlight, Apple Developer + Google Play accounts, a static
IP for the EPCS sandbox, and a contractor accessibility review —
roughly $4,200.

---

## Why each Phase carries its weight here

### Phase 1: namespace collapse
A clinician collaborator who occasionally drives the CLI doesn't
need to remember `sovereign project charter` vs. `sovereign atos
report` vs. `sovereign reflect`. `sovereign charter`, `sovereign
audit`, `sovereign notes` — flat, predictable, copy-paste friendly.

### Phase 2: MCP tool consolidation
9 tools (5 always-on + 4 spec-gated) is a list a non-engineer can
hold in their head. The previous 26 wasn't.

### Phase 3 + 4: `init` auto-spawns serve, daemon absorbs setup
The project lifetime measures days, not months. Re-onboarding a
collaborator's machine when they pull the repo for the first time
must be one command, not three. `git pull && sovereign init` →
working agent with the project's full charter context in 90 seconds.

### Phase 5b: spec-presence gate + `tools/list_changed`
Sarah edits a spec in her browser-based markdown editor while the
developer types in the terminal. Within ~100ms of her save the
agent's tool list updates and her clinical contract is in the next
turn's context. No "restart your agent." This is the difference
between collaboration and serial pair-programming.

### Phase 6: spec-as-approval, no provision/found ceremony
A clinical researcher will write a spec.md. She will not run
`sovereign atos provision`. The whole point of Phase 6 was to make
the natural artefact (a markdown file in a git repo) be the
load-bearing one. `find_approval_via_git` reading the commit log is
exactly the trust signal an IRB reviewer recognises ("the change was
authored by Dr. X on date Y, recorded in the SCM").

### Phase 7.1: pattern matcher + commit harvester + structural nudge
The audit's IRB-grade quality comes from these three streams running
in the background while the human work happens in the foreground.
Without them, every decision and every PHI-touching change would
have to be transcribed by hand. Nobody does that.

### Phase 7.2: per-turn decision extractor
This is the one that catches "I'll use libsodium because…" the moment
the agent says it, before the developer would have remembered to
record it. When Sarah reads the audit at end of week one, she
recognises **eight separate exchanges** that nobody wrote down — the
extractor caught them all.

### Phase 7.3: multi-source audit + reversal display + `--recover`
The IRB reviewer reads the Decisions section the way a code reviewer
reads a diff — top to bottom, scanning for "wait, why?" The source
priority sort (`agent > committed > extracted > inferred > observed`)
puts the highest-trust rows first. The reversal display preserves
the back-and-forth that's the IRB's actual decision audit. The
`--recover` flag means a daemon crash mid-grant doesn't lose any
of it.

---

## What's wired vs. partial here too

| Capability the demo uses | Status |
|---|---|
| `sovereign charter` capturing regulatory framework | Wired (existing M6.3 founding flow surfaces under the new flat name) |
| Spec-as-approval, IRB-style traceability | Wired (Phase 6 + Phase 5b) |
| Charter + ARCHITECTURE.md keyword extraction → structural nudge | Wired (Phase 7.1: `extract_spec_invariant_keywords`, `nudge::pending_text`) |
| Per-turn decision auto-recording | Wired (Phase 7.2 middleware; needs to be added to default pipeline config for clinical pipeline) |
| Audit as IRB submission | Wired (Phase 7.3 multi-source assembly + reversal display) |
| Local 32B + 14B-coder hot-swap on a 64 GB Mac | Wired (existing slot manager — see `invariant_appstate_arc_get_mut_order`, `project_polished_slot_management`) |
| LLM-backed `DiffDecisionExtractor` for end-of-week audit summarisation | **Trait + stub backend ready;** plugging the daemon's primary-slot LLM is the natural next step. The `agent`/`committed`/`observed` streams already deliver IRB-grade audits without it. |
| Telehealth-domain knowledge in the model | Out of scope for Sovereign — the developer brings the domain knowledge, Sarah validates it. Sovereign captures and structures the resulting decisions. |
| EPCS / DEA / SureScripts integration | Out of scope for Sovereign — these are domain libraries the developer pulls in. Sovereign records the integration decisions (which lib, why, what BAA terms). |

---

## Cross-references

- `sovereign-cli/src/init.rs`, `serve_cmd.rs` — `sovereign init` →
  background MCP server (Phase 3)
- `sovereign-cli/src/daemon_cmd.rs` — daemon-absorbs-wizard (Phase 4),
  `Reindexer::with_commit_harvester` wiring (Phase 7.1)
- `sovereign-tools/src/mcp_surface.rs`,
  `sovereign-tools/src/spec_watcher.rs` — gate + watcher (Phase 5/5b)
- `sovereign-mesh/src/commit_harvest.rs` — git → committed-source notes (Phase 7.1)
- `sovereign-tools/src/notes/patterns.rs`,
  `sovereign-tools/src/notes/nudge.rs`,
  `sovereign-tools/src/notes/response_mine.rs`,
  `sovereign-tools/src/notes/diff_extract.rs` — Phase 7.1 + 7.2 streams
- `commonwealth-api/src/middleware/decision_extractor.rs` — per-turn
  decision capture (Phase 7.2)
- `commonwealth-api/src/middleware/approval_gate.rs` — spec-as-approval
  via committed git (Phase 6)
- `sovereign-cli/src/project_cmd.rs::build_audit_report` — IRB-grade
  multi-source audit assembly (Phase 7.3)
- `sovereign-cli/src/audit_recover.rs` — `--recover` (Phase 7.3)
- `PANICKED_ENGINEER_DEMO.md` — companion brownfield-triage story
