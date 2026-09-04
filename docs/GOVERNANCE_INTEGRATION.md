# Integrating the governance stack

You have a corpus of governing text — a constitution, a charter, bylaws,
a policy manual — plus the amendments that have accumulated on top of it.
You want the current rules extracted, the contradictions surfaced, and the
adjudications recorded so that "what is the rule today, and what did it
replace?" is answerable a year from now.

You may also already run your own llama-server, your own agents, and your
own storage. This page is the whole spectrum: from adopting the stack
end-to-end down to speaking one append-only file format and linking none
of our code.

It is the governance-specific companion to
[INTEROP.md](./INTEROP.md) (point a tool you already run at a local
daemon) and [INTEGRATION_SURFACES.md](./INTEGRATION_SURFACES.md) (which
surfaces are contracts). Read this one to pick a tier.

Where this is headed, and why the interfaces have the shape they do, is
[LIVING_GOVERNANCE.md](./LIVING_GOVERNANCE.md) — a design note for work not
yet built. It is worked through end to end at a scale where the statistics
hold in [CASE_STUDY_FERNWOOD.md](./CASE_STUDY_FERNWOOD.md), and over a
codebase where most of the actors are agents in
[CASE_STUDY_ENGINEERING.md](./CASE_STUDY_ENGINEERING.md). Both use the
commitment model that [CANON_CLI.md](./CANON_CLI.md) specifies.

## The seam that makes the spectrum possible

The design splits along one line, and everything below follows from it.

**The atlas graph says which rules exist and which tensions were
surfaced.** Rules are `Claim` atoms; tensions are `EdgeType::Tension`
edges. Producing them is extraction — it costs model calls.

**The oplog says what was decided about them.** Assert, supersede,
retract, resolve, accept, dismiss, revert. The fold that turns that log
into the current active rule set is
`derive_active(&[Op<GovernanceOpKind>]) -> ActiveSet`
(`corpus-engine/src/enrichment/governance.rs:280`). It does no IO and
never calls inference.

So the governance semantics — what is in force, what was superseded and
by which decision, which contradictions are still open — are a pure
function of an append-only file. Only two operations in the whole stack
need a model: **extraction**, and **`ask`** (answering a question from
current law). That is why a minimal integration is possible at all, and
it is the fact to hold while reading the tiers.

The current law is a query over the log, not a row you mutate. History is
preserved, so "what was the guest policy in March, and why did it change?"
stays answerable because nothing is destroyed.

## The five tiers

| Tier | You bring | You get | Our code you run |
|---|---|---|---|
| 1 | A folder of documents | Desktop app, human steward in a GUI | All of it |
| 2 | A server, a scheduler | `svrn govern` verbs, scriptable | Daemon + CLI |
| 3 | Your llama-server | Same verbs, your inference | Daemon + CLI, your models |
| 4 | A Rust process | `GovernanceView`, the pure fold | `corpus-engine` as a library |
| 5 | Everything | A file format | None |

---

## Tier 1 — The whole stack

Add the folder through the desktop Library using the **Rules & decisions**
template. That writes the generalized governance ontology recipe, and on
enrich-build completion runs a migrate-ids → seed hook automatically.

What you get is the **Conflicts panel** — a per-notebook tab listing
ranked open conflicts with both rule texts, where a steward resolves,
accepts, or dismisses each one, and exports the meeting agenda and the
current-rules sheet. The tab is gated on the corpus actually carrying a
`governance_oplog.jsonl`, so it appears only for corpora under governance.

No configuration, no code, and no shell. This is the tier for a community
that wants the thing rather than the substrate.

## Tier 2 — Headless: daemon plus CLI

```sh
svrn corpus install <recipe>
svrn enrich init  <corpus> --from-corpus <corpus>
svrn enrich build <corpus> --full        # extraction — costs model calls
svrn govern seed  <corpus>               # assert every extracted rule-claim
svrn govern tensions <corpus>            # the meeting agenda, ranked
svrn govern resolve  <corpus> <tension-id> --keep <rule-id>
svrn govern accept   <corpus> <tension-id> --rationale "..."
svrn govern ask      <corpus> "how many nights can a guest stay?"
```

Worth knowing: **only `ask` needs the daemon.** `seed`, `tensions`,
`resolve` and `accept` read the `GovernanceView` read-model or append to
the oplog, and involve no model at all. If your integration is "surface
the conflicts, let a committee decide, record the decision," you can run
that whole loop model-free.

