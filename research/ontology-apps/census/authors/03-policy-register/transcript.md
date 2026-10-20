# Interview transcript: company policy register

The partner is not available. Each answer is sourced as `description` (quoted from
DESCRIPTION.md) or `assumption` (general knowledge of how companies keep policies).

Templates looked at: `sovereign recipe new --ontology governance`, `contracts`,
`engineering-org` (scaffolded to `template-*.toml` in this directory for reading only;
not part of the recipe). `governance` is the closest: rules as directive claims,
superseded over time.

## 0. Source shape

**Q.** What files is the material, and in what format?

**A.** Three kinds of document: policy memos as issued, leadership-meeting minutes, and
announcement emails. Source: description, "the policy memos as issued, the
leadership-meeting minutes where policies are approved, amended or retired, and the
announcement emails".

Format and layout are not stated. Assumption: one folder of mixed office documents
(memos and minutes as PDF/DOCX, emails saved as files), so `acquire.type = "local_file"`,
`path = "SOURCE_PATH"`, `extract.type = "described_asset"` (the mixed-folder dispatcher),
`chunk.type = "paragraph"`. Because the metadata fields this extractor writes are not
known without `validate --corpus` on the real folder, no `change.document` field is
declared (the skill forbids guessing field names). Source: assumption.

## 1. Shape

**Q.** What is this material about, and what would you want to know about each kind of
thing? Is any of them a kind of another, or a part something plays? Do you already have
any of it in a table?

**A.**
- Policies: the standing thing. Want its owner and the area it governs. Source:
  description, "Every policy, with its owner and the area it governs" and "our internal
  policies (travel, security, leave, expenses and so on)". Declared `policy` (entity)
  with `title`, `code`, `owner`, `area`.
- Area is open ("and so on"), so open text, not a closed set. Source: description
  ("and so on"), the choice of open text is an assumption.
- Owner is recorded as the role, department or person named as owner, as written (open
  text, not a ref). Source: assumption. Companies usually name a role/department
  (Head of People, Finance) rather than a person; a text field avoids a second
  RESOLVE-decided type that no claim would be about.
- Clauses: policies are made of numbered clauses; amendments change clauses and rules
  come from clauses. Source: description, "the clause before and after" and "with the
  clause it comes from". Declared `clause` (entity) with `policy` (ref policy),
  `number`, `heading`.
- Versions: a policy passes through versions. Source: description, "Which version of
  each policy is in force". Not declared as its own entity; carried as a `version`
  attribute on the decisions, amendments and rules, with an `effective` date. Source:
  assumption (a version is fully described by the decision that put it in force).
- No sub-kinds or roles. No existing table: description says "a wiki page lists
  'current policies' and is usually out of date", which is not in the material and is
  not a source. Source: description + assumption.

## 2. Assertion

**Q.** What do the sources say about those things (stating, requiring, deciding,
asking)? About what? What does each statement carry? How do you tell strong evidence
from weak? Who is speaking? What must it never do?

**A.**
- Memos require, forbid or permit: "Every rule a policy imposes (who must or must not
  do what) with the clause it comes from". Source: description. Declared `rule`
  (claim, `force = "directive"`, `deontic = [require, forbid, permit]`,
  `subject = "clause"`) with `bearer` (who), `conditions`, `exceptions`, `effective`,
  `version`. `permit` added as an assumption (policies commonly grant permissions,
  e.g. "may book business class over 6 hours").
- Minutes decide: "policies are approved, amended or retired"; and the needs ask "When
  and by whom". Source: description. Declared `policy_decision` (claim,
  `force = "declaration"`, `subject = "policy"`) with `action` closed set
  [issued, approved, amended, retired], `decided_by`, `decided_on`, `effective`,
  `version`, `replaced_by` (ref policy), `minute_ref`. "issued" added for the memo's
  own issue (description: "memos as issued").
- Amendments change clauses: "What each amendment changed (the clause before and
  after)". Source: description. Declared `clause_change` (claim, `declaration`,
  `subject = "clause"`) with `change` [added, reworded, removed], `before`, `after`,
  `effective`, `version`.
- Evidence strength: minuted decision > issued memo > announcement email (an email
  paraphrases). Source: assumption. Declared as `grades` on each claim type.
- Speakers: the leadership meeting, the policy owner (memo), the announcer (email).
  Source: assumption. First declared as `voices.attributed_to`; removed after
  validate run 1 (see below) because the document reader ignores it.
- Must never: present a proposed/draft/deferred policy as in force; act as the owner's
  ruling on an individual case. Source: assumption.

## 3. Identity

**Q.** How do you know two mentions are the same thing? Is there an ID?

**A.**
- Policy: by its reference code when it has one, else its title; a revised version is
  the same policy, a replacement policy is a different one. Source: assumption (many
  companies number policies, e.g. HR-04; the description does not say). Declared
  `identity = ["code"]`, `identity_fallback = ["title"]`, plus an `identity_criterion`.
- Clause: same number of the same policy; keeps identity through rewording; follows
  heading/subject through renumbering. Source: assumption. Declared
  `identity_fallback = ["policy", "number"]` and an `identity_criterion`.

## 4. Change

**Q.** When does a later statement replace an earlier one, and from when? Does anything
move through stages? Which document fields give date and thread?

**A.**
- A later decision on a policy replaces the earlier one from its effective date; a
  later wording of a clause replaces the earlier one; a later rule from the same clause
  replaces the earlier one. Source: description, "Which version of each policy is in
  force now, and which was in force on a given past date" (needs effective-date
  ordering, not report order) + assumption on clause granularity. Declared
  `change.supersedes = { policy_decision = "effective", clause_change = "effective",
  rule = "effective" }`.
