# Next-Edit Prediction — Design Spec

Status: **BOTH LANES IMPLEMENTED AND HARDENED — P1 rule lane + P2
model lane, 2026-07-30**. An adversarial pass (§9a) then went at all
three surfaces with `text`, `history`, and the model's own output
treated as hostile; every HIGH/MED finding was fixed with a named
regression test, and both banks re-ran green with the bars untouched.
User-facing walkthrough:
[`docs/NEXT_EDIT_IN_YOUR_EDITOR.md`](../../docs/NEXT_EDIT_IN_YOUR_EDITOR.md).
**Picking this up cold? Start at
[`NEXT_EDIT_HANDOVER.md`](./NEXT_EDIT_HANDOVER.md)** — the map, the
current verdicts, and the ranked backlog, in a tenth of this file. The daemon route (`POST /v1/edit_predictions`), the
pure induction pipeline, and the extension provider are built and
tested; both exploration spikes (§5) were retired into the real
build the same day, after the operator validated accept/jump feel
and the ambient trigger in hand. The **rule-lane eval bank SHIPPED
2026-07-30** (`gym/next-edit/`, 120 cases, all pre-registered gates
green; its first run caught the insertion-idempotence wrong-edit
bug, fixed the same day). The **model lane SHIPPED the same day**
behind the same route (`engine: "model"`, prompted Mellum2 on the
edit slot), gated by the §6 generalization bank
(`gym/next-edit/gen/`, 60 cases): **all five pre-registered gates
green on run 3** (30/30 positives correct, 0 wrong edits, wall p95
1.8 s) after runs 1–2 caught Mellum2 destructive on cross-casing
renames — that one category is **detected but deferred**
(`casing_deferred`, §4/§6) pending a deterministic rule sub-lane.
The extension consults the lane by default
(`sovereign-fim.nextEdit.modelLane`), silently inert unless the
daemon's edit slot serves the next-edit lane — which, since 2026-08-07,
**no longer requires a coder model** (§2a). Companion to
[`INLINE_COMPLETION.md`](./INLINE_COMPLETION.md) (FIM v1): this seat
reuses its glassbox conventions, its eval discipline, and its slot.

---

## 1. The feature

After a few related edits, the editor proposes the *next* edit —
possibly away from the cursor — as a diff the user accepts with Tab;
accepting applies the edit and advances to the following proposal.
Canonical case: a file with 25 `console.log` lines, the user edits
two into `console.debug`, and Tab walks the remaining 23. Prior art:
Copilot Next Edit Suggestions, Cursor's tab model, Zed's edit
prediction (Zeta).

This is a different seat from FIM on every axis, which is why it is a
separate spec and a separate surface:

| Axis | FIM (shipped) | Next-edit (this spec) |
|---|---|---|
| Input | prefix/suffix at cursor | recent **edit history** + excerpt |
| Output | insertion at cursor | **diff**, possibly away from cursor |
| Trigger | keystroke (120ms debounce) | edit-settle (an edit just landed) |
| Latency budget | ~300ms TTFT | ~1s acceptable |
| Failure cost | ignorable ghost text | a *wrong edit proposal* — precision-critical |

## 2. Thesis: two engines under one UX

The canonical case does not need a model. Once two consecutive edits
induce the same literal transformation (`console.log(` →
`console.debug(`), the remaining occurrences in the file *are* the
suggestion queue — found by string search, byte-precise, instant,
incapable of hallucination, and glassbox by construction (the induced
rule and remaining-site count are displayable verbatim). The general
case — rename propagation across casing variants, a signature change
fanning out to differently-shaped call sites, a new struct field
implying constructor updates — genuinely needs a model, and carries
all the risk: unproven quality, latency, and wrong-edit precision.

So the feature is two lanes behind one contract:

- **Rule lane (v1)**: deterministic repeated-edit engine. Fires only
  when ≥2 consecutive coalesced edits agree, and it induces **two rule
  kinds**:
  - **Literal rewrite** (`expand_rule`, from one unit) — some `find`
    occurring in the document is replaced.
  - **Anchored repeat-insertion** (`induce_insertion`, from a *pair*)
    — added 2026-08-06. An insertion has no `find`, so the single-unit
    induction cannot express it and returns `None`; a pair supplies
    both the payload (their shared insert) and the anchor (the longest
    common line-aligned tail of their left contexts). Rendered as
    `find = anchor` / `replace = anchor + payload`, so site finding,
    the already-applied exclusion and the queue all apply unchanged.
    It runs only where the literal lane declines — where both could
    speak, the literal rule is the more specific claim.
  - **Repeat block deletion** (`induce_deletion`, from a *pair*) — the
    narrowest kind, and deliberately so. Two identical **multi-line**
    deletions propose removing the block's remaining copies. Worth +4
    useful edits at 0 measured wrong fires. The multi-line requirement
    is the whole safety argument: on single-line repeats the same rule
    fires wrongly 13 times across the 387 negatives, because a short
    literal recurs innocently and a wrong deletion is the most
    destructive edit this system can make.

  That second kind exists because the golden set measured the hole:
  pure-INSERT truths scored 14.0% against 44.6% for replacements, and
  forcing a model at them moved nothing. As a fallback it is worth
  **+21 useful edits for 2 wrong fires, nothing regressed, and 0 wrong
  fires across the 387 negatives** — and the deletion kind a further +4
  at zero. System useful-fire **41.4%**, up from 37.8% (note
  `53abe423`).

  What the deletion kind does NOT attempt is on the record too:
  `delete_propagation`'s real shape is a developer bulk-removing a
  *run* of differing sibling blocks, where knowing where the list ends
  is a structural judgment rather than a repeat. Identifier-scoped line
  deletion measured 2.9% useful with 14 wrong fires on negatives, and
  single-line repeat deletion 50% with 13; both were rejected, and 38
  of the 43 episodes stay open.

  Ships the demo case with zero model risk and zero extra RAM.