`ask` is the runtime build — a turn sealed to the corpus, grounded in
current law, with the active-set retrieval filter dropping superseded
rules' evidence, cite-or-abstain gated, and supersession provenance
rendered under the answer.

Everything exits non-zero on failure, so it scripts. `svrn tools call <id>
--format json` is the stable machine-readable path; other commands print
for humans, so don't parse them.

## Tier 3 — Your models, our pipeline

Two distinct seams, and they are not the same one.

### Extraction against your endpoint

Enrichment resolves `provider:model` specs against
`~/.config/sovereign/providers.toml`:

```toml
[providers.myserver]
type = "openai-compatible"      # local daemon, vLLM, llama.cpp, OpenRouter, Together
base_url = "http://10.0.0.5:8080/v1"
```

A bare model id resolves to provider `local`. `anthropic` is the other
dialect (`/v1/messages`).

**Set `structured_output_mode` deliberately.** The extraction pipeline
asks for JSON against a schema, and providers differ in how they honor it:
`json-schema` (the provider enforces it — OpenAI, our daemon),
`json-object` (valid JSON, no schema enforcement), `tool-use-auto`, and
`tool-use-forced` (maximum adherence, not universally supported). A
llama-server that doesn't enforce `json_schema` needs `json-object`, and
guessing wrong shows up as extraction quality loss rather than an error.

**The gotcha that will bite you.** There is one egress boundary, and it
decides local-versus-remote by comparing the resolved `base_url` against
this client's own daemon base. Your llama-server on another host or port
is **remote** by that definition, even on your LAN — so personal-custody
chunks are refused to it unless a consent grant is installed for the run.
This is deliberate: the boundary protects custody, and it does not know
that your endpoint is one you trust. Plan for it rather than discovering
it mid-ingest.

### Putting Sovereign in front of your server

`[[inference.backends]]` in `sovereign-server.toml` is the way to have
Sovereign serve from an external OpenAI-compatible server — vLLM, SGLang,
llama.cpp, TGI, a LiteLLM proxy.

