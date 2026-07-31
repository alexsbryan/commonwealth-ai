# Next-Edit Prediction — Design Spec

Status: **DESIGN, approved 2026-07-30**. The render spike is built
(§5 — command-triggered, extension-only, no daemon traffic). The
tracker, daemon route, and both prediction lanes are NOT built; every
section below other than §5 describes intent, not shipped behavior.
Companion to [`INLINE_COMPLETION.md`](./INLINE_COMPLETION.md) (FIM
v1): this seat reuses its slot, its glassbox conventions, and its
eval discipline.

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
- **Model lane (v2, eval-gated)**: prompted region-rewrite on the
  resident FIM slot. Mellum2-Instruct's post-training covers code
  editing and instruction-following but is **not next-edit-trained**
  — its quality here is an open empirical question, and the lane does
  not default-on until the §6 eval says so. The prompt shape follows
  Zed's open Zeta format (edit history as diff snippets + an excerpt
  bracketed by editable-region markers; output = the rewritten
  region), which is battle-tested and costs nothing to adopt. Zeta
  *weights* (a second resident model, ~4–5 GB quantized) are a
  documented fallback for machines with headroom, not the plan of
  record: lean mode exists because the team tiers have ~3.5 GB free
  beside the primary.

## 3. Architecture (planned)

Stateless daemon, IDE-agnostic — the same posture that keeps the
JetBrains port mechanical for FIM. The daemon holds no editor state;
edit history rides along on each request.

- **Extension — edit-history tracker.** Coalesces
  `onDidChangeTextDocument` keystroke deltas into semantic edit units
  (debounce + spatial grouping), keeps a rolling per-file window
  rendered as unified-diff snippets. Edits applied by accepting
  suggestions are *included* — accepting edit N is the strongest
  signal for predicting edit N+1.
- **Daemon — `POST /v1/edit_predictions`** on the `client_router`
  (:9741), beside the FIM handler
  (`commonwealth/crates/commonwealth-api/src/routes_completions.rs`).
  Request: `{history[], excerpt, cursor, path, language, debug}`.
  Response: `{edits: [{range, new_text}], engine: "rule" | "model",
  sovereign_debug}`. The daemon returns **structured edits, never raw
  text**: it validates that model output parses, stays inside the
  editable region, and is size-bounded — an invalid or oversized
  prediction is dropped server-side, because no suggestion beats a
  wrong one. The rule lane runs first as the fast path; the model
  lane slots in behind the same response shape.
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
and surfacing policies. The model lane (v2) inherits the same
trigger point but adds a consult gate (only when the rule lane
declines AND the recent units are similar-but-not-identical) plus a
one-in-flight budget that always yields to FIM ghost text.

**When to fire — structural confidence, not a model score.** The
induced rule's context expansion (edit ± the untouched identifier
run around it, so `log`→`debug` becomes `console.log(` →
`console.debug(`) gives a specificity measure; recent history gives
a support count. The policy is a small legible table: never without
a remaining site; 2 supporting edits fire only a specific rule
(find ≥ 4 chars after expansion); 3+ supports lower the bar. One
edit never fires anything.

