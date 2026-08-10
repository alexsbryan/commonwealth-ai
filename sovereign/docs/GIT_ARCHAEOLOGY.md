# Git Archaeology — temporal grounding for atlas atoms

> Walk the code's git history once and attach **provenance**,
> **co-evolution**, and **staleness** to every atom in the
> structural atlas. Mechanical, cheap, no LLM.

## What this is

The structural atlas tells you *what* exists. Git archaeology
tells you *when it was introduced, how stable it has been, who
shaped it, and what changes alongside it*. It runs as a separate
pass after the structural atlas is built and writes a JSON sidecar
the drift report folds in.

Every atom that anchors to a code chunk gets a per-atom block:

```json
{
  "atom_id": "entity-0019",
  "file_path": "crates/sovereign-cli/src/main.rs",
  "first_seen":   { "hash": "…", "date_iso": "2026-04-20", "author_email": "…", "subject": "init: cli entry" },
  "last_modified":{ "hash": "…", "date_iso": "2026-05-07", "author_email": "…", "subject": "atos tune" },
  "stability_days": 17,
  "modification_count": 44,
  "primary_authors": ["maintainer@example.com", "maintainer@example.com"],
  "staleness": "fresh"
}
```

Plus, alongside the per-atom block, a list of **co-evolution
pairs** — files that change together more than the threshold:

```json
{
  "file_a": "crates/sovereign-tools/src/code/callees.rs",
  "file_b": "crates/sovereign-tools/src/code/callers.rs",
  "joint_commits": 7,
  "a_only": 0,
  "b_only": 0,
  "correlation": 1.0
}
```

## When this pays off

Worth running on **any code corpus you've indexed and atlased**.
The cost is small (one `git log --name-only` over the whole repo
+ a HashMap join) and the output enables three operationally
useful questions:

- **"What's load-bearing?"** — atoms with high `stability_days`
  and low `modification_count`. The architectural commitments
  that have held.
- **"What's actively evolving?"** — atoms with recent
  `last_modified` dates. The currently-changing surfaces.
- **"What couples implicitly?"** — co-evolution pairs above the
  threshold. Files the code's syntactic structure doesn't link
  but git proves are linked anyway.

Does **not** pay off for:

- Wikipedia-style corpora — atoms there don't carry `file_path`
  in their chunk metadata, so archaeology silently skips them.
- Code that lives outside a git repo — `discover_repo_root`
  errors out and the command exits cleanly with a hint.
- Repos with no commit history (fresh `git init` only) — every
  atom gets `stability_days: 0`. Not wrong, just uninformative.

## Quick start (one command)

```bash
sovereign git-archaeology <atlas-corpus-id>
```

`<atlas-corpus-id>` is typically `<id>-self-atlas` — the corpus
where the structural atlas's `atoms.json` lives. The source
corpus (where chunks live) is inferred by stripping
`-self-atlas`. Override either with `--source-corpus <id>` or
`--source-path <dir>`.

Default outputs:

- **JSON sidecar**: `~/.svrnmesh/indexes/<atlas>/atlas/git_archaeology.json`
- **Markdown digest**: stdout (or `--output <path>` to write a file)

Wall time on the sovereign self-atlas (~1,900 atoms,
~10,000 commits): under 5 seconds.

## Composable primitives (power-user path)

Defaults work for the canonical case. When you need to fine-tune:

```bash
# Custom thresholds
sovereign git-archaeology sovereign-self-atlas \
    --threshold 0.7 \                    # jaccard floor for co-evolution
    --min-joint 10 \                     # minimum joint commits
    --output /tmp/arch.md

# Run against a separate code corpus
sovereign git-archaeology my-project-self-atlas \
    --source-corpus my-project \
    --source-path /path/to/checkout

# Inside the drift orchestrator (Step 3.5, automatic)
sovereign drift detect --code /path/to/repo --narrative DOC.md
```

## Reading the report

The markdown digest has four sections:

### Stability highlights
The 10 most-stable atoms (highest `stability_days` among `Fresh`
ones). These are the load-bearing surfaces — the things engineers
should be most cautious about when modifying.

### Recent volatility
The 10 most-recently-modified atoms. These are the active edges —
where engineering attention is currently directed.

### Co-evolution clusters
The 10 highest-correlation file pairs above the threshold. Read
this as: *"if you modify A, you'll probably also need to modify
B."* Implicit coupling the call graph doesn't reveal.

