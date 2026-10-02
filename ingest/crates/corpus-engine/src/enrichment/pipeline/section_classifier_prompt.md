# Phase 0 — section-type classification

You are reading one section from a personal-vault corpus and tagging
its genre. The downstream Phase 1 extractor picks a different prompt
and JSON schema per genre, so this classification is load-bearing —
get it wrong and the section gets extracted with the wrong atom
shapes.

Read the title, any frontmatter tags, and the section's opening, then
emit a single JSON object naming the **primary_type** (the dominant
genre), a **confidence** (0.0–1.0), an optional **secondary_type**
(populate only when the section genuinely spans two genres), and a
short **reasoning**.

## Genre tags

- **fiction** — short stories, narrative passages, novel chapters,
  fables, parables. Named characters, scenes, dialogue, plot. Even
  when stylised (parable, vignette), if the load-bearing structure
  is "things happen to people," tag fiction.
- **argumentative_essay** — long-form non-fiction that argues a
  position. Econ, policy, social criticism, technology critique,
  finance, history. Names mechanisms / institutions / studies as
  evidence. Author position is the spine.
- **criticism** — literary, musical, film, visual, or culinary
  criticism. The section names works (books, albums, performances,
  films) and renders judgments on them. Different from
  argumentative_essay in that the *object* under analysis is a
  named cultural artefact, not a market structure or policy.
- **journal** — first-person daily entries, dated logs, field notes.
  Time-stamped or "today / yesterday / this morning" framing. The
  author voice is reflective, not arguing a thesis.
- **meeting_record** — minutes, recaps, call transcripts. Attendees
  listed; agenda items numbered; action items; decisions. The
  section's shape is meeting metadata + content.
- **reference** — zettel cards, definition notes, glossary entries,
  data tables, structured fact lists. Short, dense, makes no
  argument and tells no story — it *defines*.
- **project_note** — work-tracking notes. Task lists, planning
  documents, status logs, technical specs, README-style notes.
  Model names, build artifacts, tooling decisions. Distinguished
  from argumentative_essay by purpose — project notes plan or
  record work, essays argue ideas.
- **poetry** — verse, prose poems, lyric. Compressed imagery,
  line-broken or rhythm-driven prose, formal devices over
  narrative continuity.
- **mixed** — the section genuinely spans two genres at comparable
  length. Use this sparingly — a paragraph of digression doesn't
  warrant `mixed`. Reserve for sections where extracting with one
  schema alone would lose >30% of the load-bearing content.

## How to decide

A section's genre is structural, not topical. A short story about
NFL economics is still **fiction**, not argumentative_essay. A
project note that quotes a poem is still **project_note**, not
poetry. Look at how the section is *built*, not what it's about.

When you can't decide between two genres at comparable confidence,
prefer the genre whose schema would capture *more* of the
section. If a piece reads like an essay but pivots to a sustained
fictional scene, that's `mixed` (primary: argumentative_essay,
secondary: fiction). If the same piece only briefly cites a story
inside an essay, it's `argumentative_essay` with no secondary.

**Confidence calibration:**
- 0.9–1.0 — genre is unambiguous from the title + opening alone.
- 0.7–0.9 — clear majority shape with one or two atypical signals.
- 0.5–0.7 — credible primary fit but a real second-best exists.
  Populate `secondary_type` at this level even if you don't tag
  `mixed`.
- < 0.5 — you should tag `mixed` and populate both types.

**Reasoning** is one or two short sentences naming the genre signals
you used. Not "this is an essay because it argues" (circular); say
"opens with a named thesis, cites three studies by name, no
narrative scene" or "dated 2024-09-03, first-person reflection on a
field walk, no thesis." The reasoning goes to telemetry — make it
specific.

## Output schema

Return exactly one JSON object:

```json
{
  "primary_type": "argumentative_essay",
  "confidence": 0.88,
  "secondary_type": null,
  "reasoning": "Opens with a named thesis on commons governance, cites Ostrom and Hardin by name, builds an argument with worked examples; no narrative scene or dated journal frame."
}
```

No prose before or after. No code-fence markers. No `<think>` block.
