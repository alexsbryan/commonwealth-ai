# Drift Detection — narrative vs code

> Audit where your team's stated architecture has drifted from the
> actual shape of the code.

## What this is

A two-stream legibility surface:

- **Narrative stream** — what the team has *deliberately written* about
  the architecture. Stable, slow-moving artifacts: `CHARTER.md`,
  `ARCH_PRINCIPLES.md`, `SYSTEM_OVERVIEW.md`, accepted design docs,
  ADRs. The team's intentional positions.
- **Structural stream** — what the code *actually does*. Modules,
  items, cross-references, mechanically derived from the source via
  tree-sitter + SCIP.

When the two agree, you have **dual-attested** architectural truth.
When they diverge, you have **drift candidates**. The drift report is
the digest a new developer reads to deeply understand where the
codebase has departed from its own intentions.

## When this pays off

Atlas treatment is worth the LLM cost for **stable, deliberate
artifacts** the team has authored:

- `CHARTER.md`, `ARCH_PRINCIPLES.md`, `SYSTEM_OVERVIEW.md`
- `ADR/` directories (accepted decisions only — not in-flight drafts)
- `.sovereign/features/*/spec.md` (accepted state)
- Major design docs that have been reviewed and merged

It does **not** pay off for:

- `README.md` (changes whenever onboarding flow shifts)
- `CHANGELOG.md` (append-only, no architectural content)
- Auto-generated API docs (already covered by the structural atlas)
- In-flight branch documents (atoms decay before they're read)

The volatility profile is the test: if the doc rotates monthly,
extract every monthly run; if it rotates yearly, the extracted atoms
stay valid for a year and the cost amortises.

## Quick start (one command)

```bash
sovereign drift detect \
  --code /path/to/your/repo \
  --narrative /path/to/CHARTER.md \
  --narrative /path/to/ARCH_PRINCIPLES.md \
  --output ./drift.md
```

That's it. The orchestrator runs the eight underlying primitives in
sequence:

1. Probe + resolve a working chat-model slot.
2. Index your code (skipped on re-runs if cached).
3. Build the structural atlas (skipped on re-runs).
4. For each narrative doc:
   - Stamp a recipe from the template.
   - Install the corpus (markdown chunks + LanceDB index).
   - Init enrichment.
   - Build the narrative atlas via the literary_atlas pipeline.
   - Match the atlas against the structural atlas (cross-corpus).
5. Render the drift digest.

Wall time: roughly 25-30 minutes per narrative doc on first run
(LLM-bound), seconds on re-runs (everything's cached).

## Composable primitives (power-user path)

If you want to fine-tune any step, the orchestrator wraps these:

```bash
# 1. Index code
sovereign code index /path/to/repo --corpus-id myproject

# 2. Build structural atlas
sovereign enrich ingest myproject-self-atlas --source-corpus myproject

# 3. For each narrative doc — stamp a recipe
cp -r ~/.sovereign/recipes/_templates/narrative-markdown \
      ~/.sovereign/recipes/myproject-arch
# Edit ~/.sovereign/recipes/myproject-arch/recipe.toml:
#   [corpus] id = "myproject-arch"
#   [acquire] path = "/path/to/ARCH_PRINCIPLES.md"

# 4. Install + enrich
sovereign corpus install myproject-arch
sovereign enrich init myproject-arch \
  --from-corpus myproject-arch --pipeline literary_atlas
sovereign enrich build myproject-arch \
  --full --skip seed --skip configure

# 5. Match atlases
sovereign enrich atlas-cross-corpus \
  myproject-arch myproject-self-atlas

# 6. Render digest
sovereign enrich atlas-drift-report \
  --narrative myproject-arch \
  --structural myproject-self-atlas \
  --output ./drift.md
```

## Reading the report

The digest is intentionally short — one page, three sections:

### Act on (top)

Critical findings only: normative claims (`MUST`, `SHALL`, `NEVER`)
in the narrative that have no anchor in the structural atlas. Each
shows the verbatim quote, the source doc + section, and a
next-step.

This is where a new dev starts. Two paths per finding:

- **Anchor it**: locate the implementation, add rustdoc citing the
  principle. Now the next drift run will dual-attest it.
- **Revise it**: the principle has shifted; update the narrative.

### Confirmed

Comma-separated paragraph of components confirmed in both streams.
**These are the architectural foundations both the docs and the code
agree on.** A new dev can trust these as canonical when reading code.

### Investigation queue

Bucketed counts of unmatched narrative entities, classified
automatically:

- **file path** — `foo.rs` references; the structural atlas indexes
  by symbol, not filename. Look here if the team's docs reference
  source files directly.
- **method/function** — `Foo::bar`-shaped names; function-tier atoms
  are excluded from the structural atlas by default. Run with
  `--include-functions` if function-level drift matters.
- **constant/identifier** — `SCREAMING_SNAKE_CASE` names; consts
  aren't indexed as Entity atoms.
- **external library** — `tokio`, `tantivy`, model identifiers; not
  drift, just narrative discussing dependencies.
- **abstract principle** — discussion-shaped vocabulary like
  `behaviour-preserving`, `observability`. Not a code symbol.
- **self/config reference** — references to the docs themselves or
  `*.toml` config files.
- **worth a closer look** — things that didn't match a class. Real
  drift candidates often live here.

The full per-finding detail (atom IDs, chunk references, original
descriptions) lives in the JSON sidecar at `<output>.json` for
downstream tools.

## Severity rules

Severity keys on **atom shape**, not filename:

- **Critical** — `Claim` atoms with normative epistemic status
  (`must`, `shall`, `always`) and no structural anchor. The team
  stated a rule with no code evidence.
- **Critical** — `Configuration` atoms describing a structural
  pattern whose named members are absent.
- **Likely** — `Entity` atoms in the narrative with no fuzzy match.
  Rolled into the Investigation queue.
- **Note** — partial matches (compression / fanout / paraphrasing).
  Rolled into the Investigation queue.

## Troubleshooting

**"no working chat slot"** — the orchestrator probed
`/v1/chat/completions` and got either a 503 or an empty response.
Usually means no chat model is loaded:

```bash
sovereign daemon status     # confirm daemon is running
curl -s http://localhost:9741/v1/models | jq .   # what's registered
```

If models are registered but not loaded, load one explicitly via the
daemon's slot config or restart the daemon.

**"step `extract` exited with code 1"** — `enrich build` halts when
ANY chapter fails extract, even when the failures are just
"chapter body is too short". The orchestrator auto-recovers from this
by running `cluster + name + resolve` directly. If you see this from
manual `enrich build` invocations, just run those three steps
explicitly.

**"no enrichment config for corpus 'X-self-atlas'"** — the
`atlas-cross-corpus` command requires both atlases to have
`config.json` in `~/.sovereign/enrichment/`. The orchestrator stubs
one for the structural atlas; if you're driving primitives manually,
you can copy the structural atlas's stub from
`~/.sovereign/enrichment/myproject-self-atlas/config.json` (or write
a minimal one yourself).

**Critical findings have no concrete code pointer** — by design.
The orchestrator surfaces the verbatim claim; locating the
implementation is the human's job (or the LLM's, if you ask one to
search the structural atlas with the claim's keywords). A future
version will offer suggested keywords automatically.

**Cross-corpus matched 0 entities** — the matcher does
`canonical_name + alias` matching. Common reasons:

- The narrative atlas is empty (extraction never ran or produced 0
  atoms). Inspect `~/.sovereign/indexes/<id>/atlas/atoms.json`.
- The structural atlas uses qualified names (`crate::module::Foo`)
  while the narrative uses bare names (`Foo`). The matcher does
  some normalisation, but extreme cases miss. Add explicit aliases
  in your narrative (cite `` `crate::module::Foo` `` in backticks
  and the markdown extractor lifts it into `inline_code_spans`).

## Internal: rough edges

The drift report also includes an **Internal** section that
inventories the codebase's own self-marked rough edges:

- `// TODO`, `// FIXME`, `// HACK`, `// XXX` comments — places the
  team has explicitly tagged as not-finished or known-wrong.
- **Rustdoc-vs-signature drift** — places where a function's
  rustdoc claims behaviour the signature/body contradicts:
  - `# Panics` section without any `panic!`, `unwrap()`, `expect()`,
    `assert!`, `unreachable!`, `todo!`, or `unimplemented!()` in
    the body
  - `# Errors` section on a function that doesn't return
    `Result<…>`

The orchestrator runs this scan automatically. Standalone:

```bash
sovereign rough-edges <code-corpus> [--source-path <dir>] [--output ./rough.md]
```

Severity:

- **XXX** → Critical (alarm marker)
- **FIXME**, **HACK** → Likely (known broken / known-bad fix)
- **TODO** → Note (forward-looking intent)

The digest section is summary-only (counts + 5 examples per kind);
the full per-marker list lives in the rough-edges JSON sidecar at
`<output>.rough.json`.

## Out of scope (for now)

- **Live re-extraction on doc/code edits** — drift is one-shot per
  invocation. Watcher integration is future work.
- **Auto-fix suggestions** — the report names actions but doesn't
  generate diffs.
- **Tier 2/3 internal contradictions** — convergent parallel
  implementations (clustering on structural atoms) and trait-impl
  contract violations (LLM validation per impl). Pending. Tier 0
  (markers) and tier 1 (rustdoc-vs-signature drift) ship in v1.
- **Non-markdown narrative formats** — docx, asciidoc,
  restructuredtext. The extractor surface generalises (each gets its
  own Extractor impl); v1 ships markdown only.

## See also

- `corpus-engine/src/extractors/markdown.rs` — section-aware markdown
  extractor.
- `sovereign-cli/src/enrich_cmd/atlas_drift_report.rs` — digest
  renderer + classifier.
- `sovereign-cli/src/drift_cmd_orchestrator.rs` — single-command
  orchestrator with resilience patterns.
- `sovereign-recipes/_templates/narrative-markdown/` — recipe
  template + `README.md` on which docs to ingest.
- `corpus-engine/src/rough_edges.rs` — marker scanner core.
- `sovereign-cli/src/rough_edges_cmd.rs` — `sovereign rough-edges`
  CLI wrapper.