- **Model lane (v2, eval-gated — SHIPPED, default-on)**: prompted
  region-rewrite on the resident edit slot
  (`commonwealth-api/src/next_edit_model.rs`). Mellum2-Instruct is
  **not next-edit-trained**; the §6 bank answered the open question
  empirically: 30/30 correct with zero wrong edits on fan-out /
  per-site-varying / multi-line-insert generalizations, correct
  silence on every negative (including string-literal traps) — but
  destructive on cross-casing renames, so that category is deferred
  (§4). The prompt follows Zed's open Zeta shape (edit history as
  diff snippets + an excerpt bracketed by editable-region markers;
  output = the rewritten region), through the chat template with a
  hardened instruction (run 1's fabrications taught it "never undo,
  never delete unrelated code"). Zeta *weights* (a second resident
  model, ~4–5 GB quantized) remain a documented fallback for
  machines with headroom, not the plan of record.

## 2a. Which model can serve this — the edit slot's two lanes (2026-08-07)

Not to be confused with §2's rule/model lanes, which are both *inside*
next-edit. This is the axis one level down: the daemon's **edit slot**
serves two independent capabilities, and a given model may serve
either, both, or neither.

| Lane | Route | Requires |
|---|---|---|
| **Next-edit suggestion (NES)** — this spec | `POST /v1/edit_predictions` | a prompt dialect (`NextEditFormat`: `region_instruct` / `zeta2` / `sweep`). Rides the model's ordinary prompt surface, so **any competent chat model serves it**. |
| **FIM** — [`INLINE_COMPLETION.md`](./INLINE_COMPLETION.md) | `POST /v1/completions` | FIM marker tokens in the model's **vocabulary**. Only purpose-built coder models carry them (Mellum2, Qwen2.5-Coder, StarCoder2, Seed-Coder). |

The contract is `EditSlotInfo`
(`sovereign-contracts/src/types/edit_slot.rs`): each lane is an
`Option`, present **if and only if** the slot can serve it. Ask the
lane; never re-derive capability from a model name or a marker enum.

**Why this was rebuilt.** The two lanes used to be one struct with a
mandatory `FimStyle`, which made the vocab probe a gate on *both*. A
user whose only model was an ordinary chat model got no editing
assistance at all — including this seat, which needs no markers
whatsoever. That was the plumbing refusing a capability the weights
already had. Now a failed probe withholds the FIM lane and nothing
else.

**Graceful degradation.** When no `[models.edit]` is configured at all,
`EmbeddedLlamaCpp::install_fallback_next_edit_slot` serves next-edit
off the already-resident chat model rather than serving nothing. It
targets `ModelsSection::fast_path()` — the explicit `[models].fast`
GGUF when set, the primary otherwise — which is the same path the
always-resident fast slot loads at boot. So the fallback is
fast-when-there-is-a-fast and primary-when-there-is-not, and in **both**
cases the weights are already in memory: an editing keystroke can never
trigger a model load. (A cold 35B primary would stall the editor
10–20 s, which would be worse than silence.) The slot is marked
`degraded: true`, which is what drives the `advice` nudge on
`/status.inference.edit` — the user gets working suggestions and is
told what a specialist would buy them.

`degraded` is about **provenance** (did anyone choose this model for
the job), which is a different question from a `None` lane
(**capability**). Both are reportable states, neither is a failure.

**Default OFF**, behind `SOVEREIGN_NEXT_EDIT_FALLBACK` (`1`/`true` to
enable), pending a bench baseline on the fast slot — row in
[`DEFAULTS_LEDGER.md`](../DEFAULTS_LEDGER.md). An explicit
`[models.edit]` always wins over the fallback, and the fallback never
overwrites an existing arrangement.

**What the measurement says** (60-case §6 gen bank, 2026-08-07, consult
gate forced open so every case reaches the model):

| Arm | Useful | Wrong edits | Wall p95 |
|---|---|---|---|
| 35B-A3B chat primary, `region_instruct`, thinking **off** | 21/30 | 0 | 2576 ms |
| 1.5B next-edit specialist | 19/30 | 0 | 828 ms |
| Same 35B primary, thinking **on** | **0/30** | 0 | — |

Two findings. First, quality is **indistinguishable at n=30** — a
2-case spread is well inside the noise of a 30-case bank, so the
specialist's real win is ~3x latency, not correctness. That is exactly
the trade the `advice` string names. Second, **thinking suppression is
decisive, not a tuning knob**: with reasoning on, the same model emits
~1044 tokens of `reasoning_content` before its first answer byte,
against this lane's 64–1024 token grant — so every case truncates and
the arm scores zero. `NextEditFormat::uses_chat_template()` exists to
carry that distinction: chat-template dialects run on thinking-capable
general models and must suppress; the raw dialects ride completion
fine-tunes with no thinking phase to suppress
(`ConsultPlan::suppress_thinking`).

## 3. Architecture (as built)

Stateless daemon, IDE-agnostic — the same posture that keeps the
JetBrains port mechanical for FIM. The daemon holds no editor state;
edit history rides along on each request, and ALL policy (context
expansion, guards, induction, threshold) is daemon-side so every
IDE client stays a thin capture-and-render shell.

- **Extension — edit-unit capture.**
  `packages/vscode-sovereign/src/editUnits.ts` coalesces
  `onDidChangeTextDocument` keystroke deltas into semantic edit units
  (one select-and-retype or backspace-and-retype burst = one
  `{before, after}` replacement; a multi-cursor event = one unit per
  cursor), against a per-document shadow snapshot (change events
  carry the range but not the deleted text). At unit close the
  controller (`src/nextEdit.ts`) captures ±48 chars of untouched
  context per side, making each history unit self-contained.
