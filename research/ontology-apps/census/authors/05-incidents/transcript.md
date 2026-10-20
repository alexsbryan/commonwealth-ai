# Interview transcript — production incidents

Partner not available. Each answer is sourced as `description` (quoting DESCRIPTION.md) or
`assumption` (general knowledge of incident management / on-call practice).

Templates consulted: `sovereign recipe new --ontology list`, then scaffolded `engineering-org`
and `product-support` into this directory (`template-engineering-org.toml`,
`template-product-support.toml`) to look at their shape. Neither was copied; both use the
general extractor (no claim subject with an identity criterion), while this recipe is
declared for the document reader.

## Turn 1 — source shape

**Q: What is the material, and in what form?**
A: Incident chat channels exported as text, paging notifications, postmortem documents.
Files on disk, so `acquire.type = "local_file"`, `path = "SOURCE_PATH"`.
Source: description — "the incident chat channels (exported as text), the paging notifications, and the postmortem documents".

**Q (follow-up): Which extractor?**
A: `plaintext`. Chat is exported as text; paging notifications and postmortems are assumed
to be exported as text as well, so one extractor reads all three. If the pages arrive as
email (.eml) or the postmortems as markdown, a second recipe or a conversion step would be
needed. Chunker `paragraph`.
Source: description for the chat ("exported as text"); assumption for pages and postmortems.

## 1. Shape

**Q: What is this material about, and what would you want to know about each kind of thing? Is any a kind of another, or a part something plays? Any of it already in a table?**
A:
- `incident` (event): one production outage/degradation; carries its incident number(s)
  and a title. Severity, times, roles, timeline, impact and cause are what the sources say
  about it (claims below), not fixed attributes, because they change and are reported
  differently by different sources.
- `service` (entity): a production service or dependency an incident affects.
- `action_item` (entity): a postmortem follow-up, with ticket, incident, owner, due date,
  status.
- People are the base kind `person` (responders, owners); roles (incident commander,
  communications, responder) are held per incident, so they are a claim, not a `role_of`
  type — one person is IC on one incident and responder on another.
- Tables: "Today someone fills in an incident form after the fact" — a form exists, but it
  is not part of the material handed over, so no `source = { file = ... }` is declared.
Source: description for the kinds (needs 1–5) and "fills in an incident form after the fact"; assumption for roles-as-claims and for not treating the form as a table source.

## 2. Assertion

**Q: What do the sources say about those things — stating, requiring, deciding, asking? What does each statement carry? How do you tell strong evidence from weak? Who is speaking? What must it never do?**
A:
- `severity_call` (declaration, about incident): severity sev0–sev4, set_at, set_by, reason. Need 7.
- `milestone` (assertive, about incident): stage ∈ {started, detected, mitigated, resolved}, at. Graded postmortem > channel > page. Need 1.
- `role_assignment` (declaration, about incident): person, role ∈ {incident_commander, communications, responder}, from. Need 2.
- `timeline_entry` (assertive, about incident): at, actor, entry_kind ∈ {alert, page, observation, action, decision, communication, escalation}, service. Need 3.
- `impact` (assertive, about service): incident, effect ∈ {unavailable, degraded, errors, latency, data_issue}. Need 1 (services affected).
- `root_cause` (assertive, about incident): cause_kind (closed set), contribution ∈ {root, contributing}, service. Graded postmortem > investigation > hypothesis, so "the root cause stated in the postmortem" is the postmortem grade. Need 4.
- `follow_up` (commissive, about action_item) and `action_progress` (assertive, about action_item); the item's owner/due/status/incident are read on each statement and folded on the record. Need 5.
- Speakers: responders, ICs, postmortem authors, and the paging system. The bots are not subject matter (`voices.not_entities`).
- Must not: assign blame to an individual.
Source: description for the needs and the role names ("incident commander, communications, responder"), "with an owner, a due date and whether each is done"; assumption for severity scale sev0–sev4, the stage/effect/cause/entry-kind value sets, the grades, and the blameless must_not.

## 3. Identity

**Q: How do you know two mentions are the same thing? Is there an ID?**
A:
- incident: no external key is trusted, because need 6 is exactly the case where two IDs
  name one outage. `identity_criterion`: same failure of the same/overlapping services over
  an overlapping period, however often reported; a declared duplicate is the same; a
  recurrence after resolution is a different incident. Incident numbers are kept as
  `incident_id` with `by = "all"` so a merged incident keeps both numbers.
- service: `identity_criterion` on name / short name / repo / alert prefix.
- action_item: `identity_criterion` on same ticket, or same change to the same system from
  the same postmortem.
Source: description for incident — "Incidents that are the same outage reported twice are one incident."; assumption for the recurrence rule and for service and action_item criteria.

## 4. Change

**Q: When does a later statement replace an earlier one? Does anything move through stages? Which document fields give date and thread?**
A:
- Severity: a later severity call replaces the earlier one for the same incident, ordered
  by the time it was set (`change.supersedes = { severity_call = "set_at" }`), keeping the
  history for need 7. Ordered on the in-text time rather than document date because one
  channel export holds the whole incident.
- Incident stages (started → detected → mitigated → resolved) are recorded as dated
  `milestone` claims rather than a protocol fold: the need is "when", not "what state now".
