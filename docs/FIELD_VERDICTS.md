# Field Verdicts — the operator's day, with evidence in it

**Date:** 2026-08-07
**Status:** artifacts A-D SHIPPED same day (`3ddecc71` briefing,
`9e4a8208` field anchors, `19ab5f16` landing field-diff + ledger row
"Landing field-diff", `cd894a82` draft-time logging). Proven on its own
branch: the seat reviewed artifact B's landing with `--field` and cited
the field evidence in its approve. H1/H2 (the seat screen) remain
design, gated on the §5 funding conditions. Minimal by mandate:
composes two shipped systems (`docs/COMAINTAINER.md`,
`docs/FIELDGLASS.md`); the end-state surface is drawn (§3, mockup
rendered 2026-08-07) so the increments aim at a screen that already
exists on paper.

The parents' own headers declare the split this closes — "the
comaintainer reads landings; Fieldglass renders the field." Judgment
and perception currently meet in exactly one place: the operator's
head. This design moves that meeting into the three hops of the
operator's day where it is cheapest to hold, and nowhere else.

**The thesis being proven:** a verdict grounded in structure the
subject cannot author is categorically stronger than a verdict grounded
in prose the subject wrote. If that grounding does not visibly change
an operator decision within two weeks of ordinary use, the thesis is
disproven cheaply and we stop.

---

## 1. The operator's day — the journey, traced

The end user is the operator running the seat. Every artifact below is
derived from a scene; nothing ships that a scene does not demand. The
numbers are the parents': ~12.7 commits/day landing, ~1.6 operator
messages per landed commit, a 30-second morning glance as the stated
Fieldglass bar.

### Scene 0 — morning. Two rituals that should be one glance.

**Today.** The operator's morning is split across two instruments that
do not know about each other. The seat briefing
(`.claude/skills/comaintainer/SKILL.md` step 2) gathers four surfaces —
orders, overnight verdicts, pool state, ledger review-bys — with
free-form assembly. The field glance (`.claude/skills/fieldglass`) is a
separate ritual: render, extract delta, relay. Terrain never appears in
the briefing; command never appears on the field. And neither ritual
imposes a discipline on its own output — a briefing line can be a
status ("7 orders open") rather than a decision, and nothing stops it.

**With field verdicts.** One glance. The briefing becomes five fixed
lines — the five constant factors — each read from a surface that
already exists, each ending in either a decision to make or the literal
words "nothing to decide":

1. **Earth** (terrain) — the fieldglass sidecar's `.delta` and
   `.honesty`, exactly the fields the `/fieldglass` skill already
   extracts. On seat mornings this line *absorbs* the standalone relay:
   one ritual, not two. First-render / no-recent-render is said as
   such, never as "no change" (§18.2).
2. **Heaven** (timing) — everything with an age: stale `svrn posture`
   rows, ledger rows past review-by, drift staleness, sidecar honesty
   ages.
3. **Moral Law** (unity of purpose) — open orders (`co-order.sh list`)
   plus in-flight frames whose `Next` has drifted from their objective
   (the `carried[]` / `objective_sessions` advisories the frame write
   already returns).