- **Daemon — `POST /v1/edit_predictions`** on the `client_router`
  (:9741), beside the FIM handler; handler in
  `commonwealth/crates/commonwealth-api/src/routes_edit_predictions.rs`,
  pure pipeline in `commonwealth-api/src/next_edit.rs`. Request:
  `{history[], text, cursor, path, language, debug}`; response:
  `{edits: [{start, end, new_text}], engine: "rule",
  sovereign_debug}` — **offsets are UTF-16 code units** (the native
  offset space of editor clients; the daemon converts). The full
  remaining-site queue comes back in one response; the client shifts
  offsets locally as accepts land (`src/editQueue.ts`) and
  revalidates each site's old text against the live document before
  applying. Caps: text ≤ 512 KiB, ≤ 32 history units, ≤ 2 KiB per
  unit field (**bytes** — the client must measure the same way, §9a);
  beyond them is a 400 naming the offending field. A route-scoped
  2 MiB body limit refuses what could never satisfy those caps before
  serde allocates it, and the route carries the standard
  `admission()` gate because the model lane drives inference (local
  requests are always admitted).
  Silence is a 200 with empty `edits`, never an error. The model
  lane (v2) will branch behind this same response shape and inherit
  the drop-invalid-output posture: no suggestion beats a wrong one.
- **Rendering (the binding constraint).** Copilot's NES UI rides VS
  Code *proposed* APIs unavailable to third-party extensions on
  stable. Our stable-API ceiling: near-cursor rewrites render as an
  `InlineCompletionItem` with a replace range; away-from-cursor edits
  render as decorations (struck-through old text + ghost new text)
  with Tab/Esc bound behind a context key. The §5 spike exists
  because this was the highest-risk item in the design — de-risk the
  feel before building the daemon surface.

## 4. Trigger policy + precision posture

The trigger decomposes into three separately-tuned decisions —
*when to compute*, *when to fire*, *how to appear* — and the spike
(§5) exists to let the operator feel the composite. There is no
command in the product path: the system watches, and speaks only
past a threshold.

**When to compute.** Keystroke-level changes coalesce into semantic
**edit units** (one select-and-retype burst = one `{before, after}`
replacement; a multi-cursor event = one unit per cursor). A unit
closes on settle (idle after a burst) or when the next edit lands
elsewhere. Rule induction runs at each unit close — for the rule
lane this is pure string work, effectively free, so *computing*
continuously costs nothing; every cost question lives in the firing
and surfacing policies. The model lane (as built,
`next_edit_model::should_consult`) inherits the same trigger point
but adds a deterministic consult gate: only when the rule lane
declined AND the two most recent real units are
similar-but-not-identical. The gate recognises four such shapes and
**admits exactly one of them.**

**Admitted — multiline_fanout** (identical multi-line insertion,
which `expand_rule` declines by design).

**Detected, declined by name** — the other three. Each decline is a
distinct `skipped:` reason, so the shape stays countable and any
re-open has to argue against its own admission count:

| shape | `skipped:` | why |
|---|---|---|
| `fanout_insert` | `fanout_insert_deferred` | identical `{before, after}` cores, differing contexts, so induction can never reach support 2 |
| `param_insert` | `param_insert_deferred` | same target, per-site-varying replacement sharing a ≥4-char prefix (`.unwrap()` → `.expect("…")`) |
| `casing_variant` | `casing_deferred` | identical rule, exhausted, rename remains at another casing |

`casing_variant` was deferred first: §6 runs 1–2 showed Mellum2
destructive on exactly that shape, and since the variant find/replace
is fully deterministic its real home is a rule-engine sub-lane.

`fanout_insert` and `param_insert` were deferred on 2026-08-06, on the
golden set scored **per admitting gate** rather than per bank shape
(`gym/next-edit/golden/`, 1,098 cases; note `2c22ec10`). The three
reasons were never one bet:

| gate | spoke | useful | wrong | rate |
|---|---|---|---|---|
| `multiline_fanout` | 18 | 17 | 1 | **94.4%** |
| `fanout_insert` | 19 | 2 | 17 | 10.5% |
| `param_insert` | 8 | 2 | 6 | 25.0% |

Silencing the bottom two removed **23 wrong fires and cost 4 useful
edits** — every one of the 27 changed cases moved in one direction,
none regressed — taking the system from 36.0% useful / 21.0%
wrong-fire to **35.4% / 15.2%**, i.e. *below* the wrong-fire of
switching the model lane off entirely (33.1% / 15.8%). Model-lane p95
fell from 1748 ms to 9 ms, because 36 episodes now reach the model
instead of 96. Flip condition: `sovereign/DEFAULTS_LEDGER.md`.

The gate also derives a
**needle** (the before-core, or the longest common substring of the
two units' contexts) that anchors a ~24-line rewrite region near its
next occurrence from the cursor, falling back to the cursor line.
Concurrency: a per-daemon one-in-flight semaphore — a consult that
finds the slot busy is dropped immediately (`busy`), never queued,
so ghost text and chat always win the slot.

**When to fire — structural confidence, not a model score.** The
induced rule's context expansion (edit ± the untouched identifier
run around it, so `log`→`debug` becomes `console.log(` →
`console.debug(`) gives a specificity measure; recent history gives
a support count. The policy is one legible line: never without a
remaining site, never on one supporting edit, and never on a rule
whose `find` is under 5 chars after expansion.

