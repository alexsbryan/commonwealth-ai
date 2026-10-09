# uv-support — recipe-author transcript

The partner wasn't available, so I answered each question from `DESCRIPTION.md` where it gives an answer
(source: **description**, with the words quoted). Where it doesn't, I answered from general knowledge of
GitHub issue tracking and of uv (source: **assumption**).

Closest shipped template: `product-support` (from `sovereign recipe new --ontology list`). I scaffolded it
for reference only. Its customer/feature/request shape doesn't fit an open-source tracker, so I wrote the
recipe from scratch.

## Source shape (opening question, before the interview)

**Q. What is the material, and in what form does it sit on disk?**
A. The astral-sh/uv GitHub issue tracker: issues, their comments, and maintainer actions. I assume a
local JSONL export with one record per tracker item: the opening post, each comment, and each timeline
event (labeled, assigned, closed, reopened, marked duplicate, cross-referenced by a PR). Each event is
rendered to a sentence in `body`. Every record carries `id`, `issue_number`, `item`, `title`, `body`,
`author`, `author_association` and `created_at`. So: `acquire.type = "local_file"`, `path = "SOURCE_PATH"`,
`extract.type = "jsonl"` (`content_field = "body"`, `title_field = "title"`), and a paragraph chunker.
Source: **description** for the content ("our public issue tracker on GitHub at astral-sh/uv. It holds
the issues people open, the comments on them, and what we as maintainers do with them: label, assign,
link to pull requests, close, reopen, mark as duplicates"). **Assumption** for the file format, the field
names, and that the `jsonl` extractor carries the non-content fields through as document metadata (the
`change.document` stamps and metadata `source`s depend on it).

## 1. Shape

**Q. What is this material about, and what would you want to know about each kind of thing? Is any of
them a kind of another, or a part something plays? Do you already have any of it in a table?**

A. The key distinction is between the *issue* (a report) and the *problem* (the real thing behind one
or more reports). Types:
- `issue` (entity). Attributes: number, title, filed_as (bug / question / feature-request / ...),
  opened. Instances come from the `issue_number` field of each record, with no model call.
- `problem` (entity). Attributes: summary, component, nature (bug, regression, performance,
  platform-specific, upstream, missing-feature, documentation-gap, intended-behaviour, user-error),
  error_signature.
- `component` (entity): the part of uv or the command a problem lives in.
- `package` (entity): a third-party package or index involved.
- `release` (entity): a uv version.
- `platform` (entity): OS / arch / detail.
- `python_build` (entity): implementation / version / provenance.
- `pull_request` (entity).
- `github_user` (entity, from the `author` field), plus the roles `maintainer` and `reporter`, each
  `role_of = "github_user"`.
- Relations: `reports` (issue→problem), `duplicate_of` (issue→issue), `addresses` (PR→problem),
  `shipped_in` (PR→release), `involves` (problem→package).
- Event: `triage_action` (actor = maintainer, on an issue: labeled / assigned / closed / reopened /
  marked-duplicate / linked-pr / ...).
- States: `issue_state` (of issue) and `problem_state` (of problem).
- No tables exist besides the tracker itself.

Source: **description**: "the real problems behind the reports"; "which issues are about the same
thing, what each one actually is, what versions and environments it shows up in"; "crashes, wrong
resolutions, slow installs, platform quirks, questions about how a command should behave, and feature
requests"; "different uv versions, operating systems and Python versions"; "label, assign, link to pull
requests, close, reopen, mark as duplicates". **Assumption** for: the component areas, `package`,
`python_build` provenance, and the maintainer/reporter roles.

## 2. Assertion

**Q. What do the sources say about those things — stating, requiring, deciding, asking? About what? How
do you tell strong evidence from weak? Who is speaking, and are they part of the subject? What must it
never do?**

A. All claims are about a `problem`, except decisions about how a command behaves, which are about a
`component`.
- `diagnosis` (assertive): what the problem is and why. Grades, strongest first: fix-merged,
  maintainer-reproduced, maintainer-assessment, reporter-reproduced, reporter-speculation.
- `occurrence` (assertive, labelled "sighting"): seen or not seen on a given uv version, platform and
  Python. Grades: maintainer-reproduced, reporter-reproduced-minimal, reporter-report, hearsay.
- `resolution` (declaration): fixed / wont-fix / intended-behaviour / upstream / duplicate /
  needs-reproduction / cannot-reproduce / reopened / stale, with the PR and the release it was fixed in.
- `behaviour_decision` (declaration, subject `component`): how a command should behave.
- `feature_request` (directive, request).
- `commitment` (commissive): will-fix / planned / pr-welcome / investigating.

Voices: maintainer, reporter, other-user, bot. Generic speaker references ("the reporter", "the original
poster", bots) never become entities.

must_not:
- Don't call something fixed without a maintainer or a merged PR saying so.
- Don't present a reporter's guess as the maintainers' diagnosis.
- Don't treat a duplicate closure as a resolution.

Source: **description**: "what each one actually is" (→ diagnosis); "what versions and environments it
shows up in" (→ occurrence); "what we decided or shipped about it" (→ resolution, behaviour_decision);
"questions about how a command should behave, and feature requests" (→ behaviour_decision,
feature_request). **Assumption** for: the grade scales, `commitment`, the voices, and the must_not
lines. I did not set `voices.self`: the partner is one maintainer among several, and their comments are
part of the subject matter.

## 3. Identity

**Q. How do you know two mentions are the same thing? Is there an ID?**

A.
- issue: issue number (external key).
- problem: no ID. Two mentions are one problem when one change to uv (or one upstream fix) would resolve
  both, regardless of wording, uv version, OS or Python version. A shared error message alone is not
  enough, and different symptoms traced to one cause are one problem. A maintainer duplicate marking means
  the same problem. This is written as `identity_criterion`, with no descriptive fallback.
- package: PEP 503 normalised name.
- release: version string.
- pull_request: repository + number.
- github_user / maintainer / reporter: login.
- component, platform, python_build: judged on descriptive keys.

Source: **description**: "many report the same underlying problem in different words, often across
different uv versions, operating systems and Python versions"; "mark as duplicates"; "which issues are
about the same thing". **Assumption** for: the one-fix criterion, the error-message caveat, and the
external keys (GitHub/PyPI conventions).

## 4. Change

**Q. When does a later statement replace an earlier one, and from when?**

A. A later `diagnosis` replaces an earlier one for the same problem: a question becomes a bug, or a bug
becomes a misunderstanding. The same goes for `resolution` (closed → reopened → fixed),
`behaviour_decision` and `commitment`. All of these supersede on the comment's date (`created_at` →
`document_date`). The thread is `issue_number` and the document id is the record `id`. `occurrence`
and `feature_request` accumulate rather than supersede. The issue and problem histories (`issue_state`,
`problem_state`, `triage_action`) record how each got where it is.

Source: **description**: "A report can start as a question and turn out to be a bug, or start as a bug
and turn out to be a misunderstanding"; "where it stands now and how it got there". **Assumption** for:
using comment time, not issue-open time, as the clock, and for which claim types accumulate.

## 5. Derivation

**Q. What should it notice that no single document says — contradictions, patterns, larger structures?
What looks like a contradiction but isn't?**

A. Look for tensions among diagnosis, occurrence, resolution and behaviour_decision on the same problem
(`same = ["subject"]`, not the default subject + clock, so a later sighting can contradict an earlier
fix). The main case is a problem declared fixed in release X and then seen again on a later version (a
regression or incomplete fix). Another is two maintainer decisions that disagree about a command.

Not conflicts:
- Different behaviour on different platforms or Python versions.
- Reclassification of a report (question ↔ bug).
- A sighting on a version older than the fix.
- A duplicate closed while the problem stays open.
- A reporter's guess that a maintainer later corrected.

Pattern: `circular_flow` over `duplicate_of` with min 2 (issues marked duplicates of each other).

The larger structure the user asked for (problem ← many issues) is carried by the `reports` relation
plus the problem identity criterion.

Source: **description**: "which issues are about the same thing ... where it stands now and how it got
there, and what we decided or shipped about it". **Assumption** for: the regression tension, the
not_conflicts list, and the duplicate-cycle pattern.

## Validation runs

### Run 1: `sovereign recipe validate recipe.toml`
```
✓ Validation passed
Derived from your declarations:
  clock: document_date — supersession folds on document dates (the default; ...)
  tension selector: embedding top-k (k = 10, floor = 0.5) over diagnosis, occurrence, resolution, behaviour_decision — ...
  source: issue ← document fields issue_number (number: value) — one atom per identity value, ...
  identity: issue → number (external key, strict merge)
  identity: problem → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: component → area + command (descriptive keys, judged merge)
  identity: package → normalized_name (external key, strict merge)
  identity: release → version (external key, strict merge)
  identity: pull_request → repository + number (external key, strict merge)
  source: github_user ← document fields author (login: value) — ...
  identity: github_user → login (external key, strict merge)
  identity: maintainer → canonical name (default; declare `identity = [...]` for an external key)
  identity: reporter → canonical name (default; declare `identity = [...]` for an external key)
  question shapes: enumerate [issue, problem, component, package, release, pull_request, github_user, maintainer, reporter]; relations [reports, duplicate_of, addresses, shipped_in, involves]; events [triage_action]; aggregate [diagnosis by component, diagnosis by issue, occurrence by issue, occurrence by uv_version, resolution by pull_request, resolution by fixed_in, resolution by issue, behaviour_decision by issue, behaviour_decision by shipped_in, feature_request by issue, commitment by target_release]
  document fields: document_date ← `created_at` ..., document_thread ← `issue_number`, document_id ← `id` — ...
```
Reading this against the answers:
- Clock, tension types, the issue/github_user sources and the document stamps match Change and
  Derivation.
- **Wrong:** `maintainer` and `reporter` resolved to "canonical name". Per Identity they are GitHub
  logins. Fix: declare a `login` attribute and `identity = ["login"]` on both.
- **Inconsistent:** `problem` carried `identity_fallback = ["component", "error_signature"]`, which
  contradicts the criterion's "same error message alone is not enough". Fix: remove the fallback, so
  RESOLVE decides on the criterion alone.

### Run 2: after the identity fixes
```
✓ Validation passed
  ...
  identity: maintainer → login (external key, strict merge)
  identity: reporter → login (external key, strict merge)
  question shapes: ... aggregate [..., occurrence by issue, occurrence by uv_version, ...]
```
Identity now matches. **Wrong against Shape/Assertion:** aggregates come only from `ref` attributes, and
OS, arch and Python version were plain text on `occurrence`. So "what environments does this problem
show up in" had no aggregate. Fix: declare `platform` (os, arch, os_detail) and `python_build`
(implementation, version, provenance) entities, and make `occurrence.platform` and `occurrence.python`
refs to them.

### Run 3: after adding platform / python_build
```
✓ Validation passed

Derived from your declarations:
  clock: document_date — supersession folds on document dates (the default; set `change.clock = "narrative"` for order within a work)
  tension selector: embedding top-k (k = 10, floor = 0.5) over diagnosis, occurrence, resolution, behaviour_decision — cross-document declared corpora select the embedding net; the classifier judges each pair
  source: issue ← document fields issue_number (number: value) — one atom per identity value, model atoms with that value merge into it; 0 value(s) excluded
  identity: issue → number (external key, strict merge)
  identity: problem → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: component → area + command (descriptive keys, judged merge)
  identity: package → normalized_name (external key, strict merge)
  identity: release → version (external key, strict merge)
  identity: platform → os + arch + os_detail (descriptive keys, judged merge)
  identity: python_build → implementation + version + provenance (descriptive keys, judged merge)
  identity: pull_request → repository + number (external key, strict merge)
  source: github_user ← document fields author (login: value) — one atom per identity value, model atoms with that value merge into it; 0 value(s) excluded
  identity: github_user → login (external key, strict merge)
  identity: maintainer → login (external key, strict merge)
  identity: reporter → login (external key, strict merge)
  question shapes: enumerate [issue, problem, component, package, release, platform, python_build, pull_request, github_user, maintainer, reporter]; relations [reports, duplicate_of, addresses, shipped_in, involves]; events [triage_action]; aggregate [diagnosis by component, diagnosis by issue, occurrence by issue, occurrence by uv_version, occurrence by platform, occurrence by python, resolution by pull_request, resolution by fixed_in, resolution by issue, behaviour_decision by issue, behaviour_decision by shipped_in, feature_request by issue, commitment by target_release]
  document fields: document_date ← `created_at` (RFC 2822 or ISO 8601, written as ISO 8601), document_thread ← `issue_number`, document_id ← `id` — stamped on each claim from the one document its evidence lands in; a claim in none or several is left unstamped and counted
```
Every derived facet now matches an answer above. I didn't change anything further.

Notes:
- The validator doesn't print `tension.same` or the `patterns` block, so I couldn't check those against
  the output.
- `scaffold-product-support.toml` in this directory is the reference template I generated with
  `sovereign recipe new`. My attempt to delete it was blocked by the permission system, so it is still
  there.
