# GVC recipe — authoring transcript

The partner wasn't available. I asked each question myself and answered it from `DESCRIPTION.md`
(quoted) or, where the description is silent, from general knowledge of US news coverage
of shootings (marked `assumption`).

## Turn 1 — source shape (before the five questions)

**Q. What are the files, and in what format?**
A. News articles, one per record, with text, title, outlet, publication date, URL and the
database incident the article was gathered for. I picked one JSONL file (`jsonl` extractor,
`content_field = "text"`, `title_field = "title"`), read with `local_file` at `SOURCE_PATH`,
chunked `paragraph` at 3000 chars (articles are short, so "a man … he was identified as"
stays in one chunk).
Source: description for the content: "English-language news articles about gun violence
incidents in the United States, gathered around incidents listed in a public gun violence
incident database". **Assumption** for the format (JSONL) and every field name (`text`,
`title`, `source`, `date_published`, `url`, `incident_id`).

## 1. Shape

**Q. What is this material about, and what would you want to know about each kind of thing?
Is any a kind of another, or a part something plays? Is any of it already in a table?**

A. Incidents, the people in them, where they happened, and the outlets reporting them.
- **incident** (entity): when, where (ref `location`, plus city/state), killed, injured,
  circumstance, weapon. Source: description: "What I want kept track of is the incidents
  themselves rather than the articles … what happened in it, where and when". The
  attributes past that (circumstance categories, weapon) are an **assumption**.
- **participant** (entity, `role_of = "person"`): name, role (shooter / suspect / victim /
  bystander / officer / witness), age, gender, residence, incident. Source: description:
  "who was involved and what happened to each of them"; "one gives the shooter's name and
  another only 'a man'". The role list, age, gender and residence are an **assumption**
  (these are the details shooting reports usually carry).
- **involved_in** (relation, participant → incident). Source: description: "who was
  involved". Added after validation (see below).
- **location** (entity, `specializes = "place"`): street address, venue, neighborhood, city,
  county, state. Source: description: "one names the street and another only the city".
- **news_outlet** (entity, `specializes = "institution"`): name, outlet type, city, state.
  Source: description: "Local papers, TV station sites and wire stories each tell it their
  own way". Read from the document's `source` metadata field: **assumption**.
- **law_enforcement_agency** (entity, `specializes = "institution"`). Source: description:
  "as police released details". That it should be its own type is an **assumption**.
- Table? The incident database exists, but the description doesn't say it's in the
  material, so I declared no table source. Source: **assumption**.

## 2. Assertion

**Q. What do the sources say about those things? About what? How do you tell strong evidence
from weak? Who is speaking, and are they part of the subject? What must it never do?**

A. Each article asserts facts about an incident and its people. Six claim types:
`toll_report` (killed/injured counts, about the incident), `incident_account` (when, where,
how, why, about the incident), `identification` (name, age, gender, role, about a
participant), `outcome_report` (killed / wounded / unharmed, about a participant),
`suspect_status` (sought / arrested / charged …, about a participant), all
`force = "assertive"`; and `official_ruling` (`force = "declaration"`, a medical examiner's
or prosecutor's determination, about the incident).
Source: description: "which source said what"; "one says two were hurt and a later one says
one of them died". How the claims split into types, and `official_ruling`, are an
**assumption**.

Evidence grades, strongest first: official record > on-record official > named witness or
family > unnamed source > unattributed. Source: **assumption** (the usual journalistic
attribution hierarchy).

Voices: claims are attributed to police, prosecutor, medical examiner, court record,
hospital, witness, family member, or the outlet itself. "The reporter / the station / the
newsroom / staff writer" never become entities. Source: description: "as police released
details" for police. The rest is an **assumption**.

must_not: never present an identification, arrest or charge as a finding of guilt; never fill
in a detail no source reported; never settle a disagreement by silently picking one
version. Source: **assumption**, though the last follows from "which source said what,
especially where sources disagree".

## 3. Identity

**Q. How do you know two mentions are the same thing? Is there an ID?**

A.
- **incident**: there's no ID in the article text. Same incident = same date (allowing
  "late Saturday" / "early Sunday"), same city, and compatible people and place. A
  difference in precision isn't a difference. Two shootings in one city on one day are
  separate when their victims or places don't match. A follow-up story (arrest, funeral)
  belongs to the original incident. Declared as `identity_fallback = ["occurred", "city",
  "state"]` plus an `identity_criterion`. Source: description: "which articles are about the
  same incident", "many articles describe the same incident". The criterion itself is an
  **assumption**.
