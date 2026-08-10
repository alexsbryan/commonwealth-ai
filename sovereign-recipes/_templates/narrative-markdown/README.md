# Narrative-markdown recipe template

Use this template to ingest a stable team-authored markdown document
into an atlas. The output supports drift detection: comparing the
team's stated architecture against the structural reality of the code.

## When to use this

Atlas treatment pays off for **stable, deliberate artifacts**:

- `CHARTER.md` — team values, governance, scope
- `ARCH_PRINCIPLES.md` — architectural rules + rationale
- `SYSTEM_OVERVIEW.md` — component inventory + dataflow
- `ADR/*.md` — accepted architectural decision records
- `.sovereign/features/*/spec.md` — accepted feature specs

It does **not** pay off for volatile docs:

- `README.md` — changes whenever onboarding flow shifts
- `CHANGELOG.md` — append-only, no architectural content
- Auto-generated API docs — already covered by the structural atlas
- In-flight branch documents — atoms decay before they're read

The two-stream framing: stable narrative artifacts get LLM-deep
extraction (this template); the codebase gets cheap structural
extraction (`sovereign code index`); a drift report compares the two.

## Usage

1. Copy this directory to `~/.svrnmesh/recipes/<your-corpus-id>/`.
2. Edit `recipe.toml`: set `[corpus] id`, `[corpus] name`, and
   `[acquire] path`.
3. Run `sovereign enrich init --corpus-id <your-corpus-id>` to ingest
   the markdown into a chunk index.
4. Run `sovereign enrich ingest <your-atlas-id> --strategy
   extraction_first --source-corpus <your-corpus-id>` to produce the
   narrative atlas.
5. Run `sovereign enrich atlas-cross-corpus <your-atlas-id>
   <structural-atlas-id>` to match narrative entities to code.
6. Run `sovereign enrich atlas-drift-report --narrative <your-atlas-id>
   --structural <structural-atlas-id> --output drift.md`.

For the orchestrated single-command path, see
`sovereign drift detect --help`.

## Convention: one atlas per doc

Treat each major narrative document as its own atlas. ARCH_PRINCIPLES
and SYSTEM_OVERVIEW have different atom-shape distributions (rule-shaped
vs map-shaped); separate atlases give the drift report cleaner signal
isolation.

A team with three stable docs runs three copies of this template, gets
three narrative atlases, and the drift command takes them all:

```
sovereign enrich atlas-drift-report \
  --narrative myproject-charter \
  --narrative myproject-arch \
  --narrative myproject-overview \
  --structural myproject-self-atlas \
  --output drift.md
```
