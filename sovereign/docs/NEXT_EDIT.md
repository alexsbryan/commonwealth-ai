# Next-Edit Prediction — Design Spec

Status: **BOTH LANES IMPLEMENTED AND HARDENED — P1 rule lane + P2
model lane, 2026-07-30**. An adversarial pass (§9a) then went at all
three surfaces with `text`, `history`, and the model's own output
treated as hostile; every HIGH/MED finding was fixed with a named
regression test, and both banks re-ran green with the bars untouched.
User-facing walkthrough:
[`docs/NEXT_EDIT_IN_YOUR_EDITOR.md`](../../docs/NEXT_EDIT_IN_YOUR_EDITOR.md). The daemon route (`POST /v1/edit_predictions`), the
pure induction pipeline, and the extension provider are built and
tested; both exploration spikes (§5) were retired into the real
build the same day, after the operator validated accept/jump feel
and the ambient trigger in hand. The **rule-lane eval bank SHIPPED
2026-07-30** (`gym/next-edit/`, 120 cases, all pre-registered gates
green; its first run caught the insertion-idempotence wrong-edit
bug, fixed the same day). The **model lane SHIPPED the same day**
behind the same route (`engine: "model"`, prompted Mellum2 on the
FIM slot), gated by the §6 generalization bank
(`gym/next-edit/gen/`, 60 cases): **all five pre-registered gates
green on run 3** (30/30 positives correct, 0 wrong edits, wall p95
1.8 s) after runs 1–2 caught Mellum2 destructive on cross-casing
renames — that one category is **detected but deferred**
(`casing_deferred`, §4/§6) pending a deterministic rule sub-lane.
The extension consults the lane by default
(`sovereign-fim.nextEdit.modelLane`), silently inert unless
`[models.fim]` is resident. Companion to
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
  when ≥2 consecutive coalesced edits match the same induced literal
  rule. Ships the demo case with zero model risk and zero extra RAM.
- **Model lane (v2, eval-gated — SHIPPED, default-on)**: prompted
  region-rewrite on the resident FIM slot
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
a support count. The policy is a small legible table: never without
a remaining site; 2 supporting edits fire only a specific rule
(find ≥ 4 chars after expansion); 3+ supports lower the bar. One
edit never fires anything. Sites the rule was **already applied to**
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
  the evidence). Needs `[models.fim]` resident; the runner probes
  and says exactly that when it is missing.

## 7. Config (as built)

Extension settings under the existing `sovereign-fim.*` namespace:
`nextEdit.enable`, `nextEdit.settleMs`, and `nextEdit.modelLane`
(default **true** since the §6 verdict; it sends `model_lane: true`
on the wire). Daemon side: deliberately **no new config** — the
model lane serves whenever `[models.fim]` is resident AND the
request opts in; enablement is a client concern, policy is
daemon-side. No other knobs were added: temperature, region size,
and timeout are constants in `next_edit_model.rs` until an eval
says they should move.

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
can be scored without a daemon and without a resident `[models.fim]`
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
drop-noop, default-off). Extension vitest covers the pure cores
(`editUnits`, `editQueue`) and the client against the mock daemon.
Both §6 banks are built and green: `python3
scripts/next_edit_eval.py` (rule lane, weight-free) and `python3
scripts/next_edit_gen_eval.py` (model lane, needs `[models.fim]`)
each exit 0 iff their gates pass — run the first after any
`next_edit.rs` change, both after any `next_edit_model.rs` or
prompt change. The two workspace scripts remain the
definition-of-done gate for the Rust side.