- **participant**: same incident, compatible role, age and gender. "A 23-year-old man" and
  the named 23-year-old victim of the same shooting are one person. In different incidents,
  only the same full name with a compatible age counts. Source: description: "one gives the
  shooter's name and another only 'a man'". The rule is an **assumption**.
- **location**: street address + city + state, judged. Source: **assumption**.
- **news_outlet**: outlet name, strict, from the `source` field. Source: **assumption**.
- **law_enforcement_agency**: name + jurisdiction, judged. Source: **assumption**.
- **Follow-up: is there a database incident ID?** The incidents come from a database, so each
  article probably carries the ID of the incident it was gathered for. I didn't make it an
  identity key: the description says the user wants the tool to find which articles go
  together, and the text won't contain the ID. I stamped it as `document_thread` instead
  (`change.document.thread = "incident_id"`). Source: description: "gathered around
  incidents listed in a public gun violence incident database". The field and its name are
  an **assumption**.

## 4. Change

**Q. When does a later statement replace an earlier one, and from when?**

A. By publication date. A later toll, identification, outcome or suspect status replaces an
earlier one for the same incident or person (`supersedes` on `document_date` for
`toll_report`, `identification`, `outcome_report`, `suspect_status`). Incident accounts and
official rulings don't supersede: competing accounts should stay side by side as
discrepancies. Document date ← `date_published`, document id ← `url`.
Source: description: "The articles were written over days or weeks as police released
details, so early reports and later ones disagree"; "a later report corrects an earlier
one". Which types supersede, and the field names, are an **assumption**.

## 5. Derivation

**Q. What should it notice that no single document says — contradictions, patterns, larger
structures? What looks like a contradiction but isn't?**

A. Mainly two things: which articles cover the same incident (identity, above), and where
sources disagree about the same incident or person (tension across all six claim types,
`same = ["subject"]` so reports from different dates are still compared, labelled
"discrepancy"). Things that aren't conflicts: a toll that rises because a victim died; a
street versus a city; a name versus "a man"; a suspect who goes from sought to arrested to
charged; "late Saturday" versus "11:45 p.m."; one wire story reprinted by several outlets.
No graph patterns are declared. Source: description: "especially where sources disagree or a
later report corrects an earlier one"; "one names the street and another only the city".
The other non-conflicts and the choice to declare no patterns are an **assumption**.

## Validation runs

### Run 1 — `sovereign recipe validate recipe.toml`

```
Validating recipe: recipe.toml
✓ Validation passed

Derived from your declarations:
  clock: document_date — supersession folds on document dates (the default; set `change.clock = "narrative"` for order within a work)
  tension selector: embedding top-k (k = 10, floor = 0.5) over toll_report, incident_account, identification, outcome_report, suspect_status, official_ruling — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: participant → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: location → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  source: news_outlet ← document fields source (name: value) — one atom per identity value, model atoms with that value merge into it; 0 value(s) excluded
  identity: news_outlet → name (external key, strict merge)
  identity: law_enforcement_agency → name + jurisdiction (descriptive keys, judged merge)
  question shapes: enumerate [participant, location, news_outlet, law_enforcement_agency]; relations [(none)]; events [incident]; aggregate [incident_account by location]
  document fields: document_date ← `date_published` (RFC 2822 or ISO 8601, written as ISO 8601), document_thread ← `incident_id`, document_id ← `url` — stamped on each claim from the one document its evidence lands in; a claim in none or several is left unstamped and counted
```

