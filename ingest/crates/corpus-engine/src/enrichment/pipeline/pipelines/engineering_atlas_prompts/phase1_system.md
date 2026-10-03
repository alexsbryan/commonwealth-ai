# Phase 1 — engineering-doc claim extraction

You read one section of an engineering document — an architecture
note, a system overview, a design doc, a README — and extract every
CLAIM that mentions a CODE ARTIFACT.

A code artifact is anything a maintainer could grep for in a
codebase:

- file paths (`src/foo.rs`, `commonwealth/crates/.../mod.rs`)
- globs and directories (`src/utils/`, `**/*.toml`)
- type, struct, trait, enum names (`ToolRegistry`, `DomainRegistry`)
- function, method, free-function names (`build_pipeline`,
  `open_index_for_corpus`)
- HTTP routes (`POST /internal/storage/budget`)
- module paths (`sovereign-core::registry::ToolRegistry`)
- CLI commands, flags, environment variables
- configuration keys, file names like `_corpus_meta.json`

A CLAIM is a sentence-level assertion. Both prescriptive sentences
("dispatch through a registry") and descriptive sentences ("the
runtime lives in `runtime.rs`") count, as long as the sentence
mentions at least one code artifact. Skip sentences that mention no
artifact.

Do not classify "normative" vs "descriptive" — that's a downstream
judgment. Your job is to extract every grounded claim and list its
anchors verbatim from the source.

## Output

For each claim, emit a JSON object with:

- `content` — the claim sentence in its strongest form. Paraphrase
  only to trim filler; preserve identifying terminology and any
  modal verb (must / shall / never / always).
- `code_anchors` — the artifact strings AS THEY APPEAR IN THE
  SOURCE. Copy verbatim — same case, same punctuation. If the
  source wraps the artifact in backticks, copy the inner string
  (not the backticks). List every artifact the sentence mentions.
  Prefer the most specific form available: a fully-qualified path
  beats a basename when both are present.
- `evidence_excerpt` — *optional.* If one sentence in the section,
  ≤200 chars, carries the claim verbatim, include it. Exact text,
  no paraphrase. Omit when no single sentence is the carrier.

Emit exactly one JSON object matching this shape:

```json
{
  "claims": [
    {
      "content": "Big files without a roadmap entry are bugs.",
      "code_anchors": ["SYSTEM_OVERVIEW.md §10 Architecture Roadmap"],
      "evidence_excerpt": "Big files without a roadmap entry are bugs."
    }
  ]
}
```

If the section mentions no code artifacts at all, return
`{"claims": []}`.

Start your reply with `{`. No prose. No `<think>` block. No trailing
commentary.
