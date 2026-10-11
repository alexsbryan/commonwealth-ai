# Blind-author protocol (order ontology-layer-3-any-author, step 3)

The brief a blind session receives, one session per system, and how it is run. Decisions of 2026-10-09
(campaign.md, Decisions) are folded in: the docs every user gets lose their examples drawn from the
three test systems (`neutral-docs.patch`, applied to the repo in the next cargo slot); the
product-support template stays (added 2026-10-02, before any uv-support work); the session may run
`recipe new --ontology` and never reaches the registry's `enron-sample*` or `email-archive` recipes; it
runs outside the repo with no MCP and no project memory.

## Environment

One directory per system outside the repository, `~/blind-author-20261009/{ward,uv,gvc}/`, holding
only: `DESCRIPTION.md` (that system's one section of `descriptions.md`, frozen at 571fb892c),
`recipe-author.toml` (the skill, unchanged), `v1.md` and `SCHEMA.md` (copies with exactly
`neutral-docs.patch` applied), and `BRIEF.md` (below).

The session is `claude -p` (2.1.295) started in that directory:

```
claude -p --setting-sources "" --strict-mcp-config --restricted --no-session-persistence \
  --tools Read,Write,Edit,Glob,Grep,Bash --disallowedTools WebFetch WebSearch \
  --allowedTools Read Write Edit Glob Grep "Bash(sovereign recipe validate:*)" "Bash(sovereign recipe new:*)" \
  --permission-prompts none --output-format stream-json --verbose \
  "Read BRIEF.md in this directory and carry it out." > transcript.jsonl
```

Substitution, named: the order asked for `--bare`. On this host `--bare` cannot authenticate (it reads
only `ANTHROPIC_API_KEY`/`apiKeyHelper`, and the account is OAuth: "Not logged in"). The flags above
give the same isolation by other means: no settings sources (so no user hooks), no MCP, file tools
confined to the working directory (`--restricted`), Bash limited to the two `recipe` verbs, a fresh
directory with no `CLAUDE.md` and no project auto-memory (none exists at `~/.claude/CLAUDE.md`). What
remains of the harness context is the account email the client attaches. Default model and effort.

## BRIEF.md (verbatim)

> You are writing a Sovereign corpus recipe for a person who owns some material. Their own description
> of it is in `DESCRIPTION.md`; it is everything you know about them. Work as the recipe-author skill
> (`recipe-author.toml`, its `synthesis` prompt) describes, with one difference: the partner is not
> available. Where the skill tells you to ask the partner, answer the question yourself from the
> description, and where the description does not answer it, from general knowledge of the domain.
>
> You may read only the files in this directory: `DESCRIPTION.md`, `recipe-author.toml`, `SCHEMA.md`
> (the full recipe field reference) and `v1.md` (the version-1 ontology guide, also inside SCHEMA.md).
> You may run `sovereign recipe new --ontology list`, `sovereign recipe new --ontology <name> ...` to
> look at or scaffold a template, and `sovereign recipe validate <file>`. The skill's tools
> (`recipe_write_structured`, `registry_browse`, `recipe_read`, `web_search`, ...) are not available;
> write TOML with your file tools instead. Do not look for other recipes or for anything about this
> material beyond general knowledge.
>
> 1. Ask the skill's five interview questions (shape, assertion, identity, change, derivation) in
>    order, plus any follow-up the schema needs, and answer each one. In `transcript.md` record each
>    question, your answer, and its source: `description` (quote the words) or `assumption`.
> 2. Write ONE recipe, `recipe.toml`, with a version-1 `[enrichment.ontology]` (`type = "atlas"`).
>    For acquire and extract: `acquire.type = "local_file"` with `path = "SOURCE_PATH"` (a placeholder
>    for where the material sits) and the extractor you would expect this material to need. The
>    ontology is the work.
> 3. Run `sovereign recipe validate recipe.toml`. If it fails, fix the recipe and run it again until
>    it passes; record each run and its output in `transcript.md`. Then read the derived facets it
>    prints against your answers, and fix the declaration where one is wrong.
> 4. End with a short summary: the types you declared, each one's identity, and the assumptions you
>    are least sure of.

## Contamination found in the allowed docs (replaced by neutral-docs.patch)

- v1.md:137-178 = SCHEMA.md:1465-1506 — metadata `source` example: `company` by mail domain, `person` by
  address with `employer` ref, `exclude = ["pdq.net"]` (a Houston ISP in the Enron mail), `email.msn.com`,
  `espn.go.com`. Drawn from the mail-CRM work; it hands crm-ward its identity rule. Replace with a neutral
  metadata source (e.g. a catalogue's `accession` field).
- SCHEMA.md:1113-1114 — `exclude` and `refs` field docs: "regional ISP", "Contact → Account", the same
  `employer` example. Replace.
- v1.md:115-135 = SCHEMA.md:1443-1463 and the prose v1.md:92-113 = SCHEMA.md:1420-1441 — protocol fold
  example: `record_update` -> `resolved` when `action = "resolved_by_authority", role = "maintainer"`,
  reopen/correction behaviour. Issue-tracker-shaped; hands uv-support its state rule. Replace with a
  neutral domain (e.g. a permit application approved by an authority).
- v1.md:77-90 = SCHEMA.md:1405-1418 and SCHEMA.md:966 — `change.document = { date, thread = "thread_id",
  id = "message_id" }` "for mail", "a mail thread". Mild (email is a generic source), but it is our
  Ward stamp verbatim. Replace with neutral field names.
- SCHEMA.md:808 — `EvidentialFieldDecl` example `{ evidence = "document_thread", right = 190, of = 210 }`:
  a measured number from our runs on the surface (retires with order 1 step 8). Withhold the section.
- SCHEMA.md:135, 572, 581 — "Architecture-over-Enron Phase …" in the reconciliation, email and
  described-asset docs. Names the corpus; mild. Strip the phrase.
- v1.md:180-184 = SCHEMA.md:1508-1512 and SCHEMA.md:795 — "a list of recipients". Negligible; leave.
- v1.md:225-227 = SCHEMA.md:1553-1555 and the skill (recipe-author.toml:327, the "Ten worked declarations"
  comment in the example) point to `svrn/docs/specs/ONTOLOGY_PRIMITIVES.md` §1, whose §8 holds a worked
  example from one of these systems. Withhold the file; the templates carry §1.
- `_templates/ontology-v1/product-support/recipe.toml:3-6,53-57,77-88` — a ticket tracker: tickets with
  status, "the same customer asking for the same thing twice is one request", "which issues a release
  resolved". Generic, not drawn from uv, but the closest neighbour to uv-support (decision 2).
- `_templates/ontology-v1/contracts/recipe.toml:10-15` — scaffold id `my-deals`. Contracts, not gas
  deals; leave.
- The skill itself (recipe-author.toml) is clean: its examples are SEC financing and numismatics; the
  word "mailbox" at line 78 is a source shape. Its tools `registry_browse` and `recipe_read`
  (lines 17-18, 189) expose the catalogue, which holds `enron-sample*` and `email-archive` recipes:
  those tools are not available to the blind session, and its prompt says so.
- No gun-violence or news-incident example was found in any allowed doc.


## Must not read

Anything under `research/ontology-apps/` except its one description; any gold or `GOLD_SPEC`; any recipe
`.toml` other than the ontology-v1 templates (our three recipes, `enron-sample*`, `email-archive`, the
`sovereign-recipes/` catalogue, `~/.svrnmesh/recipes/`); `~/.svrnmesh/bench-corpora/`; results, scores
and preregistrations; `quality/campaigns/`; `.sovereign/features/`; `svrn/docs/specs/ONTOLOGY_METHOD.md`
and `ONTOLOGY_PRIMITIVES.md`; the notes store; git history. `--restricted` enforces the file half.

## Does the skill's interview cover the five axes?

Shape and derivation: adequate. Assertion: covers claim kinds, force, subject, grades and voices, but
never asks what VALUES a claim carries (the attributes of a claim, their families, which are closed
sets with `values`, units), which this campaign's records need (a deal's terms, an issue's versions,
an incident's casualties). Identity: one line ("same thing? an ID?"). It does not ask, per type, which
attributes make the key, what the fallback is when the key is absent, which document fields already
carry it (`source = { metadata }`), what distinguishes two look-alikes (two incidents in one city on one
day), or how many documents fold into one record (articles to one incident, reports to one problem).
Change: asks only when a later statement replaces an earlier one. It does not elicit state over time:
the `state` kind, a closed set of state values, which claims move a record into which state and who has
authority to (the `folds` / `by = "protocol"` rules), report time versus effective time,
`change.document` field names, corrections and reopenings. The skill's prose never mentions `state`,
`folds`, `paths` or `sets` at all, so a blind author reaches them only through SCHEMA.md, and after the
replacement above, without an example. These gaps are findings for step 4, not fixes for this order.

