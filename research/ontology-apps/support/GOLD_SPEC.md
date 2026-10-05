# Gold spec: support cases from an issue tracker (v1, 2026-10-04)

The second fit for composed types (ONTOLOGY_PRIMITIVES.md §8; §1.10's support lead). The first,
crm-proof, composes deals from a mailbox, blocked by counterparty. This one composes **cases**
from issue-tracker documents, blocked by product area across many reporters, from a different
source shape (issue JSON, not mail headers). One code path must serve both recipes; only the
recipe differs.

Written before any document was read. Labelled by hand by a model of a different family from the
extractor (the extractor is the local Qwen primary).

## Corpus

Public issues and comments of `astral-sh/uv` (GitHub), fetched read-only with `gh api`. Data and
labels live outside git at `~/.svrnmesh/bench-corpora/uv-support/` (`raw/`, `gold/`); text is the
authors', used for evaluation only, never redistributed.

- **Selection.** Up to 45 issues closed as duplicates, together with their targets (each closed
  issue's "duplicate of #N"), plus the other issues opened in the same weeks, up to about 150
  issues in all.
- **Documents.** Each issue body and each comment is one document, with fields `id` (issue
  number, or the comment id), `thread` (the issue number), `author`, `created_at`, `labels`
  (issue only) and `body`. Bot comments are kept but marked.
- **Timeline events** (closed, reopened, labelled, marked duplicate, cross-referenced, PR merged)
  are kept as their own documents with the same fields. Structured state is a source, not a gap.

## Labels

- **case**: one underlying problem or request, whatever the number of issues reporting it. Fields:
  - `id`;
  - `summary` (one line);
  - `kind` (`bug` | `feature_request` | `question`);
  - `area` (the repository's own area label when one applies, else a short noun);
  - `documents` (every document that states something about this case).
- **membership**: each document belongs to the case or cases it states something about.
  - Most documents have exactly one case.
  - A document that states nothing about any case (thanks, bot noise) has none.
  - A "+1" or "same here" is a member with no state.
- **case_state** (claim on a case): a document that shows where the case stands. Fields:
  - `case`;
  - `state`: `reported` (described by a user); `confirmed` (a maintainer reproduces it or accepts it as valid); `in_progress` (a fix or design is being worked on, or a PR is open); `resolved` (fixed, implemented, answered, or released); `declined` (won't fix, not planned, works as intended, or closed as invalid);
  - `quote` (verbatim, at most 200 characters, or the event's own text);
  - `document`;
  - `date`.
- **Uncertain labels**: mark any label you are unsure of with `"uncertain": "<why>"`.

## Rules

- **Duplicates.** Issues a maintainer closes as duplicates of each other are one case.
- **Shared root cause.** Two issues are one case only when a maintainer says so in either
  thread. Otherwise they are separate cases, even when they look alike.
- **Spin-offs.** A thread can spin off a new case (a comment raising a different problem); its
  documents join that new case.
- **Maintainer links are evidence, not truth.** A "duplicate of" link the thread itself disputes is
  marked uncertain.

## Folds

The cases are split into **tune** and **read**:
- balanced by case count;
- by area, so no area spans both folds;
- no case spans both folds.

Tuning reads tune only. Read is opened once, at the gate.

## Output

`gold/cases.json`:

```json
{"repo": "astral-sh/uv", "fetched_at": "", "documents_read": [],
 "cases": [{"id": "c1", "summary": "", "kind": "bug", "area": "", "documents": [], "fold": "tune"}],
 "case_states": [{"case": "c1", "state": "reported", "quote": "", "document": "", "date": ""}],
 "none": [],
 "uncertain": [{"label": "", "why": ""}]}
```

## Scoring

- **Composition**: B-cubed, CEAF-e and LEA over member documents. Ambiguous documents (several
  cases) are excluded, and their count is reported.
- **State**: per document, the gold state on the gold case's matched atom.
- **Yield**: typed atoms per declared type.

Scored by `sovereign-eval`'s entity-resolution scorer, extended rather than re-written.

## v1.1 — conventions the labellers converged on (2026-10-04)

The spec did not say these; all three batches decided them the same way, and a re-label follows them.

- **Labels.** A maintainer's `bug`, `help wanted` or `good first issue` label is `confirmed`; `wontfix`
  is `declined`; `enhancement` alone is no state.
- **Bare "closed".** The thread decides it, through `state_reason` and the comments around it. A
  reporter's withdrawal is `declined` (marked uncertain), and a close that follows a fix is
  `resolved`. A close reversed within minutes has no state.
- **Merged PRs.** A merged PR is `resolved` only when it does what the case asks. A partial step is
  `in_progress`.
- **"Closed in favour of #N".** This joins the case exactly like a duplicate, even without the label.
  A redirect to several issues makes the document a member of each.
- **Commits in any fork** (not only zaniebot/uv), and cross-references whose title is unrelated, belong
  to no case.
- **Maintainer-filed issues.** Their first description is still `reported`.

Joining: `merge_gold.py` applies the six cross-batch joins the labellers found, each with the
maintainer's words. It then closes folds over threads: three threads moved so that no case spans
both folds.