- Stages: proposed -> approved/issued -> amended -> retired. Source: description
  ("approved, amended or retired") + assumption ("proposed"). Not declared as a
  `by = "protocol"` fold: the protocol fold orders moves by `change.document.date`
  (report time), and no document date field can be named without
  `validate --corpus` on the real files; an undated input stays pending, so the fold
  would never move. Current status is instead read from the in-force (latest effective,
  unsuperseded) `policy_decision`: `action = "retired"` means retired. Source:
  assumption / engine constraint.
- Document fields: unknown for `described_asset`; none declared. Source: assumption.
- Owner: `by = "most"` (first drafted `latest`; changed because `latest` orders by the
  document-date clock, which nothing stamps here). Source: assumption.

## 5. Derivation

**Q.** What should it notice that no single document says? What looks like a
contradiction but isn't?

**A.**
- The current set of policies in force (the thing the wiki fails at) is derived from
  supersession over decisions, not stated anywhere. Source: description, "Today a wiki
  page lists 'current policies' and is usually out of date".
- Discrepancies: an announcement email stating a clause differently from the memo; two
  sources giving different dates or deciders for the same decision; two wordings of a
  clause in force at the same time. Source: assumption. Declared `tension.between =
  ["rule", "clause_change", "policy_decision"]`, label "discrepancy", default
  comparability (same subject, same clock).
- Not conflicts: an amendment replacing earlier wording; rules for different staff
  groups; an exception granted by the clause; an email summary that does not change
  the requirement; an unadopted proposal. Source: assumption.

## 6. Questions

**Q.** Once it is read, what will you ask of it?

**A.** The six needs in the description, walked to the declaration (source: description):
1. Every policy, owner, area -> `policy` enumeration; `policy.owner`, `policy.area`.
2. Version in force now / on a past date -> `policy_decision` superseded on `effective`;
   its `version`, `action`; clause wording at a date from `clause_change` superseded on
   `effective`.
3. When and by whom approved/amended/retired -> `policy_decision.action`, `decided_on`,
   `effective`, `decided_by`, `minute_ref`.
4. What each amendment changed -> `clause_change.before` / `after` / `change` on a
   `clause`.
5. Which policy replaced a retired one -> `policy_decision.replaced_by` (ref policy) on
   the `action = "retired"` decision.
6. Every rule with its clause -> `rule` (deontic require/forbid/permit, `bearer`,
   `conditions`) whose subject is a `clause`, whose `policy` ref gives the policy.
Extra, assumption: "Which rules bind contractors?" -> `rule.bearer`. Every question
lands in a declared type; none required a new kind.

## Validate runs

### Run 1: `sovereign recipe validate recipe.toml` (exit 0)

```
Validating recipe: recipe.toml
✓ Validation passed

Derived from your declarations:
  clock: document_date — supersession folds on document dates (the default; set `change.clock = "narrative"` for order within a work)
  tension selector: embedding top-k (k = 10, floor = 0.5) over rule, clause_change, policy_decision — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: policy → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: clause → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  question shapes: enumerate [policy, clause]; relations [(none)]; events [(none)]; aggregate [policy_decision by replaced_by]
  ⚠  `voices` (attributed_to) are applied by the general extractor only; the document reader attributes no statement to a speaker.
```

Reading the facets against the answers:
- The warning: `voices` does nothing under the document reader this declaration
  selects. Fix: removed the `[enrichment.ontology.voices]` block (speaker strength is
  already carried by `grades`).
- `clock: document_date` is the block default; nothing stamps document dates (no
  `change.document`). The supersession I need is per type on `effective`, which is
  declared. But `policy.owner` used `by = "latest"`, which folds on the document-date
  clock and so would never fold. Fix: `by = "most"`.
- Identity: both RESOLVE-decided under their criteria, as intended.
- Question shapes: enumerate policy and clause; aggregate decisions by `replaced_by`
  (need 5). Matches.

### Run 2: `sovereign recipe validate recipe.toml` (exit 0)

```
Validating recipe: recipe.toml
✓ Validation passed

Derived from your declarations:
  clock: document_date — supersession folds on document dates (the default; set `change.clock = "narrative"` for order within a work)
  tension selector: embedding top-k (k = 10, floor = 0.5) over rule, clause_change, policy_decision — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: policy → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: clause → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  question shapes: enumerate [policy, clause]; relations [(none)]; events [(none)]; aggregate [policy_decision by replaced_by]
```

No warnings. The remaining facets match the answers. Not run: `--corpus` (no material
available), so no `fill:` notes or metadata field list were seen.

## Summary

Types and identity:
- `policy` (entity): `identity = ["code"]`, fallback `["title"]`, criterion "same
  standing policy across versions; a replacement is a different policy".
- `clause` (entity): fallback `["policy", "number"]`, criterion "same numbered clause of
  the same policy, through rewording and renumbering".
- `rule` (claim, directive, require/forbid/permit, subject clause).
- `policy_decision` (claim, declaration, subject policy).
- `clause_change` (claim, declaration, subject clause).

Least sure:
- The extractor (`described_asset`) and therefore no document-date metadata; a
  protocol status fold and `by = "latest"` were given up because of it.
- Clause as a RESOLVE-decided entity: memos without numbered clauses would make
  clause identity weak.
- Policies having a reference code.
- Owner as open text, `by = "most"` (a changed owner may be outvoted by older
  mentions).
- Supersession of `rule` per clause on `effective`: a clause imposing two rules has them
  share an effective date, so neither should retire the other, but this is not verified.