That minimum went 4 → 2 → 5 on 2026-08-06, and the reversal is not a
flip-flop — **the objective changed.** The 4 → 2 move maximised
`useful-fire`, which the scorer defines as `useful + partial`, so it
rewards a rule that fires wide and happens to be right somewhere.
Told that a user simply does not accept a wrong fire, the question
became "what is the most useful we can be at each level of wrong",
and on that question 2 is dominated outright. Swept at 3 rule kinds
over the whole golden set (1,098 cases, rule lane isolated), reporting
STRICT useful — every proposed hunk one the author actually made —
beside the wrong count:

| min chars | strict useful | wrong (of which negatives) |
|---|---|---|
| 2 (was) | 138 | 52 (32) — dominated |
| 4 | 139 | 48 (28) |
| **5** | **141** | **39 (27)** |
| 6 | 143 | 38 (27) |
| 16 | 116 | 17 (7) |

5 rather than 6 because 141-vs-143 is inside sampling noise while the
wrong-fire plateau starts at 5: the value sits on the plateau's edge
rather than at an argmax over 168 swept cells, which one bank cannot
support.

Measured paired, 2 → 5, rule lane isolated: **13 wrong fires removed
and 0 added** (5 of them on negatives, where silence was the correct
answer), +3 strict-useful, and 25 fewer over-offers. wrong-fire
15.3% → 12.8%; `useful-fire` 40.5% → 37.4%, which falls *because* it
counts the over-offers being removed. The honest cost: 4 positives
that were `useful` are now `missed`, concentrated in the shapes that
already did well (`literal_fanout` 96.7% → 88.9%, `rename_casing`
86.7% → 76.7%).

**Why a stricter bar yields MORE strict-useful** — the counter-intuitive
part, and the thing to know before touching this constant.
`MIN_RULE_CHARS` is a *router*, not just a filter: declining the short
rule falls through to the pair kinds (§2.2–2.3), which re-induce from
the same history and anchor on a whole line. `fetch` →
`-c core.fsmonitor=false fetch` matched 62 sites and scored `partial`;
the anchored rule induced instead, `` `git `` →
`` `git -c core.fsmonitor=false ``, matches 8 and scores `useful`.

A guard-dependent bar (short rules allowed when word-guarded) was
measured and rejected: it leaves wrong at 52 and wrong-fire at 15.5%,
no better than baseline, because the wrong fires ARE guarded
identifier renames (`neg_literal_trap`, 28 of the 32) matching
innocent occurrences. Guarding does not make a short rule safe.

**The support tier is gone as a consequence, not as a tidy-up.** The
policy used to read "2 supports fire only a specific rule (≥4 chars);
3+ lower the bar (≥2)". When the sweep set both arms to the same
minimum the distinction stopped distinguishing anything, and it was
collapsed into `support >= 2 && find >= MIN_RULE_CHARS` — a single
condition that survived the later 2 → 5 move unchanged. More support
does not buy a shorter rule.
One edit never fires anything. Sites the rule was **already applied to**
are excluded: an insertion-shaped rule (replace contains find) still
matches textually at every already-edited site, and re-proposing one
would stack the insertion (`await await fetch(`) — the §6 bank's
`a11` probe caught exactly this before the exclusion existed
(`next_edit.rs::already_applied`).

