# Ontology declaration v1 — ten users, five axes

Status: **design proposal, fourth iteration — version 1 of a versioned
surface** (2026-09-01). Follows
[`CUSTOM_ONTOLOGIES_AS_BUILT.md`](CUSTOM_ONTOLOGIES_AS_BUILT.md), which
established what the custom path does today and why "declare types,
generate the schema" is the move (its M1 and M2). Iteration one derived
a flat list of primitives from ten users. Iteration two answered a
review that named the positions the draft took. This iteration makes
each primitive earn its keep, and finds that they are not fifteen peers
but facets of five axes. Nothing here is built.

## 0. Positions this design takes

Four choices shape everything below; each has a classic alternative.

**Things are basic.** Entities are the re-identifiable particulars that
claims, states, events and relations point at (Aristotle; Strawson's
*Individuals*). Facts-first or process-first (the *Tractatus*,
Whitehead) is closer to how the hardest users strain the system, so
events and states are full subjects of claims, but retrieval lives on
re-identification and things stay basic.

**Kinds are ontology; types are ideology.** The eleven atom kinds and
fourteen edge types are what retrieval plans quantify over, so they are
the real commitment (Quine). Declared types are predicates over them.
Kinds stay closed; types stay open; and the kinds face the same
ten-user test as everything else (section 3).