**How to appear — never scroll uninvited.** If the next site is in
the viewport, decorate it in place. If off-screen, surface only a
one-line hint at the cursor's line end (`⇥ rule · N sites · next:
line L`); the first Tab jumps and decorates, subsequent Tabs
accept+advance — mid-chain, revealing is expected. Esc suppresses
the rule for the session (no re-nagging). Any manual edit clears
the proposal; the next settle re-evaluates, so continuing the
pattern by hand simply re-offers with more support.

## 5. Render spike (built, throwaway)

`packages/vscode-sovereign/src/nextEditSpike.ts` + pure core
`nextEditSpikeCore.ts`. Command **"svrn fim: Next Edit (render
spike)"**: a deterministic stand-in for the predictor (prefers the
`console.log(` → `console.debug(` scenario; falls back to renaming
occurrences of the word under the cursor) drives the *real* target
UX — reveal the next site if off-screen, render the diff as
strikethrough-old + ghost-new decorations with an end-of-line hint,
Tab accepts and advances, Esc dismisses, any external document edit
invalidates the proposal. No daemon traffic; nothing fires without
the explicit command; delete the module when the real provider lands.

What it is meant to answer, in the operator's hands: does
decoration-rendered diff-preview read as "a proposed edit"? Does Tab
interception behind a context key coexist with the suggest widget,
snippets, and FIM's own ghost text (`!inlineSuggestionVisible` in the
`when` clause)? Does accept-then-jump feel like Copilot NES or like
fighting the editor? **Spike 1 verdict (operator, 2026-07-30):
accept/jump feels good.**

**Spike 2 — ambient trigger** (same files plus `editUnits.ts` +
`ruleInduction.ts`, both pure and vitest-covered): the §4 policy
made real, still extension-only. A per-document shadow snapshot
recovers deleted text (VSCode change events carry the range but not
what it said), a `UnitCoalescer` closes edit units on settle
(`sovereign-fim.nextEditSpike.settleMs`, default 600ms), induction +
threshold run at each close, and the hybrid surfacing policy renders
via the spike-1 mechanics. Ambient by default behind
`sovereign-fim.nextEditSpike.ambient`; the manual command remains as
a demo entry. The real build moves induction behind
`/v1/edit_predictions` (§3) — what spike 2 validates is trigger +
surfacing *feel*, which stays client-side either way.

## 6. Eval — before any model lane defaults on

`gym/next-edit/` in the mold of `gym/fim/` (see
[`INLINE_COMPLETION.md`](./INLINE_COMPLETION.md) §4/§7 and
`gym/fim/harvest.py`):

- **Harvest from this repo's git history**: commits containing N≥3
  similar hunks are natural repeated-edit episodes — replay the first
  k hunks as edit history, hold out the rest, score whether the
  predictor produces the held-out hunk (exact / normalized).
- The **rule lane's contract is ~100%** on literal-repeat episodes —
  it is deterministic; any miss is a coalescing or induction bug, not
  model noise.
- The **model lane** gets a hand-curated generalization bank
  (renames, signature fan-out — cases the rule lane structurally
  cannot fire on), sized like FIM's 60-case bank.
- Same accept-proxy discipline as FIM: first-line/normalized match,
  plus a wrong-edit rate — the metric that gates default-on.

## 7. Config sketch (not final)

Extension settings under the existing `sovereign-fim.*` namespace
(enable flag, history window, trigger debounce). Daemon side: no new
model table — the lanes reuse `[models.fim]`; a `[models.fim]`
subkey enabling the model lane arrives with v2. Deliberately no
just-in-case knobs before the eval exists.

## 8. Phasing

1. **P0 — render spike** (§5): done.
2. **P1 — rule lane end-to-end**: history tracker → route → rule
   induction → rendering. No model, no new RAM. Ships the canonical
   case completely.
3. **P2 — model lane** behind the same route, prompted Mellum2
   first, gated on §6.
4. **Deferred**: cross-file edits, JetBrains port, optional Zeta
   slot for high-RAM machines, marketplace publish.

## 9. Glassbox

Every response carries `sovereign_debug`: `{engine, rule (find →
replace, verbatim), sites_remaining, prompt_chars, timings_ms,
drop_reason}` — a dropped model prediction is *reported as dropped*,
not silent. Daemon logs under a `next_edit` tracing target, which
must be added to the default tracing allowlist (custom targets are
dark unless listed — the allowlist is pinned by tests).

## 10. Verification surface

Planned, mirroring FIM v1's: weight-free unit tests for edit
coalescing and rule induction; route tests in `commonwealth-api`
(shape, validation-drop, debug block); extension vitest over the
pure cores (the spike's site-scan core ships tested now); the §6
bank as the quality gate. The two toolbox scripts remain the
definition-of-done gate for the Rust side.