### Staleness queue
Atoms anchored to code that has changed since the atlas was
built. These are the candidates for re-extraction or LLM
re-validation. Empty when archaeology runs immediately after the
atlas build (the canonical pipeline).

The full per-atom and per-pair detail lives in the JSON sidecar
at `~/.svrnmesh/indexes/<atlas>/atlas/git_archaeology.json` for
downstream consumers.

## Severity rules

Every atom is one of two staleness states:

- **Fresh** — file hasn't been touched since the atlas was built.
  The atom's extraction is presumed-current.
- **Moved** — at least one commit has touched the file since the
  atlas's `atoms.json` mtime. The atom needs re-validation. The
  drift report's Investigation queue picks these up under
  *"source moved since extraction."*

Co-evolution pairs key on **jaccard correlation × min joint
commits**. The defaults (0.5, 5) drop scaffolding-era false
positives where two files were edited once together and never
again.

## Troubleshooting

**"is not a git repository"** — `discover_repo_root` walked from
`source_path` and `git rev-parse --show-toplevel` failed. Either
the corpus was indexed from a non-versioned tree (rare) or the
working tree is corrupted. Pass `--source-path /known/repo`.

**"no chunk index for corpus"** — the source corpus has no
`_corpus_meta.json`. The bare `~/.svrnmesh/indexes/<id>/`
directory holds the SCIP graph DB but not the chunks.lance.
Re-index: `sovereign code index <path> --corpus-id <id>`.

**"0 atoms enriched (N skipped: no path)"** — every atom in the
atlas anchors to a chunk whose `metadata_raw` lacks `file_path`.
This is the Wikipedia-shaped corpus case; archaeology is
code-only by design.

**"0 co-evolution pairs"** — likely the repo is too young
(`min_joint_commits` not met) or too monolithic (every commit
touches the same files). Drop `--min-joint 2` to inspect, but
expect noise.

**Authors look duplicated** (`alice@laptop.local`,
`alice@example.com`, `alice@phone`) — v1 ships raw author email.
Mailmap-aware normalization is v2 work alongside Person-Knowledge
Locus atoms.

## Out of scope (for now)

- **Renames** — `git log --follow` doesn't compose with
  `--name-only` over multi-file batch. Files moved across history
  surface as two distinct atoms with disjoint provenance.
  `follows_renames: false` is stamped on the sidecar so consumers
  know.
- **Per-symbol provenance** — v1 keys archaeology by `file_path`.
  Symbol-level granularity (e.g., "this function was first
  introduced in commit X even though the file existed earlier")
  needs blame-aware walk and is v2 territory.
- **Person-Knowledge Locus atoms** — statistical aggregation of
  expertise per (person × component). The data is already in the
  walker's output; promoting it to a first-class atom type with
  active/dormant distinction is v2.
- **Lineage atoms** — multi-commit narrative reconstructions
  ("this decision came to be via commits X, Y, Z; PR #1247
  contains the reasoning"). v2 — needs new envelope variant +
  LLM enrichment + witness-check extension in
  [archaeology-eval](./ARCHAEOLOGY_EVAL.md).
- **Substantive-commit-as-narrative-corpus** — treating PR
  descriptions and load-bearing commit messages as narrative
  atoms in the cross-corpus drift matcher. v2.
- **Branch-aware archaeology** — `--all` is the v1 default. A
  future flag will scope to a branch or to commits since a
  reference (e.g., release tag).

## See also

- [`docs/DRIFT_DETECTION.md`](./DRIFT_DETECTION.md) —
  narrative-vs-code drift; archaeology slots in as its
  Provenance & Evolution section.
- [`docs/ARCHAEOLOGY_EVAL.md`](./ARCHAEOLOGY_EVAL.md) — how to
  measure that archaeology output is actually correct + how to
  run a regression suite of inquiries.
- `corpus-engine/src/git_archaeology.rs` — walker + enrichment +
  co-evolution core.
- `crates/sovereign-cli/src/git_archaeology_cmd.rs` — CLI
  wrapper, atom↔chunk↔path join.
- `crates/sovereign-cli/src/drift_cmd_orchestrator.rs` — Step 3.5
  insertion point that fires archaeology automatically inside
  `sovereign drift detect`.