- Action items move open → in_progress → done / wont_do; the record holds the latest
  reading (`status`, `by = "latest"`), likewise owner and due.
- Document date/thread fields: unknown. The `plaintext` extractor's metadata is not
  documented and there is no material to run `validate --corpus` on, so `change.document`
  is NOT declared rather than guessed. Consequence: the `by = "latest"` folds on
  action_item have no document dates to order on until it is declared.
Source: description for "How severity changed during an incident" and "whether each is done"; assumption for everything else.

## 5. Derivation

**Q: What should it notice that no single document says? What looks like a contradiction but isn't?**
A: Discrepancies between sources on when an incident reached a stage, and on its cause
(channel hypothesis vs postmortem). `tension.between = ["milestone", "root_cause"]`,
`same = ["subject"]`, label "discrepancy". Not conflicts: different stages at different
times; one moment in two time zones or relative vs absolute; root vs contributing cause;
a hypothesis the responders ruled out. Duplicate reports are handled by identity, not
tension.
Source: assumption (the description names no derived pattern beyond need 6).

## 6. Questions

**Q: Once it is read, what will you ask of it?**
A (walked to the declaration):
1. Every incident with severity, services affected, started/detected/mitigated/resolved →
   `incident` records; `severity_call` (latest by set_at); `impact` (subject service,
   `incident` ref); `milestone` stage + at.
2. Who responded and in what role → `role_assignment.person` / `.role`.
3. Timeline → `timeline_entry` at / actor / entry_kind / service.
4. Root cause in the postmortem → `root_cause` graded `postmortem`.
5. Follow-ups with owner, due, done → `action_item` owner / due / status / incident, from `follow_up` and `action_progress`.
6. Same outage reported twice is one → `incident.identity_criterion`.
7. Severity changes → `severity_call` history under supersession on set_at.
Source: description (the seven numbered needs).

## Validation runs

### Run 1 — `sovereign recipe validate recipe.toml` (exit 0)
```
✓ Validation passed
  clock: document_date — supersession folds on document dates (the default; ...)
  tension selector: embedding top-k (k = 10, floor = 0.5) over milestone, root_cause ...
  identity: service → canonical name (default; declare `identity = [...]` for an external key)
  identity: incident → RESOLVE under its identity_criterion, ...
  identity: action_item → RESOLVE under its identity_criterion, ...
  question shapes: enumerate [service, action_item]; relations [(none)]; events [incident]; aggregate [severity_call by set_by, role_assignment by person, timeline_entry by actor, timeline_entry by service, impact by service, root_cause by service]
  ⚠  the document reader ... makes no record of `service` (entity): no statement kind is about them, RESOLVE decides none of them and no source supplies them.
  ⚠  `voices` (attributed_to) are applied by the general extractor only; the document reader attributes no statement to a speaker.
```
Reading against the answers: both warnings contradict the answers. Services must be
records (need 1 asks which services an incident affected), and the attributed_to voices did
nothing under the document reader. Fixes: gave `service` an `identity_criterion` and made
`impact` a statement about the service (with an `incident` ref) so RESOLVE decides service
records; removed `voices.attributed_to`.

### Run 2 — `sovereign recipe validate recipe.toml` (exit 0)
```
Validating recipe: recipe.toml
✓ Validation passed

Derived from your declarations:
  clock: document_date — supersession folds on document dates (the default; set `change.clock = "narrative"` for order within a work)
  tension selector: embedding top-k (k = 10, floor = 0.5) over milestone, root_cause — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: service → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: incident → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: action_item → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  question shapes: enumerate [service, action_item]; relations [(none)]; events [incident]; aggregate [severity_call by set_by, role_assignment by person, timeline_entry by actor, timeline_entry by service, impact by incident, root_cause by service]
```
Reading against the answers: identity for all three types is RESOLVE under the criterion,
as answered (incident in particular is not keyed on an ID, per need 6). Tension over
milestone and root_cause as answered. Aggregates match needs 1–3 (impact by incident,
role_assignment by person, timeline_entry by actor). The clock line is the default
document_date; severity supersession is declared on `set_at` and is not echoed by this
output, and no document date field is declared (see Change). No further fix.

## Summary

Types and identity:
- `incident` (event) — RESOLVE under identity_criterion (same outage, however reported; recurrence is new).
- `service` (entity) — RESOLVE under identity_criterion (same deployed system by name/alias/repo/alert prefix).
- `action_item` (entity) — RESOLVE under identity_criterion (same ticket or same work from the same postmortem).
- `person` — base kind, not declared.
- Claims (identity n/a): `severity_call`, `milestone`, `role_assignment`, `timeline_entry`, `impact`, `root_cause`, `follow_up`, `action_progress`.

Least-sure assumptions:
1. One `plaintext` extractor fits all three sources; and no `change.document` fields are declared, so `by = "latest"` on action_item owner/due/status has no clock to order on until the extractor's date field is known (`validate --corpus`).
2. Severity scale sev0–sev4 and the closed value sets for stage, effect, cause_kind and entry_kind.
3. A recurrence after resolution is a different incident.
4. Making `impact` a statement about the service (with an incident ref) rather than about the incident.
5. Ordering severity supersession on the in-text `set_at` time.