Reading the facets against my answers:
- **Wrong: incident has no identity line.** I'd declared it as an `event` with
  `participants`. Only entity types get an identity rule, so the cross-article incident
  identity, which matters most, was never applied. **Fix:** `incident` is now
  `kind = "entity"`. Its people are linked through the new `involved_in` relation and
  `participant.incident`.
- **Wrong: location resolves "over the claims declaring it their subject".** No claim has
  `location` as its subject, so RESOLVE would have nothing to work on, and the locations
  Phase 1 found would be retired. **Fix:** I removed location's `identity_criterion`, so it
  falls back to the judged descriptive keys.
- Correct: the clock (document_date), the tension types, the participant identity, the
  outlet and agency identities, and the document-field stamps.

### Run 2 — `sovereign recipe validate recipe.toml`

```
Validating recipe: recipe.toml
✓ Validation passed

Derived from your declarations:
  clock: document_date — supersession folds on document dates (the default; set `change.clock = "narrative"` for order within a work)
  tension selector: embedding top-k (k = 10, floor = 0.5) over toll_report, incident_account, identification, outcome_report, suspect_status, official_ruling — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: incident → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: participant → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: location → street_address + city + state (descriptive keys, judged merge)
  source: news_outlet ← document fields source (name: value) — one atom per identity value, model atoms with that value merge into it; 0 value(s) excluded
  identity: news_outlet → name (external key, strict merge)
  identity: law_enforcement_agency → name + jurisdiction (descriptive keys, judged merge)
  question shapes: enumerate [incident, participant, location, news_outlet, law_enforcement_agency]; relations [involved_in]; events [(none)]; aggregate [incident_account by location]
  document fields: document_date ← `date_published` (RFC 2822 or ISO 8601, written as ISO 8601), document_thread ← `incident_id`, document_id ← `url` — stamped on each claim from the one document its evidence lands in; a claim in none or several is left unstamped and counted
```

Every facet now matches the answers above. Incident and participant are resolved across
articles under their criteria (both are subjects of claims). Location, agency and outlet use
keys. Discrepancies are sought across the six report types. The `involved_in` relation and
`incident` can now be asked about.

## Summary

**Types declared**

| Type | Kind | Identity |
|---|---|---|
| incident | entity | RESOLVE under criterion (same date ± overnight, same city, compatible people and place); fallback occurred + city + state |
| participant | entity, role of person | RESOLVE under criterion (same incident, compatible role/age/gender); fallback name + incident |
| involved_in | relation participant → incident | — |
| location | entity, specializes place | street_address + city + state (judged) |
| news_outlet | entity, specializes institution | name (strict), from the document's `source` field |
| law_enforcement_agency | entity, specializes institution | name + jurisdiction (judged) |
| toll_report, incident_account, official_ruling | claims about incident | — (assertive; ruling is declaration) |
| identification, outcome_report, suspect_status | claims about participant | — (assertive) |

**Assumptions I'm least sure of**
1. **The source format and its field names** (JSONL with `text`, `title`, `source`,
   `date_published`, `url`, `incident_id`). If they're wrong, the document date doesn't stamp
   and supersession has no clock, so this is the first thing to check against the real files.
2. **Not using the database incident ID for identity.** If every article carries a
   reliable ID, incidents could be keyed on it outright. I kept it as a thread stamp because
   the user asked for the grouping to be tracked rather than given.
3. **Which claim types supersede.** I left incident accounts and rulings out so competing
   versions show up as discrepancies. The user might want a corrected time or place to
   replace the earlier one.
4. **The evidence grades and the participant role list.** These come from general
   journalistic practice, not the description.