**How to appear — never scroll uninvited.** If the next site is in
the viewport, decorate it in place. If off-screen, surface only a
one-line hint at the cursor's line end (`⇥ rule · N sites · next:
line L`); the first Tab jumps and decorates, subsequent Tabs
accept+advance — mid-chain, revealing is expected. Esc suppresses
the rule for the session (no re-nagging). Any manual edit clears
the proposal; the next settle re-evaluates, so continuing the
pattern by hand simply re-offers with more support.

## 5. Exploration spikes (RETIRED 2026-07-30 — absorbed into the real build)

Both spikes ran extension-only ahead of any daemon work, were
validated in the operator's hands the same day, and were then
deleted in favor of the §3 build (the coalescer and the surfacing
mechanics survived verbatim; induction moved to Rust). Kept here as
the record of what was de-risked and in what order.

**Spike 1 — render mechanics** (command-triggered, hardwired
stand-in predictor): proved that decoration-rendered diff preview
reads as "a proposed edit", that Tab interception behind a context
key coexists with the suggest widget, snippets, and FIM's own ghost
text (`!inlineSuggestionVisible` in the `when` clause), and that
accept-then-jump feels like Copilot NES rather than fighting the
editor. **Operator verdict: accept/jump feels good.**

**Spike 2 — ambient trigger** (the §4 policy running in-extension,
TypeScript induction, no daemon): proved that edit-settle triggering
with the structural-confidence threshold and the never-scroll
surfacing feels right in real typing — including select-and-retype
and backspace-and-retype capture through the shadow-snapshot
coalescer. **Operator verdict: "nailed it."** The TypeScript
induction was then ported to Rust (`next_edit.rs`) and deleted
client-side; the coalescer and the rendering mechanics moved into
the production controller unchanged.

## 6. Eval — rule-lane bank BUILT; model lane still gated

**Rule-lane bank shipped 2026-07-30**: `gym/next-edit/` (harvester +
cases + pre-registered gate table in its README) and
`scripts/next_edit_eval.py`, in the mold of `gym/fim/` +
`scripts/fim_eval.py` (see [`INLINE_COMPLETION.md`](./INLINE_COMPLETION.md)
§4/§7). No model weights needed — it runs against any daemon build
with the route.

- **Harvested from this repo's real git history** (deterministic, no
  RNG): a commit where ≥3 single-line hunks induce the same expanded
  rule is a natural episode — replay the first k hunks as edit
  history (k mirrors the firing table: 2, or 3 for short rules),
  send the mid-edit document, hold out the remaining commit-edited
  sites as the expected queue. 80 positives + 25 negatives
  (dissimilar-edit and exhausted episodes) across 10 languages, plus
  15 authored probes: guards, UTF-16 astral offsets, CRLF, tabs,
  deletion/insertion shapes, wrap order, MAX_EDITS cap, and each row
  of the threshold table.
- Ground truth is an **independent Python replica** of the
  expansion/guard/site/threshold logic written from this spec, with
  authored intent hand-asserted at bank-build time — a replica↔Rust
  divergence fails loudly whichever side is wrong.
- Gates (pre-registered in the bank README; a miss is a named bug,
  never moved): G1 zero malformed/wrong edits + exact authored
  queues · G2 100% fire + 100% held-out recall on positives ·
  G3 100% silence on negatives · G4 wall p95 ≤ 150 ms. **Verdict
  2026-07-30: all four PASS** (120/120, p95 7 ms) — after the first
  run caught the a11 insertion-idempotence bug (§4), proving the
  bank bites. Over-offer is reported, not gated (the queue
  deliberately offers every remaining site).
- **G5 declines, added 2026-08-28, and the population G1/G2 read
  narrowed to match.** The syntax oracle (§4a, 2026-08-06) and the
  `MIN_RULE_CHARS` 4→5 sweep (2026-08-07) both landed after these
  fixtures were cut, and between them decline 25 of the 120 by
  design. Scored against the original bar those cases could only
  ever fail, so G2 sat permanently red and stopped carrying signal —
  the failure §18.1 names, arrived at from the other direction. Each
  declined case now carries `expect.declined_by`, and that
  annotation is a CHECK: G5 re-derives the mechanism every run (the
  oracle by the no-grammar counterfactual, the threshold by the
  daemon's own `below_threshold` verdict, the pair route by
  text-equivalence of the anchored edit) and goes red both when a
  mechanism stops holding and when an annotated case starts passing.
  **Verdict 2026-08-28: all five PASS** — 95 scored (G1 authored
  queue 9/9, G2 57/57, G3 29/29, G4 p95 24 ms), 25 declined and
  re-verified (14 `syntax_oracle`, 8 `min_rule_chars`, 3
  `pair_fallback`). All three of G5's failure modes were watched to
  fail before the verdict was published.
- **Model-lane generalization bank SHIPPED 2026-07-30**:
  `gym/next-edit/gen/` (60 hand-curated cases across 11 languages —
  no git mining; generalization episodes need intent a harvester
  cannot infer) + `scripts/next_edit_gen_eval.py`. Case expectations
  are validated at authoring time against a Python replica of the
  consult gate (in `author.py`, mirroring `next_edit_model.rs`) —
  the same replica↔Rust divergence discipline as the rule bank, and
  it caught two authoring bugs before anything ran. Gates
  (pre-registered): GM1 structural 0 · GM2 gate determinism 100% ·
  GM3 wrong-edit ≤5% of fires (the default-on decider) · GM4
  usefulness ≥60% of positives correct · GM5 wall p95 ≤6 s.
  **Verdict, run 3 (2026-07-30): all five PASS** — 30/30 positives
  fired and content-correct, 0 wrong, 0 malformed, 20/20 negatives
  silent, p95 1.8 s — after runs 1–2 caught the casing failure that
  became the §4 deferral (the bank README's deferral record holds
  the evidence). Needs the edit slot's next-edit lane live — an
  explicit `[models.edit]`, or the §2a fallback; the runner probes and
  says exactly that when neither is there.

## 7. Config (as built)

Extension settings under the existing `sovereign-fim.*` namespace:
`nextEdit.enable`, `nextEdit.settleMs`, and `nextEdit.modelLane`
(default **true** since the §6 verdict; it sends `model_lane: true`
on the wire). Daemon side: deliberately **no new config** — the
model lane serves whenever the edit slot's next-edit lane is present
AND the request opts in; enablement is a client concern, policy is
daemon-side. That lane comes from an explicit `[models.edit]`
(deprecated alias: `[models.fim]`) or, when there is none, from the
§2a fallback under `SOVEREIGN_NEXT_EDIT_FALLBACK` — an env flag, not a
config key, precisely because it is default-off pending a baseline and
should not read as a settled knob. No other knobs were added:
temperature, region size, and timeout are constants in
`next_edit_model.rs` until an eval says they should move.

## 8. Phasing

1. **P0 — exploration spikes** (§5): done, retired.
2. **P1 — rule lane end-to-end** (capture → route → induction →
   rendering; no model, no new RAM): **done 2026-07-30**. Ships the
   canonical case completely. Known scope edges, deliberate:
   per-document history (switching files resets it), single-file
   sites only, multi-cursor typing bursts fragment into
   non-inducible units (benign silence).
3. **P2 — model lane** behind the same route, prompted Mellum2,
   gated on §6: **done 2026-07-30**, default-on after the bank's
   run-3 verdict. Known scope edge, deliberate: casing-variant
   renames are detected but declined (§4) — the model proved
   destructive there and the shape is deterministic anyway.
4. **Deferred**: the casing-variant **rule sub-lane** (fire the
   detected variant find/replace through the rule engine —
   byte-precise, no model; re-activate the bank's `deferred_casing`
   cases when it lands), cross-file edits, JetBrains port, optional
   Zeta slot for high-RAM machines, marketplace publish.

## 9. Glassbox

Every `debug: true` response (the first-party extension always opts
in) carries `sovereign_debug`: `{rule_find, rule_replace, rule_key,
support, sites, edits_capped, reason_silent, timings_ms}` — silence
is *explained* (`no_rule` / `below_threshold` / `no_sites`), never
mute. When the request opted into the model lane, a `model`
sub-object explains that lane the same way: `{consulted, reason,
skipped, needle, needle_hit, region:{start,end}, model_id, slot,
dropped, timings_ms:{inference}}`. `skipped`
(`rule_fired`/`gate`/`casing_deferred`) says why the gate never
consulted; `dropped`
(`unavailable`/`busy`/`timeout`/`truncated`/`error`/`region_empty`/
`region_too_large`/`region_has_markers`/`invalid`/`noop`/
`inconsistent`/`already_applied`) says why a consult produced no
edits — a dropped model prediction is reported as dropped, never
repaired. The last two are the V0 content verifier (§9b). The daemon logs one line per prediction
under the `next_edit` tracing target (path, history size, support,
sites, proposed, silent-reason, engine, model state, elapsed), which
is in the daemon's default tracing allowlist and pinned by the
allowlist test in `sovereign-cli-daemon/src/lib.rs`.

## 9a. Hardening pass (2026-07-30)

Before productionizing, three adversarial reviews ran against the
route, the pure pipelines, and the extension, treating `text`,
`history`, **and the model's own output** as attacker-controlled (a
malicious repo's content reaches the prompt, so a model that echoes
it back is an attack path). Every HIGH/MED finding was fixed rather
than waived, each with a regression test named for the failure. The
banks re-ran green afterwards with the bars untouched (run 4 = run 3
exactly), so none of this cost quality.

The findings worth carrying forward, because each encodes a rule
that is easy to re-break:

- **A timeout cancels nothing.** Engine dispatch goes through
  `spawn_blocking`, and dropping a `JoinHandle` *detaches* — the
  generation keeps running and keeps the slot's context lock. The
  one-in-flight permit therefore rides **into** the spawned task, so
  it outlives the handler's 15 s budget; releasing it on timeout
  would have meant every timed-out consult left a live generation
  behind while the next request sailed through `try_acquire`.
- **The rule lane had the worst bug, and it needed no opt-in.**
  `already_applied` re-derived `find`'s positions inside `replace` at
  every site, so a self-similar rule over a large file was quadratic:
  a crafted 512 KiB request measured **23 s** of blocking CPU on one
  tokio worker. Alignments are now computed once per rule, degenerate
  rules are declined, and the site scan is bounded.
- **Guards relative to the region bound nothing when the region is
  unbounded.** `REGION_LINES` caps lines, not bytes, so one minified
  line made the region the whole file. Region is now capped in bytes
  (`MAX_REGION_BYTES`), an over-budget window is declined by name, and
  blank or marker-bearing regions are refused — a blank region makes
  every returned byte pure invention with nothing to measure it
  against.
- **Shrink needs two bounds.** The growth cap was one-sided: a
  truncated completion is *smaller* than the region, and diffed whole
  it reads as "delete the rest". Bounded absolutely (a big region cut
  short) and proportionally (a small region gutted in place, whose
  line delta is zero), plus `finish_reason == "length"` is dropped.
- **Repairing suspicious output manufactures wrong edits.** The old
  marker-stripping repair would silently delete a real file line that
  happened to *be* a marker, and rewrote every line ending on the way
  through. Markers now drop the output whole. Fences must wrap the
  entire reply and close on the *first* fence, so chat-shaped answers
  ("Sure! Here you go… I also removed the dead code") can't splice
  commentary into the file.
- **CRLF was a silent whole-region rewrite.** A CRLF region against an
  LF reply makes every line differ, collapsing the line-LCS into one
  hunk spanning the region — the model got free rein over code it
  never claimed to touch, and a faithful echo stopped registering as
  a noop. Line endings are normalized to the region's before diffing.
- **Two rulers on one contract kill the lane silently.** The client
  capped unit fields in UTF-16 *chars* while the daemon caps them in
  UTF-8 *bytes*, and a fixed-offset context slice could split a
  surrogate pair into a lone surrogate that `serde_json` rejects.
  Either one 400s the **whole request**, and because the offending
  unit stays in the history window it poisoned every later prediction
  until it aged out — behind a green status bar, since 4xx was
  swallowed. The contract now lives in one pure module
  (`packages/vscode-sovereign/src/wireLimits.ts`), and a 4xx clears
  history and says so once.
- Route posture: the endpoint now carries the same `admission()` gate
  as every other inference route (it drives a model, so a paused peer
  must not reach it; local requests are always admitted), plus a
  route-scoped body cap so the documented limits are a contract check
  rather than the only defence.

Two residual risks are accepted deliberately, not overlooked. **No
version token on the wire**: the first-party client already
revalidates each site's old text against the live document before
applying, so a stale prediction degrades to a no-apply rather than
corruption — a wire field is a bigger contract change than the
residual justifies. **Trailing prose with no fence** is undetectable
in general; it is bounded by the shrink/growth caps and measured at
0/30 by the bank.

## 9b. V0 content verifier (2026-07-31)

The structural guards bound how *much* the model may change; none of
them bound what the change *says* — the 2026-07-31 adopt bakeoff
measured that gap directly (Sweep 1.5B fired well-formed,
guard-passing, content-wrong edits on 10/10 model-negative cases).
But the gate only consults when the exemplars agree on a
transformation, and for the identical-core shapes that transformation
fixes the correct content. `next_edit_model::verify_pattern` holds
every hunk to it, deterministically, before edits reach the wire:
a hunk must advance the pattern (add the exemplars' `after` — for
`param_insert`, the shared prefix; tails vary per site and are not
judged), must not move against it (re-introducing removed content, or
failing to delete on a deletion-shaped pattern), must not re-apply it
where the hunk's own lines already carry it, and must not stack two
copies with only whitespace between (one site doubled, never two
sites). Hunks are checked over their full-line spans because
char-trimming moves the evidence of a re-application into the
surrounding line; multi-line content is matched as trimmed line
sequences so indentation drift cannot hide a duplicate. Any bad hunk
drops the whole prediction (`inconsistent` / `already_applied`) — the
same posture as the guards. The checks are derived from the gate's
shape definitions only, never from eval-bank content, and both stages
share one `exemplar_pair` so they cannot disagree about which edits
form the pattern.

**Falsification verdict (2026-07-31, three iterations — each fix
shape-derived, then re-controlled).** The pre-registered protocol:
Mellum2 must hold its run-4 verdict under V0 (no correct fire eaten),
and Sweep 1.5B's 25% wrong-fire rate must fall to the ≤5% gate.
The control leg caught two precision bugs, both fixed at the shape
level: a line-fragment exemplar (an `after` starting mid-line, e.g.
`",\n    \"retries\": 3"`) was invisible to line-wise matching —
fixed by whitespace-normalized content matching; and a completion
format's trailing-newline artifact sank whole predictions — fixed by
exempting whitespace-only hunks, with the corollary that an
all-whitespace prediction is a **noop**, not an edit. That corollary
rewrote the bakeoff's diagnosis: Sweep's "10 content-wrong edits" on
the model-negative traps were formatted echoes (a bare trailing
newline) shipped as edits — the model was correctly declining all
along and the wire wasn't. Final numbers: **Mellum2 control 29/30 ·
0 wrong · p95 1807 ms** — the one drop (`sf06`, `already_applied`)
reproduced 3/3 fresh samples as a genuinely corrupted rewrite
(`sock_ FD`, `,- scratch`) that the bank's count-ruler scores
*correct*; a ruler blind spot V0 closes, kept as a drop, not a bug.
**Sweep 1.5B + V0 passes all five gates: GM3 0/24 fires wrong (from
25%) · GM4 22/30 unchanged (nothing correct eaten) · p95 1112 ms
(~0.6× Mellum).** Mellum2 remains the default; Sweep is now a viable
verified low-latency lane, pending dogfood receipts.

> **SUPERSEDED 2026-08-05 — these Sweep numbers are stale, and the
> weights moved under them.** The published GGUF is now
> `sweep-next-edit-1.5b.q8_0.**v2**.gguf` and scores **27/30 useful,
> 0/28 fires wrong, p95 749 ms** on the same bank, bit-stable across two
> runs, with the consult gate making identical decisions (GM2 60/60) —
> so this is a new checkpoint, not harness drift. Measured offline via
> `examples/next_edit_score`, which runs this route's own pipeline;
> full table and caveats in
> [`bench/next-edit-bakeoff/RESULTS_PHASE0.md`](../bench/next-edit-bakeoff/RESULTS_PHASE0.md).
> Mellum2's row has NOT been re-measured since July and should be
> assumed equally stale.

## 9c. Offline scoring + the zeta2 dialect correction (2026-08-05)

`commonwealth-api/examples/next_edit_score.rs` serves this route's
contract over **any** OpenAI-compatible endpoint, so a candidate model
can be scored without a daemon and without a resident `[models.edit]`
slot. It is not a reimplementation: the route and the scorer both call
`next_edit_model::{plan, finish}` and `routes_edit_predictions::
{validate_wire, predict_response}`, so every decision — caps, consult
gate, region guards, prompt, parse, diff, V0 verifier, and the exact
`sovereign_debug` shape — is shared. Splitting the lane at the inference
call is what makes that possible; a second copy of the ordering would be
the "two rulers on one contract" defect §9a already caught once.
Validated by reproducing the rule bank's published verdict exactly
(120/120, p95 7 ms). Both §6 banks run against it unmodified via
`--endpoint`, and `scripts/next_edit_bakeoff.py` drives arms from a
manifest.

**The `zeta2` format was wrong and had never been exercised.**
`build_prompt_zeta2` emitted `<|marker_1|>` / `<|marker_2|>` sentinels,
written from a prose model-card description. Zeta-2's canonical
`sample.prompt` (published in `zed-industries/zeta-2`) brackets the
editable region with **git-merge markers**: `<<<<<<< CURRENT`, then
`=======`, after which the model resumes past `<[fim-middle]>` and
terminates the UPDATED side with `>>>>>>> UPDATED`. Against the real
weights the old dialect failed to parse **100%** of the time (0/30, 19
`invalid` + 11 `truncated`); corrected, the same arm scores 27/30. The
terminator is now the stop string (which is why the parser strips it
rather than requiring it — llama.cpp consumes a matched stop), and the
region poison check refuses `=======`, which also declines Markdown
setext underlines and live merge conflicts. That is the correct trade:
an ambiguous region boundary corrupts a file, silence costs one
suggestion. `zeta2_region_markers_match_the_published_sample_prompt`
pins the constants so this cannot silently rot again.

## 9d. The journal + outcome telemetry (2026-08-07)

The lane is being handed to a small group of Go and TS/React
developers, with their experience returning as the evidence. That
requires the answer to "was the suggestion any good?" to exist
somewhere other than in what someone remembers, so an episode is now a
**record** with a joinable identity.

**The journal layer is not next-edit-shaped.** `svrn journal` is a
generic verb, so the machinery lives in
`sovereign_contracts::types::journal`: a `JournalStream` descriptor
(file stem + its own disable env var) owning file layout, UTC-day
rotation, the byte cap, retention, and the four-way off-switch (global
env, global marker, per-stream env, per-stream marker — one decider,
`JournalStream::enabled`). Next-edit is the first stream, not the only
possible one; adding another is a `const JournalStream`, its own serde
types, and one row in the CLI's view registry (`journal_cmd::VIEWS`),
touching neither this lane's module nor any `match` on feature names
(§4 — open sets are registries).

**Two lines, one join.**
`sovereign_contracts::types::next_edit_journal` owns this lane's
vocabulary; the stream appends to
`~/.svrnmesh/journal/next-edit-<UTC date>.jsonl`, 14-day retention,
8 MiB/day cap:

- `NextEditEpisode` — one per `POST /v1/edit_predictions`: engine, whether
  the model fired, support/sites/proposed, the rule-lane silence reason,
  the consult reason or gate refusal or drop, `region_bytes`, model id /
  slot / format / `degraded`, `suppress_thinking`, language, file
  **extension**, total and inference ms. Built in `predict_response`
  where the facts are, appended by the route — so the offline scorer,
  which shares that pipeline, constructs the record and discards it
  rather than polluting a developer's numbers.
- `NextEditOutcomeLine` — one per outcome the editor reports, joined by
  `episode_id` (now on the response body, NOT debug-gated).

**No code, structurally.** `NextEditEpisode` has no `serde_json::Value`
field and no free-form string field, so nothing code-bearing has a
channel to the file — not the document, region, needle, rule
find/replace, proposed rewrite, or path. The model lane's debug value is
read by a **named allowlist** (`reason`, `skipped`, `dropped`,
`region_bytes`, `suppress_thinking`, `timings_ms.inference`), so a new
debug field is invisible to the journal until someone adds its name.
Two canary tests hold it: `no_code_bearing_field_can_reach_a_line` and
`debug_extraction_carries_no_code`.

**Four outcomes, and absence is counted.** `accepted` | `dismissed` |
`diverged` | `superseded`, each a name for a path `nextEdit.ts` already
took. There is deliberately **no `unknown` on the wire**: an episode
nobody resolved is the absence of a line, counted at read time. This is
§18.1's four verdicts in this lane's vocabulary — passed / failed /
could-not-judge / never-ran — and it exists because collapsing every
non-accept into `dismissed` yields an acceptance rate that looks precise
and is wrong. `diverged` in particular is not a rejection: the developer
typed on, which says nothing about quality, and it is the most common
ending by far. `svrn journal stats` therefore computes the rate over
`accepted + dismissed` only, prints `None` as *nothing judged yet*
rather than 0%, and labels anything under 20 judged episodes an early
signal rather than a number.

**Invisible, as a hard requirement** (decision note `09599af1`). Outcome
reporting adds no command, keybinding, prompt, status-bar state, or
notification. `reportOutcome` is fire-and-forget with a 2 s deadline and
swallows every failure — daemon down, 404 from a daemon predating the
route, timeout, 400. A telemetry failure must never become a user-facing
failure, and four vitest cases assert exactly that. On the daemon side
`record` drops its join handle, so no append can make a request wait or
fail; an IO error is one `warn`.

**Consent surface: `svrn journal [<stream>] <sub>`** (`stats` | `show` |
`bundle` | `off` | `on` | `clear`), in the DEFAULT build — an end-user
binary that recorded how a feature behaved with no way to read, bundle,
or stop it would be indefensible. Unscoped `off` writes the global
`DISABLED` marker (covering streams added later); `journal next-edit off`
writes only this stream's. **There is no send/submit/upload subcommand
and no network path out of the module.** `bundle` writes one file and
prints the complete list of fields in it, collected from the written
bytes rather than from the records that went in — and that collector is
feature-agnostic, so a new stream is audited by the same code the day it
is added. Ledger row (default-ON local write):
`sovereign/DEFAULTS_LEDGER.md`.

## 10. Verification surface

As built, mirroring FIM v1's: weight-free unit tests over the pure
pipelines — `commonwealth-api/src/next_edit.rs` (expansion, guards,
induction, threshold table, site ordering, UTF-16 mapping) and
`next_edit_model.rs` (consult gate incl. the casing deferral,
casing renderer, region selection, rewrite parsing, line-LCS diff)
— plus route tests in `routes_edit_predictions.rs` (rule fire,
explained silence, debug opt-in/out, UTF-16 on the wire, actionable
400; model-lane fire via a stubbed inference service, gate refusal,
rule-fired precedence, unavailable-service silence, drop-invalid,
drop-noop, default-off). The §9d journal adds 19 contracts tests — 9 over
the generic `journal` layer against two synthetic streams (isolation,
per-stream vs global off, prune sparing a neighbour's history, byte cap,
truncated-tail counted not guessed, foreign filenames never parsed as
day-files) and 10 over this lane's vocabulary (the code-bearing canary
plus the honesty set: `diverged_is_never_counted_as_dismissed`,
`unreported_episodes_are_unknown_not_dismissed`,
`nothing_judged_is_none_not_zero_percent`) — 4 in commonwealth-api (the
allowlist canary, rule-only absence vs `false`, fallback
distinguishable), and 12 over `svrn journal` (the bundle manifest must
match the written file exactly and list no field that can carry code;
the registry's names must be unique and must not shadow a subcommand;
a small judged population must be labelled, not quoted bare).
Extension vitest covers the pure cores
(`editUnits`, `editQueue`) and the client against the mock daemon —
including the four invisibility cases: a 404, a 500, an unreachable
daemon, and a missing `episode_id` must each change nothing the
developer sees.
Both §6 banks are built and green: `python3
scripts/next_edit_eval.py` (rule lane, weight-free) and `python3
scripts/next_edit_gen_eval.py` (model lane, needs a live next-edit
lane — `[models.edit]` or the §2a fallback)
each exit 0 iff their gates pass — run the first after any
`next_edit.rs` change, both after any `next_edit_model.rs` or
prompt change. The two workspace scripts remain the
definition-of-done gate for the Rust side.
