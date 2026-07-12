# TEACHABLE — coach the assistant in chat, own what it learns in settings

Status: P0 IN PROGRESS (plan approved 2026-07-11): explicit conation
capture, enforcement rungs 1–2 + prompt (one active lesson per rung),
first-application whisper, settings pane, coach + capture-precision
measurement. Companion measurement harness:
`tests/e2e/PERSONA_QA_DESIGN.md`. Reach for this when the persona loop's
observations are preference-shaped (see §10); pursue conventional fixes first.

## 1. The posture, and the constraint that shapes everything

The frontier apps ship "memory" as an opaque, platform-owned dossier bolted
onto a model with effectively unlimited instruction-following headroom. This
system is different in both directions, and the design leans into both:

- The model is never fine-tuned — the system IS the context assembly (the
  situated-agent principle). Learning is therefore *inspectable by
  construction*: everything the assistant has learned is a short list of
  plain sentences in settings, and it travels across your mesh nodes and no
  further.
- The models are small open weights (2B–35B), and for them **attention is
  the scarcest resource in the system.** Every token of instruction competes
  with the evidence and the question itself; clause piles measurably degrade
  judgment (the parsimonious-prompts finding; the chaos-QA discipline:
  short ORDERED decision procedures, stop at first match, never long
  parallel rule lists); irrelevant context actively hurts (bench isolation
  measured −0.23 from cross-corpus contamination). A teaching system that
  accumulates rules into a growing constitution would make the assistant
  *worse* with every lesson — the opposite of teaching.

So the two-sentence product is: **you can teach it in chat, the way you'd
coach a person; everything it learns is a list you own in settings** — and
the engineering thesis underneath is: **a lesson is an intent, and the
system's job is to honor that intent at the cheapest possible attention
cost, which is usually not a prompt token at all.**

## 2. Two lanes — facts are not lessons

Users teach two different things in chat, and they need different homes:

- **Facts** ("we use Vulkan, not ROCm"; "my sister's name is June") — the
  knowledge lane. Home: notes / personal corpora. Retrieved and cited like
  any evidence, subject to the grounding gate. This lane exists today.