Treat this as **experimental**. It is real and working, but
`sovereign-server` is build-from-source rather than one of the three
shipped binaries, so the key names are unsettled. See
[INTEROP.md](./INTEROP.md#9-going-the-other-way).

## Tier 4 — Link the library

Depend on `corpus-engine` and use three things:

```rust
use corpus_engine::enrichment::{GovernanceView, GovernanceOpKind, derive_active};
use corpus_engine::oplog::Oplog;

// The joined read-model: rules with status, tensions with disposition,
// integrity issues. Missing files read as empty, so a corpus mid-setup
// degrades rather than erroring.
let view = GovernanceView::from_atlas_dir(&atlas_dir)?;

for rule in view.active_rules()   { /* current law */ }
for t    in view.open_tensions()  { /* the agenda  */ }

// Or fold the log yourself — pure, no IO, no inference.
let ops    = Oplog::<GovernanceOpKind>::new(&atlas_dir).read_all()?;
let active = derive_active(&ops);
```

`view.dead_law_sections()` returns the section ids your retrieval must
drop so an answer is never grounded in a rule no longer in force. If you
run your own RAG, this is the one call that keeps it honest.

**Licensing matters at this tier and not before.** The code is
AGPL-3.0-or-later. Calling the daemon over HTTP does not engage it;
linking the crate does.

## Tier 5 — The file format, and nothing else

The minimal surface. Your agents write JSON lines; anything at all reads
them.

`<atlas_dir>/governance_oplog.jsonl` holds one act per line, internally
tagged on `"op"` so each line is self-describing, with content-addressed
ids prefixed `gov-`. Illegal field combinations are unrepresentable —
each act carries exactly its own fields.

```jsonl
{"id":"gov-4c1a9f","v":1,"ts_unix":1771027200,"actor":"ingest","op":"assert_rule","rule":"atom_9f2…","source_doc":"charter.md"}
{"id":"gov-8ad3e0","v":1,"ts_unix":1773532800,"actor":"human:priya","op":"supersede","new_rule":"atom_c41…","old_rules":["atom_9f2…"],"rationale":"House meeting 2026-03-14"}
{"id":"gov-b72d15","v":1,"ts_unix":1773532800,"actor":"human:priya","op":"resolve_tension","tension":"edge_17","via":"gov-8ad3e0","endpoints":["atom_9f2…","atom_c41…"]}
{"id":"gov-2e90c4","v":1,"ts_unix":1773619200,"actor":"human:sam","op":"accept_tension","tension":"edge_22","rationale":"Tolerated until the spring review."}
{"id":"gov-77f1ab","v":1,"ts_unix":1773619200,"actor":"human:sam","op":"dismiss_tension","tension":"edge_31","rationale":"Detector noise — different topics."}
{"id":"gov-05c8de","v":1,"ts_unix":1773705600,"actor":"human:sam","op":"retract_rule","rule":"atom_5b8…","rationale":"Obsolete after the porch rebuild."}
{"id":"gov-91b6f2","v":1,"ts_unix":1773792000,"actor":"human:priya","op":"revert","targets":["gov-8ad3e0"],"rationale":"Recorded against the wrong article."}
```

The envelope is four fields — `id`, `v`, `ts_unix`, `actor` — and the act
is **flattened onto the same object**, which is why `"op"` sits beside
`"actor"` rather than nested under it.

`id` is content-addressed: `gov-` plus a short hash over
`(prefix, ts_unix, actor, body)`, with the body serialised in field
declaration order so the id is deterministic across runs and builds. Two
byte-identical acts by the same actor in the same second collide by
design; appending in real time means it doesn't arise in practice.

`actor` is the field your integration must get right — `human:<name>` for
an adjudication a person made, a machine label such as `ingest` otherwise.
See the attribution constraint below.

Seven acts, and that is the entire vocabulary. Lines carry a version
(`GOVERNANCE_OPLOG_VERSION`, currently `1`); a reader that doesn't
understand a declared version **refuses the line rather than
misinterpreting it**, so you can extend without silent corruption.

You still need `atoms.json` and `edges.json` beside it to supply the rule
and tension universe — the oplog references atom and edge ids, it does not
define them. If you extract rules yourself, you own both files and the
oplog is the only format you have to match.

**Read this before you commit to Tier 5.** The `~/.svrnmesh/` directory
layout is currently listed as *internal, no compatibility promise* in
[INTEGRATION_SURFACES.md](./INTEGRATION_SURFACES.md). The oplog format
itself is versioned and stable in practice, but its location is not yet a
contract. If you are building here, open an issue saying so — that is the
path by which a surface gets promoted, and this is a good candidate.

---

## Four constraints that apply at every tier

**Agents propose, humans dispose.** This is the one that will shape your
integration most. Any op that is not an `AssertRule` and whose actor does
not begin with `human:` surfaces in the view as
`GovernanceIssue::UnattendedAct`. Extraction and seeding are machine work
and are expected to be; adjudication is attributed to a person by design.
Your agents can surface tensions, rank them, draft rationales, and prepare
the agenda — and when one records a decision under its own name, the view
reports it rather than hiding it. Build the human step in rather than
around.

**Every act is reversible.** A single general `Revert` tomb-stones prior
ops during the fold, and is itself revertible — reverting a `Revert`
re-applies the originals. Because a human adjudication is usually a small
bundle (assert + supersede + resolve), naming the whole bundle makes the
undo atomic. You never need a compensating hand-written op.

**A tolerated contradiction is a first-class state.** `accept_tension`
exists because real common law carries known contradictions that nobody
intends to resolve, and forcing every conflict to a resolution would
falsify the record. It requires a rationale — an accepted contradiction
must say why. Note that it is distinct from `dismiss_tension`, which means
the detector was simply wrong.

**Decisions survive re-extraction.** Edge ids are re-minted on every atlas
rebuild, but content-hash rule ids are stable, so adjudications also record
their endpoint rule-id pair. The view matches by edge id, then by pair,
then by mootness — a conflict whose rule has since been superseded is not
open. Only a genuinely dangling decision, where a rule's text was edited
away, surfaces as an issue for a steward to re-adjudicate. You can re-run
extraction weekly without relitigating settled questions.

## The red lines

If you replace our answering path with your own, these are the three
properties you are taking responsibility for:

- **RL-1, no confabulated rule.** An answer cites a rule that exists.
- **RL-2, honest abstention.** When the rules don't cover the question,
  the answer says so and stops, rather than padding with adjacent rules.
- **RL-3, no dead law.** No answer is grounded in a rule no longer in
  force. `dead_law_sections()` is the mechanism.

One honest limitation on RL-3: dead law is dropped at *section*
granularity. Chunk-level retrieval cannot surgically excise one rule's
sentence from a chunk it shares with neighbours, so an amended section is
dropped wholesale and the superseding decision — which lives in its own
kept section — is relied on instead. Un-amended provisions co-located in a
dropped section are lost with it. The precise fix is sub-chunk
(atom-span) filtering, and it is not built yet.

## A worked corpus to test against

`sovereign-recipes/maple-house/` is a seeded charter-plus-amendments
corpus with planted ground truth: a founding Charter of numbered Articles
and dated house-meeting Decisions that amend it, including three genuine
cross-section conflicts and one decoy that shares vocabulary without
conflicting. `truth.json` is exhaustive.

It is small, it is readable in a sitting, and its structure is the same
shape as a constitution with amendments. Point your integration at it
before you point it at a real corpus — a detector that flags the parking
decoy as a conflict with the overnight-guest rule is telling you something
you want to know early.

`sovereign/bench/governance/` holds the two lanes that gate this: a
precision/recall detector lane over tension edges, and an answering lane
carrying the three red lines.

---

## Not built yet — the served tier

**Nothing in this section exists. Do not build against it.** It is
recorded here so the gap is visible to anyone weighing the five tiers,
and so the test that would settle it is written down before the work
starts rather than after.

The five tiers above vary by *depth* — how much of our code you run.
They all assume you run it yourself, in your own process. The missing
axis is *transport*: a governance server other services can call.

### The shape

One small binary placed beside what you already run. It takes two
configuration values — an atlas directory and a `base_url` — and serves
governance over HTTP. The `base_url` points either at your own
OpenAI-compatible endpoint (llama.cpp, vLLM, SGLang, TGI) or at a
sovereign daemon, which is the same `providers.toml` seam Tier 3
already uses.

This is a small binary rather than a platform for one reason: the fold
is pure, so most of what a server would serve is file-backed pure
functions, and the only two operations needing a model are extraction
and `ask`.

Four topologies fall out of that:

| Topology | Models run | Corpus lives | We hold |
|---|---|---|---|
| Sidecar, your models | Your endpoint | Your network | Nothing |
| Sidecar, our daemon | Local daemon | Your network | Nothing |
| Hosted | Ours | Ours | Everything |
| Split | Your endpoint | Your network | Atlas + oplog only |

The split topology is the one worth designing for. Extraction runs where
the documents are — which the egress boundary already makes the default
posture — and only the reader runs elsewhere. In every topology,
including the hosted one, the pure fold means you can export the oplog,
replay it offline with no model, and get the identical answer. Exit is a
file copy.

### What is missing

Stated plainly, because the gap is larger than the shape suggests:

- **There are no governance HTTP routes.** Not incomplete ones — none.
- **There is no container story.** No Dockerfile or compose file exists
  anywhere in the repo.
- `sovereign-server` is build-from-source rather than one of the shipped
  binaries, and its `[[inference.backends]]` block is experimental with
  unsettled key names.
- `~/.svrnmesh/` is still not a contract, and the oplog lives there.
- Storage is per-corpus files on a filesystem, with no object-store
  backend.

### One prerequisite, in the right order

The adjudication verbs are currently implemented **twice** — once in the
CLI (`govern_cmd/resolve.rs`) and once in the desktop
(`governance_commands.rs`), carrying the same two-op logic. A server
written against either would be the third copy of one decider, which
ARCH_PRINCIPLES §10.6 forbids and the smell table names.

So the order is: promote the handlers into a governance substrate crate
that the desktop, the CLI, and a server all depend on — *then* add
routes. Reversed, the third copy becomes the foundation every later
surface is built on.

### The test that settles it

One falsifiable check, registered now:

> A machine with no models installed and no sovereign install drives the
> **entire** governance loop against Maple House over HTTP with `curl` —
> seed, list tensions, resolve one, accept another, dismiss the decoy,
> and ask a question whose correct answer depends on a supersession
> recorded earlier in the same session.

Pass and the served tier exists; the rest is packaging. Fail and the
result names which leg is missing rather than yielding a partial credit.
The existing two bench lanes still gate detector quality and the red
lines — this test adds transport, not new instruments.