4. **The Commander** (the role's own state) — `co-directive-log.sh
   --stats` per-kind edit rate; `verdicts.jsonl` tail, overrides
   pending review first.
5. **Method** (discipline) — the mechanical gates' last word: lint/test
   artifacts under `target/*/latest`, contract nightly verdict.

**What the operator does differently:** reads five lines, makes the
decisions they carry, and is done with "what needs me today" in one
pass. The briefing stays a logged directive (kind=briefing), so its
quality remains a trainable lane with zero new plumbing.

**Technical facts:** every line is a read the seat already knows how to
perform; the Earth extraction is copy-in from the fieldglass skill. No
script — assembly is six read-only calls, and the Fieldglass house rule
(manual flow is the contract until proven) applies verbatim.

**Derives:** artifact A (a skill-file edit). Nothing else.

### Scene 1 — mid-morning. A verdict draft arrives; the operator judges the judge.

**Today.** A worker reports done. The seat runs
`scripts/co-review.sh <ref>`; the bundle it assembles
(`co-review.sh:53-95`) is diff, commit message, gate summaries, matched
notes, ledger greps. The draft verdict reaches the operator citing
prose-side anchors — ARCH §§, note ids, ledger slugs
(`gym/comaintainer/contract.txt`). Now the M0 supervision moment does
its work: the operator approves or edits. But to judge a citation like
"§3.1, file too long" the operator must either trust the seat's reading
of the diff or open the diff themselves — which at ~12.7 landings/day
is exactly the thing the seat exists to prevent. The one evidence class
that is *checkable without reading anything* — the field — is absent
from the bundle.

**With field verdicts.** The bundle gains one section,
`=== FIELD EVIDENCE (fieldglass sidecar) ===`, read from the standing
`~/.sovereign/arch/<corpus>/fieldglass.json` that the morning glance
already maintains — no render runs. It is scoped to the commit's
changed files, from fields that already exist:

| Evidence line | Sidecar source |
|---|---|
| offender status + line count | `files[].offender`, `files[].lines` |
| tollbooth pressure | `files[].commits_90d`, `attention.tollbooths` membership |
| bridge score | `files[].bridge` |
| comprehension tax | `attention.comprehension_tax` membership |
| clone arcs touching changed files | `dup_arcs[]` filtered on path |
| layer violations touching changed crates | `flow_edges[]` with violation kind |

And the verdict grammar grows one anchor form the operator can check
mechanically: `field:<class>:<path>`, class one of
`offender | tollbooth | bridge | dup | tax | layer-violation`. The gym
resolver's existing `basis-exists` check (`score.py`, the
`basis-exists` / `basis-bears` pair) gains one arm: a `field:` anchor
exists iff that (class, path) pair is in the sidecar the verdict record
names — so the record gains `sidecar_head` and `sidecar_unix`.

**What the operator does differently:** approves or edits a draft whose
strongest citations *resolve* — "revise: this lands 300 lines in a file
the field already rings red, `field:offender:runtime/retrieval/mod.rs`"
is checkable in one click on the page that is already open from
Scene 0. The approve/edit loop gets faster and the edits get more
informative, which is the flywheel's food.

**Honesty at this hop:** the section header always states evidence age
— sidecar `head`, `generated_unix`, commits the reviewed ref is ahead.
A missing sidecar prints `ABSENT — named, not omitted` (the bundle's
existing §18.3 discipline). Charter-side, one paragraph: field evidence
is cited only with age stated; a claim depending on *current* structure
against a stale sidecar is `could-not-judge(missing: fresh field)`, not
a guess. Scene 2 is where freshness becomes mechanical.

**Derives:** artifact B (~40 lines in `co-review.sh`, ~15 in
`score.py`, a sentence each in `contract.txt` and `CHARTER.md`).

### Scene 2 — the crux. Was the landing what the worker said it was?

**Today.** This is the parents' shared characteristic failure: green
that is not real. Every section of today's bundle is either authored by
the worker (diff, message) or attestable by it (gates it can quote);
Scene 1's field section helps but describes the *morning's* terrain,
not what this landing just did to it. §18 was distilled from 818 notes
because plausible-green kept spending trust; the first Fieldglass
render proved the field sees what diff review misses (a 139-line
near-clone family, a 25-method × 11-crate trait matrix — findings no
landing review had surfaced).

**With field verdicts.** At landing review the seat can ask the terrain
itself: `co-review.sh --field` runs one degraded render to scratch —

```
svrn code fieldglass --no-dup --out <scratch>/landing-<ref>.html --json
```

— doubly safe for the glance baseline (degraded renders never replace
it, as-built rule; `--out` lands elsewhere anyway), then diffs the
changed files' rows against the standing sidecar. Three checks, no
more:

1. **Growth** — `lines` delta per changed file; any offender transition
   (`offender: false → true`) is the headline.
2. **Coupling** — new violation-kind `flow_edges` touching the changed
   crates; `fan_in` deltas on changed files.
3. **Freshness, mechanical now** — the scratch render's own
   `honesty.scip_commits_behind` against the reviewed ref. If the
   Reindexer's 30-second poll has not caught the landing, the
   structural diff is `could-not-judge(missing: SCIP at <ref>)` — never
   "no structural change". The seat re-runs a minute later; no
   wait-loop is built.

The result is a small `field_evidence` object in the bundle and the
verdict record. What it cannot see is stated in it: the dup tier is
skipped (`--no-dup`; the O(n²) pass), so a copy-paste landing surfaces
at the next morning glance — one honesty line, printed always.

**What the operator does differently:** at the moment of
approve/edit/override, the draft carries one section the worker could
not have authored and the seat could not have hallucinated. A landing
narrated as "small focused change" that the diff shows pushing a file
over 1200 lines, or adding an against-the-grain edge, is the exact
contradiction this design exists to produce. The operator sees it as a
verdict line, not as next quarter's archaeology.

**Derives:** artifact C (~60 lines in `co-review.sh`; opt-in flag; one
DEFAULTS_LEDGER row, below).

### Scene 3 — between times. The contradiction becomes the curriculum.

**Today.** Overrides mint training episodes (`co-review.sh --override`,
the recorded rule: "logging the override is what mints the training
episode"), but their labels are tier B — operator-settled, never
gating.

**With field verdicts.** A Scene 2 contradiction is a **tier-A label by
construction**: the instrument (SCIP + git through a deterministic
renderer) settled who was right, which is the gym's own definition of
tier A. No machinery ships for this in this initiative — the verdict
record already carries the evidence and the sidecar refs; harvesting is
the gym's existing business. The scene is stated so the value is
counted, not so a harvester gets built (open question 3).

**What the operator does differently:** nothing, and that is the point
— the flywheel eats for free.

## 2. The artifacts — everything the journey demands, nothing more

| Artifact | Scene it serves | What it is | Cost |
|---|---|---|---|
| A | 0 | five-factors briefing | one skill-file edit; no script |
| B | 1 | field section in the bundle + `field:` anchor + resolver arm | ~55 lines total across `co-review.sh`, `score.py`, `contract.txt`, `CHARTER.md` |
| C | 2 | `--field` landing diff | ~60 lines in `co-review.sh`; one ledger row |
| D | the surface (§3–4, needed at H1) | drafts logged at draft time: `{id, status: pending, …}` + resolution records | ~20 lines in `co-directive-log.sh`; no new concept — a status field on existing records |

Concept count: +1 citation form, +1 flag. Stores: +0. Renders on cron:
+0.

**Deliberately NOT built** (named non-goals, each refused because no
scene demands it): no new render modes, no thumbnails or scoped
mini-pages, no per-commit auto-render, no MCP tool, no committed
terrain snapshots (the gym's future business), no briefing script
(funding condition below), no wait-loop on the Reindexer, no episode
auto-harvester (Scene 3 stays free), and no daemon surface **until
H2's single route pair** (§4 loop 2) — which is deferred, separately
funded, and the only machinery this design will ever add.

## 3. The surface the journey converges on — the seat screen

One screen, drawn before anything is built so every increment aims at
it. Interactive mockup (rendered 2026-08-07, real repo data, the
focus-lighting works):
<https://claude.ai/code/artifact/390dfcae-d535-436d-b8f5-e15bf8eac419>.
Every element on it answers one of three questions — *what needs me,
is the evidence real, what happens if I act* — and anything answering
none of them is off the screen.

Four fixed zones plus two edges, each hosting the scene it serves:

| Zone | Scene it hosts | Content |
|---|---|---|
| Left rail | Scene 0 | the five factors, one line each in fixed order; an accent badge counts decisions; a factor with none says "nothing to decide" and dims — reported, never hidden |
| Center queue | Scenes 1–2 | every directive kind (review, order, escalation, briefing, ask) as one card anatomy: kind chip, source, gist; expanded: draft, basis chips, evidence block, `a`/`e`/`r` action row |
| Right field | the evidence in Scenes 1–2 | the fieldglass treemap, ambient and dimmed at rest showing only the strongest standing shapes; focusing a card lights its `field:` anchors and scopes the map |
| Honesty strip | every scene's ages | one home, one voice: SCIP lag, embedding age, heat coverage, what the landing pass skipped, **seat liveness** (§4) |
| Command line | intake (Scene 1's t=0) | the only free text on the screen — everything else is a decision on a draft |

**The five laws of the screen:**

1. **Fixed homes.** Fieldglass's layout-stability law promoted to the
   whole surface: the morning eye reads delta-from-familiar, so
   nothing ever moves to be noticed.
2. **Decision or dim.** Only decisions get luminance; the screen's
   brightness map IS its priority map. Nothing is removed — absence
   dims, it never disappears (§18.2).
3. **One interaction grammar.** Focus a thing → it explains itself and
   lights its evidence; act with one key (approve / edit / reject);
   escape ascends. Same gesture on a verdict, an order, an
   escalation, a factor. One grammar learned once is the largest
   cognitive-load lever on the screen.
4. **Evidence age everywhere, one voice.** Top bar carries field
   freshness; every evidence block names its snapshot; the honesty
   strip carries every input's age; unknown renders as unknown. The
   screen never claims green — gates may say "pass" (they are code);
   the field only shows shapes.
5. **The field is ambient until summoned.** The map is context, not a
   dashboard competing for attention.

**The noise budget — banned from the screen:** worker live logs and
progress bars (the harness owns those; a parked worker arrives as a
card, not a stream); scores, health numbers, trend charts; token
meters (budget appears only when an order crosses one — as a card);
notifications carrying no decision; any second accent color. One
accent means "needs you"; the Fieldglass semantic colors are inherited
unchanged, and each ships with its own mark shape (ring, glow, glyph,
arrow) so no evidence class is identified by color alone.

**Interaction grammar:** `j`/`k` next/prev card · `enter` descend
(expand + light evidence) · `a`/`e`/`r` act on the focused draft — the
edit is the training datum · `esc` ascend to ambient · click a
`field:` chip to flash its file · terrain clicks are Fieldglass
drill-throughs exactly as shipped · the prompt starts intake.

## 4. Runtime wiring — the screen is a window onto the seat, not a peer

The seat (a Claude session booted on the comaintainer skill today; a
daemon-hosted model at P5) remains the only intelligence. **The JSONL
logs are the bus.** Three loops:

**Loop 1 — seat → screen (read-only, all horizons).** The seat's
obligations do not change: directives are already logged
(`co-directive-log.sh`), verdicts already logged (`co-review.sh`),
orders already files (`co-order.sh`). The screen renders
`directives.jsonl` (queue), `verdicts.jsonl` (verdict bodies + field
evidence), order files, the sidecar (Earth line + terrain), `--stats`
(Commander), gate artifacts (Method). One protocol prerequisite —
**artifact D, drafts logged at draft time**: append
`{id, status: pending, draft, reasoning, citations}` when the seat
drafts, and a separate resolution record referencing the id when the
operator acts. Append-only, same file, ~20 lines in
`co-directive-log.sh`. Without it the queue cannot exist; with it the
queue is a `jq` filter.

**Loop 2 — screen → seat (H2 only).** The only genuinely new wire.
`a`/`e`/`r` appends a decision record and delivers it to the seat as
what it already is — the operator's turn in the seat's conversation:
one route pair on the daemon (already an HTTP server at :9741),
`GET /seat` renders from the files, `POST /seat/decision` appends and
nudges the seat via the harness's session-to-session bridge; fallback
is a turn-boundary sweep of pending decisions via hook (the mechanism
that injects notes today). Until H2 the upstream channel is the
operator's terminal, and the card displays the one-line reply to type.
**Intake stays conversational but card-shaped:** the order interview
is already specified as one exchange per field — each question arrives
as an ASK card (forced-choice or short answer), the assembled order as
the ORDER draft card. No chat pane needed.

**Loop 3 — seat → workers (unchanged, and the screen gets it free).**
Workers stay Claude subagents the seat spawns on order approval (cap
3, engine from the order's Engine line). Workers never write judgment
surfaces — the seat is the sole writer, which is what keeps the queue
trustworthy; worker lifecycle reaches the screen only as seat-verified
cards. The free loop nobody builds: subagent transcripts land in the
local transcript dir, which is exactly what feeds the agent-heat
panel — **workers are visible on the field as read/write heat with
zero new plumbing.** The operator watches the campaign's footprint on
the terrain while it runs.

**The engine swap is a non-event for the screen.** The seat's identity
is the charter plus the file protocol, not the model; `co-review.sh`
already takes `--engine daemon|claude`; a daemon-hosted director
writes the same records to the same files. The screen cannot tell
which engine drafted a card — and the gym is what proves the swap
lossless before it happens. The file bus is what makes the UI
contract engine-agnostic by construction.

**Two constraints designed around, not away:**

- **The seat is turn-based and single-threaded** — decisions batch at
  its turn boundaries; a queued approval may take effect a minute
  later. Acceptable at M0 (COMAINTAINER §9 names the bottleneck risk),
  and artifact D makes decision-to-send latency measurable for free.
  If it ever exceeds the pool's pace, that is §9's finding, with the
  number attached.
- **Seat liveness belongs on the screen.** A dead seat session means a
  stale queue that looks calm — the exact silent failure this house
  hates. The honesty strip carries `seat: last write <age>`, amber
  past a threshold; an unknown seat state renders as unknown, never as
  quiet.

**M0 note:** the moment decisions flow through `POST /seat/decision`,
"never send unsupervised" stops being charter-enforced and becomes a
structural gate on the send path — M1's own requirement
(COMAINTAINER §7) arriving early as a side effect of the interface,
not as its own project.

## 5. Build order = journey order, then the surface

Scene 0 → 1 → 2, then H1 → H2. The scene artifacts are cheapest-first
and risk-last: A is protocol-only and starts producing
operator-decision data immediately; B is pure reads over an existing
file plus the citation grammar C's evidence will be cited in; C is the
only piece that runs a render, so it lands last, behind a flag, with
its flip condition written before first use (§18.4).

The surface arrives as three horizons, each a projection of the same
drawn layout — which is why nothing built early is thrown away:

| Horizon | Surface | New machinery | Funded when |
|---|---|---|---|
| H0 — with artifact A | terminal briefing formatted as the rail (five lines, badges as counts); fieldglass page open beside; cards are the seat's draft messages | none — protocol only | now (it IS artifact A) |
| H1 — read-only page | the fieldglass renderer grows the rail + queue columns, read from the logs; actions display the one-line reply to type | a render module + artifact D (draft-time records); still static, air-gapped, no runtime | the H0 rail format survives two weeks without structural edits — the shape proven in text before pixels |
| H2 — wired actions | `a`/`e`/`r` write through | the one daemon route pair (§4 loop 2) | a week of H1 use in which the operator's typed replies all match the displayed one-liners — the page already carries every decision, so wiring is transcription |

**Ledger row (C is a default-off ship, house rule):** flip `--field`
default-on when, across 5 landing reviews, the field pass completes,
adds under 90 seconds wall-clock, and its evidence appears in the
drafted verdict at least twice. Review-by two weeks from first use.
Rejected if cost or noise makes seats skip it — recorded, not argued
with.

**Briefing-script funding condition (stated now, falsifiable):** script
the Scene 0 assembly only if a week of mornings shows it costing >2k
tokens (`cache-audit`) or the seat mis-assembling a factor twice.

**Factor demotion rule:** a factor line reading "nothing to decide"
every morning for a week is demoted to on-request. The five-factor
frame is a lens, not a liturgy.

## 6. Done-when, per scene — and the journey metrics

- **Scene 0:** five consecutive seat mornings brief in five lines, and
  at least one operator decision traces to a line the old four-surface
  briefing did not carry (Earth and Heaven are the candidates — terrain
  and cadence are the two surfaces the old step lacked).
- **Scene 1:** a real landing's verdict cites a `field:` anchor the
  resolver confirms; the bundle names sidecar age on every run; a
  deliberately wrong anchor (`field:offender:<calm file>`) fails
  `basis-exists` — the gate watched to fail (§18.1).
- **Scene 2:** a landing verdict carries a `field_evidence` block with
  a nonzero structural delta, and either (a) one live landing where the
  field-diff contradicts the worker's narrative — thesis proven on real
  traffic — or (b) two weeks of landings with zero contradictions,
  reported as a finding about this pool's current honesty, not
  reframed as progress.

Journey metrics, baselines computable today: morning rituals (2 → 1 on
seat mornings); decisions per briefing line (unmeasured today — the
directive log makes it countable from day one); share of verdict
citations that resolve mechanically (0% today; every `field:` anchor
counts); contradictions caught at the landing instead of the
post-mortem (0 today by construction — the instrument doesn't run at
landings); and from artifact D on, decision-to-send latency per
directive kind (the pending→resolved gap in the log — the number that
settles whether the seat's turn-based batching is ever the
bottleneck).

## 7. The deletes line (funding rule)

- On seat mornings, the standalone fieldglass relay merges into the
  Earth line: one ritual where there were two. The fieldglass skill
  survives untouched for seatless days.
- The seat's free-form landing narration ("gates look green, diff looks
  reasonable") is retired in favor of typed evidence lines — prose
  survives only in `rationale`, already capped at two sentences by the
  contract.
- At H1, the fieldglass page and the briefing merge into one surface:
  two pages become one, and the screen adds zero stores — it is a
  reader of logs that exist. H2's route pair is the only machinery
  this design ever adds, and it is separately funded.
- No knobs beyond `--field`, which carries its own kill condition in
  its ledger row.

## 8. Open questions for the operator

1. **Corpus resolution.** `co-review.sh` needs the sidecar path; today
   `~/.sovereign/arch/` holds one corpus. Proposed: newest
   `*/fieldglass.json` wins, corpus named in the bundle header. Fine,
   or read it from `.sovereign/project.toml`?
2. **Earth-line behavior.** Should the Scene 0 briefing also open the
   page (`--open`) by default, or only when the Earth line carries a
   decision?
3. **Contradiction accounting.** Scene 3 counts contradictions as
   tier-A labels but ships no harvester. Auto-mint a gym episode per
   contradiction, or keep it manual until the first few are audited by
   hand?
4. **Queue ordering.** Arrival order or severity order? Recommended:
   arrival — severity re-ranks under the operator's feet and violates
   law 1 (fixed homes); urgency belongs in the factor badges, not in
   queue churn.
5. **Parked workers on the card.** When a worker parks at a
   safety-switch boundary, should its frame-vs-order diff render
   inline as the card body, or stay a pointer into the terminal?
6. **Escalation shape.** Is the forced-choice card (`1` / `2` /
   answer-in-words) the right default for escalations, or should they
   always open free-form? Forced-choice is faster and logs cleaner
   labels; free-form respects that escalations are by definition the
   calls that resist typing.