- **Lessons** ("stop the sources lecture"; "shorter unless I ask"; "when I
  paste something, just give the verdict") — the behavior lane. Home: a new
  `lesson` note kind + a settings surface. This lane is what TEACHABLE adds.

The persona-QA study showed users already teach in the behavior lane — the
skeptic transcripts are coaching ("You're asking *me* to do the search?").
Today that signal is dropped; the app is identical the next session.

## 3. The lesson object — three representations, one intent

A lesson is stored once but exists in three forms, because the human, the
model, and the runtime need different things from it:

```
id             stable id (note id)
display        the rule as the USER reads it in settings, plain language
               ("Don't mention corpora, retrieval, or indexes — users
                don't care about the machinery.")
prompt_form    the COMPILED minimal-token imperative, only used when the
               lesson must ride the prompt ("No jargon: corpus, index,
               retrieval, chunk.") — fewest words that carry the rule,
               drafted by the fast slot at save time, re-derivable
enforcement    WHERE the intent is honored (see §7): param | transform |
               retrieval | prompt — chosen DETERMINISTICALLY at compile
               time from the directive taxonomy, cheapest first (P0 ships
               param | transform | prompt)
scope          activation context ("pasted content", "quick questions",
               a corpus id, a conversation) — empty = global. Global
               lessons are expensive (§6) and the capture flow says so.
taught_from    verbatim chat excerpt + conversation/turn ref (provenance)
drafted_display  the pre-edit draft sentence, kept ONLY when the user
               edited the card before saving (the consented correction
               pair — §11)
compiler_version  stamps the derived fields so they can be re-derived
               wholesale (recompile-on-model-swap is a P1 command)
created / last_affirmed / enabled
```

The record splits into **source of record** — `display`, `taught_from`,
the lifecycle fields — and **derived artifacts** — `prompt_form`,
`enforcement`, `params` — re-derivable from source at any time (§11).
Display sentences for param/transform lessons come from deterministic
templates; the fast slot phrases only prompt-rung lessons, where the
model's judgment is already required.

Lessons are SHAPES, not instances — the same discipline as prompt tuning
(no teaching-to-the-test): capture generalizes "tldr this" + annoyance into
"prefers terse verdicts on pasted content", never a per-question patch.

## 4. Capture — consent-shaped, card-shaped

Detection first ships **explicit-only** (high precision) and hangs off
the **ConationQuery cell** — the router's home for imperatives directed
at the assistant ("stop", "shorter please"), whose handler already parses
coaching directives into one-turn transforms
(`runtime/handlers/conation.rs`). An earlier draft anchored capture on
the Metalingual cell; implementation reality disagrees: the shipped
Metalingual *handler* is a source-anchored vocabulary lookup ("how does
SEP use this term"), and the streaming path escalates corpus-anchored
metalingual turns to KnowledgeQuery. Coaching lives in Conation.
Widening to other cells is P1, evidence-gated on the study showing real
misses. (Small-model corollary, unchanged: detection is routing-assisted
and heuristic-floored, not another judgment we ask a 2B to make unaided —
the routing-degradation finding says the deterministic floor is what
survives model swaps.)

What separates teaching from a one-turn adjustment is linguistic aspect,
and it is lexically cheap — the **durative floor**:

- **Deictic** ("make this shorter", "tldr") → today's behavior,
  untouched: the handler transforms the prior reply and forgets.
- **Durative** ("keep answers short **from now on**", "**stop**
  mention**ing** sources", "**always** explain like I'm five") → the same
  transform runs, AND a detached post-turn draft proposes a lesson.
  Markers: always / never / from now on / going forward / every time /
  stop|quit|keep <gerund>, on word boundaries, capped at 40 words (a long
  paste is content, not coaching). False fires are the failure mode;
  misses are fine — the coaching still worked for that turn, and
  restating with "always" is a natural retry.

This also fixes a quiet dishonesty in the shipped handler: today "keep
answers short from now on" is silently narrowed to a one-turn transform —
the app accepts a standing instruction and discards the "standing" part.

Capture never costs the turn anything: the draft runs in a detached spawn
after the reply, compiles deterministically (§7), and shows a card — the
same UI grammar as the information-request card, tethered to a
`lesson_drafted` narration chip:

> ◈ Learn this? — "Keep answers under a paragraph unless asked to go deep."
> [Save] [Edit] [Not this]

The card is simultaneously the consent moment, the legibility moment, and
the correction moment (fix a mis-drafted lesson before it exists — the
drafter is itself a small model, so the human check is load-bearing; an
edit is kept as the drafted/corrected pair, §11). A malformed draft is
silently dropped — no card at all beats a wrong card.
**Never silent learning.** Dismissals are not stored.

Implicit capture (frustration arcs, rephrase chains, cancels — shapes the
persona harness already classifies) ships later and only ever produces
*suggestions* on the same card — see §9 phasing.

## 5. Storage and the settings surface

Lessons are notes (`kind = lesson`) — inheriting persistence, hygiene
tooling, and mesh gossip with the existing privacy machinery (private notes
structurally never leave the node; default follows `node.default_privacy`).

Settings gets a "What I've learned" pane: the list, each entry showing
the display rule, its scope, its enforcement point (a chip in USER
language: "answer length" / "wording check" / "standing reminder" — the
no-jargon bar applies to our own settings copy), the teaching excerpt +
date behind a disclosure, a toggle, delete. Superseded lessons render
struck-through with "replaced by". Deleting is real deletion. **No
in-pane text editing in P0** — the edit affordance lives on the capture
card, before the lesson exists; afterwards, delete-and-reteach-in-chat IS
the product's own loop. This pane is the trust story — boring, plain,
complete. No hidden lessons, ever.

## 6. Attention management — the core design problem

This section is the design. Everything else is plumbing around it.

**P0 has no selector.** With one active lesson per rung (supersede is
structural — §5, §7), there is no selection problem to solve: the prompt
path carries zero or one compiled sentence, injected at the primary
synthesis sites where the prompt-budget sensor already measures and
degrades it; tiny-prompt handlers never carry the block. The machinery
below — retrieval-scored selection, K=3, slot budgets, placement — is
P1's, and none of it is built before scoped lessons exist.

- **Zero is the default.** Most turns carry NO coaching tokens. A lesson
  enters the prompt only when (a) its enforcement point is `prompt` —
  already the last resort — AND (b) its scope matches the turn. Unscoped
  prompt-lessons are the most expensive object in the system, and both the
  capture card and the settings pane say so ("this will apply to every
  answer — scope it?").
- **Hard cap, ordered, non-contradictory.** The coaching block is capped at
  K≈3 compiled lessons on the primary slot, K≈1 on fast slots, rendered as
  a short ordered list (stop at first match) — never parallel prose rules.
  The hygiene pass enforces non-contradiction *structurally*: saving a
  lesson that conflicts with an existing one supersedes it (recency wins,
  shown in settings as strikethrough + "replaced by"), so the block can
  never contain a contradiction for the model to reconcile — small models
  don't reconcile, they degrade.
- **Slot-aware budgets.** The block's allocation comes from the
  prompt-budget sensor and respects slot input gates (FastShort refuses
  oversized prompts today; a coaching block that tips a turn over a slot
  gate is a bug). Under pressure the coaching block is dropped FIRST — a
  missing lesson degrades style; a missing evidence chunk degrades truth.
- **Placement is empirical, not assumed.** Small models have strong
  primacy/recency attention patterns, and the right position for the block
  (system-adjacent vs question-adjacent) is a measurable question — the
  today-anchor precedent (injected into system AND refinement prompts)
  shows placement decisions here get validated, not guessed. The harness
  A/Bs placement on replay banks before the position is frozen (§8).
- **Selection is retrieval, not enumeration.** Candidate lessons are
  embedded and scored against the turn + conversation frame like any other
  context source; scoped lessons only activate in scope; ties break to
  most-recently-affirmed. The selector is deterministic given the
  embeddings — no LLM call spent deciding which lessons to spend tokens on.

## 7. Compile to the cheapest enforcement point

The deepest attention win is not needing the prompt at all. At save time a
lesson compiles to the cheapest mechanism that can honor it, checked in
order:

1. **Parameter** — "shorter answers" is a length-target change on
   matching turns: the resolved output budget's SOFT target (which the
   length directive renders from), never the `max_tokens` hard ceiling —
   the truncation-bug class the budget redesign killed must not regress.
   Zero extra prompt tokens, deterministically honored.
2. **Transform** — "no jargon" is a presenter-side lexical rewrite backstop
   (the presenter already strips tool-call leaks structurally); "always
   show sources as footnotes" is a rendering rule. Zero attention cost,
   enforced even when the model forgets. P0's transform is a conservative
   whole-word term-avoid pass that runs post-grounding-gate, after the
   citation passes, and skips `[Source: …]` spans and code fences — it is
   structurally unable to touch citation anchors or verified facts.
3. **Retrieval/config** — "stop searching the web unless I ask" toggles
   affordance behavior; "prefer my notes over wikipedia" is a retrieval
   weight. Config, not prose.
4. **Prompt** — only intents that genuinely require the model's judgment in
   generation ("explain like I'm five", "hedge less") ride the prompt, in
   compiled `prompt_form`, under §6's caps.

The settings pane shows the enforcement chip per lesson, which keeps the
system honest and legible: "this one costs attention" is visible. When a
prompt-lesson's effect could be had structurally, moving it down the ladder
is a hygiene-pass suggestion — the same structural-fix-over-prompt-patch
ethos as the rest of the codebase.

**First application is acknowledged once.** The first answer a saved
lesson influences carries a one-time whisper ("◈ Kept: <rule>"); after
that, silence — a good colleague writes it down once, then just does it.
Every influenced turn records `lessons_applied` in message metadata
regardless, so provenance stays inspectable without nagging.

## 8. Measurement — teachability is a persona-QA scenario

Most products ship memory and hope. Here the harness makes teaching a
regression-testable claim, with attention cost as a first-class metric:

- **Coach scenario**: a `coach` persona (≈20 lines of personas.toml)
  teaches a lesson in session A (ordered, deterministic — not the weighted
  random picker); a fresh session B asks the same frozen questions.
  Before/after deltas are measured DETERMINISTICALLY — answer length
  (`answerLen` is already journaled) and the jargon-leakage regex — not a
  new judge dimension: the implemented posture judge scores
  admits/agency/clean on gap turns only, has no brevity dim, and is
  uncalibrated for one. The delta is a REPORT (tiny N, always stated),
  not a gate. What IS gated: zero regression on grounding verdicts and
  competence-when-present — a lesson must never buy style with truth (the
  chaos-gate red-line metric is the guard).
- **Attention regression**: with the coaching block at cap, grounded-answer
  quality on lesson-irrelevant turns must not move. This is the direct test
  of §6 — if 3 compiled lessons measurably degrade unrelated answers, the
  cap is too high for that model tier, and the number comes down.
- **Placement A/B**: block position validated on deterministic replay banks
  (same questions, block system-adjacent vs question-adjacent) before
  freezing.
- **Capture precision**: the card must not fire on non-coaching turns —
  the existing persona mix is the negative-control traffic, the journal
  records a per-turn `lessonCard` field on every run, and the gate is a
  zero-tolerance COUNT (hallucinations-shaped): one false fire is a
  detector bug to fix, never a threshold to loosen.

## 9. Phasing

- **P0 — the loop closes, cheapest-first.** Explicit capture
  (ConationQuery routing + durative floor → deterministic compile,
  fast-slot phrasing for prompt-rung only → card) → `lesson` notes (one
  active lesson per rung, supersede-with-strikethrough) → "What I've
  learned" settings list (toggle / delete / provenance; no in-pane
  editing) → enforcement rungs 1–2 (length param + term-avoid transform)
  and rung 4 (prompt, K=1) → first-application whisper. Measured on day
  one (§8): coach A/B report + capture-precision zero-tolerance gate.
- **P1 — scoped lessons + hygiene + K=3.** Scope field live; supersede
  pass; retrieval-selected block at full cap; placement A/B run and
  frozen. Also: recompile-all-lessons on compiler/model change (the
  `compiler_version` stamp makes this a command, not a migration); K≈1
  coaching on fast slots; Metalingual-cell widening if the study shows
  misses; cancel-arm durative awareness ("stop mentioning sources"
  currently takes the cancel sub-shape and replies "Stopped." while the
  card appears — accepted P0 quirk).
- **P2 — suggested lessons from implicit signal.** Frustration arcs
  generate card *suggestions* — same consent flow, never auto-saved.
- **P3 — mesh.** Public-scoped lessons gossip across the user's nodes;
  taught on desktop, applies on mobile.

## 10. When to reach for this from the improvement loop

Conventional routes first. The trigger for TEACHABLE is when the loop's
observed failures are **preference-shaped rather than correctness-shaped**
— i.e., when the honest fix would otherwise be yet another clause in the
global system prompt, which the attention constraint forbids outright on
these model tiers. Current examples from the persona study: jargon leakage
and the agency deficit are app-wide defaults (fix conventionally, once, for
everyone — ideally at enforcement rungs 1–3, not the prompt); but the
*residual* after those fixes — one user wants terse verdicts, another wants
depth; one wants sources footnoted, another finds them noise — is exactly
per-user, preference-shaped, and unfixable by any global anything. That
residual is TEACHABLE's job, and the ladder in §7 is what lets a
2B-on-a-laptop honor it without spending the attention it doesn't have.

## 11. Forward architecture — the system-of-record commitment

TEACHABLE is the behavior-lane instance of a larger commitment: **the
personal store is the system of record; every compiled form is a derived
artifact that must be reconstructible from it — and destroyable.** Future
layers consume the lesson store; they do not replace it.

- **Source vs derived.** A lesson's source of record is `{display,
  taught_from}` — plain language, provenanced, hard-deletable. The
  compiled fields (`prompt_form`, `enforcement`, `params`) are derived,
  stamped `compiler_version`, and re-derivable wholesale — a model swap
  recompiles lessons overnight (P1 command); it does not migrate them.
- **The ladder generalizes.** Fact → store (notes/corpora); rule →
  structure (params, transforms, config); disposition → conditioning.
  The ladder's declared future rung 5 is *conditioning* — a
  self-cartridge / adapter trained FROM prompt-rung lessons and their
  provenance, absorbing what genuinely needs the weights (voice,
  register, taste). The enforcement ladder is the pre-cartridge form of
  the same prefill commitment: never pay context for standing knowledge
  that structure can honor. Zero rung-5 scaffolding ships before then.
- **Consent is the capability gate.** Chat text is untrusted flow; it
  becomes standing program state only through the card — an explicit
  human check between data and control. Dismissals are never stored,
  aggregate counts included: the consent commitment outranks signal
  harvest.
- **The correction pair is kept.** An edit on the card stores
  `drafted_display` alongside the saved sentence — a consented preference
  pair, collectible only at the moment of consent: the future
  taste-model seed, and today a drafter-quality metric (edit rate on
  saved lessons).
- **The sycophancy counterweight is structural.** No enforcement rung
  touches the grounding gate: transforms run post-gate, the prompt block
  is subordinated to grounding rules, and factual claims are scored
  against evidence provenance — never against preference. A lesson can
  shape style; it cannot buy agreement with truth.