**Categories are chosen for expedience, only when someone would write
them** (Carnap's frameworks; Thomasson). A facet none of the ten users
would write does not land, and a facet the system can infer is not
declared.

**A claim is content plus force, with a truthmaker.** Discourse about
the domain is part of the domain, answered by separating what a claim is
about from what the speaker does with it (Frege; Searle), and by
requiring every claim to carry an evidence anchor (Armstrong).

The rule that follows: **kinds are fixed, types are declared.** A
user's `coin`, `rule`, `symptom` or `request` specializes one kind,
carries declared attributes, and is generated into the extraction
schema. Every measured retrieval consumer keys on kinds and universal
fields (as-built §0.5), so nothing downstream learns the user's nouns.

### 0.1 Versioned, not final — the axes are the interface

This is not the last design of this surface, so the surface carries
its version: `[enrichment.ontology] version = 1` names the declaration
language below. Absent means version 0, today's prose `guidance`
block, which keeps working unchanged for as long as any recipe uses
it.

What makes a new version cheap is that the pipeline never reads the
version. It reads five policy structs, one per axis, and a version is
nothing more than a parser from TOML to those structs:

```rust
pub struct OntologyPolicies {
    pub shape:      ShapePolicy,      // declared types, attributes, subtypes, roles, endpoints, sources, labels
    pub assertion:  AssertionPolicy,  // claim types: subject, force, deontic, grades, anchors, scope; voices; must_not
    pub identity:   IdentityPolicy,   // per-type keys, fallbacks, merge policy
    pub change:     ChangePolicy,     // clock, supersedes
    pub derivation: DerivationPolicy, // tension, patterns, configurations, arguments
    pub prose:      ProsePolicy,      // guidance
}

pub trait OntologyLanguage {          // one impl per version, in a registry keyed by the integer
    fn version(&self) -> u32;
    fn parse(&self, block: &toml::Table) -> Result<OntologyPolicies>;
    fn schema_doc(&self) -> &'static str;   // the SCHEMA.md section
}
```

Version 0 fills `prose` and a `label` or two and leaves every other
policy at its default, which is exactly today's behaviour. Version 1
fills all six. The composer, parser, resolver, tension selector,
supersession fold, build report and inspector consume
`OntologyPolicies` and nothing else. A version 2 is a new
`OntologyLanguage` impl and, at most, an additive field on a policy
struct with a default — the same discipline the atom envelopes already
live under. Agree on the interface, delegate the syntax.

Three version numbers now exist in a recipe and bump for different
reasons:

| Field | Bumps when | Owner |
|---|---|---|
| `[corpus] schema_version` | the recipe container changes shape and readers must opt in | `recipe.rs` |
| `[enrichment.ontology] version` | the declaration language changes: a new axis, a facet's meaning, a value family | an `OntologyLanguage` impl and its `SCHEMA.md` section |
| `[enrichment] prompt_version` | prompt text changes for the same declarations | the genre's prompt assets |

What a version freezes: the facet names and meanings, the four value
families, the defaults for derived facets, and the mapping to policy
structs. What it does not freeze: prompt wording, the model,
thresholds — those move under `prompt_version` and the ledger. Readers
keep every shipped version; writers (the interview, the scaffold) emit
the latest; `svrn recipe migrate` rewrites a recipe from one version
to the next as a reviewable diff, never silently. An atlas records the
policies that produced it, so a viewer applies the right rules to what
it is showing without knowing the version either.

The examples in section 1 all carry `version = 1`; it is shown in the
first two and elided in the rest.

## 1. Ten users

Each entry: who, what they have, three questions they ask, and the
declaration they would write on the proposed surface.

### 1.1 The numismatist

Scanned catalogues, hoard reports, journal articles. Asks: which mints
struck for Offa; what is the weight range of the Series E sceattas; who
disputes the 780s dating of the light coinage and on what grounds.

```toml
[enrichment.ontology]
version = 1

[[enrichment.ontology.types]]
name = "coin"
kind = "entity"
description = "A coin type: ruler, mint, denomination, metal."
attributes = [
  { name = "ruler",        type = "ref",      of = "ruler" },
  { name = "mint",         type = "ref",      of = "mint" },
  { name = "denomination", type = "text" },
  { name = "metal",        type = "text",     values = ["gold", "silver", "billon", "copper"] },
  { name = "weight",       type = "quantity", unit = "g" },
  { name = "struck",       type = "time",     range = true },
]

[[enrichment.ontology.types]]
name = "sceatta"
kind = "entity"
specializes = "coin"

[[enrichment.ontology.types]]
name = "ruler"
kind = "entity"
role_of = "person"

[[enrichment.ontology.types]]
name = "mint"
kind = "entity"

[[enrichment.ontology.types]]
name = "attribution"
kind = "claim"
force = "assertive"
subject = "coin"
attributes = [{ name = "proposed_date", type = "time", range = true }]
grades = ["die-link", "hoard-context", "stylistic", "metrological"]

[enrichment.ontology.tension]
between = ["attribution"]
```

### 1.2 The co-housing steward

A charter, six years of minutes, a pinned-messages channel. Asks: what
is the guest rule today; which charter articles have been amended and
by which decision; which rules contradict each other. **Built**
(`maple-house`, `proxy-company`).

```toml
[enrichment.ontology]
version = 1
guidance = """The governing rules of a community: founding documents plus dated decisions ..."""

[[enrichment.ontology.types]]
name = "topic"
kind = "entity"

[[enrichment.ontology.types]]
name = "rule"
kind = "claim"
label = "rule"
force = "directive"
deontic = ["require", "forbid", "permit"]
subject = "topic"
attributes = [{ name = "conditions", type = "text" }, { name = "valid", type = "time", range = true }]

[enrichment.ontology.change]
supersedes = { rule = "valid" }

[enrichment.ontology.tension]
label = "conflict"
between = ["rule"]
same = ["subject", "valid"]
not_conflicts = [
  "two separate exemptions to the same rule",
  "a rule for visitors versus a rule for members",
  "rules governing different places or resources",
  "an additive rule layered on another",
]
```

### 1.3 The rare-disease patient community

A moderated forum plus the twenty papers everyone cites. Asks: what has
helped people with the joint symptoms; what should I not combine with
the standard treatment; what does the evidence say versus what members
report.

```toml
[[enrichment.ontology.types]]
name = "treatment"
kind = "entity"
attributes = [{ name = "dose", type = "quantity", unit = "mg" }]

[[enrichment.ontology.types]]
name = "drug"
kind = "entity"
specializes = "treatment"
identity = ["rxnorm_id"]

[[enrichment.ontology.types]]
name = "affects"
kind = "relation"
from = "treatment"
to = "symptom"
attributes = [{ name = "effect", type = "text", values = ["improves", "worsens", "contraindicated"] }]

[[enrichment.ontology.types]]
name = "finding"
kind = "claim"
force = "assertive"
grades = ["trial", "case-series", "member-report"]

[enrichment.ontology]
must_not = ["give dosing advice", "present a member report as a trial result"]

[enrichment.ontology.voices]
not_entities = ["the poster", "the moderator"]
attributed_to = ["paper", "clinician", "member"]
```

### 1.4 The contracts practice

Executed agreements and the case law they rely on. Asks: which
agreements carry a change-of-control clause; what does "Affiliate" mean
in this agreement; when does the Acme non-compete expire; which
authority has been distinguished since.

```toml
[[enrichment.ontology.types]]
name = "party"
kind = "entity"
role_of = "organization"

[[enrichment.ontology.types]]
name = "defined_term"
kind = "entity"
attributes = [{ name = "definition", type = "text" }, { name = "agreement", type = "ref", of = "agreement" }]

[[enrichment.ontology.types]]
name = "obligation"
kind = "claim"
force = "directive"
deontic = ["require", "forbid", "permit"]
subject = "party"
attributes = [{ name = "valid", type = "time", range = true }, { name = "deadline", type = "time" }]

[[enrichment.ontology.types]]
name = "cites"
kind = "relation"
from = "case"
to = "case"
source = { file = "courtlistener_citations.csv", from = "citing_id", to = "cited_id", attributes = { treatment = "treatment" } }

[enrichment.ontology.change]
supersedes = { obligation = "valid" }
```

### 1.5 The engineering org

Design docs, ADRs, incident reviews, runbooks. Asks: which ADR decided
against gRPC internally, and is it still in force; what depends on the
billing service; which incidents were caused by a config change.

```toml
[[enrichment.ontology.types]]
name = "component"
kind = "entity"
identity = ["path"]
attributes = [{ name = "path", type = "text" }, { name = "owner", type = "ref", of = "team" }]

[[enrichment.ontology.types]]
name = "service"
kind = "entity"
specializes = "component"

[[enrichment.ontology.types]]
name = "incident"
kind = "event"
attributes = [{ name = "severity", type = "text", values = ["sev1", "sev2", "sev3"] }, { name = "opened", type = "time" }]

[[enrichment.ontology.types]]
name = "decision"
kind = "claim"
force = "declaration"
subject = "component"

[[enrichment.ontology.types]]
name = "root_cause"
kind = "claim"
force = "assertive"
subject = "incident"

[enrichment.ontology.change]
supersedes = { decision = "document_date" }

[enrichment.ontology.tension]
between = ["decision"]
```

### 1.6 The due-diligence investigator

Filings, leaked spreadsheets, press, a registry export. Asks: who sits
on both boards; where does money flow in a circle; which counterparties
exceed ten percent of revenue. **Built** as the investigation path.

```toml
[[enrichment.ontology.types]]
name = "organization"
kind = "entity"
identity = ["registry_id"]
identity_fallback = ["name", "jurisdiction"]

[[enrichment.ontology.types]]
name = "person"
kind = "entity"
identity_fallback = ["name", "employer"]

[[enrichment.ontology.types]]
name = "counterparty"
kind = "entity"
role_of = "organization"

[[enrichment.ontology.types]]
name = "payment"
kind = "event"
participants = { from = "organization", to = "organization" }
attributes = [{ name = "amount", type = "quantity", unit = "USD" }, { name = "date", type = "time" }]

[[enrichment.ontology.patterns]]
type = "circular_flow"
edge_types = ["payment"]
min_entities = 3
```

### 1.7 The literary scholar

A novel, its drafts, the critical literature. Asks: how does Alyosha's
faith change across the book; which scenes show the brothers as a
triad; what do critics disagree about in the Grand Inquisitor chapter.
**Built** as the literary base.

```toml
[[enrichment.ontology.types]]
name = "character"
kind = "entity"

[[enrichment.ontology.types]]
name = "inner_state"
kind = "state"
of = "character"

[[enrichment.ontology.types]]
name = "reading"
kind = "claim"
force = "assertive"
scope = "about_work"

[enrichment.ontology.change]
clock = "narrative"

[enrichment.ontology.voices]
not_entities = ["the narrator"]

[enrichment.ontology.derive]
configurations = true

[enrichment.ontology.tension]
between = ["reading"]
```

### 1.8 The researcher's notebook

Reading notes, half-formed positions, arguments copied from sources, a
log of changing views. Asks: what is my current position on X and how
did it change; who argues against it; which of my claims are someone
else's.

```toml
[[enrichment.ontology.types]]
name = "position"
kind = "claim"
force = "assertive"
attributes = [{ name = "held_by", type = "text", values = ["me", "source"] }]

[enrichment.ontology.voices]
self = "me"
attributed_to = ["source", "me"]

[enrichment.ontology.change]
supersedes = { position = "document_date" }

[enrichment.ontology.derive]
arguments = true
```

### 1.9 The materials lab

Two hundred papers and the group's own reports. Asks: what yields have
been reported for catalyst X under 300 K; which papers contradict each
other on the mechanism; which methods measured it.

```toml
[[enrichment.ontology.types]]
name = "material"
kind = "entity"
identity = ["cas_number"]
attributes = [{ name = "cas_number", type = "text" }]

[[enrichment.ontology.types]]
name = "catalyst"
kind = "entity"
specializes = "material"

[[enrichment.ontology.types]]
name = "measurement"
kind = "claim"
force = "assertive"
subject = "material"
attributes = [
  { name = "property",    type = "text",     values = ["yield", "conductivity", "band_gap"] },
  { name = "value",       type = "quantity" },
  { name = "temperature", type = "quantity", unit = "K" },
  { name = "method",      type = "ref",      of = "method" },
]
anchors = ["table", "figure", "text"]

[enrichment.ontology.tension]
between = ["measurement"]
same = ["subject", "property", "temperature"]
```

### 1.10 The product support lead

Tickets, call transcripts, a feedback board, release notes. Asks: how
many customers asked for offline mode this quarter; which features are
mentioned in churn conversations; which issues did the 3.2 release
resolve.

```toml
[[enrichment.ontology.types]]
name = "customer"
kind = "entity"
role_of = "organization"
identity = ["account_id"]

[[enrichment.ontology.types]]
name = "ticket"
kind = "event"
source = { file = "tickets.jsonl", attributes = { opened = "created_at", status = "status" } }

[[enrichment.ontology.types]]
name = "request"
kind = "claim"
force = "directive"
deontic = ["request"]
subject = "feature"
identity_fallback = ["subject", "customer"]
attributes = [{ name = "customer", type = "ref", of = "customer" }, { name = "sentiment", type = "text", values = ["positive", "neutral", "negative"] }]
```

## 2. Five axes, not fifteen primitives

Every facet of the first two drafts was run through three questions.
What does a user hit without it? Which code owns it today? Could the
system infer it instead of asking? A facet with no failure is dropped; a
facet that is inferable is derived rather than declared. What survives
sorts into five axes, and the five are the classic questions an
ontology answers: what exists, what is said, what is the same, what
changes, what follows.

### Axis 1 — Shape: what a thing is

Declared once per type. Owner today: `EntityTypeDecl` /
`RelationshipTypeDecl` (`recipe.rs:791-830`) and investigation's
`response_schema` (`investigation/extract.rs:182`), which already
compiles declared names into the extraction enum.

| Facet | Without it | Reuses | Declare or derive |
|---|---|---|---|
| `kind` | no schema can be generated; the type is prose | `AtomType` | declare |
| `attributes` in four value families: text (optional `values`), quantity (optional `unit`), time (optional `range`), ref (`of`) | coin weight, dose and yield are sentences in a description; `same` has nothing to compare; aggregates have nothing to count | investigation's attribute bag; **additive** `attributes` map on Entity, Relation, Claim, Event | declare, few |
| `specializes` | sceatta is a foreign type; "enumerate coin" misses it; attributes duplicated | schema enum carries both names; child inherits attributes and identity | declare when a user has a hierarchy (4 of 10) |
| `role_of` | Acme is three atoms that share a referent and must not be merged | recorded as a `State` on the rigid atom with a `Transition` when it changes | declare when a type is a part something plays (4 of 10) |
| `from` / `to` (relations), `participants` (events) | a `minted_by` between two coins resolves silently | `resolve_entity_ids` at Phase 3 | declare for relations and events |
| `source` (a file and column mapping) | citations re-extracted by a model with errors, which the legal recipe already warns against | `structure_first` ingestion (`atlas/strategies/code_walk.rs`), no model call | declare when a table exists (3 of 10) |
| `label` | the UI says "claim" to a steward who says "rule" | `Vocabulary` → Conflicts panel | declare, defaults to the name |

Dropped from the first draft: `polarity` and `directional` as
primitives (the first is an enum attribute, the second a boolean that
defaults true); seven value types collapsed to four families; imports
and vocabulary as separate blocks (each is a facet of a type).

### Axis 2 — Assertion: what a source says

Declared per claim type, plus one corpus-level block for who speaks.
Owner today: `Claim` already keeps force, strength and kind apart as
`discourse_act`, `epistemic_status` and `claim_kind`; `ClaimScope`
already has `Fictional`. The structure is Toulmin's: content, force,
qualifier, backing, data — plus speaker and world.

| Facet | Without it | Reuses | Declare or derive |
|---|---|---|---|
| `subject` (a declared entity, event or state type) | governance overloads `attributed_to` as the topic; `same` has nothing to key on | **additive** `Claim.subject: Option<AtomId>` | declare |
| `force` (assertive, directive, declaration, commissive) | rules and findings are indistinguishable; supersession applies to the wrong things | `discourse_act` | declare, one word |
| `deontic` (require, forbid, permit, request), normalized so forbid X is stored as require not-X | "must not host after 10pm" and "must end hosting by 10pm" count as two rules or a conflict | `claim_kind` | declare for directive types only |
| strength | — | `epistemic_status` | derive: the extractor judges it per claim |
| `grades` (an ordered evidence scale) | member-report and trial are the same; `must_not` cannot be enforced | an enum attribute with a reserved name, read by the answer gate | declare when the domain has a scale (3 of 10) |
| anchor | a claim with no truthmaker; nothing to grade | `Claim.evidence`, today a `Vec` that may be empty | default: mandatory; `anchors` names table or figure only when they matter (1 of 10) |
| `scope` (in_work, about_work) | what critics say merges with what is true in the novel | `ClaimScope::Fictional` | default universal; declare for fiction |
| `voices`: `not_entities`, `self`, `attributed_to` | the forum poster and the narrator become Person atoms — the conversation genre's exact bug class | the conversation prompt rules, enforced in the parser | declare per corpus when a speaker exists (4 of 10) |
| `must_not` | the honesty rule lives as prose in one recipe | `custom_instructions` on the answer gate, plus the extraction prompt | declare per corpus |

### Axis 3 — Identity: when two are one

Declared per counted type, with a default that is reported. Owner
today: the Enron reconciliation knobs, `atlas_canonical`. The one rule
the second iteration added: a merge is a hypothesis, so it is a claim.

| Facet | Without it | Reuses | Declare or derive |
|---|---|---|---|
| `identity` (an external identifier) | Williams the company merges with Williams the surname (note `51c23280`'s recall ceiling) | reconciler key | declare when an ID exists; it is primary |
| `identity_fallback` (descriptive keys) | nothing to match on when the ID is absent | reconciler fuzzy path | declare; default canonical name, printed in the build report |
| merge policy | — | `judge_when_uncertain` and thresholds | derive: an external key merges strictly, a descriptive key is judged |
| reified merges | merges happen in the reconciler and vanish; the one place the system's own decisions carried no evidence | a `Claim` with `claim_kind: "same_as"`, both ids, a grade, an anchor; retirable | always on, not declared |
| `same` (which fields make two claims comparable) | guest parking versus guest nights flagged as a conflict — the Maple House decoy | the Phase 6 candidate filter | declare per tension; defaults to subject plus the type's clock |

### Axis 4 — Change: what holds when

Declared per corpus, with per-type overrides. Owner today:
`derive_active` (`governance.rs:280`), `Transition` edges and
trajectories (`resolution.rs:645`).

| Facet | Without it | Reuses | Declare or derive |
|---|---|---|---|
| `clock` (document_date, narrative, none) | — | section order; document dates | derive: document dates present means document_date; declare only for narrative |
| `supersedes = { type = clock }` | "what is the rule today" is unanswerable; a restated rule retires the original | `derive_active`, generalized to any listed type, folding on the named clock so valid time and document time stay apart | declare (6 of 10) |
| trajectory on state types | — | `Transition` edges | derive: every `kind = "state"` gets one |
| validity | — | a time-family attribute on the claim type | covered by Axis 1 |

### Axis 5 — Derivation: what the system infers

Declared per corpus. Owner today: `TensionStrategy` and the templated
classifier (`configurable_atlas.rs:254-284`), `PatternDecl`, Phase 8,
`ArgumentReconstruction`, `atlas_traversal` plans.

| Facet | Without it | Reuses | Declare or derive |
|---|---|---|---|
| `tension.between` and `not_conflicts` | precision 0.10 on the governance fixture, measured | the Phase 6 template; `not_conflicts` versioned with the recipe because it is never complete | declare (7 of 10 name a tension) |
| selector | — | graph vs embedding top-K | derive from corpus shape: cross-document uniform text selects embedding |
| `patterns` | circular flows are invisible | the three detectors | declare (1 of 10, already built) |
| `configurations`, `arguments` | interpretive rollups and reconstructed arguments do not run | Phase 8; the philosophy machinery via `rollups` | declare, a boolean each (3 of 10) |
| aggregates | — | a count over typed atoms | derive: any claim type with a `ref` attribute can be counted by it |
| question shapes | — | `atlas_traversal` plans, `atom_enum`, overview claims | derive from the declarations; `recipe validate` prints what the corpus will answer |

Dropped as declared facets: tension selector, question shapes,
aggregates, merge policy, trajectory, clock in the common case, strength
per type. Each is inferable from something the user already wrote.

### The axes against the users

| | Shape | Assertion | Identity | Change | Derivation |
|---|---|---|---|---|---|
| 1 coin | ● | ● |  |  | ● |
| 2 gov | ● | ● | ● | ● | ● |
| 3 med | ● | ● | ● |  |  |
| 4 law | ● | ● |  | ● |  |
| 5 eng | ● | ● | ● | ● | ● |
| 6 dd | ● | ● | ● |  | ● |
| 7 lit | ● | ● |  | ● | ● |
| 8 notes | ● | ● |  | ● | ● |
| 9 lab | ● | ● | ● |  | ● |
| 10 ops | ● | ● | ● |  |  |

Every user declares Shape and Assertion. Six touch Identity, six
Change, seven Derivation. The declared surface is twenty facets across
five axes; the numismatist writes eleven of them and the lab thirteen.

## 3. The kinds under the same test

The eleven atom kinds, by which user's declarations produce them:

| Kind | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | Produced by |
|---|---|---|---|---|---|---|---|---|---|---|---|
| Entity | ● | ● | ● | ● | ● | ● | ● | ● | ● | ● | every genre's Phase 1 |
| Claim | ● | ● | ● | ● | ● | ● | ● | ● | ● | ● | every genre's Phase 1 |
| Question | ● | ● | ● | ● | ● | ● | ● | ● | ● | ● | every genre's Phase 1 |
| Relation | ● | ● | ● | ● | ● | ● | ● |  | ● | ● | every genre's Phase 1 |
| Event | ● | ● |  | ● | ● | ● | ● |  | ● | ● | every genre's Phase 1 |
| State |  |  |  |  | ● |  | ● | ● |  |  | every genre's Phase 1; roles |
| Configuration |  |  |  |  |  |  | ● | ● |  |  | Phase 8 |
| ArgumentReconstruction |  |  |  |  |  |  |  | ● |  |  | philosophy genre |
| Position, Opposition |  |  |  |  |  |  |  | ● |  |  | typed-extension pass, bench-side |
| Asset |  |  |  | ● |  | ● |  |  | ● |  | the described-asset extractor |

Every kind is reached. Two type-level additions passed the test (roles,
subtypes) and leave the kinds closed. One absence stands: there is no
mereological edge among the fourteen — `Involves` links participants,
`Composes` links a Configuration to its constituents, neither is
part-of. No user asked for it; it stays open (section 6).

Two atom-model changes in total, both additive with serde defaults: an
`attributes` map on the four extractable kinds, and `subject` on
`Claim`. The reified merge uses the existing `Claim` kind.

## 4. The interface — five questions

The surface serves a numismatist who writes twenty lines, a lab that
writes eighty, and a steward who clicks a template. Three layers, each
optional: `guidance` alone (version 0, today's fallback); types with
attributes under `version = 1` (the layer that makes "your own types"
true); the four corpus-level blocks `voices`, `change`, `tension`,
`derive`, each defaulting to today's behaviour when absent. A version 1
block with no types yields the same policies as version 0, so adding
the version line is always safe.

The interview is one question per axis, in the user's words. The agent
already asks the first (`sovereign/modes/recipe-author/skill.toml:333-338`).

| Axis | The agent asks | It writes |
|---|---|---|
| Shape | What is this material about, and what would you want to know about each kind of thing? Is any of them a kind of another, or a part something plays? Do you already have any of it in a table? | `types`, attributes, `specializes`, `role_of`, `source`, `label` |
| Assertion | What do the sources say about those things — stating, requiring, deciding, asking? About what? How do you tell strong evidence from weak? Who is speaking, and are they part of the subject? What must it never do? | claim types with `force`, `deontic`, `subject`, `grades`; `voices`; `must_not` |
| Identity | How do you know two mentions are the same thing? Is there an ID? | `identity`, `identity_fallback` |
| Change | When does a later statement replace an earlier one, and from when? | `change.supersedes` and its clock |
| Derivation | What should it notice that no single document says — contradictions, patterns, larger structures? What looks like a contradiction but isn't? | `tension`, `patterns`, `derive` |

`recipe validate` checks every `ref`, `specializes`, `role_of`, `from`,
`to` and `subject` resolves to a declared type or to one of the base
entity kinds the atlas already emits (`person`, `concept`, `institution`,
`work`, `place`, `initiative` — read from `EntityType`, not a copy), so
`role_of = "person"` needs no `person` declaration while §1.1 must declare
`mint`, §1.2 `topic`, §1.4 `organization` (the base kind is `institution`);
declaring a base kind stays legal and adds attributes to it. It checks every `same` names a declared attribute or
`subject`, and prints the question shapes the corpus will answer. The build
report counts atoms per declared type,
names the zero-coverage ones, shows the identity criterion each type
resolved to, and lists reified merges. The inspector filters by
declared type. The Conflicts panel already speaks the labels.

## 5. What survives contact

Kept: entities basic, kinds closed, the demand-driven method, `subject`
as the is-about relation, merges as claims, force and deontic as
separate fields, mandatory anchors, supersession on a named clock,
roles and subtypes at the type level.

Changed in this iteration: fifteen peers became twenty facets on five
axes; seven facets were demoted from declared to derived (selector,
question shapes, aggregates, merge policy, trajectory, the common-case
clock, strength); two were demoted from primitives to attributes
(polarity, directional); three blocks were folded into type facets
(imports, vocabulary, and the identity block into per-type keys). The
interview went from twelve questions to five.

Added in this iteration: the surface is versioned, and the version is
cheap because the five axes are the interface (section 0.1). The
pipeline reads `OntologyPolicies`; a version is a parser.

## 6. Open questions

- **Part-of.** No mereological edge exists. Whether a `part_of` relation
  type maps onto an existing edge or the fourteen become fifteen waits
  for a user who asks.
- **`grades` and `effect` as reserved attribute names.** Both are
  conventional today. If the answer gate and the polarity-aware paths
  read them by name they should be declared facets; if not, they stay
  conventions.
- **Versioning `not_conflicts`.** Tied to the recipe's `prompt_version`
  here. Whether adjudications recorded under one version are re-examined
  under the next is the governance oplog's question.
- **Inference that is wrong.** Derived facets (the clock, the selector,
  merge policy) need to print what they inferred, so a user can see and
  override it. That is the same rule as the identity default.

## 7. What this changes in the as-built plan

M1 becomes Axis 1 plus `subject`: types on all four extractable kinds
with attributes, subtypes and roles, generated into the schema and
parser. M2's genre hooks are Axes 2, 4 and 5 as corpus-level blocks.
Axis 3 is the reconciler with its decisions reified. M6 and M7 are
absorbed. The order of landing is unchanged: M0 (ground on build), Axis
1, then Change and Derivation because six and seven users need them,
then Identity as the counted types show up. The ten users are the
acceptance set; a facet none of them would write does not land.
