# A Self-Hosted, Federated, Locally-Grounded AI Assistant — Requirements Specification

**Status:** clean-room specification, derived by reverse-engineering a working
system. **Audience:** an implementation team with no access to the original
source. **Purpose:** state *what the system must do and must never do*, in
terms that admit many implementations.

---

## 0. How to read this document

### 0.1 What this is

This is a requirements specification reconstructed from a system that exists
and works. Every requirement below was earned: it describes either a capability
the system delivers, or a constraint that was written into the system after a
specific failure. Where a requirement looks unusually specific or unusually
paranoid, that is why.

### 0.2 What is deliberately absent

No programming language, framework, library, database, wire protocol version,
file path, module boundary, type name, function name, environment-variable
name, or port number appears in this document except where it is genuinely a
requirement (for example, "must be compatible with the prevailing third-party
chat-completion HTTP convention" is a requirement, because interoperability
with unmodified third-party clients is a product goal; *which* HTTP framework
serves it is not).

Decomposition is likewise absent. The domains in Part II are a *reading* order,
not a module structure. A conforming implementation may organise itself however
it likes.

### 0.3 Conformance language

- **MUST** / **MUST NOT** — a conformance requirement. A build that violates one
  is non-conforming even if everything else works.
- **SHOULD** — strongly expected; deviation requires a stated reason.
- **MAY** — permitted latitude.
- **INVARIANT** — a property that must hold at all times, enforced
  structurally (§2.5), not by convention or review.
- **BAR** — a falsifiable acceptance threshold. A bar without a measurement
  procedure is not a bar.

### 0.4 Rationale blocks

Requirements marked **⟨why⟩** carry the failure that produced them. These are
the most valuable content here. A team that reads only the MUSTs and skips the
⟨why⟩ blocks will rebuild the same bugs, because most of these failures are
*plausible-looking successes* — well-formed output, exit code zero, no error
logged, and wrong.

### 0.5 Vocabulary

Terms in **bold small caps on first use** are defined in Appendix A. The
vocabulary is offered so requirements can be stated precisely; it does not
prescribe naming.

---

# PART I — CONTEXT

## 1. Product thesis

### 1.1 The problem

General-purpose assistants are remote, unaccountable about what they know, and
opaque about where an answer came from. Local assistants are private but weak
and knowledge-poor. This system's thesis is that a private, locally-hosted
assistant can be made *epistemically trustworthy* — able to answer from sources
it can name, to say when it cannot, and to prove it — and that several such
machines, cooperating voluntarily, can serve models and knowledge that no one
of them could serve alone.

Three properties define the product. All three are load-bearing; dropping any
one produces a different product.

1. **Sovereignty.** Content and computation stay on machines the user
   controls. Nothing leaves the machine unless the user opts in, per channel,
   and every outbound channel is enumerable and individually disableable.
2. **Groundedness.** An answer is a claim *plus its evidence*. The system MUST
   be able to say which source supports which sentence, MUST refuse to
   fabricate figures, and MUST abstain audibly rather than answer plausibly
   from nothing.
3. **Federation without a centre.** Peers form voluntary trust rings. There is
   no master node, no registry service, no account server. Membership is
   symmetric and revocable, and every node runs identical software.

### 1.2 Actors

| Actor | Description | Primary needs |
|---|---|---|
| **End user (non-technical)** | Runs a desktop application. Never opens a terminal. | Ask questions of their own documents; add sources; know where answers came from; recover from failure unaided |
| **Operator** | Runs the daemon deliberately; may run headless. | Lifecycle control, configuration, diagnostics, deployment |
| **Peer node** | Another installation, member of the same trust ring | Serve and consume inference and knowledge under fair rules |
| **Guest** | Holds a time-limited grant from a member; is not a member | Bounded access to a named capability for a bounded window |
| **Third-party client** | An unmodified tool speaking a common inference API | Point at this system and work |
| **Source author** | A domain expert (journalist, lawyer, researcher) publishing an ingestion specification | Author, test, and share a source definition without writing code |
| **Autonomous agent** | Software driving the system's own maintenance surfaces | Query code structure, record decisions, coordinate with peers |

### 1.3 Deployment shapes

The same system MUST support all of the following without forks:

- **S1 — Solo desktop.** One machine, one user, GUI only. No network peer.
- **S2 — Solo headless.** A long-running background service on one machine,
  driven by CLI and HTTP.
- **S3 — Attached desktop.** GUI attached to a separately-running background
  service on the same machine, sharing one port and one state root.
- **S4 — Trust ring.** Several machines on a LAN or a private overlay network,
  pooling inference and knowledge.
- **S5 — Multi-tenant host.** One machine serving several isolated principals
  over the network, with per-tenant knowledge and document isolation.
- **S6 — Thin client.** A mobile or remote client with no local model, no local
  knowledge, and no local reasoning, driving a host over the network.
- **S7 — Air-gapped install.** A shared machine with every outbound network
  capability compiled out or switched off, with an auditable statement of what
  can still leave.

### 1.4 Scope boundaries

**In scope:** knowledge acquisition and indexing, retrieval, grounded
synthesis, local model hosting, peer federation, authoring surfaces, the
system's own maintenance tooling, and the evaluation apparatus that proves all
of it.

**Out of scope:** training or fine-tuning models; a hosted service; user
accounts; billing; any centrally-operated component.

---

# PART II — CROSS-CUTTING REQUIREMENTS

These govern every domain in Part III. They are stated once here and are not
repeated at each site. A conflict between a cross-cutting requirement and a
domain requirement resolves in favour of the cross-cutting one.

## 2. Cross-cutting requirements

### 2.1 Epistemic honesty (X-EH)

**X-EH-1 (MUST).** Absence MUST be reported, never defaulted. When a value is
unavailable, the system MUST propagate a typed *absence* distinguishable from
every legitimate value. It MUST NOT substitute a plausible default, a zero, an
empty collection, or a neutral score.
⟨why⟩ A scoring term whose input was missing was silently treated as neutral
across every peer of every fleet. Six months of tuning discussion took place
about a term that was, in production, a constant.

**X-EH-2 (MUST).** A check MUST have four possible verdicts, not two:
**passed**, **failed**, **could-not-judge**, **never-ran**. Any surface
reporting the health of a check MUST be able to express all four. Collapsing
*could-not-judge* or *never-ran* into either *passed* or *failed* is
non-conforming.
⟨why⟩ A gate that never ran and a gate that passed are indistinguishable on a
two-valued display, and the difference is the whole value of the gate.

**X-EH-3 (MUST).** A zero-work run MUST NOT report success. A test selection
that matched nothing, a scan that examined nothing, a fan-out that reached
nothing, and an evaluation over an empty set MUST each exit with a distinct
non-success status naming the empty scope.

**X-EH-4 (MUST).** Where the system substitutes one thing for another — a
different model, a different source, a fallback path — it MUST either refuse,
or name the substitution in its own output. Silent substitution is
non-conforming.

**X-EH-5 (MUST).** An empty "what was lost" report MUST mean *nothing was
lost*, and MUST NOT be reachable by a path that failed to look. Every loss
detection site MUST record what it was asked for even when it cannot report
what happened to it.
⟨why⟩ A question about a source reachable only from a peer came back
confidently answered from an unrelated local source, because the fan-out
discarded the name of what it failed to reach before building the response.

**X-EH-6 (MUST).** A single observation MUST NOT be reported as a measurement.
Any claim of the form "X is faster/better than Y" MUST be accompanied by the
number of trials, the spread, and the conditions; a difference within the
measured noise band MUST be reported as *not distinguishable*, not as a win.

**X-EH-7 (MUST).** An instrument MUST be validated before its result is
believed. An arm of an experiment MUST first be proven *connected* — shown able
to move the number at all — before a null result from it is reported.

**X-EH-8 (MUST).** A change to a judge, scorer, threshold, or veto MUST be
reported in **both** directions: the cases it newly catches and the cases it
newly lets through. Reporting only the direction the change was designed to fix
is non-conforming.

**X-EH-9 (MUST).** Where the system declines or refuses, it MUST name the
reason as a typed value, not as prose, and the reason MUST be distinguishable
by machine. Distinct refusal causes MUST NOT collapse into one.
⟨why⟩ "This request is malformed" and "this source does not contain that" were
one indistinguishable refusal; a caller's own invented parameter was read three
times running as a coverage limitation of the data.

### 2.2 Provenance and attribution (X-PR)

**X-PR-1 (MUST).** Every retrievable unit of content MUST carry, as a required
non-defaultable property, (a) whether it was **acquired** from a source the
system indexed or **manufactured** by the system itself, and (b) its
**granularity** — whether it is verbatim source text or a derived summary.

**X-PR-2 (MUST).** Manufactured content MUST NOT be quotable as a citation. It
MAY orient an answer; it MUST NOT ground one.

**X-PR-3 (MUST).** The acquired classification MUST have no public
constructor. It MUST be stampable only by a small, enumerable set of **doors** —
the specific code paths that genuinely observe the fact — and that door set MUST
be pinned by an automated check that fails when a new door appears.
⟨why⟩ While provenance was a string in an untyped property bag, a missing key
and a misspelled key were the same value, and system-manufactured text was
indistinguishable from indexed source text.

**X-PR-4 (MUST).** When content crosses a trust boundary, its custody class MUST
be joined with the receiving node's own fact about that boundary at maximum
restrictiveness. A peer MUST NOT be able to talk its own content down to a
looser class, and a missing granularity from a peer MUST read as the *refusing*
value.

**X-PR-5 (MUST).** An answer MUST record, per turn: which route was taken,
which sources were consulted, which were unreachable, which model served it,
which verification steps ran, and what each concluded. This record MUST survive
persistence and be re-readable after a restart.

**X-PR-6 (MUST).** A citation MUST identify *where* in a source it came from at
the finest granularity the source structure supports — not merely which
document. Where a source declares internal structure, the citation MUST name the
structural unit.

**X-PR-7 (MUST).** Any two mechanisms that decide whether a quotation is
verbatim MUST agree, or one of them MUST be removed. Where a strict and a
tolerant check both exist, the strict one MUST govern what is *labelled*.
⟨why⟩ A quotation was labelled with a confident source location by a tolerant
check, then demoted by a stricter one running later, and shipped asserting
provenance that another checker had just refused.

### 2.3 Observability (X-OB)

**X-OB-1 (MUST).** Every decision that changes what the user receives MUST be
visible at a diagnostic verbosity level, without a rebuild and without a code
change. A branch of production code with no diagnostic event is non-conforming.

**X-OB-2 (MUST).** Diagnostic output MUST reach the operator in every
deployment shape, including a detached background service with no attached
terminal. A diagnostic channel that works only in the foreground is not a
diagnostic channel.
⟨why⟩ A subsystem's arming line, its results, and both of its failure paths
were all discarded by the deployed service for weeks, while every health
surface reported normal.

**X-OB-3 (MUST).** Where a process is composed of stages, the system MUST be
able to attribute its own wall-clock time across those stages, and MUST emit a
**residual** — time claimed by no stage — including when the residual is zero.
An unattributed stage MUST therefore surface as unexplained seconds rather than
disappear.

**X-OB-4 (MUST).** Attribution MUST be recorded from *observed execution*,
never inferred from configuration. Recording "the fast path was enabled" is not
recording that the fast path ran.

**X-OB-5 (MUST).** Where the system defers, waits, retries, or declines, it
MUST emit an event at the moment it does so, carrying enough state to reconstruct
why. A silent wait is non-conforming.

**X-OB-6 (SHOULD).** Any expensive repeated operation SHOULD carry a mechanism
to answer "did this reuse prior work?" by comparing identities, not by observing
that a duration was short.

### 2.4 One decider (X-SD)

**X-SD-1 (INVARIANT).** Every threshold, formula, schema, storage layout,
identity key, readiness predicate, and eligibility rule MUST have exactly one
implementation. Two implementations of one rule is a defect regardless of
whether they currently agree.
⟨why⟩ This is the single highest-frequency defect class in the source system.
Documented instances include: a readiness check implemented once as "does the
index report itself complete" and once as "does a directory exist"; a storage
layout hand-spelled at 147 sites with no constant anywhere; an eligibility
filter and its user-facing disclosure implemented as two hand-mirrored
conditionals; a preview tool and the operation it previews computing different
answers to the same question.

**X-SD-2 (MUST).** Where a decision must be visible in two places (a preview
and its execution; a filter and its disclosure; a producer and its consumer),
both MUST call the same implementation. Copying a schema, even verbatim, creates
a second decider that will drift.

**X-SD-3 (MUST).** Identity MUST derive from essence, never from an address, a
counter, a row number, a sequence number, or a name prefix. Two things that are
the same MUST compute the same identity from different machines at different
times.

**X-SD-4 (MUST).** Where a declared property (a tool's parameters, a source's
schema, a capability's shape) is both *presented* to a component and *enforced*
against it, the presented form MUST be derived from the enforced form, not
authored beside it.

**X-SD-5 (MUST).** A per-item attribute set MUST be a total record with no
default. Adding a new item MUST fail to build until every attribute is supplied.
⟨why⟩ Per-item attributes lived in ten separate dispatch tables across four
components, three ending in a catch-all. A new item compiled clean and silently
inherited the wrong latency class, the wrong output budget, and no operation
binding at all.

### 2.5 Structural enforcement (X-ST)

**X-ST-1 (MUST).** An invariant MUST be encoded so it cannot be forgotten. The
acceptable mechanisms, in order of preference: make the illegal state
unrepresentable; make the illegal construction fail to compile; make the illegal
state fail an automated check on every run. A comment, a convention, a review
checklist, or a note in documentation is not an enforcement mechanism.

**X-ST-2 (MUST).** A generative model MUST NOT be asked to guarantee a
behaviour that code can enforce. Where a prompt asks a model not to do
something, and the consequence of it doing so is a correctness failure, a
deterministic guard MUST exist at the exit seam regardless. Asking is not a
guarantee.
⟨why⟩ Every instance of "the schema already asks it not to" in the source
system was followed, on measurement, by an instance of it doing so anyway.

**X-ST-3 (MUST).** Where a set of code paths must all route through one funnel,
that constraint MUST be checked mechanically — for example by a build-time
assertion that no member of a region reaches the underlying primitive directly.
⟨why⟩ The first run of such a check found a path that had escaped the funnel.

**X-ST-4 (MUST).** A liveness bound MUST NOT be a configuration option. A
timeout, deferral cap, or budget that exists to prevent an indefinite wait MUST
be compiled in, and configuration MUST be able only to tighten it, never to
weaken or remove it.
⟨why⟩ A "stand aside for foreground work" predicate had no memory of how long
the asker had already stood aside; a health probe firing at half the yield
window pinned it true forever, and three consecutive end-to-end runs died with
no diagnosis.

**X-ST-5 (MUST).** Where two channels must not be confused (per-request output
versus broadcast output; two trust surfaces of the same API), they MUST NOT
share a type. The confusion must fail to compile, not fail a review.

### 2.6 Degradation and failure semantics (X-DG)

**X-DG-1 (MUST).** A build failure, a link failure, a bad configuration, and a
crashed subprocess MUST all be reported as failures by any gate that claims to
have checked the thing they prevented. A gate that counts only one class of
diagnostic and reports the rest as clean is non-conforming.

**X-DG-2 (MUST).** Results that cannot be attributed to the run that requested
them MUST be reported as unattributable, not returned. Where a shared artifact
can be overwritten by a concurrent run, the reader MUST detect this and refuse.

**X-DG-3 (MUST).** A stale artifact MUST NOT be able to masquerade as a fresh
one. Any cached, exported, or reported artifact MUST carry a freshness stamp,
and the reader MUST report age. Where an artifact is regenerated, the previous
one MUST be removed *before* the run, so that "no artifact" unambiguously means
"this run produced nothing".

**X-DG-4 (MUST).** Fail-closed by default at trust boundaries; fail-open only
where a stated analysis shows failing closed is worse, and then only with the
degradation surfaced to the user.

**X-DG-5 (MUST).** A degraded mode MUST be visible in the product, not
inferable from a configuration file. When a capability is running in a reduced
form, the surface using it MUST say so.

**X-DG-6 (MUST).** Recovery MUST be a user-reachable state, not a restart. A
crashed subordinate process MUST leave the interface alive and offer a
reconnect, rather than taking the interface with it.

**X-DG-7 (MUST).** Retry and restart policies MUST count *consecutive*
failures and reset only on positive evidence of health (a generation that
survived a stated duration). A sliding wall-clock window is non-conforming: it
is unreachable for any subordinate whose failure cycle is longer than the
window.
⟨why⟩ A very large model took minutes to load, served briefly, and died. A
wall-clock crash-loop window never triggered, and the restart loop exhausted
accelerator address space and took the whole application down.

**X-DG-8 (MUST).** Backoff MUST be floored at the previous generation's
*measured* start-up cost, so an expensive subordinate can never re-enter
start-up back-to-back.

### 2.7 Extension shape (X-EX)

**X-EX-1 (MUST).** A closed set of alternatives MUST be represented as an
exhaustive typed set with no catch-all, such that adding a member fails to
build until every site handles it. Control flow over a closed set MUST NOT be
driven by string comparison.

**X-EX-2 (MUST).** An open set of alternatives (extensions, plugins,
user-authored sources, third-party tools) MUST be a runtime registry with an
explicit unknown-member policy, and the unknown-member behaviour MUST be
specified and tested — not left as whatever the dispatch happens to do.

**X-EX-3 (MUST).** Open natural-language classification MUST NOT be performed
by keyword lists. It MUST use a learned representation with a calibrated
decision boundary and a measurable abstention rate.

**X-EX-4 (MUST).** Configuration that describes *what*, rather than *how*, MUST
be data — declarative, external, versioned, and loadable — not code. The set of
things that must be data includes at minimum: source ingestion specifications,
tool declarations, retrieval pipeline composition, role profiles, model
selection per hardware class, and layer/dependency policy.

**X-EX-5 (MUST).** Where a new extension type is added, the cost MUST be one
declaration plus one implementation, with no edits to any dispatch site. If
adding an extension requires editing a switch, the extension point is
mis-shaped.

**X-EX-6 (SHOULD).** Every extension point SHOULD ship a template that is
itself validated by an automated check, so the scaffold cannot rot into a wrong
example. A paragraph of documentation does not reduce the cost of reaching for a
shared surface; a working block already wired to it does.

### 2.8 Privacy and data residency (X-PV)

**X-PV-1 (MUST).** No content leaves the machine except through channels the
user has enabled. The complete set of outbound channels MUST be enumerable by
the system itself and each MUST be individually disableable.

**X-PV-2 (MUST).** Outbound capability MUST be removable at build time, not
only switchable at runtime, so an air-gapped deployment can be *audited* rather
than *trusted*.
⟨why⟩ Three tools reached the public internet on ordinary conversational turns
with no configuration switch. The permission system did not gate them because
the permission check was consulted at exactly one call site, and the
conversational path did not pass through it.

**X-PV-3 (INVARIANT).** Content classified as personal or local-scope MUST NOT
be shareable to peers by any path. This MUST be enforced at more than one layer
— at minimum: at the source declaration, at ingestion, and at every egress
seam — so that no single mistake is sufficient to leak.

**X-PV-4 (MUST).** Sources marked sensitive MUST be excluded from ambient
(unrequested) retrieval while remaining explicitly askable.

**X-PV-5 (MUST).** A user's own usage telemetry MUST NOT be replicated to
peers. Contribution accounting (what I did *for others*) and activity
accounting (what I did *for myself*) MUST be separate stores with different
replication policies.

**X-PV-6 (MUST).** Diagnostic reports MUST be local files the user reads before
sending. No automatic upload of any diagnostic, crash record, or usage record.
A report MUST state its own contents, derived from what is actually in it.

**X-PV-7 (MUST).** Where the system records behavioural telemetry locally, the
record type MUST be structurally incapable of carrying content — no free-form
field, no untyped value — so that no document text, path, or user input has a
channel into the file. The consent surface MUST be able to print the complete
field list of the file it writes, collected from the written bytes rather than
from a maintained list.

**X-PV-8 (MUST).** Where content is temporarily entrusted to a peer for
processing, the grant MUST be explicit, scoped to named peers, time-limited,
revocable, and MUST NOT mutate the content's standing sharing posture.
Teardown MUST be verified by at least two independent paths.

### 2.9 Configuration and defaults (X-CF)

**X-CF-1 (MUST).** Every configuration knob MUST be declared in one registry
carrying its name, default, purpose, and lifecycle status. An undeclared knob
MUST fail an automated check.

**X-CF-2 (MUST).** Every capability shipped disabled or dark MUST carry a
ledger entry stating the falsifiable condition that would enable it and a
review-by date. Shipping dark without that entry is non-conforming.
⟨why⟩ Without this, disabled capabilities accumulate indefinitely and nobody
can distinguish "not yet proven" from "abandoned".

**X-CF-3 (MUST).** Path and location derivations MUST come from single
accessors, with hand-rolled derivations mechanically banned.

**X-CF-4 (MUST).** A configuration field and the runtime state it purports to
describe MUST NOT be allowed to diverge silently. Where a runtime policy can
override a configured value, any consumer asking "what is in effect" MUST read
the effective value, never the configured one.
⟨why⟩ A grant-minting operation decided "can an outsider reach me" by reading
the configured bind address. An encryption policy had overridden that bind, so
the mint produced links that could never connect.

### 2.10 External contract durability (X-CO)

**X-CO-1 (MUST).** Artefacts authored outside the repository — user-written
source specifications, third-party integrations, published data bundles — MUST
keep loading indefinitely. The compatibility discipline MUST include: new fields
default; renamed fields keep aliases; removed alternatives get a deprecation
path that names the replacement; a schema version gates only changes readers
must opt into; and a regression fixture suite pins old forms.

**X-CO-2 (MUST).** Any value a third party can observe and depend on — a
reference code shown to a user, a wire field name, a public identifier — is a
contract. Changing it MUST be treated as a breaking change.

**X-CO-3 (MUST).** Where a capability's data is published for others to
download, the publish operation MUST refuse a bundle with a defect the
downloader cannot repair. Publish is the last point at which such a defect is
still the publisher's.

---

# PART III — CAPABILITY DOMAINS

## 3. D1 — Knowledge acquisition

### 3.1 The pipeline

**KA-1 (MUST).** The system MUST provide a staged acquisition pipeline with, at
minimum, these separable stages: **acquire** (obtain bytes), **extract** (bytes
→ documents), **filter** (drop documents by predicate), **segment** (documents →
retrievable passages), **embed** (passages → vectors), **index** (persist for
retrieval).

**KA-2 (MUST).** Each stage MUST be an extension point (X-EX-2) with a
registry, so a new acquirer, extractor, filter, or segmenter is an
implementation plus a declaration.

**KA-3 (MUST).** A whole pipeline MUST be configurable by a single declarative
**source specification** (X-EX-4) that names the stage implementations and their
parameters. The specification MUST be authorable by a domain expert with no
programming knowledge.

**KA-4 (MUST).** The pipeline MUST NOT itself embed text or run a generative
model. Both capabilities MUST be *injected* by the caller as opaque
functions, so the acquisition layer has no dependency on any inference runtime
and can be driven by a local engine, a remote endpoint, or a test stub without
modification.

**KA-5 (MUST).** Built-in acquirers MUST cover at minimum: bulk archive
download, dataset-repository fetch, local file and directory, parameterised REST
API, and web crawl. A runtime-registered custom acquirer seam MUST exist for
sources needing bespoke logic (multi-step resolution, authentication, format
negotiation).

**KA-6 (MUST).** Built-in extractors MUST cover at minimum: structured markup
archives, line-delimited and nested JSON, markup documents with and without
section detection, delimited text, columnar binary formats, plain text, source
code, electronic mail with attachments, conversational exports from at least two
major assistant vendors, and a dispatcher for binary payloads that routes by
content type to sub-extractors.

**KA-7 (MUST).** Filters MUST be composable with explicit any/all semantics.

**KA-8 (MUST).** Segmenters MUST cover at minimum: paragraph, sentence,
fixed-size, semantically-clustered, whole-document pass-through, and at least
one turn-aware segmenter for conversational content.

**KA-9 (MUST).** At least one extractor MUST be **deterministic and typed**:
producing structured entity records from structured input (tabular, columnar)
with no model in the loop, so that a class of corpora exists whose facts are
never model-originated.

### 3.2 Source specifications as an open platform

**KA-10 (MUST).** The specification schema MUST be open. A domain expert MUST be
able to author a specification for a new source, test it, and publish it,
without touching the system's code.

**KA-11 (MUST).** Specifications MUST support declared **parameters** (typed,
with defaults and required flags) resolved and validated at install time — so
one specification serves many instances (one per company, per jurisdiction, per
dataset slice).

**KA-12 (MUST).** Where a specification produces one installed instance at a
time, installing a second MUST refuse or replace *explicitly*, naming both, and
MUST record which instance is resident. Two paths that can produce instances of
the same family MUST use disjoint identifier namespaces.

**KA-13 (MUST).** The parameterised REST acquirer MUST support URL templating,
several pagination strategies, following document links discovered in
responses with bounded concurrency, and rate limiting.

**KA-14 (MUST).** Section-extracting extractors MUST emit a **miss report**
sidecar naming what they expected and did not find, so a specification author
can debug a source they cannot see.

**KA-15 (MUST).** The system MUST provide a specification **catalogue** with:
a bundled offline snapshot (so the system works with no network), an optional
live refresh, integrity verification of fetched entries, and a resolution order
of local override → remote → bundled. There MUST be exactly one authoritative
copy of the catalogue in the source tree.

**KA-16 (MUST).** A specification lifecycle surface MUST exist: validate, test,
publish, list.

**KA-17 (MUST).** A **verdict ladder** MUST separate judgment-free stage
observation from pass/fail policy, so the observation layer is reusable and the
policy layer is the only place a bar lives.

**KA-18 (SHOULD).** An assisted authoring loop SHOULD exist in which a model
drafts and iterates a specification against real fetched data, with checkpoints,
a decision log, and the ability to run trials.

### 3.3 Acquisition safety

**KA-19 (MUST).** Crawl safety MUST be hardcoded, not per-specification
configurable: robots directives honoured, a per-domain rate limit, an
identifying user agent, crawl scope enforced against the seed domain, and a
warning when a download materially exceeds its estimate.

**KA-20 (MUST).** Ingestion MUST be resumable. A long ingest MUST checkpoint
progress and resume without re-doing completed work.

**KA-21 (MUST).** Incremental update MUST be supported: a per-document revision
manifest, a diff producing additions/updates/deletions, a three-phase apply
(delete, then update, then add), and resumable progress.

**KA-22 (MUST).** Scope expansion MUST be possible in place: relax the filter
set, ingest only the newly-admitted documents, and rebuild derived indexes —
without re-acquiring or re-embedding what is already present. The filter set and
its signature MUST be recorded with the corpus so expandability is decidable.

**KA-23 (MUST).** Content-addressed binary payloads MUST be stored once, keyed
by content hash, with optional typed parsed-form caches beside the raw bytes and
an append-only ledger of what was stored. Every future binary-bearing vertical
MUST inherit this substrate unchanged rather than growing its own.

### 3.4 Sensitive and personal sources

**KA-24 (MUST).** Personal sources (a user's notes vault, a watched folder, a
document library) MUST be structurally local-scope and non-shareable by default
(X-PV-3).

**KA-25 (MUST).** Watched sources MUST be **read-only at the source**. The
system MUST NOT write into a directory a user has asked it to observe.

**KA-26 (MUST).** Multi-root sources MUST deduplicate by content, not by path.

**KA-27 (MUST).** Optional scanned-document text recovery MUST be available
in every deployment shape, not only the graphical one.
⟨why⟩ The capability existed but its installation was performed only by the
desktop application, so a headless operator could enable it and get nothing —
every scanned document silently recorded as "no text found".

---

## 4. D2 — Storage, retrieval and provenance

### 4.1 Index structure

**ST-1 (MUST).** Retrieval MUST be hybrid: dense vector similarity *and* lexical
keyword search over the same passages, fused into one ranking.

**ST-2 (MUST).** One corpus MUST occupy one self-describing on-disk location
with authoritative metadata recorded alongside the data.

**ST-3 (MUST).** The on-disk layout MUST have exactly one decider (X-SD-1) and
a mechanical ratchet preventing hand-spelled layout knowledge from
re-accumulating.

**ST-4 (MUST).** A **shard** — an index holding a contiguous identifier range —
MUST be structurally identical to a whole index. Search MUST NOT be able to tell
the difference. Three shard operations MUST exist: report statistics, extract a
range into a new index, merge N indexes into one.

**ST-5 (INVARIANT).** The pair *(corpus identifier, passage identifier)* is the
citation handle and MUST be globally unique. Enumeration of installed corpora
MUST deduplicate deterministically and MUST exclude out-of-band directories.

**ST-6 (MUST).** **A directory is not an installed corpus.** Readiness MUST be
decided by a positive completion signal from the index itself, at the canonical
location, by one decider shared by every reader (X-SD-1). In-flight ingests MUST
write to a distinct location that no reader mistakes for an installed corpus.
⟨why⟩ Status reporting answered "installed?" by testing directory existence.
It reported a zero-second-old ingest as installed, and reported partition
directories as corpora nobody could install or remove.

**ST-7 (MUST).** Readiness MUST be a three-or-more-valued answer (**ready**,
**building**, **absent**, and any further distinguishable states), never a
boolean.

### 4.2 Embedding compatibility

**ST-8 (INVARIANT).** The embedding model is a cross-node interoperability
contract. A corpus MUST record the model, dimensionality, pooling, and
normalisation used to build it. Opening with an incompatible model MUST fail
loudly, never coerce.

**ST-9 (MUST).** Nodes sharing a corpus MUST produce bit-compatible vectors.
Any operation that mixes vectors from two models MUST be impossible.

**ST-10 (MUST).** Any cache of embeddings MUST key the *instruction* used to
produce them into its identity, not only the model. A model fingerprint cannot
see an instruction change, and a cache that reports fresh while holding vectors
from a different semantic space is worse than a cold cache.

### 4.3 Index maintenance — the decay nothing reports

**ST-11 (MUST).** The system MUST detect and remediate the retrieval-quality
decay that accumulates in an incrementally-appended index.
⟨why⟩ Answering a query over an index that has been appended to since its last
optimisation means running the index *and* a linear scan over the appended
remainder, then merging. Nothing fails: results stay correct, no error is
logged, and every health metric reports well. It only gets slower —
continuously, in proportion to mutation rate. One corpus fed by a continuous
updater had reached a search latency 22× that of a static corpus of comparable
size before any maintenance existed.

**ST-12 (MUST).** Maintenance MUST have one implementation with two surfaces: an
operator-invoked command reporting before/after state, and an automatic
background sweep. The background sweep is the one that matters, because the user
who most needs a healthy index will never open a terminal.

**ST-13 (MUST).** The sweep MUST be **cheap-check, rare-act**: every cycle
performs only a metadata read per corpus, and does real work only past a stated
floor.

**ST-14 (INVARIANT).** Index rebuilding is **not idempotent**. Unconditional
repeated rebuilds add versions and remove none, degrading a healthy corpus.
The rebuild phase MUST therefore be gated on positive evidence that work is
needed. Ungated on a cadence, the healer is the leak.

**ST-15 (INVARIANT).** Compaction is non-destructive, so storage grows until
pruning. Pruning is destructive and irreversible: the operator surface MUST have
no default and MUST refuse a zero-retention prune; the automatic sweep MUST use a
retention window far outside any plausible in-flight reader.

### 4.4 Passage provenance

**ST-16 through ST-20** are stated in §2.2 (X-PR-1 … X-PR-5) and apply here in
full. Additionally:

**ST-21 (MUST).** Reading provenance MUST distinguish "what class is this"
(with an explicit refusing value for unknown) from "did any door record a class
at all" (with an explicit *nobody-stamped* value). A pool in which nothing is
stamped MUST leave provenance machinery *off*, not refuse the turn.

**ST-22 (MUST).** The set of things the system itself manufactures MUST be
enumerable and pinned by an automated check. Every manufacturer MUST be content
the process genuinely builds.

### 4.5 The passage → structure join

**ST-23 (MUST).** Where a source declares internal structure (chapters,
sections, articles), the system MUST maintain an explicit **join** from stored
passages to the structural unit each came from. This join is what allows a
citation to name a location rather than a document.

**ST-24 (MUST).** The join MUST be computed by locating each stored passage
within its source document. Because stored passage text is rarely a verbatim
slice of the source (ingestion prepends titles and reflows whitespace), the
locator MUST attempt several normalisations and MUST **reject ambiguity rather
than resolve it by taking the first match**. Unlocatable passages MUST be
reported, not dropped (X-EH-1).

**ST-25 (MUST).** Join status MUST be three-valued: **no structure declared**,
**structure declared but join missing**, **join present**. Conflating the first
two is what allows this to rot invisibly — no caller can distinguish a broken
corpus from a flat one.
⟨why⟩ The join was specified, three production readers depended on it, and it
was never written. On measurement, 9 of 1,788 local corpora had a populated
join. Every file-backed corpus had an empty one, and both benchmark corpora were
among them.

**ST-26 (MUST).** A repair path MUST exist to backfill the join on existing
corpora, resolving both the case where a corpus owns its passages and the case
where passages live in a parent corpus.

**ST-27 (MUST).** Publishing a corpus bundle MUST refuse an unjoined bundle
(X-CO-3). A corpus that declares structure and joins none MUST fail publication,
naming the offending corpora and the repair. A partial join is legitimate; an
override MUST print the same facts rather than taking a quieter path.

### 4.6 Retrieval composition

**ST-28 (MUST).** Retrieval orchestration MUST be **data**: an ordered list of
named steps executed by one runner, with one diagnostic event per step carrying
the passage count before, after, and the delta.

**ST-29 (INVARIANT).** *The intent decides HOW to answer — model tier,
expansion, synthesis shape. It never decides WHERE knowledge lives.* Which
knowledge sources exist is a property of the installation, not of the request's
classification. Every retrieval composition MUST therefore share one
evidence-gathering head.

**ST-30 (MUST).** Step lists MUST be pinned by tests, so reordering is an
explicit reviewed act rather than an accident.

**ST-31 (MUST).** Per-request variation MUST ride shared state, not divergent
code paths.

**ST-32 (MUST).** Retrieval cost MUST NOT be linear in the number of installed
corpora more than once per turn. A turn issuing several fan-outs MUST scope
every fan-out after the first to the sources that the first one *ranked* well.
**BAR:** per-turn retrieval wall time must be flat, within measurement noise,
across at least a 10× range of installed corpus count.

**ST-33 (INVARIANT).** *A scope drawn from presence rather than from ranking is
vacuous.* A per-source fan-out with no score floor returns something from every
readable source, so "produced a hit" means "the index opened". Any scoping,
gating, or admission decision MUST key on a *relevance* signal, and MUST run at
a position in the pipeline where that signal exists.
⟨why⟩ The first version of the scoping fix selected 50 of 50 corpora and
measured as an exact no-op. Separately, an injector granted itself 5–8 of ~30
evidence slots in 14 of 14 questions because its selector ranked on properties
of how well a source was *enriched* rather than what was *asked*.

**ST-34 (INVARIANT).** *Fix the position of a selector, not its predicate.* A
selector running before the ranking signal exists cannot be repaired by adding a
relevance predicate; two attempts to do so measured byte-identical results.

**ST-35 (MUST).** Where a step is permitted to inject content *after* the noise
floor (because what it injects has no lexical overlap the floor would survive),
that exemption MUST be paired with an explicit topical gate, because nothing
downstream can reject the injected content.

### 4.7 Unavailability disclosure

**ST-36 (MUST).** Retrieval can lose a source in at least two ways — locally
(not built, no vector index, incompatible embedding) and remotely (peer refused
or unreachable). Both MUST produce a typed record naming the source and a closed
enumerated reason.

**ST-37 (MUST).** The readiness predicate that *drops* a source and the
disclosure that *reports* it MUST be one implementation (X-SD-1).

**ST-38 (MUST).** The disclosure MUST be narrowed by the same sensitivity,
allow-list, and tenant-ceiling filters the surviving sources passed, so a
disclosure can never name a source the user disabled or another tenant's.

**ST-39 (MUST).** The marker MUST be appended **by code**, as a pure function of
the loss list, at the answer exit — not by asking the model to relay guidance.

**ST-40 (BAR).** A turn that lost nothing MUST render byte-for-byte identically
to a build without the feature.

### 4.8 Recency and collection-shaped corpora

**ST-41 (MUST).** Per-document recency MUST be **source-agnostic and emergent
from indexing**: one sidecar mapping document identity → last-indexed time,
stamped at the single point where every re-index path converges. Any source that
re-indexes content — a freshness updater, a watched-folder edit, an incremental
delta — MUST therefore make that content "fresh" with **no per-source code**.

**ST-42 (MUST).** The recency join key MUST ride on the retrievable unit, so
derived structures (graph nodes, summaries) can sort fresh-first without a second
lookup.

**ST-43 (MUST).** A corpus MAY be indexed as **one** searchable body while being
enriched **per member** (per article, per document), producing many sibling
member-scoped derived structures under one parent.

**ST-44 (INVARIANT).** Where such a corpus exists, exploration surfaces MUST
enumerate members carrying non-empty derived structure, and explorability MUST be
gated on **content count, never on the presence of a directory**.
⟨why⟩ That distinction is what keeps a parent corpus with an empty derived
structure from claiming a map it does not have.

**ST-45 (MUST).** Where nothing on disk carries a member's human title, a
derived title MUST be generated from its identifier rather than presenting the
raw identifier.

**ST-46 (MUST).** A corpus MUST be able to declare presentation metadata
(category, icon) that flows through its authoritative metadata onto the retrieval
summary — so a consumer reads category off the index without re-resolving the
source specification.

---

## 5. D3 — Enrichment

### 5.1 Multiple coexisting strategies

**EN-1 (MUST).** The system MUST support more than one enrichment strategy,
selected per corpus by declaration, and MUST have one document reconciling all
of them so that "enrichment" is never assumed to mean one thing. At minimum
three strategies must coexist:

- **Whole-corpus field analysis** — a multi-phase pass that analyses a corpus
  holistically (structure → clustering → alignment → tensions → open questions),
  parameterised by a pluggable **domain** encoding the epistemic conventions of a
  knowledge field.
- **Per-document typed graph** — extraction of a typed assertion graph
  (entities, claims, events, questions, positions, oppositions, argument
  reconstructions, configurations, assets) with a pluggable pipeline per content
  genre, exemplar-guided extraction, and phase-level caching.
- **Progressive tiers** — dense embeddings, then an entity graph with
  graph-propagated relevance, then a hierarchical summary tree built by recursive
  clustering and summarisation.

**EN-2 (MUST).** Enrichment providers that would create a dependency cycle MUST
be injected through a seam rather than depended on directly.

### 5.2 Phase caching and model identity

**EN-3 (MUST).** Each cached enrichment phase MUST record the model that
produced it, and MUST decline to reuse a phase produced by a different model. A
model swap MUST force recomputation rather than silently mixing outputs from two
models in one graph.

**EN-4 (MUST).** There MUST be exactly one helper constructing this cache
identity, so every read and write carries the same notion of identity.

### 5.3 Entity extraction

**EN-5 (MUST).** A dedicated span-typed entity extractor MUST be available,
generation-agnostic behind a seam so alternative implementations share one
persistence, deduplication, and provenance path.

**EN-6 (MUST).** The extractor that actually ran MUST be recorded per corpus,
with its model identity, threshold, and label set.

**EN-7 (MUST).** Where an alternative implementation is measured and rejected,
the selection knob MUST remain as the documented way to re-test it, and the
rejection MUST be recorded with its measurement (X-CF-2).

### 5.4 Hierarchical summarisation

**EN-8 (MUST).** A hierarchical summary tier MUST be retrofittable onto an
already-installed corpus *additively*, reusing existing passage embeddings, with
document-level resume.

**EN-9 (MUST).** Clustering thresholds MUST be **corpus-shape aware**. A
threshold tuned for one content shape MUST NOT be applied to another.
⟨why⟩ A conversation-tuned minimum-cluster floor applied to a per-file notes
corpus reduced half of a live vault to title-only synthetic nodes.

**EN-10 (MUST).** Query-time use of summary nodes MUST be served by a derived
index with a freshness gate and an exhaustive fallback.

**EN-11 (MUST).** An injected summary passage MUST carry a provenance handle
back to the evidence it summarises, and MUST be rendered to the synthesis step
under a label that identifies it as a derived summary — never under a label
implying it is a primary source.
⟨why⟩ Summary passages, being URL-bearing and lacking passage identifiers,
were rendered under a web-search label. This was a direct cause of fabrication.

### 5.5 Identity resolution and reconciliation

**EN-12 (MUST).** A multi-origin merge primitive MUST exist, operating on typed
entity records with provenance, producing a **reversible** operation log.

**EN-13 (INVARIANT).** Merge signals MUST be **identity-grade only** — exact
name folding, nickname and initial-surname forms, exact shared contact
identifiers or aliases, organisation-plus-role. Fuzzy cross-attribute matching
and bare-name aliasing MUST NOT be used.
⟨why⟩ Fuzzy signals chained thousands of records into one polluted cluster.

**EN-14 (MUST).** The candidate-pair scan MUST be blocked so it is sub-linear in
practice, not a full quadratic comparison.

**EN-15 (MUST).** A glassbox diagnostic MUST exist that explains, for any merge
decision, which signal fired and on what evidence.

**EN-16 (MUST).** A reversible operation MUST actually be reversible. Where an
operation log claims to support undo, the log envelope MUST carry the identity,
version, actor, and timestamp needed to find the matching forward operation.
⟨why⟩ A documented reversible split was unimplementable, because the log it
was written against carried no operation identity and no actor.

**EN-17 (MUST).** Where several append-only logs exist, they MUST share one
envelope carrying provenance, with each tenant contributing only its own act.

### 5.6 Governance and living rules

**EN-18 (SHOULD).** The system SHOULD support an event-sourced governance
capability over a corpus of rules: detecting tensions between rules, presenting
ranked open conflicts with both texts, and recording adjudications (resolve,
accept, or explicitly *not-a-conflict* — a distinct act from accepting).

**EN-19 (MUST, where EN-18 is implemented).** Adjudications MUST survive a
rebuild that renumbers graph identifiers. They MUST therefore record their
endpoint *rule pair*, not only an edge identifier, and MUST resolve **mootness**
— a conflict whose rule has been superseded is not open. Only a genuinely
dangling decision (whose rule text was edited away) MUST surface as an issue.

**EN-20 (MUST, where EN-18 is implemented).** Retrieval MUST drop passages from
superseded rules — **no dead law** — and the answer path MUST run the same
cite-or-abstain discipline as every other grounded surface.

### 5.7 Code as an enriched graph

**EN-21 (SHOULD).** A deterministic strategy SHOULD lift an indexed source-code
corpus into the same typed graph — content-hash nodes, generated summaries as
descriptions, bounded function and call edges — so that code becomes queryable
(multi-hop call chains) and patchable (incremental delta rebuild) through the
same surfaces as prose.

---

## 6. D4 — Local inference substrate

### 6.1 Residency

**IN-1 (MUST).** The system MUST host local models in named **residency slots**
by role — at minimum a fast/low-latency slot, a primary/high-capability slot, a
code slot, and an embedding slot — with lazy loading.

**IN-2 (MUST).** Slots MUST be able to alias one another so a single resident
copy serves several roles, and the resolution order for an alias MUST have one
decider.

**IN-3 (MUST).** Remote and hybrid providers MUST be supported behind the same
interface as local ones, so a deployment can mix local and remote capacity, with
failover.

**IN-4 (MUST).** Model selection per hardware class MUST be data (X-EX-4).

**IN-5 (MUST).** The system MUST report, per slot: which model is resident,
whether it is resident or cold, and whether it is running in a degraded
substitute form. **Cold is not an outage** and MUST be reported distinctly.

### 6.2 Serving a model larger than the accelerator

**IN-6 (SHOULD).** The system SHOULD serve models exceeding accelerator memory
by placing selected tensors on host memory.

**IN-7 (MUST, where IN-6 is implemented).** The local-fit gate MUST NOT charge
for bytes that never reach the accelerator. It MUST obtain the host-resident
byte count from the engine's own projection, and MUST NOT condition that
discount on a related-but-different setting.
⟨why⟩ Pairing the discount with an apparently-related placement flag was tried,
measured, and is wrong: it changed the mapping into an anonymous copy and
exhausted memory.

**IN-8 (MUST, where IN-6 is implemented).** A placement that happens to work
because of an unsupported-operation fallback is an **accident, not a
declaration**. It MUST be recorded as such, with the lever that would make it
structural named and its enabling condition ledgered (X-CF-2).

**IN-9 (SHOULD).** Where an architecture's prefill cost is bounded by a
per-token constant rather than by batch width, this MUST be established by
pre-registered sweeps before effort is spent optimising the wrong axis, and the
negative result MUST be recorded so it is not re-attempted.

### 6.3 Crash isolation

**IN-10 (SHOULD).** The system SHOULD support running a slot's computation in a
supervised child process, so that a fatal fault in the numerical engine kills
only the child while the host keeps serving its control plane, gossip, and
status surfaces, and observes the exit as an event it re-plans around.

**IN-11 (MUST, where IN-10 is implemented).** The value of the process boundary
is **crash isolation and the distributed case, not throughput.** A measured
result MUST be recorded: for a model that fits one accelerator, N process
replicas *lose* to in-process multi-sequence batching. The replica-pool path
MUST NOT be presented as a parallelism strategy.

**IN-12 (MUST, where IN-10 is implemented).** The child MUST speak a lossless
native protocol carrying the request verbatim, so constrained generation,
sampling modes, and allow-lists survive the boundary. A lossy translation is
non-conforming.

**IN-13 (MUST).** Where a distributed model is hosted in a child, the host MUST
withhold the model entirely from any in-process path, so nothing can lazily load
a second copy.

**IN-14 (MUST).** Where a host plans a distribution and a child executes it, the
**plan** MUST be shipped, not merely the participant list. A child that re-plans
against post-warm resource state will cut the work differently, miss every warm
cache, and fall back to bulk transfer.

**IN-15 (MUST).** A change in the participant set MUST be handled by kill-and-
respawn, not by in-place reload, unless in-place reload is proven safe against
the fault it exists to survive.

**IN-16 (MUST).** While a distributed cluster is re-forming, requests for
distributed-class work MUST fail fast with a typed unavailability and cascade to
alternatives — never silently fall back to a local load of a model far too large
for the machine.
⟨why⟩ Falling back to a local load of an 80–90 GB model by a path that looked
like resilience killed a working session.

### 6.4 Architecture compatibility gating

**IN-17 (MUST).** Where a model architecture is known to fault on a given
compute backend, the system MUST detect this **before loading weights** — from
model metadata alone — and MUST substitute a safe alternative discovered
alongside it, surfacing a non-fatal notice. Where no safe alternative exists,
start-up MUST fail with a clear in-application message rather than a native
crash.

**IN-18 (MUST).** A pre-load smoke test in a subprocess MUST guard the remaining
path, and a native crash MUST produce a durable, user-submittable record.

**IN-19 (MUST).** Crash records MUST be local-first and never auto-uploaded
(X-PV-6), with a redacted export the user can review and send deliberately.

### 6.5 Editor-facing completion

**IN-20 (SHOULD).** The system SHOULD serve inline code completion (fill-in-the-
middle ghost text) and **next-edit prediction** (after repeated similar edits,
propose the remaining sites as a tab-through queue).

**IN-21 (MUST, where IN-20 is implemented).** These are **two lanes on one
slot**. A failed capability probe for one lane MUST withhold only that lane,
leaving the other served.
⟨why⟩ A failed marker probe used to discard the whole slot, so a user on an
ordinary conversational model got no editing assistance at all.

**IN-22 (MUST).** Next-edit prediction MUST have a **deterministic rule lane**
(pure induction from observed edits, no model) as well as any model lane. Rule
kinds MUST render through one shared representation so that site finding, the
already-applied exclusion, thresholds, and the queue are shared — new inductions,
not new pipelines.

**IN-23 (MUST).** The objective MUST be **"most useful at each level of wrong"**,
not "maximise useful fires". A user does not accept a wrong edit. Any metric that
counts partial successes in full is rewarding fan-out and MUST be paired with a
precision metric over the individual proposals.

**IN-24 (MUST).** Site selection heuristics that are proven on some languages
MUST be an explicit allow-list, and adding a language MUST require a
measurement.
⟨why⟩ A syntax-aware site filter improved two languages and measurably
*harmed* a third.

**IN-25 (MUST).** All policy MUST live on the serving side, so editor
integrations stay thin.

**IN-26 (MUST).** An episode MUST be recordable as **metadata only** (X-PV-7),
joined to what the developer actually did with the suggestion — accepted,
dismissed, diverged, superseded. **Diverged MUST NOT be folded into dismissed**,
and an unreported episode MUST count as *unknown*, never as a dismissal
(X-EH-2).

**IN-27 (MUST).** Outcome reporting MUST add zero user-visible surface:
fire-and-forget, short deadline, every failure swallowed including an
older-server rejection.

**IN-28 (MUST).** The local record layer MUST be feature-agnostic: file layout,
rotation, size cap, retention, and a multi-level off-switch MUST live in one
generic mechanism with one enabling decider. A second record stream MUST be a
declaration plus types, touching no dispatch on stream names.

### 6.6 Capability advertisement

**IN-29 (MUST).** A model MUST publish one **capability claim** per kind of work
it does well, carrying at minimum a work-kind tag, an affinity, and a latency
class. The tag vocabulary MUST be standardised at its core with an open
extension namespace (X-EX-2).

**IN-30 (MUST).** Schedulers MUST score requests against claims using one
shared composed scoring function with protocol-level and per-scheduler
operational adjustments. There MUST be exactly one implementation of the
composition (X-SD-1).

**IN-31 (MUST).** Call sites MUST declare a **workload requirement bundle**, not
a slot literal. Fast-versus-primary MUST be an emergent scoring outcome, with
the local node treated as the degenerate one-node fleet.

---

## 7. D5 — Reasoning runtime

### 7.1 Shape of a turn

**RT-1 (MUST).** A turn MUST proceed: classify → decide policy → begin a
cancellable session → dispatch by classified intent → record provenance →
extract durable memory at conversation end.

**RT-2 (INVARIANT).** *The classifier emits facts; the runtime applies policy.*
Classification MUST be testable without a model, and thresholds MUST be
calibratable without touching the classification interface.

**RT-3 (MUST).** Policy decision MUST be a pure function of the classification
and a threshold set, producing a tier and a **move kind** — commit, propose, or
ask.

**RT-4 (MUST).** Every per-intent attribute MUST be a column of one total record
(X-SD-5): recorded name, wire slug, user-facing phrasings, trace label, default
capability and latency class, retrieval budget with and without evidence, output
budget floor, operation binding, and tool-catalogue filter. Adding an intent MUST
be a member, a row, and exemplars — and MUST fail to build if any column is
omitted.

**RT-5 (MUST).** Handler *dispatch* over the closed intent set MUST remain
explicit control flow (X-EX-1); only the *attributes* move to data.

### 7.2 Classification

**RT-6 (MUST).** Classification MUST be a **stack**: cheap learned pre-checks
first, then a coarse model pass, then a refining pass. The stack MUST be
assembled by exactly one wiring path that every surface calls, with an automated
assertion that every axis is wired.
⟨why⟩ The desktop and background service silently diverged from the benchmark
harness for weeks: benchmarks improved while shipped surfaces under-routed.

**RT-7 (MUST).** Classifier exemplars MUST be embedded in the deployed artefact,
so the stack works regardless of working directory or packaging layout, with
override paths for development.

**RT-8 (INVARIANT).** *An axis's embedding instruction must match what the axis
discriminates BY.* An axis separating **speech acts** (what the speaker is
doing) MUST use an instruction that encodes the act and discards the topic. An
axis separating **subject matter** (personal versus external; this thread versus
past threads; time-sensitive versus evergreen) MUST use a topical instruction.
Mixing them is not mis-tuning; it is inexpressible — no threshold pair exists.
⟨why⟩ An intent axis embedded under a retrieval instruction could not classify
its own hand-authored exemplars: leave-one-out accuracy 60.6%, negative margin
information, between-class scatter 12.3% of variance. Under a speech-act
instruction the same bank and scorer delivered 41–49% coverage at 88% precision,
against 0–9% before. Moving all axes to the new space at once regressed the
topical axes, and the failure was visible only because a live negative set
showed a true negative and a true positive 0.003 apart.

**RT-9 (MUST).** Where axes occupy different spaces, the per-turn embedding cost
MUST NOT grow: axes sharing a space MUST share one vector, and there MUST be one
decider mapping axis → space that returns *unknown* for an axis it does not know,
so calibration skips rather than guessing.

**RT-10 (MUST).** Thresholds are comparable **only within a space**. Moving an
axis between spaces MUST re-calibrate it; carrying a constant across is
non-conforming.

**RT-11 (MUST).** Coarse-pass parsing MUST recover a truncated verdict rather
than silently falling through to a default, and MUST do so only when the
recovery is unambiguous (exactly one label shares the prefix). A degrade MUST
print on a channel the harness enables.
⟨why⟩ On measurement, ~7% of model-decided turns were silently re-routed
because the parse failed and the fall-through arm chose a plausible default.
Raising the output budget did not stop the truncation.

**RT-12 (MUST).** Where a pre-check hard-commits a route above another
pre-check built to catch an adjacent case, the higher one MUST reject the
adjacent shape itself — the lower one can never see what the higher one claims.

**RT-13 (MUST).** A pre-check that commits a route MUST also commit any
dependent scope, or the intent alone will search the wrong universe.

### 7.3 Threshold calibration discipline

**RT-14 (MUST).** Scoring MUST be separated from gating on every axis, so a
threshold sweep is pure arithmetic over one embedding pass.

**RT-15 (MUST).** A calibration tool MUST sweep exhaustively with candidate
thresholds at **midpoints between observed scores** — never a random or linear
sample, and never a threshold placed *on* an observation (floating-point
subtraction moves the boundary, not the comparison).

**RT-16 (MUST).** The calibration bank MUST be authored to **fail somewhere**,
and MUST contain abstention cases. A bank with no abstention cases cannot
measure the failure mode that matters.

**RT-17 (MUST).** A margin floor MUST be clamped non-negative. An unconstrained
fit will happily propose a gate that fires when the *negative* class won.

**RT-18 (MUST).** The tool MUST flag an axis with too few cases in either class
and print "read the shipped numbers, not the fitted gate". Reporting a headline
on the same cases the gate was tuned on is non-conforming.

**RT-19 (MUST).** **The calibration tool MUST write no constant.** It names the
constant and the file and stops.

**RT-20 (MUST).** Per-case attribution MUST exist: a confusion count is never
enough, because the next question is always *which ones*. The bucketing rule MUST
have one implementation shared by the counter and the namer, so the listing can
never contradict the totals.

**RT-21 (MUST).** A missed case MUST be able to name **what it lost to**. Where
a score is a one-versus-rest maximum, the identity of the winner MUST be carried
through to the report and to the production diagnostic log. Where the positive
class is a centroid and no single exemplar is responsible, the field MUST be
honestly empty.

**RT-22 (INVARIANT).** *Grow the bank before moving the constant.* A fit against
a small bank can look strictly dominant and be wrong in both directions at once.
Any proposed constant move MUST be re-scored against a bank grown with cases
chosen to **break** it, not to confirm it.

**RT-23 (INVARIANT).** *After re-filing exemplars between classes, re-check the
abstain cases, not just the positives.* Consolidating two classes that shared a
neighbourhood inflates the margin of everything in it — margin is relative, and
the absorbed class was the runner-up holding it down. This can *create* a false
positive rather than relabel a pre-existing one.

**RT-24 (MUST).** Where an axis's ceiling is a property of the representation
rather than of the threshold, this MUST be established by controlled refutation
and recorded, with the control case kept so the refutation stays measurable.
⟨why⟩ Two plausible exemplar fixes were tested and produced byte-identical
results; the ceiling was topic dominance, and recovering coverage needs
per-class thresholds or topic normalisation, not more exemplars.

**RT-25 (MUST).** Score-distribution **drift** MUST be tracked: a dated snapshot
of the fitted state, and later runs diffing cushions and separation against it. A
regression MUST be *claimed* only when the encoder and the bank are both
unchanged (both recorded, the bank by content digest); otherwise deltas print as
evidence with a stated reason they are not attributable.

**RT-26 (MUST).** The comparability key for a drift baseline MUST be the exact
model artefact. Two quantisations of one model can be interchangeable *for
decisions* and still differ by several times the drift epsilon.

**RT-27 (MUST).** Where an accuracy metric has saturated, the reporting MUST add
what accuracy hides: **layer attribution** (which decisions the cheap layer owned
versus which woke an expensive call), per-class precision/recall, and ranked
confusions. Abstention MUST be measured where it exists — at the individual
gates — and MUST NOT be reported at a layer where it is unobservable.

**RT-28 (MUST).** Start-up embedding of static exemplars MUST be avoidable via a
pre-computed cache shipped with the artefact, validated against the live model
by a sentinel probe, with a pure no-inference freshness gate and a regeneration
command wired into the release process.

### 7.4 Planning and execution

**RT-29 (MUST).** A plan MUST be a directed acyclic graph of typed steps, with
at minimum: reason, tool call, user input, branch, reason-with-tools,
await-user-information, and delegate.

**RT-30 (MUST).** The one textual grammar shared between planner and executor
(step-output references) MUST be owned end-to-end by one component — emitter,
parser, and a test that the prompt describing it stays in sync.

**RT-31 (MUST).** The planner MUST be **masked to the tool surface**, not merely
shown it. Structured output MUST pin the tool identifier per branch and bind
that branch's parameters to the tool's own schema **verbatim**. Copying the
schema is a second decider (X-SD-2).
⟨why⟩ Watched live, same model and prompt: told to use a nonexistent tool, the
unconstrained planner emitted it; the masked planner could not.

**RT-32 (MUST).** A tool whose parameters are not a typed object MUST be
**refused** from the masked surface, never widened back to a free-form parameter
bag — a widened shape compiles, masks nothing, and leaves a plan looking
constrained while its arguments stay free.

**RT-33 (INVARIANT).** *The mask makes fabrication impossible, not honesty
automatic.* Closing an in-vocabulary hole sharpens the out-of-vocabulary one:
the masked planner picks the nearest *legal* identifier. Every such substitution
hazard MUST have its own guard at the execution seam.

**RT-34 (MUST).** Declared enumerations MUST be rendered into the planning prompt
**in full and never truncated**. A partial list biases the planner to the head of
it while the tool still rejects everything below the cut.

**RT-35 (MUST).** A bad parameter MUST be an *ok-valued refusal* carrying its
reason, not an error.
⟨why⟩ Measured: failing a bad parameter as an error made the executor replan,
drop a constraint, fail again, and answer from parametric memory with no tool
output at all. A dead tool step does not degrade the honesty machinery — it
deletes it.

**RT-36 (MUST).** Before a non-idempotent tool step runs, the executor MUST
write a durable *started* record keyed by a **content-derived** idempotency key,
flipping it to *completed* after the effect returns.

**RT-37 (MUST).** On resume: a *completed* record MUST cause the step to be
skipped with its recorded result; a *started-but-not-completed* record MUST halt
and surface for review rather than blind-replaying a side effect.

**RT-38 (MUST).** The idempotency key MUST be content-derived, not
(task, step-identifier) — so it matches across a replan that reissues the same
action under a new step identifier. Exactly-once MUST be proven in both
directions by test.

**RT-39 (MUST).** A **delegate** step MUST run a scoped tool loop in its own
context, with only a typed contract flowing back to the orchestrator — the
declared return fields plus an always-present anomalies channel. Raw
observations MUST stay in the worker's transcript. The firewall MUST be proven
by test.

**RT-40 (MUST).** Where two tool-invocation loops exist for different shapes,
the parser and schema projection MUST be shared (X-SD-1).

**RT-41 (MUST).** Execution MUST support best-of-N sampling with pluggable
selection, evaluation passes, and per-step permission and approval gating.

### 7.5 Tools

**RT-42 (MUST).** The system MUST provide, at minimum, tools for: local hybrid
search with coverage assessment; web search and single-URL fetch (both behind
X-PV-2 removability); direct corpus query; enriched-graph retrieval; map-reduce
document summarisation and analysis; shell, file, mail, and calendar actions
behind sandboxing and approval.

**RT-43 (MUST).** External tool servers MUST be integrable over at least two
transports, configured in one place, loaded by one shared loader that **every**
conversational surface calls.

**RT-44 (MUST).** An adapted external tool's descriptor MUST be enriched with a
synthesised example call derived from its input schema, so the planner reliably
emits a tool step rather than a reasoning step.

**RT-45 (MUST).** An external tool's **effect** and **idempotency** MUST be
inferred where undeclared, from its verb, so that a mutating call picks up the
approval gate and the replay ledger while a read does not.

**RT-46 (MUST).** The **declared half of a tool** — identity, behavioural
properties, parameter schema, worked examples, required permissions — MUST be
data (X-EX-4), with exactly one decider per tool. A declaration plus a handler
MUST be sufficient to register a tool with no boilerplate, and a declaration
delegating to an already-registered tool with defaults MUST need no code at all.

**RT-47 (MUST).** Tools whose descriptors are genuinely computed (from what is
installed, from a closed constant set, from an external asset) MAY keep coded
descriptors, and the exceptions MUST be enumerable.

**RT-48 (MUST).** Parameter validation MUST be derived from the declaration, not
hand-written per tool.

**RT-49 (MUST).** A file attached for a tool's use MUST be distinguishable from
a document attached for retrieval. The former binds a path to the turn; the
latter is ingested and the path discarded.

### 7.6 Memory

**RT-50 (MUST).** **Working memory** MUST be compressed every message into a
small bounded structure (current goal, facts, active documents).

**RT-51 (MUST).** What a long thread carries forward MUST be **constant-capacity
and independent of conversation length**, composed of at minimum: a rolling
visible window; a structured **conversation frame** with named sections; and
retrieval over dropped history.
**BAR:** total carried context must not grow with thread length.

**RT-52 (INVARIANT).** The frame MUST be **structured, not a prose blob**. Two
reasons, only one of which is cost: a blob must be rewritten to be updated, and
re-narration is where named entities get dropped; and a blob is not
*renderable* — "what do you remember about this conversation?" must be answerable
from sections.

**RT-53 (MUST).** Both memory channels MUST narrate themselves — memory being
read (with indices and best similarity) and memory being written — and recall
MUST be recorded in turn provenance so a verified recall is distinguishable from
a lucky parametric guess.

**RT-54 (MUST).** **Long-term memory** MUST be extracted at conversation end,
each item carrying confidence, creation time, and last use, with decay and
pruning below a threshold.

**RT-55 (MUST).** Memory embeddings MUST be persistent, computed on write, with
a model-swap guard: a stored embedding whose model identity differs from the
current provider's MUST be recomputed.

**RT-56 (SHOULD).** A hierarchical memory tier SHOULD exist with incremental
maintenance — descend, then attach / re-summarise / split / rebuild under
statistical triggers — with every trigger emitting a glassbox trace. Scope keys
MUST prevent a summary node from spanning two privacy scopes.

**RT-57 (MUST).** Where recall runs before routing, it MUST gate on a
mode-derived signal, never on a per-turn classification that is not yet
available.

**RT-58 (MUST).** Routing corrections MUST be remembered and fed back into
classification as negative examples.

### 7.7 Skills and taught behaviour

**RT-59 (MUST).** A **skill** MUST be a declarative bundle merging routing
hints, planning templates, prompt overrides, memory rules, and capability
requirements into the runtime.

**RT-60 (MUST).** Skills MUST carry a signature and a derived trust level, and a
skill declaring local-only privacy MUST short-circuit every remote path.

**RT-61 (SHOULD).** The system SHOULD let a user teach durative behaviour in
conversation ("keep answers shorter from now on"), compiling the intent
**deterministically** to the cheapest enforcement rung available (parameter →
transform → prompt) and offering it as a consent card. Dismissal MUST store
nothing.

**RT-62 (INVARIANT, where RT-61 is implemented).** **No taught behaviour may
touch the grounding machinery.** Facts are scored by evidence provenance, never
by preference. Enforcement rungs MUST be structurally unable to alter citations
or grounding verdicts.

**RT-63 (MUST, where RT-61 is implemented).** Every influenced turn MUST record
which lessons applied. There MUST be at most one active lesson per rung, with
saving superseding and retiring the prior one, and a settings surface where a
user can see and remove what was learned.

### 7.8 Synthesis roles

**RT-64 (MUST).** The synthesis path MUST be organised as data-defined roles —
at minimum a **router**, a **synthesiser**, and a **critic** — each carrying its
own verification predicate. *The predicate defines correctness; the benchmark
measures it.*

**RT-65 (MUST).** There MUST be exactly one prompt-body builder that every
synthesis site calls, and exactly one traced route resolution with a typed
reason. Both MUST be pinned by equivalence tests.
⟨why⟩ Without this, the live path was mis-identified three times.

### 7.9 Durable state

**RT-66 (MUST).** Durable state MUST sit behind one interface with at least
three implementations: an embedded single-file store (the default), a networked
store (for a shared host), and an in-memory store (for tests). Functional tests
MUST run against real text search, not a stub.

**RT-67 (MUST).** The interface MUST be **decomposed by concern**, so a caller
narrows its dependency to what it actually uses rather than taking a
general-purpose handle on everything.
⟨why⟩ A general-purpose handle became a de-facto database connection: dozens of
call sites reached through the conversational runtime for work that is not a
conversational turn, coupling unrelated surfaces to the runtime's lifecycle and
making them report "still loading" for operations the database could already
answer.

**RT-68 (MUST).** Every record MUST carry a monotonic logical **version**, and
every deletable record MUST be soft-deletable, such that **two independently
evolved stores can be union-merged without a schema migration.**

**RT-69 (MUST).** Message history MUST be full-text searchable.

**RT-70 (MUST).** State covered MUST include at minimum: conversations and
messages, tasks, memories, documents and document assets, per-corpus state,
routing history, search budget, permissions, and the per-attempt execution ledger
(RT-36).

**RT-71 (MUST).** Ledger methods MUST default to no-ops on implementations that
do not need durability, so a non-durable test double is unaffected by their
existence.

---

## 8. D6 — Grounding and epistemic integrity

This domain is the product's central claim. Every requirement here is a MUST.

### 8.1 The grounding gate

**GR-1.** A grounded answer path MUST verify the draft's claims before release.

**GR-2 (INVARIANT).** **The evidence universe is the load-bearing design
decision.** Verification MUST run against the *sealed corpus* — per-claim search
over the source of record — not merely against the prompt's snapshot of it.
Failed claims' corrective passages MUST feed the repair, replacing rather than
deleting.
⟨why⟩ An earlier verification design was empirically ruled out as net-negative;
the verdict reversed only when the judge's reachable evidence was widened.

**GR-3 (INVARIANT).** **The widening must be complete.** Every mechanism inside
the gate MUST see the same evidence universe the drafter saw. One universe built
once, then narrowed differently for three consumers, is non-conforming.
⟨why⟩ The judge could reach the corpus, but its starting snapshot was capped at
the first 8 of a typically 28-passage retrieval, and a holistic scan saw every
passage truncated and no summaries at all — while the drafter received the whole
set. Measured over 18 audit passes: 38 of 57 failed claims (67%) had their
support outside the failing mechanism's own view, and **zero passes ever came
back clean**, so every turn paid a repair and a re-audit it did not need.

**GR-4.** Any window bound inside the gate MUST be *derived* from the retrieved
set, not a constant. Where a constant no longer governs anything, it MUST be
removed rather than left as a knob.

**GR-5.** The gate MUST be a ladder with named stages: hold → verify →
short-answer corrective retry, or per-claim audit → **mark** for long form →
grounded abstention. It MUST fail open on judge failure, and the fail-open MUST
be recorded.

**GR-6 (INVARIANT).** **Marking discharges the grounding function completely.**
A long-form draft whose audit found failures MAY be released with those claims
marked, because *nothing is regenerated*: the released text is the audited draft,
and each failed claim rides out as a holding that flips the turn's epistemic
verdict. Re-synthesis discharges a presentation preference, at a measured cost of
tens of seconds per repaired turn.

**GR-7.** Where a repair path and its re-audit are both disabled, they MUST be
one switch. A configuration reaching "rewrite on, re-audit off" MUST be
unreachable.
⟨why⟩ That exact configuration leaked confabulations from zero to one and was
reverted.

**GR-8.** A disabled path that fires MUST be **visible in the product**, not
inferred from a flag: the stage record MUST reflect the branch actually taken
(X-OB-4).

**GR-9.** Judge prompts MUST be byte-pinned to the calibration harness, so a
calibrated operating point transfers.

**GR-10.** The claim-search interface MUST be structurally unable to widen
*corpus* scope. It may widen within the seal; it may never leave it.

### 8.2 Citation

**GR-11.** A released citation MUST name where its quote came from at structural
granularity (ST-23).

**GR-12 (INVARIANT).** **Only an exact match may carry a location, and it must
release the source's own characters, not the model's copy of them.** Handing back
the source span makes "a labelled quote survives the strict re-check" structural
rather than re-derived: a substring of a passage cannot be demoted by a check
that looks for substrings of passages (X-PR-7).

**GR-13.** A partial or cross-passage match MUST still ground and MUST still
release the model's span — it just ships without a location.

**GR-14.** Location labels MUST sit **outside** the quotation marks, because the
post-hoc verifier reads what is between them as source text.

**GR-15.** Location labels MUST NOT be folded into whatever set the
citation-attribution check counts as grounded. Adding them there quietly loosens
a fabrication guard.

**GR-16.** Locations MUST be computed inside the same filtering and reordering
that produces the evidence list, never resolved at a call site — a misaligned
location names the wrong place with full confidence.

**GR-17.** Citation minting SHOULD go through one door that refuses a quote the
sealed member does not hold verbatim, and refuses a quote whose granularity may
not be quoted (X-PR-2). A drop MUST be a named refusal value, not a silent
filter.

**GR-18.** The set of emitted citation records MUST be **projected from** the
released answer, not maintained as a second list that agrees by hand (X-SD-1).

### 8.3 Post-synthesis verification

**GR-19 (INVARIANT).** **The post-synthesis guard verifies the turn's evidence,
not the prompt's rendering of it.** The verifier MUST receive the untruncated
passages *in addition to* whatever the prompt contained, so the source set is a
strict superset and the change can only remove false demotions.
⟨why⟩ The prompt rendering truncated every passage to a prompt budget. On
~2000-character passages the guard was reading roughly the first 30% of each and
calling the rest absent. Measured by replaying frozen transcripts through the
real deciders: **50 of 80 released citations (62.5%) shipped labelled
unverified, and 55 of 55 of those spans are verbatim in the turn's evidence —
zero fabrications.** The discriminator was purely the quote's offset within its
own passage.

**GR-20.** The prompt budget constant MUST NOT be raised to "fix" this class. It
is a decision about how much of each passage the synthesis model reads, and it
re-prices every turn.

**GR-21.** The composite-quote catch MUST be preserved: a spliced quote is
non-contiguous in any passage under any normalisation.

### 8.4 Numeric integrity

**GR-22 (INVARIANT).** **The model never originates a number.** Where a corpus
declares an authoritative typed source, this MUST be enforced three ways: the
model narrates only compact figures the tool emitted; any derivation is appended
**verbatim as rendered by the system**, not by the model; and a deterministic
audit value-matches every figure in the prose against the tool's outputs.

**GR-23.** The audit MUST cover bare numerals, not only currency and percentage
forms, where the domain produces them — declared as an opt-in on the tool's own
output with an explicit allowed-token set.

**GR-24.** The audit MUST also run on **refusal** turns: a model reciting a
figure from parametric memory over a refusal must be caught.

**GR-25.** On violation the narration MUST be **withheld** and replaced by the
tool's own verbatim rendering, naming each untraceable numeral. Zero
unattributable numerals by construction, not by model compliance (X-ST-2).

**GR-26.** A user-facing export MUST exist that writes the underlying figures so
a third party can independently re-derive the totals.

### 8.5 Authority binding

**GR-27 (INVARIANT).** **The audit MUST bind to the answer exit on every
dispatch surface, not to a routing decision.** Arming MUST be corpus-granular,
derived from tools' own declarations of the domains they are authoritative for,
and triggered by the retrieved-evidence pool intersecting a declared domain.

**GR-28.** The allowed basis for an armed turn MUST be: figures the tool emitted,
∪ numerals inside verified verbatim spans, ∪ numerals the user's own question
contained.

**GR-29.** An armed streaming turn MUST force hold mode, so no token is released
before the audit, and MUST skip any post-stream refinement rewrite.

**GR-30.** Corpora declaring nothing MUST be structurally untouched: empty
intersection, no metadata, byte-identical delivery.

**GR-31 (INVARIANT).** **The same emptiness that is a no-op per corpus is a
DEFECT per surface.** Arming reads the tool registry of the process serving the
turn. A surface that can *install* an authoritative corpus but does not
*register* the tool declaring authority over it will never arm and will answer
ungrounded — while every gate stays green, because the gates run in processes
that do register it.
⟨why⟩ Exactly this shipped: one surface registered the acquirer and not the
authoritative tool, and answered a financial question with a figure absent from
the source and a manufactured quotation.

**GR-32.** Registration on an install-capable surface MUST therefore be
**unconditional** — never gated on user tool configuration — and pinned by a
source census.

**GR-33.** Coverage MUST be **code**: an exhaustive per-intent table with three
dispositions (covered / no-op by construction / excluded by decision), pinned by
test, with non-intent-keyed exits dispositioned explicitly.

**GR-34.** Which corpora a tool is authoritative *for* MUST have exactly one
implementation, keyed on the corpus's own declaration plus the presence of the
typed source — **never on the identifier's spelling** (X-SD-3). The tool's claim
index and any user-facing coverage surface MUST resolve through it, so a corpus
cannot be answerable by one and invisible to the other.

### 8.6 Coverage disclosure

**GR-35.** A user-facing **coverage card** MUST derive, from the same source the
tool answers from: what the corpus answers, over what period, as of which
version, and its named structural limits.

**GR-36 (INVARIANT).** Three properties MUST be structural rather than
remembered:
- the card type MUST carry no coverage ratio, and MUST NOT carry the counts
  from which one could be computed;
- a declared limit MUST carry no severity field, so no renderer can style a
  refusal as a fault;
- capability and boundary MUST share one presentation rule, so neither can be
  demoted to fine print without visibly demoting the other.

**GR-37.** Nothing in the derivation or the presentation may name a specific
subject, so a second installed instance renders truthfully with no new copy
written.

**GR-38.** The demand-side question — *of the things people actually ask for,
how many miss for a reason we could fix* — SHOULD be answerable from logs a run
already produced, with **no new store and no cadence**. It MUST be emitted from
the single covering entry point rather than from individual outcome branches,
because that is what makes the *denominator* answerable at all.

**GR-39.** A miss MUST count as fixable only under a declared membership test
against the corpus's own coverage sidecar; everything else reports *unclassified*
and never enters the numerator. A store-level source limitation cannot classify
an individual ask.

**GR-40.** Zero asks MUST exit with a named reason, never score zero (X-EH-3).

**GR-41.** A cross-language log contract MUST be pinned on **both** sides: the
emitter by a grep-able anchor, and the reader by a test that renders a real event
through a real formatter and parses it.
⟨why⟩ That test caught a field arriving quoted, which the reader's first draft
would have matched zero times while reporting a clean score.

### 8.7 Progress narration and receipts

**GR-42.** On streaming surfaces the gate MUST narrate itself — check start,
per-claim verdict, revision start, check complete — over a channel that **drops
on full and never applies backpressure**.

**GR-43.** Every narration element MUST be frame-driven. The interface MUST NOT
invent progress, and retrieval-only signal MUST be provisional: the moment tokens
stream with no gate signal, the display MUST yield.

**GR-44.** A quiet **verification receipt** MUST persist on the delivered
message, for release actions only, never for fail-open verdicts.

**GR-45.** Persisted metadata MUST be returned verbatim across every surface
boundary, so a live answer carries the same provenance a reloaded one does.

### 8.8 Epistemic state

**GR-46.** An answer MUST be a typed object carrying per-claim provenance, an
overall verdict, and — where the answer is incomplete — a conjecture about what
is missing and how it could be acquired.

**GR-47.** A turn with marked claims MUST carry a **mixed** verdict, rendered
under the answer.

**GR-48.** There MUST be exactly one judge of "did we answer". A second judge
MUST NOT be minted.

### 8.9 Stack attribution

**GR-49.** Where two mechanisms (an incumbent and a replacement) both run, the
system MUST be able to say **which one spent the turn**, per stage, with a closed
set of owners including a *shared* owner for work belonging to neither.

**GR-50.** Per-**call** attribution MUST be available where per-stage is
insufficient, through **one funnel every model call in the region passes**,
tagged with a closed mechanism set, enforced structurally (X-ST-3).

**GR-51.** Per-call rows MUST NOT be folded into the per-stage ledger — doing so
would saturate the unattributed residual to zero and destroy the detector
(X-OB-3).

**GR-52.** An empty call list MUST be resolved by comparing summed durations
against the stage's own wall clock, never read as a free turn.

**GR-53.** Test runs MUST NOT append to the production record stream. The record
MUST still be *built* under test; only the write is suppressed.
⟨why⟩ Unit tests driving the funnel with stubs appended dozens of synthetic
turns into the same stream that latency analysis read by index.

---

## 9. D7 — Federation

### 9.1 Shape

**FE-1 (MUST).** Every node runs identical software. There is no master, no
coordinator, and no central service.

**FE-2 (MUST).** In one sentence: the federation translates *"complete this
request with model X"* into a plan that runs the work on one or more nodes, holds
a standard API endpoint open, and keeps the plan healthy as nodes come and go.

### 9.2 Identity and membership

**FE-3 (MUST).** Every node MUST persist a durable asymmetric keypair. The
identity MUST be **mesh-independent**: it survives leaving, joining, and
switching, so a node is the same node in every trust ring it belongs to.

**FE-4 (MUST).** Joining MUST be by a short human-transcribable key. The
receiving side MUST store only a hash and discard the plaintext.

**FE-5 (MUST).** Join MUST include proof-of-possession of the node's identity
key; a bad proof MUST be rejected.

**FE-6 (MUST).** Identity keys MUST be protected by an anti-downgrade rule: a
relayed record without the key MUST NOT strip a known one.

**FE-7 (MUST).** A node MUST be able to belong to **many** trust rings and be
active in exactly one. Parked memberships MUST retain their full roster and
credentials, so returning to one is a **resume** — no handshake, no key redeemed.

**FE-8 (INVARIANT).** **Writing a membership does not make it active.** The save
operation MUST touch only that membership's own storage. Activation MUST be a
separate step used only by the two operations that *establish* a membership, plus
the explicit switch.
⟨why⟩ Saving used to re-point the active marker at its subject, which made
every caller an implicit switcher — including a per-round background re-persist,
so a round still in flight for the just-parked ring could silently undo a switch.

**FE-9 (MUST).** File-then-pointer ordering MUST hold, so the active marker never
names a location with no membership in it.

**FE-10 (MUST).** Leaving MUST clear the pointer *and* remove the departed
membership. Leaving a pointer naming deleted state reads as healthy at boot while
making that membership permanently unforgettable.

**FE-11 (INVARIANT).** A ring MUST carry **two separate credentials**: one that
authorises ongoing participation and never rotates, and one that admits new
joiners and rotates freely.
⟨why⟩ They were one field. Rotating an invitation re-keyed participation and
partitioned the rotator from its own ring.

**FE-12 (MUST).** Invitation expiry MUST live on the ring, not in per-node
memory, or it dies on restart and is never armed on any member that did not
personally mint it.

**FE-13 (MUST).** The participation credential SHOULD NOT ride the wire between
upgraded peers. A round SHOULD carry a keyed proof bound to the **sender** and to
a short time window, so a captured proof is neither transferable nor durable.

**FE-14 (MUST).** The authorisation result MUST report **which predicate won**
as a typed value. An *offered* proof that fails MUST be a hard refusal, never a
fall-through — otherwise stripping the proof buys the weaker predicate.

**FE-15 (MUST).** Credential rotation MUST be refused while the fleet is mixed,
and the refusal MUST distinguish **two populations** with different remedies: a
peer that authorises on the legacy predicate needs *upgrading*; a peer simply not
yet observed since start-up needs one *round*. Collapsing them tells operators
their fleet is un-migrated when it is not.

**FE-16 (INVARIANT).** **The confirmation is local observation, never a peer's
claim.** An upgraded peer deliberately withholds the credential, so a zeroed
field stopped being evidence of an old build; reading it as one made two upgraded
nodes report each other as legacy and block rotation on both sides.

**FE-17 (MUST).** Because that observation is in memory, the rotation refusal
MUST run at least one round before it is willing to refuse. **It must never
report a verdict from an instrument it has not run.**

### 9.3 Discovery and state replication

**FE-18 (MUST).** Local discovery MUST work on a LAN with no configuration, and
membership MUST work transitively over a private overlay network.

**FE-19 (MUST).** Shared state MUST converge by epidemic replication: a periodic
round to a small random peer sample, three-phase digest/delta/response, last-
writer-wins on timestamp.

**FE-20 (MUST).** Replicated payloads MUST include at minimum member state,
inference plans, knowledge plans, ledger entries, and ring configuration.

**FE-21 (MUST).** Round-trip latency between members MUST be measured
periodically and shared, as a smoothed estimate.

**FE-22 (MUST).** Hardware capability MUST be detected, not configured, with a
fallback chain across accelerator vendors.

**FE-23 (MUST).** Reachability information a peer publishes MUST be **signed
per-node with a monotonic version**, so an attacker past the admission gate
cannot force a peer unreachable or downgrade its transport.

**FE-24 (MUST).** Policy that must only ever tighten (such as an encryption
requirement) MUST merge **stricter-wins**, not last-writer-wins.

### 9.4 Transport

**FE-25 (MUST).** *How this node reaches a peer* MUST be decided in exactly one
place: a seam resolving **(peer, traffic class) → ordered candidate endpoints**.
Call sites keep their own clients and timeouts and append route paths.

**FE-26 (MUST).** At least two transports MUST be supportable behind that seam:
a conventional address-based one, and a **key-addressed encrypted** one that
dials a peer by its public key with no address, bridging to the ordinary local
listeners.

**FE-27 (MUST).** A routing transport MUST concatenate a chosen transport's
candidates ahead of a default, so a failed or absent dial degrades to the
fallback **on the same request**, automatically. Success feedback MUST route back
to the producing transport.

**FE-28 (MUST).** Migration order between transports MUST be encoded by traffic
class, not by configuration flags: control plane first, bulk transfer next,
inference streaming last.

**FE-29 (MUST).** Enablement MUST have three states — automatic (on only when
the node is in a ring, so a solo node never contacts third-party infrastructure),
explicitly on, and a kill switch.

**FE-30 (MUST).** A self-hosted relay configuration MUST be possible, and a
"no third-party services at all" posture MUST be achievable — noting that
configuring one's own relay alone does not achieve it if name resolution still
uses a third party. The honest posture MUST be documented per service.

**FE-31 (MUST).** The transport MUST survive networks that block direct
connections, by relaying over a common port, through an authenticating proxy if
one is configured.

**FE-32 (MUST).** What is **not** covered by the encrypted transport MUST be
stated. Where raw traffic between compute processes stays outside the seam, the
system MUST NOT claim blanket end-to-end encryption, and a surface-by-surface
posture document MUST enumerate every listener, its default binding, its
authentication, and the honest gaps.

### 9.5 Trust surfaces and admission

**FE-33 (MUST).** There MUST be exactly two listeners with two trust domains: a
**client** surface reachable by peers and remote callers, and an **internal**
surface carrying control-plane traffic.

**FE-34 (INVARIANT).** **Holding a dial string is not a credential.** A
key-addressed endpoint accepts anyone, and the dial string is public — it rides
in every invitation and is replicated as part of membership. What the handshake
*does* prove is the dialer's key. The acceptor MUST therefore route on
**(protocol selector, dialer identity)**, never on the protocol selector alone.
⟨why⟩ Routing on the selector alone handed every dial-string holder whatever
the local client listener grants its own machine: the full client API, no bearer
token. Watched failing before the fix.

**FE-35 (INVARIANT).** **Which listener serves a route is the guard; "is the
caller local" is not.** A bridging acceptor forwards by connecting to the local
interface, so on every listener it feeds, a local peer address proves nothing.
⟨why⟩ Narrowing a protocol selector from "any dial-string holder" to "any
member" was a reduction, not a fix: a member then landed on the operator's own
binding and could mint credentials for an outsider on someone else's node, with
nothing presented. A local-address guard on those handlers would have read as a
fix and gated nothing.

**FE-36 (MUST).** The client router MUST therefore be bound **more than once**,
as distinct surfaces with distinct policies, at minimum:

| Surface | Reached by | Trusts a local peer address | Serves control-plane routes |
|---|---|---|---|
| Operator | a real local caller | yes | yes |
| Peer | an authenticated member | yes | **no** |
| Guest / stranger | an unauthenticated dialer | **no** | no |

No address reachable by the bridging acceptor may point at the operator surface.

**FE-37 (MUST).** A protocol selector that authenticates nothing MUST be
**refused** for strangers rather than downgraded — there is no safe downgrade.

**FE-38 (MUST).** Where a selector must remain open to non-members (because
joining is how one becomes a member), that MUST be a stated, bounded, documented
open edge with the sensitive routes guarded by their own predicates.

**FE-39 (MUST).** A **guest grant** MUST be supported: short-lived, revocable,
bound to a closed scope enumeration whose path list is the **only** route
allow-list there is. A guest is not a member: no participation credential, no
replication, no ability to mint further grants — because no scope variant names
the control plane.

**FE-40 (MUST).** The authorisation layer MUST NOT match on scope variants. It
MUST ask the grant whether it permits the path, so adding a scope is a variant
plus its path list and touches neither authorisation nor the wire (X-EX-5).

**FE-41 (INVARIANT).** **A guest grant cannot be tested over the local
interface**, and every such test passes for the wrong reason, because the local
caller is admitted before any bearer is read. Guest-path acceptance MUST be
proven on a genuine two-machine path.

**FE-42 (MUST).** Grant minting MUST be available only on the operator surface.
The internal surface has no per-request authentication, so a mint route there
would let any peer forge guest credentials.

**FE-43 (MUST).** Control-plane and administrative routes MUST be local-only,
defended in **three layers**: router middleware, per-handler check, and a pinned
test asserting the guard under the *production listener shape*.
⟨why⟩ A listener configured without connection-info propagation leaves the
handler unable to see the peer address, and the guards then fail closed for
*every* caller — a failure the naive test cannot see.

### 9.6 Public API surface

**FE-44 (MUST).** The client API MUST expose a chat-completion endpoint
conforming to the prevailing third-party convention — the request and response
shape unmodified third-party clients already speak — plus an embeddings
endpoint, a model listing, a status summary, and a capability manifest. The
choice of *which* convention is a compatibility requirement, not a design
choice: pick the one the ecosystem's clients emit.

**FE-45 (MUST).** The model listing MUST enumerate **names this node can
dispatch by name**, built from the same source the name resolver uses — so a
listed identifier resolves and an omitted one does not. It MUST carry residency
and which nodes advertise each name.
⟨why⟩ The listing was built from a replicated key-value scan, and advertised
identifiers that chat completions then refused.

**FE-46 (SHOULD).** Compatibility shims for other prevailing client conventions
SHOULD be provided as **pure translation** over the same handlers, with their
current limitations documented in place.

**FE-47 (MUST).** A knowledge search endpoint MUST determine target corpora,
fan out, merge, and rerank.

**FE-48 (MUST).** Requests carrying a local-only privacy requirement MUST be
rejected at the federated endpoint rather than served remotely.

### 9.7 Scheduling

**FE-49 (MUST).** The decision topology MUST be explicit and enumerable, with
exactly one mechanism per decision:

| Decision | Nature |
|---|---|
| Is this turn eligible to leave the machine at all? | One predicate, shared by every consumer |
| Peer versus local for an eligible turn | Capability score × operational adjustments |
| Resolving a **named** target | Name resolution plus a tiebreak — *not* the scorer |
| Which local slot serves a peer's request | Canonical latency-class map plus hint veto |
| Synthesis tier | Request shape and evidence shape |
| Placement of a model larger than one node | Explicit placement policy |
| Partitioning collaborative ingestion | Compatibility filter plus proportional blocks |

**FE-50 (INVARIANT).** A **hard** named target is a constraint: unknown MUST be
an error, never a substitution. A **soft** configured preference is a preference:
unknown MUST fall through to ranked selection with local as the last rung, and
the fall-through MUST be recorded.

**FE-51 (MUST).** A peer route that **fails** is not the same as an unknown
name. Where the local node holds the same identifier and merely lost a tiebreak,
a peer failure MUST be served locally. Where the peer was the sole holder, it
MUST fail loudly. Serving the named identifier locally is *honouring* the name,
not substituting for it.

**FE-52 (MUST).** Every entry point MUST resolve through **one** route cascade;
per-method code may build only the terminus.
⟨why⟩ One entry point routed inline against a single peer, so it gave up after
one declining peer, skipped in-flight booking, and was the reason four successive
features each had to be written twice.

**FE-53 (MUST).** A resolved identifier MUST go on the wire, so a strictly-
resolving peer cannot refuse the turn into a silent local substitution.

### 9.8 Scheduler quality — measurement discipline

**FE-54 (MUST).** Routing decisions MUST be **measurable**, not merely plumbed.
⟨why⟩ Retrieval, grounding, and synthesis each had a benchmark and a tight
loop; this layer had unit tests on individual factors and end-to-end tests of
*plumbing*, and nothing measured whether a decision was **good**.

**FE-55 (INVARIANT).** **A product of dimensionless multipliers ranks; it does
not predict.** No scoreboard is definable over it, and it cannot decline a hop
that costs more than it buys. An objective with **units** — predicted time to
answer, as named addends — can.

**FE-56 (MUST).** Instrumentation MUST record one decision per decision point
carrying: the whole candidate set, each score breakdown, **each input stamped
with its provenance and age**, every candidate excluded before scoring and why,
and the verdict — joined by identity to one outcome record per completion.

**FE-57 (MUST).** Replay MUST group records by decision identity, **never by
adjacency** — a live log interleaves requests — and MUST report a join rate to
gate on.

**FE-58 (MUST).** The decision MUST be extractable as a **pure total function**
over a snapshot of what a decider believes, with time passed in rather than read.
Asynchronous gathering above the line, deciding below it.

**FE-59 (MUST).** A seeded discrete-event simulator SHOULD exist over that pure
function, with independent random streams per concern so switching arms cannot
perturb the world the arms are compared in, and with fidelity knobs defaulting
inert so prior results still reproduce.

**FE-60 (MUST).** Replay MUST split **scorer agreement** from **policy
agreement**, running off independent inputs so one defect cannot cascade into the
other. Both ratios MUST return zero on an empty denominator, never a vacuous
one.

**FE-61 (MUST).** A predictive objective MUST return either named addends or a
typed **unpredictable** reason. It MUST NEVER default a rate: a guessed rate is a
fabricated fact with a unit attached.

**FE-62 (MUST).** *Unpredictable local* (therefore no hop) and *infeasible
local* (therefore any feasible peer wins) MUST be distinct. Collapsing them
points the decision in opposite directions.

**FE-63 (MUST).** Capability MUST be a **filter, not a scoring term**.
Candidates MUST be banded by a *relative* edge derived at runtime from what the
decider currently knows — never an absolute threshold and never a table of model
names — and a normal request MUST be served from the top band.

**FE-64 (MUST).** Two quality hazards MUST be counted separately: a
**downgrade** (served below the requester's own local capability — a real
regression) and a **declined upgrade** (a stronger node was feasible). Both MUST
be computable from decision records alone, so the identical function scores a
production capture.

**FE-65 (INVARIANT).** **A ranking objective with no ties herds.** Once a filter
makes candidates homogeneous, a deterministic objective sends everything to one
node. A tie-breaking sampler over candidates **whose predictions are within
noise** is a prerequisite, not a refinement — and what makes "within noise"
expressible is that predictions have units.

**FE-66 (INVARIANT).** **A signal that can only describe peers you already
chose cannot fix a cost that lives in the peers you did not.** Response-carried
freshness measured at 4–7% coverage against a mechanism worth 9–22% at full
coverage; coverage is a property of traffic density, not of wiring.

**FE-67 (INVARIANT).** **Do not wire a probe whose measurement does not describe
the thing being scored.** A throughput probe run against a small model, then
extrapolated to a large candidate on a linear size law, is a *measured
regression* — decode is bandwidth-bound, the law is false, and a one-sided clamp
means the error can only push large models down.

**FE-68 (MUST).** A per-model measurement facility MUST exist for the human
deciding whether to add a machine, and it MUST deliberately **not** feed the
ranked dispatch. Same number, different consumer; the record must say so, so a
later reader "completing the wiring" does not ship the regression.

**FE-69 (MUST).** Where production is blind to a signal, the dead producers MUST
be **deleted**, so the blindness is by construction rather than by accident, and
so the as-shipped baseline measures the shipped system rather than a state it
happens to be in.

### 9.9 Distributed placement

**FE-70 (MUST).** Where a model exceeds one node, the placement policy MUST
apportion each device a **contiguous** range whose **bytes** — not unit count —
are proportional to its memory.
⟨why⟩ Modern large models have deeply non-uniform per-unit mass: a measured
62× spread between adjacent units. Count-proportional apportionment hands a small
node a heavy run and exhausts its memory.

**FE-71 (MUST).** Ranges MUST stay contiguous, so single-stream decoding keeps
its minimum hop count and a unit's parts are never scattered.

**FE-72 (MUST).** The split the preview shows and the split the load executes
MUST be **one function** (X-SD-1).

**FE-73 (MUST).** A pre-flight planner MUST exist that answers "does this fit
this fleet" from metadata alone — no model load, no accelerator — reporting per
device bytes held and whether each device *individually* fits.

**FE-74 (MUST).** Per-device fit MUST be one decider used by both the preview
and the load. Aggregate pooled memory is **not** sufficient: a cluster clearing
the aggregate gate can still hand one device more than it has.

**FE-75 (MUST).** The fit result MUST return **one row per device, fitting rows
included** — a pass/fail return forces the preview to keep its own traversal, and
a second traversal is the drift being removed.

**FE-76 (MUST).** *Cannot judge* MUST be distinct from *pass* (X-EH-2): an
unread metadata table would otherwise clear every device on the strength of
zeros.

**FE-77 (MUST).** An overflow refusal MUST be a **new** typed variant, not a
reuse of "insufficient cluster" — pooling more memory does not fix an overflow,
and the wrong message sends the operator looking for a peer that is already
there. The refusal MUST advise *lowering* the headroom factor and MUST name the
one legitimate override.

**FE-78 (MUST).** An overflow MUST **park** rather than retry: it is not
time-fixable, and the existing participant-change event is the only thing that
could change the answer.

**FE-79 (MUST).** The headroom factor MUST be operator-set through one
resolution order, and the preview MUST default to that same resolution — so the
preview's headroom is the one the load executes with.

**FE-80 (MUST).** Liveness of a participant already resolved MUST be read from
**membership**, not from a probe over the link that participant's own bulk
traffic is saturating. Only unknown participants pay the full probe.

**FE-81 (MUST).** A bridged endpoint's local port MUST be **stable across a
peer's reachability change** — retarget in place, never rebuild — because that
port *is* the participant's identity in the compute layer's device list.
⟨why⟩ Minting a new port made an unmoved peer read downstream as a stream of
different participants.

### 9.10 Benchmarking a running configuration

**FE-82 (MUST).** A measurement facility MUST **measure the configuration that
is loaded and never load the one it wants to measure.** There must be no
slot argument, therefore no slot to get wrong. An optional model argument is an
**assertion**, verified against the resident configuration, failing loudly on
mismatch.

**FE-83 (MUST).** It MUST time real streamed responses at the frame level, so
the number includes the actual distribution and transport path. Steady-state rate
and time-to-first-token MUST be reported separately, never smeared.

**FE-84 (MUST).** A rate the server does not report MUST render as *not
available* — never estimated from character counts.

**FE-85 (MUST).** The probe MUST be **fixed and versioned**. There MUST be no
knob whose adjustment invalidates comparison against every prior record while
looking like harmless tuning.

**FE-86 (MUST).** Validity guards MUST exist and each MUST be earned by an
observed false result. At minimum: which slot served it; per-frame timing;
placement re-read after the run; participant liveness before **and** after; a
warm-up; host survival; a minimum frame count; an inter-trial spread ceiling; and
a terminal-reason check.

**FE-87 (INVARIANT).** **The obvious served-slot guard does nothing.** A
response's model field is typically a verbatim echo of what the client
requested. The real check MUST understand every hosting mode, including one where
the in-process view reports the model as *not resident* for a perfectly healthy
externally-hosted instance. States that mean "something else is answering" MUST
NOT count as serving.
⟨why⟩ Measured live: with the intended host still starting, requests returned a
rate physically impossible for that model, and the naive field check passed
cleanly.

**FE-88 (MUST).** A run tripping a guard MUST still be **written** — a discarded
failure teaches nobody and makes the tool retry-until-lucky — but MUST never be
returned by lookup.

**FE-89 (MUST).** The key a record files under MUST be constructible identically
by every consumer, derived from the same facts (only participants holding work;
topology, not a mode string), or every record is unfindable.

**FE-90 (MUST).** A record MUST carry the **pre-image of its own key**, so a
reader can say what the number was *for*. The pre-image MUST be **checkable**:
recomputing the key from the stored fields, with a mismatch treated as *absent*
rather than quoted.

**FE-91 (MUST).** A record MUST carry the **conditions** it met — co-resident
work, memory before and after, uptime, wall-clock span — and these MUST sit
**beside** the key and never in it. Keying on them gives every run a unique
unmatched key and makes the variance the field exists to expose structurally
invisible.

**FE-92 (MUST).** An empty co-resident list MUST render as the *finding*
"nothing else was resident", not as silence. An older record MUST say "not
recorded", never imply a quiet machine.

**FE-93 (SHOULD).** Measurements SHOULD travel between peers, because a
measurement is worth most to the machine that did not take it. Three constraints
make travel safe:
- **Peer records never enter the local store.** Lookup answers only "what did
  *this* machine measure"; a peer's number is offered *beside* the local answer,
  never as it.
- **Invalid runs do not travel.** A failure is diagnostic material for the
  operator who caused it and noise — or a mis-read capability claim — to everyone
  else.
- **Origin comes from the transport envelope, not the payload.** A node must not
  be able to claim to be someone else by writing a name into bytes it controls.

**FE-94 (MUST).** Values entering a content-derived key MUST be quantised to a
precision the serialisation round-trip preserves, or the same run computes two
keys and leaves an orphan the conflict resolution can never overwrite.

**FE-95 (MUST).** Durable local write MUST precede publication, so the facility
works with no daemon and a failed publish reads as "not shared yet", never as a
lost record. Republication at start-up MUST restore the durable history, or every
node's history evaporates one restart at a time while looking intact locally.

### 9.11 Fairness and admission

**FE-96 (MUST).** Peer requests MUST pass an admission gate; local requests MUST
admit unconditionally. Refusal MUST be a standard busy response with a retry
hint.

**FE-97 (MUST).** The gate MUST implement a runtime-mutable global ceiling
(including zero = reject all) **plus a per-origin concurrency cap**, so one peer
cannot consume the pool.

**FE-98 (MUST).** Caps SHOULD be **reciprocity-scaled** from the contribution
ledger: a contributor's effective cap rises toward the ceiling.

**FE-99 (MUST).** There MUST be exactly one canonical wire form for an origin
identifier, and exactly one parser. A display form MUST never be accepted as a
wire form. A present-but-malformed value MUST still be gated and tallied, and the
status surface MUST name the rejected raw value, when it was last seen, and the
expected form.

**FE-100 (MUST).** Traffic that names an origin and traffic that does not MUST
be gated by **disjoint** mechanisms — one returning early when the other applies
— so a request meets exactly one and is never double-gated.

**FE-101 (MUST).** Principal resolution for unnamed traffic MUST have exactly
one resolver: presented credential → credential fingerprint; else a declared
identity from a local caller; else anonymous.

**FE-102 (MUST).** The client-fairness share rule MUST take **no weight
argument**, so weight-ordering unfairness is *unexpressible* rather than merely
avoided. A single caller alone on the host MUST be untouched.

**FE-103 (MUST).** The client-fairness gate MUST NOT be able to refuse on depth
— that decision belongs to exactly one shedding mechanism — and MUST leave no
waiter parked.

**FE-104 (INVARIANT).** **A gate keyed on the presence of an identifier is dark
until every path stamps it.** Measured: four concurrent peer requests served with
the in-flight counter never leaving zero, because federated inference never
stamped the header. Pause, yield, and ceiling were all inoperative.

**FE-105 (MUST).** A refusal produced by *this* gate MUST be exempted from peer
health tracking — a busy response is a healthy peer declining, and booking it as
a fault quarantines that peer.

**FE-106 (MUST).** A peer that refuses with a stated retry delay MUST be
excluded from candidacy for that delay, capped, and cleared by any success —
**before** any manifest fetch, so a yielding peer costs nothing rather than a
cheaper something.
⟨why⟩ An exclusion rather than a score discount is required because the
scorer's availability term is clamped, so its strongest possible "no" is a bounded
multiplier: a peer better on other terms still wins and still gets refused.

**FE-107 (MUST).** What a node **advertises** and what it **enforces** MUST come
from one writer consulting the same predicates.
⟨why⟩ A node refusing every peer request advertised full availability for as
long as it kept refusing.

**FE-108 (MUST).** Foreground yield MUST be bounded (X-ST-4). Every consumer
parking on a level predicate MUST pair it with a deferral budget, and the
override MUST be announced from inside the shared helper so no checkpoint can
adopt the bound and forget to report it. The exit MUST be typed, so "resuming —
foreground idle" cannot be printed after a cap override.

### 9.12 Contribution and activity accounting

**FE-109 (MUST).** A **contribution ledger** MUST be an append-only event log
with pure aggregation into per-node totals. It MUST have **no balance, no
exchange rate, and no ranking** — the units are incommensurable and MUST NOT be
folded into one number.

**FE-110 (MUST).** A pull-side transfer MUST be credited to the node that
shipped the bytes, with the origin explicit in the schema.

**FE-111 (MUST).** A separate **activity ledger** MUST answer "what is *my* node
doing, even as a ring of one", recording resource work that never crosses a peer
boundary, and MUST be **excluded from replication** (X-PV-5).

**FE-112 (MUST).** Where a surface does its work in-process and never crosses an
accounting boundary, its slice MUST be **derived** from provenance already
persisted, not written by a new path.

**FE-113 (SHOULD).** Peer preferences (local-only, non-replicated affinity
multipliers) SHOULD let a node de-prioritise a peer, applied at manifest
advertisement so the peer's own scorer naturally routes elsewhere.

### 9.13 Collaborative ingestion

**FE-114 (SHOULD).** Peers SHOULD be able to share the compute cost of building
a corpus, partitioned into storage-proportional contiguous blocks over peers with
compatible embedding capability, skipping zero-storage peers.

**FE-115 (MUST).** A per-node storage budget MUST be enforced at exactly one
place — the point where capacity is published — by clamping the advertised free
capacity. Every scheduler then reads one value and the clamp self-enforces for
both local installs and peer-driven distribution.

**FE-116 (MUST).** Personal-scope sources MUST require an explicit grant
(X-PV-8), gated by a **grantability marker** set only by the source builders that
genuinely produce assistable content, so structurally-similar sources stay
un-assistable.

**FE-117 (MUST).** The allow-list MUST be enforced at **two** points: at
enrolment and at each work-unit handout.

**FE-118 (MUST).** After merge, a sample MUST be re-computed locally and
compared against the peer-produced result, with the check reported in
user-visible terms.

### 9.14 Replicated key-value state and hosted apps

**FE-119 (MUST).** A replicated namespaced key-value store MUST exist with
last-writer-wins resolution, per-namespace isolation, and time-to-live garbage
collection. Namespaces MUST be excludable from replication.

**FE-120 (SHOULD).** A sandboxed application platform SHOULD exist over that
store: a replicated manifest, a declared permission set, lifecycle management,
and a reverse proxy.

**FE-121 (MUST, where FE-120 is implemented).** Application authorisation MUST
derive the application identity from a host-set property the sandboxed code
cannot spoof, checked fail-closed against the granted subset.

**FE-122 (MUST).** Application-facing operations MUST be **deterministic and
read-only** where they present figures: no model may originate a number on this
surface either (GR-22).

**FE-123 (MUST).** Application-facing data operations MUST be
**backend-agnostic**, projecting different underlying representations into one
data contract, so a new backend is an adapter and not a host change.

**FE-124 (MUST).** The operations' **logic** MUST live in a host-independent
library shared by the sandboxed host and any development server, so there is one
source of truth.

**FE-125 (MUST).** Where a precomputed artefact is served, it MUST be folded
deterministically and MUST pass a **verbatim-citation audit** before it is
served: every cited passage must resolve, every embedded quotation must be a
verbatim substring of its passage. A failing artefact MUST NOT be served.

**FE-126 (MUST).** Artefact caching MUST be keyed on a content fingerprint, with
a schema version that forces a rebuild when a computation change must reach
existing installs.

**FE-127 (MUST).** Typed cards with absent data MUST produce absent cards, and
**unknown card types MUST be skipped** — the forward-compatibility seam.

**FE-128 (INVARIANT).** Where a parsed structure is a **subset** of its source,
that MUST be documented at the type and honoured: anything reasoning about
*shape* must read the complete unit list, not the parsed subset.
⟨why⟩ 13,373 of 16,404 units in one archive carried no parseable boundary
marker. Reading shape from the parsed subset cost one analysis 90% of its
evidence, and counting only marker-bearing text saw 19.9% of the archive while
reporting a ratio off by 5×.

**FE-129 (MUST).** Where several presentations show a clock, the offset MUST be
inferred **once per build** and handed to every presentation. Two inferring
separately is two chances to disagree in front of the reader — which shipped.

**FE-130 (MUST).** Distribution MUST be self-contained artefacts with integrity
verification refusing tampering, plus curation. Trust = integrity + curation.

**FE-131 (MUST).** Where the sandbox platform cannot gate host operations
per-window, this MUST be stated as an **isolation caveat** and untrusted
third-party applications MUST NOT be admitted until a genuine isolation boundary
exists.

### 9.15 Artefact transfer and process supervision

**FE-132 (MUST).** Peers MUST be able to transfer model artefacts and corpus
shards directly, in both push and pull directions, with range requests supported
so a participant can fetch only the portion it needs.

**FE-133 (MUST).** A host preparing a distributed load MUST be able to ask a
participant to seed its portion **before** the load begins, and MUST distribute
only to participants that pass an eligibility profile — a settling period plus
quarantine on instability, surfaced to the operator.

**FE-134 (MUST).** Because a participant fault aborts the whole computation,
distributed inference MUST require host supervision. This MUST be stated, not
assumed.

**FE-135 (MUST).** Where a topology stratifies into anchors (holding the split)
and consumers, anchors MUST advertise their anchor profile, and the candidate
filter MUST exclude non-anchors so a casual peer never joins the split. Anchors
MUST get the stricter eligibility profile.

**FE-136 (MUST).** A participant **leaving** MUST trigger an immediate re-plan
(prune before the compute layer aborts), while pure additions keep a debounce.
Shrink-fast, grow-slow.

**FE-137 (MUST).** Host election MUST be re-evaluated each cycle over replicated
membership, with a pinned host winning while eligible, and the elected host MUST
be published so a soak test can assert a **no-split-brain** invariant.

**FE-138 (MUST).** Split-brain during convergence MUST be bounded by the
eligibility settle plus a quorum gate, with consumers falling back locally.

**FE-139 (MUST).** Managed subordinate processes MUST have an explicit lifecycle
state machine (starting / running / unhealthy / failed / stopped), graceful
termination with a timeout before forced termination, periodic health polling
with a sampled latency window and an unresponsive threshold, a **graceful
departure** countdown state machine (announced → rebalancing → draining →
complete), and a fault detector collapsing health changes into events.

**FE-140 (MUST).** A deterministic in-process test harness MUST be able to
orchestrate many simulated nodes, each with its own state and listeners, with a
fluent hardware-profile builder and a mock model server that counts requests.
Integration coverage MUST include formation, convergence, assignment, end-to-end
inference, fault recovery, graceful pause and resume, capability routing,
multi-model portfolios, knowledge fan-out, and ledger accuracy — with
**deterministic timing and no real waits**.

**FE-141 (SHOULD).** Rented or ephemeral compute pods SHOULD be able to join as
inference participants scored by the same balancer, under a **separate trust
model**: not replicated, owner-private, transport-pinned, and authenticated by
their own credential.

---

## 10. D8 — Client surfaces

### 10.1 General

**UI-1 (MUST).** All surfaces MUST run against the same reasoning runtime. A
capability MUST NOT exist on one surface only.

**UI-2 (MUST).** The commissioning of a runtime MUST be **one recipe** below
every host, with hosts supplying only their own inputs. There MUST be an
automated census asserting no host carries its own copy.
⟨why⟩ Four hosts each carried a ~600-line copy, and only one of eleven optional
capabilities was wired by all of them.

**UI-3 (MUST).** The client half of the turn protocol MUST have one
implementation, so a surface can drive a turn without holding a runtime — and
without ending by reading the store, which only works from inside the process
that owns it.

### 10.2 Graphical surface

**UI-4 (MUST).** The graphical application MUST be organised around **user
intent**, not around system structure. A workable top-level shape is: **Ask**
(grounded conversation, the landing surface), **Library** (knowledge home),
**Workshop** (making things), **Reflect** (personal/wellbeing lane), and
**Settings**.

**UI-5 (MUST).** Library MUST present knowledge as a shelf of **notebooks**,
each with its own conversation and exploration surfaces, and one add path
covering catalogue installs, folders, vaults, and imports.

**UI-6 (MUST).** Scope MUST be stated in plain language on the asking surface
("Asking ‹notebook›"), and per-notebook conversation history MUST resume.

**UI-7 (MUST).** Layout MUST be **token-driven**, with a small set of global
primitives for the scroll container, the content measure, and the header band.
⟨why⟩ Surface hosts are clipping boxes, so a body that fails to establish its
own scroller is clipped with no way to reach content past the fold. An audit
found one panel hiding 2,442 pixels of content behind an overflow rule that could
never fire. A regression gate MUST drive every route, measure composited
geometry, and fail on unreachable content.

**UI-8 (MUST).** A live turn MUST survive a conversation switch. Stream events
MUST be keyed by conversation and collected by a registry independent of which
conversation is displayed, with re-attach on return restoring the affordance and
the partial text.
⟨why⟩ Without this, a turn the user navigated away from was orphaned: events
were dropped by an identity guard, and the persisted row is written only after
the stream ends, so on return there was no row, no loading indicator, and the
answer never landed. Most visible on exactly the slow, offloaded turns a user is
most likely to navigate away from.

**UI-9 (MUST).** Chat dispatch MUST be single-flight, with the in-flight user
message preserved across binding transitions.

**UI-10 (MUST).** Accessibility seams MUST be shared across surfaces: per-turn
completion announcement (announced on completion, never per token) and modal
focus trapping with restore. Dynamic accessibility behaviour MUST be verified by
assistive-technology testing, not only by automated scanning.

### 10.3 Command-line surface

**UI-11 (MUST).** The command-line surface MUST be one user-facing verb space,
however it is implemented internally. The list of verbs MUST have one
authoritative declaration, pinned by test.

**UI-12 (MUST).** The verbs a first-time user needs MUST work in the *shipped*
artefact. A verb MUST NOT be un-gated before its implementation ships, because
un-gating first converts a clear "not in this build" into a worse "cannot find
component".
⟨why⟩ The first command a fresh installation's user types required a large
developer artefact the installer never shipped.

**UI-13 (MUST).** Where a shipped build cannot serve a verb, the refusal MUST
name what the build *can* do, never point at a build step the user cannot run.

**UI-14 (MUST).** A known workflow MUST be roughly three commands. If it is not,
the friction is a defect in the surface, not something to wrap ceremony around.

**UI-15 (MUST).** Every command MUST do real work. Aspirational placeholders
that print an intention and exit successfully are non-conforming.

**UI-16 (MUST).** Exit codes MUST be meaningful and documented per command, with
distinct codes for "did not fit", "bad arguments", "assertion failed", "nothing
measurable", and "backend unreachable".

**UI-17 (MUST).** A status command MUST write its answer to standard output.

**UI-18 (MUST).** Long-running background services MUST rotate their own logs by
copy-truncate (preserving the file identity held by service supervisors), with a
size cap, a backup count, and a sweep.

### 10.4 Network surface and multi-tenancy

**UI-19 (MUST).** An HTTP surface MUST offer REST plus streaming, with
per-tenant isolation on knowledge and uploaded documents, and a server-side
approval channel.

**UI-20 (MUST).** Per-turn streaming MUST go down the requesting connection, and
MUST NOT share a **type** with any broadcast fan-out, so a cross-tenant leak does
not compile (X-ST-5).

**UI-21 (MUST).** A fair scheduler MUST bound concurrent turns with a weighted
queue, per-origin cap, live queue position over the streaming channel, and
shedding with a retry hint over REST — sharing one policy core with the peer
admission gate, so both are fair by identical rules.

**UI-22 (MUST).** Secure by default: bind to the loopback interface, and refuse
at start-up a non-loopback bind with authentication disabled, with an explicit
opt-out.

**UI-23 (MUST).** **A configuration that enables authentication with an empty
credential set MUST NOT serve unauthenticated traffic.** Authentication engaging
only when a mode *and* a non-empty credential set are both present is a gap the
exposure guard does not close.

**UI-24 (MUST).** Capabilities whose safety rests on "one operator owns this
box" MUST be removable at build time (X-PV-2), grouped by what they grant:
**privilege** (anything reaching a shell, ingesting a server-side path, or
mounted after the authentication layer) and **egress** (anything reaching the
public network).

**UI-25 (MUST).** A route registered *after* the authentication layer and
guarded only by a local-address check is **not** protected: a same-host reverse
proxy satisfies it for every remote caller.

### 10.5 Thin client

**UI-26 (MUST).** A thin client MUST be supportable with **no** local inference,
runtime, or knowledge: transport, a display cache, and a fail-closed connectivity
monitor.

**UI-27 (MUST).** Long context MUST be host-side: the client sends only the new
turn and a conversation identifier, never re-uploads history and never embeds.

**UI-28 (MUST).** The client MUST re-emit the same stream events the shared
rendering layer consumes, so one rendering implementation serves both.

**UI-29 (MUST).** Local-only sources MUST be privacy-badged on the client.

### 10.6 Support and diagnostics

**UI-30 (MUST).** A three-layer support surface MUST exist, in the order a
person hits them:
1. **Fix it yourself** — a health panel running a fixed check set over gathered
   facts, with a pure evaluation function, a terminal-free repair hint on every
   non-OK check, and **unknown** rendered for an unreachable probe (never a
   fabricated verdict).
2. **Report the machine** — a diagnostic report file for **any** reason, not
   only a crash, with an unknown reason degrading to *other* (a user trying to
   report a problem must never be blocked by an enumeration).
3. **Report one answer** — a per-turn report for the complaint that machine
   state cannot explain, assembled from the delivered message's **persisted**
   metadata rather than from in-memory state.

**UI-31 (MUST).** Each report MUST carry a short speakable **reference code**
derived from the message identity by a **pinned** function — a wire format, not
an implementation detail (X-CO-2).

**UI-32 (MUST).** Source text in a report MUST be opt-in per report, defaulted
off, with the gate enforced by the renderer rather than trusted to the caller.

**UI-33 (MUST).** Every report MUST be a file the user reads before sending, and
MUST **state its own contents**, derived from what is actually in the file
(X-PV-6).

**UI-34 (MUST).** Documentation MUST exist in two registers: a no-terminal guide
for end users and a maintainer guide, with the maintainer guide pointing at the
user one.

### 10.7 Deep links and shell integration

**UI-35 (SHOULD).** The application SHOULD register a custom URL scheme for
create/join flows, carrying reachability hints, plus a system tray with status
and a pause control.

---

## 11. D9 — Authoring and extensibility

### 11.1 Workflows

**WF-1 (SHOULD).** A typed dataflow **workflow** capability SHOULD exist —
steps producing artefacts, run by a runner, over local model steps — with a
content-addressed cache and collection mapping.

**WF-2 (MUST, where WF-1 is implemented).** The step-kind catalogue MUST be the
single source from which any authoring schema is derived (X-SD-4).

**WF-3 (MUST).** Two run entry points MUST exist: one building its own provider
(for command-line and automatic triggers) and one taking an **injected** provider
plus an optional per-step observer (for a graphical host streaming progress).

**WF-4 (MUST).** A workflow MUST be diffable byte-for-byte against the
production pipeline stage it claims to replicate, or the claim is unproven.

**WF-5 (SHOULD).** Natural-language workflow authoring SHOULD mirror source-
specification authoring: a constrained structured author, validation, and test.

**WF-6 (MUST).** The authoring project model MUST carry an artefact **kind**, so
one checkpoint, decision-log, and workspace machinery backs every artefact type.

### 11.2 Liftable authoring package

**WF-7 (SHOULD).** The authoring surface SHOULD be **liftable**: buildable
against only the protocol contracts, with no dependency on the reasoning runtime,
the knowledge substrate, or the federation layer — enforced by an automated
boundary gate against a written contract.

**WF-8 (MUST, where WF-7 is implemented).** A headless authoring client MUST
exist that authors and tests against any conforming endpoint — the proof the
package is independently usable.

**WF-9 (MUST).** Where a shared vocabulary is needed by both the authoring
package and the knowledge substrate, it MUST be carved into a leaf both reach
**downward** to. The upper layer must never reach up.

### 11.3 Interoperability

**WF-10 (MUST).** The system MUST be pointable-at by unmodified third-party
tools, and MUST publish task-oriented recipes per socket plus a statement of
which surfaces are contracts and which are not.

**WF-11 (MUST).** A conformance tester for the capability protocol SHOULD ship
standalone, with minimal dependencies, so a third party can certify their own
implementation.

---

## 12. D10 — Code intelligence and self-maintenance

This domain exists because the system maintains itself with agent assistance.
Every requirement here is about making an autonomous agent's beliefs about the
codebase *checkable*.

### 12.1 Symbol and call graph

**CI-1 (MUST).** A compiler-resolved symbol and call graph MUST be maintained,
answering at minimum: define a symbol; who calls it; what it calls; transitive
blast radius; concept search.

**CI-2 (MUST).** Graph queries MUST be exact where the language permits, and
MUST catch dispatch that text search misses.

**CI-3 (MUST).** Where a stored classification field is unreliable, the **one
decider** MUST derive classification from the field that is reliably populated,
and the unreliable fields MUST be documented as not-to-be-read (X-SD-1).

**CI-4 (MUST).** The graph MUST be **live**: the query tools and the updater
MUST share one handle, so an update is visible without a restart.

**CI-5 (MUST).** Freshness MUST be two-tier: a cheap, embed-free, syntax-level
overlay on every save giving fresh definitions in milliseconds, and a heavy
whole-workspace export that is **demoted** — never blocking, rate-limited,
quiescence-gated with a starvation cap, and de-prioritised at the operating-system
level so it yields to interactive work.

**CI-6 (INVARIANT).** A cooldown MUST be stamped at the **end** of the run it
gates, by a guard covering every exit path including panic, and MUST exceed the
slowest measured run.
⟨why⟩ Stamped at start and shorter than the run it gated, the gate reopened
before the previous run had released; continuous editing pinned it at an ~88–90%
duty cycle holding ~14 GB.

**CI-7 (INVARIANT).** A freshness stamp MUST **travel with the rows** it
describes, forward-only.
⟨why⟩ An importer carried the rows but not the source's timestamp, so a merged
handle's clock never advanced: it only ever received imports and never recorded a
rebuild of its own, reporting a staleness equal to process uptime forever while
serving fresh data. Two freshness surfaces then disagreed permanently, and one of
them advised an action that added a second full export on top of the one already
running.

**CI-8 (MUST).** The export MUST be **one-writer and guarded**: staged then
atomically renamed, under a cross-process lock, refusing to replace a populated
graph with an empty one — so a query in flight always sees a complete graph and a
restart mid-export cannot empty it.

**CI-9 (MUST).** Follow-up passes MUST run under the **same** permit as the pass
that scheduled them, with a bounded count; a wall-clock watchdog and a scope
guard MUST clear both the worker flag and the claim on hang or panic, record the
failure, and log loudly.
⟨why⟩ A follow-up pass that re-acquired the sole permit self-deadlocked: status
read *active* for hours while every request coalesced silently.

**CI-10 (MUST).** A refresh command MUST **verify**: nudge, poll to a named
verdict (completed / failed / crashed / wedged / unreachable), and print an
explicit result — with a local fallback through the **same** lock, exiting with an
error rather than racing.

**CI-11 (INVARIANT).** **One project owns one workspace.** Registration MUST
refuse a root that is an ancestor or descendant of a registered root, naming the
conflict, with an explicit override.
⟨why⟩ Nested registrations collapse the freshness pipeline: every save inside a
shared subtree dirties all overlapping projects, each queues its own
whole-workspace export on the single permit, and the queue never drains. Observed:
four nested projects all permanently rebuilding, one never built.

**CI-12 (MUST).** Index posture MUST be **in-band**: every query result MUST
carry a health trailer, and a degraded or aging index MUST NOT be able to
masquerade as "no matches" (X-DG-3).

**CI-13 (MUST).** Staleness MUST carry calibrated confidence levels, not a
boolean.

**CI-14 (MUST).** External analysis tool resolution MUST have **one decider**:
the invoking process's environment first, then well-known per-user toolchain
locations. Resolution MUST return the **absolute path**, and the child MUST be
spawned with an augmented environment — both halves are required, because these
tools shell out to their own runtimes.
⟨why⟩ A background service runs with a minimal environment while a diagnostic
runs in the operator's shell. Resolving by name in two processes let the
diagnostic report a tool as present that the service could not execute, and the
index silently stayed empty. The diagnostic MUST therefore return a **warning**,
never a pass, when a tool resolves only through the calling shell.

### 12.2 Derived architecture and reconciliation

**CI-15 (SHOULD).** The system SHOULD derive *what the codebase does* from its
own call graph — clustering entry points that share a reachable spine into
**capabilities** — and reconcile those against the prose documentation, producing
**corroborated / undocumented / drifted** findings.

**CI-16 (MUST, where CI-15 is implemented).** Drift judgement MUST be biased
hard toward precision: one phantom contradiction destroys trust.

**CI-17 (MUST).** The **deterministic floor** of the reconciliation MUST run in
public continuous integration: every repository path a governing document cites
MUST resolve, and machine-local citations MUST fail the build. Model-dependent
layers stay out of the critical path.

**CI-18 (SHOULD).** Duplication SHOULD be detectable along four independent
axes, each with its own instrument, because they find different things:
- **identity** — one name defined in more than one place;
- **role** — one purpose under many names;
- **shape** — one concept with the same field structure under different names;
- **behaviour** — one algorithm written twice.

**CI-19 (INVARIANT).** An identity collision counts **only when at least two
definitions are each reachable by another component**. A collision whose every
definition is private has nothing to import, so no amount of adoption retires it.
⟨why⟩ Measured, 239 of 275 reported collisions (87%) were exactly that, and a
work order had already been derived from the inflated number and was unreachable
when written.

**CI-20 (MUST).** Shape matching MUST compare **no names at any step** — field
names and field types only — with inverse-document-frequency weighting so
ubiquitous fields score near zero, and a symmetric score so containment does not
score perfectly for any small structure swallowed by a large one. It MUST have a
pinned positive control and a measured precision with a confidence interval.

**CI-21 (MUST).** These instruments MUST be **mirrors, not gates**: no
threshold, no exit code, nothing to ratchet — feeding dispositions into a
human-owned register.

**CI-22 (MUST).** A **concept register** MUST declare one canonical owner per
domain noun, with a ratchet baseline. Minting a new type MUST be preceded by a
cheap read-only query answering "does this concept already exist and who owns
it".

**CI-23 (MUST).** A baseline re-minted after a counting-rule change is **not
comparable** to one stamped before it, and this MUST be recorded at the mint.

### 12.3 Notes, decisions and coordination

**CI-24 (MUST).** A **decision record store** MUST exist, queryable, replicated
across peers, holding decisions, invariants, todos, and failed attempts —
written at the moment, not at session end.

**CI-25 (MUST).** A **work atlas** MUST give cross-machine awareness of what
other agents and humans are doing, with two grades of signal: explicit
**declarations** (scope, intent, time-to-live) and passive **observations**
derived from edit activity, aged out.

**CI-26 (INVARIANT).** A released declaration is **gone** — no surface records
it. Peers see live state, not a log of everything ever attempted.

**CI-27 (MUST).** The intent field is load-bearing: a colleague must be able to
read it and immediately know whether their work overlaps.

**CI-28 (MUST).** Blast-radius queries MUST surface overlapping peer claims
inline, treated as a collision warning rather than an informational note.

**CI-29 (SHOULD).** A **session continuity** surface SHOULD exist: distil a
session transcript into a structured frame, index frames for selection by branch
match, prompt overlap, and recency, and inject the relevant frame at session
start. Reads MUST be pure filesystem reads, so handoff survives a dead service.

**CI-30 (MUST).** Context spend MUST be auditable — where the budget went, by
file and by session, with counterfactual pricing of the levers that would change
it.

### 12.4 Agent-facing quality surfaces

**CI-31 (MUST).** Compilation and test feedback MUST be available through a
wrapped gate that: resolves the project's real build configuration; reports
build-script, feature, and link failures as failures (X-DG-1); refuses a
zero-test run (X-EH-3); and refuses unattributable results (X-DG-2).

**CI-32 (MUST).** Watcher liveness MUST be heartbeat-driven and self-healing,
and every status response MUST carry a liveness object. When the watcher is not
live, the result is **orphaned** and MUST be reported as such — never as fresh.

**CI-33 (MUST).** A configured-but-dead watcher MUST be detectable by a probe of
the same signal; a configuration-presence check cannot see it.

**CI-34 (MUST).** A **posture** command MUST aggregate the freshness and verdict
of every quality subsystem into one table, each row naming its own refresh
command.
⟨why⟩ Two subsystems were both weeks stale with nothing aggregating that fact.

**CI-35 (SHOULD).** An architecture-health **evidence surface** SHOULD render
one deterministic self-contained page: a stable-layout map of every source file,
with layer violations, duplication, co-change communities, churn, and agent
read/write heat painted on it as drill-downs. It MUST render **evidence only** —
no scores, no gates — and MUST carry an honesty footer stating what the picture
cannot see, including the age of every input.

**CI-36 (MUST).** An increment mode MUST recompute **activity** measurements
over a window while **structure** stays full-history on the same stable layout,
written to its own artefact so the default baseline is never touched.

### 12.5 Refactor machinery

**CI-37 (SHOULD).** Large mechanical refactors SHOULD be **priceable before
anything is applied**: an entry gate per item, a seed edit, a compile, and
deterministic classification into *ruled* versus *residue*, with four per-unit
verdicts. It MUST be read-only, with a restore guard enrolling every file before
writing it.

**CI-38 (INVARIANT).** The refactor ledger MUST have **no database and no
stored progress**. A holding is open *if and only if* its detector still fires on
it, so closing re-runs the instrument and nothing can be marked done by hand. A
dead worker session is then a no-op needing no reconciliation.

**CI-39 (MUST).** Every ledger run MUST carry a **negative control**; a detector
whose control site went silent is *could-not-judge* and closes nothing.
⟨why⟩ This caught a mis-specified control on its first live run.

**CI-40 (INVARIANT).** Scoping MUST be a **post-filter**, never a narrowed
input, wherever a predicate is population-relative — narrowing the input makes
such a predicate wrong, not cheap.

**CI-41 (MUST).** Judgement keys MUST carry no coordinates, so a judgement
survives its subject moving; a rename is the one lossy case and MUST surface as a
named orphan.

**CI-42 (MUST).** A **wire differ** MUST prove that adopting a typed value at an
untyped site does not change the emitted bytes, with one implementation of that
question used by every consumer, and a negative control kept deliberately
failing.

### 12.6 Autonomous coding surfaces

**CI-43 (SHOULD).** A **canonical agent-tool surface** SHOULD exist: a small set
of primitives (inspect a working directory polymorphically over file / directory
/ find / grep; write a file; patch a file; replace a function; build; smoke test;
declare done; declare a plan; hand off between roles) that every external runner
translates to and from — so runners are compared on judgment, not on tool
vocabulary.

**CI-44 (SHOULD).** A **role layer** SHOULD operate over the same model weights
via different prompts, tool subsets, and forced first tools — at minimum planner,
implementer, and evaluator. Role profiles MUST be data (X-EX-4) shared with the
synthesis role layer (RT-64), not a second definition.

**CI-45 (SHOULD).** A **graded coding battery** SHOULD measure end-to-end coding
agents, runner- and model-agnostic, across several languages, judged on an ordinal
scale against anchor rubrics on several dimensions per problem, dispatched through
a runner registry.

**CI-46 (SHOULD).** A **unified solver loop** SHOULD exist for any
test-driven-development-shaped workflow: one entry taking a trial and returning a
result, with an explicit **polarity** (maximise passing tests, or generate one
failing test), and an observed variant emitting per-round progress. A verbless goal
entry MUST dispatch by observed state — failing tests present → fix; none →
pin-then-green — with explicit verbs available.

**CI-47 (MUST, where CI-46 is implemented).** The solver MUST be hosted as an
**asynchronous job API**: submit (returning immediately with the detected
framework, test command, and model), poll state and rounds, stream round events,
and cancel. It MUST be local-only, with a concurrency cap per working directory
and globally, and MUST use the host's own inference endpoint as its backend.

**CI-48 (INVARIANT).** A client-supplied test command reaches a shell. Any route
accepting one MUST be in the **privilege** removability group (UI-24), regardless
of whether it sits behind the authentication layer.

---

## 13. D11 — Evaluation apparatus

The evaluation apparatus is not an accessory. It is the mechanism by which every
claim in this specification stays true.

### 13.1 General discipline

**EV-1 (MUST).** Retrieval, routing, synthesis, enrichment, grounding, honesty,
and scheduling MUST each be measurable, and there MUST be **one composed suite**
that spans the whole chain by *composing* the individual harnesses, not by
reinventing them.

**EV-2 (MUST).** The composed suite MUST be the front-line regression check. The
individual harnesses are for drilling into a lane the suite flags.

**EV-3 (MUST).** Lanes MUST carry an explicit tier, and the tier MUST be read
before the number:
- **hard** — deterministic, baseline-diffed, build-breaking;
- **soft** — judge-mediated; variance must not flake the build;
- **tracked** — advisory; an absolute verdict is a finding about the current
  system, not a regression — each paired with a **hard** gate that re-scores the
  same artefact and fails only on regression against a committed baseline.

**EV-4 (MUST).** Gate logic MUST be one shared metric/direction/tolerance
primitive across all lanes (X-SD-1). A first run MUST pass.

**EV-5 (MUST).** Two run modes MUST exist: a lean pre-push tier down-sampling
slow lanes to a stratified whole-unit subset, and a full release tier.

**EV-6 (INVARIANT).** **A sampled lane's baseline is cap-specific.** It covers a
different subset than the full run, so changing a sample size requires
re-capturing that lane's baseline or its gate false-fires against a stale one.

**EV-7 (MUST).** Baselines MUST record the **concrete artefact** that produced
them — resolving any alias to the specific model — and MUST stamp that identity
into every transcript row. An alias is worthless the moment it is re-pointed.

**EV-8 (MUST).** A durable per-model report tree MUST be derivable from
suite-keyed baselines, grouping variants under one heading while keeping each on
its own row, and **surfacing unattributed legacy baselines rather than folding
them in**.

**EV-9 (MUST).** Noise bands MUST be documented per lane type, with baseline-age
semantics and a legitimate re-mint path, so "is this real?" is answerable without
guessing.

### 13.2 Judges and calibration

**EV-10 (MUST).** Any judge MUST be **forced-choice** with evidence quotes and
**could-not-judge first-class** (X-EH-2).

**EV-11 (MUST).** A judge MUST pass a **calibration gate** against a
hand-labelled bank, with stated sensitivity and specificity floors, before its
verdicts are used.

**EV-12 (MUST).** Reporting MUST use confidence intervals, and a difference MUST
be marked significant only when the intervals are **disjoint**.

**EV-13 (MUST).** Where several lanes judge per criterion, the apparatus MUST be
shared: one forced-choice judge, one calibration gate, one weighted scorer, one
reporting formula. A lane binds to it and owns no private copy.

**EV-14 (MUST).** A criterion vocabulary MUST be a closed, auditable set, and a
comparison across vocabulary versions MUST be **refused**.

**EV-15 (MUST).** Where criteria are selected, they MUST be selected by
**question type, never by probe content**, so no corpus vocabulary can reach a
criterion and the teach-to-the-test audit surface is the closed vocabulary rather
than the generated criteria.

### 13.3 Honesty and calibration benchmarks

**EV-16 (MUST).** A benchmark MUST exist that measures the product's central
claim: **answer capably and cited when the facts are present; abstain honestly
when they are not; unfooled by distractors.**

**EV-17 (INVARIANT).** The bank MUST enforce a **fairness contract at load**:
answerable items must ship a witness; absent items must not.

**EV-18 (INVARIANT).** The scorer MUST have **two red lines** and MUST NEVER
blend competence-when-present with honesty-when-absent — so neither a
hallucinator nor a blanket-abstainer can game it.

**EV-19 (MUST).** It MUST drive the **live production path**, sealed to one
corpus, with the corpus installing machine-stably from a committed
specification, so the gate reproduces across machines.

**EV-20 (SHOULD).** A **process** layer SHOULD sit over the outcome layer,
scoring *which situated behaviour* failed — grounding citation, gap naming,
actionable abstention, outside-knowledge restraint — by re-scoring the
transcripts the outcome benchmark already produced, so there is no benchmark-local
loop for a benchmark-only scaffold to live in.

**EV-21 (SHOULD).** A **metamorphic** benchmark SHOULD test whether a frozen
model reasons from a causal mechanism or a memorised label, using a forced-choice
probability distribution in one forward pass, a provably-blind negative control,
and anytime-valid early stopping to a go/no-go verdict — distilling a per-model
characterisation that is read free per query and invalidated by fingerprint.

**EV-22 (SHOULD).** A **safety** benchmark SHOULD score any wellbeing-adjacent
lane with **two-tier, never-averaged** scoring: a safety number (proportion of
turns with zero red lines, which must reach effectively 100%) and a quality
composite over the safe turns only. A hand-labelled calibration bank MUST gate any
rubric change on breach sensitivity.

**EV-23 (MUST).** A deterministic pre-routing wellbeing gate MUST exist where
such a lane exists, and any change to it MUST re-pass the adversarial suite.

### 13.4 Negative controls

**EV-24 (MUST).** A test suite MUST prove it **can fail**. Every other measure
(spec counts, coverage, fixture liveness) describes what tests *reached*.
Negative controls break the product on purpose and require the tests claiming
that coverage to go red.

**EV-25 (MUST).** Negative controls MUST have two layers: staged broken
conditions against the real-mode invariant pack, and declared source mutations
reported as **caught / survived / stale**. A *survived* verdict is a bug report
about the suite.

**EV-26 (MUST).** Negative controls MUST gate. A suite whose negative controls
are advisory is a suite nobody has checked.

### 13.5 Contract census

**EV-27 (MUST).** A **use-case contract** MUST declare, as data: the command
surface, the sequenced journeys, the promises those journeys serve, and the
ledger of commands belonging to no journey.

**EV-28 (INVARIANT).** **A step that asserts an exit code asserts nothing**,
because tools of this kind exit successfully when they find nothing. Every step a
lane runs MUST assert **output** — read inline, or proven by a later step.

**EV-29 (MUST).** The census MUST split steps **a lane runs** from steps
**nothing runs**, because a step in a never-run journey is a written intention,
and adding an exit-code assertion to it satisfies a ratchet without adding
evidence.

**EV-30 (MUST).** At least three gates MUST be **hard zeros**: every live step
asserts something; every live read step asserts output; every live journey
asserts output somewhere. Never-run debt MUST be capped and shrink-only.

**EV-31 (MUST).** The reported number and the enforced number MUST come from one
renderer (X-SD-1).

**EV-32 (MUST).** Journeys needing state a throwaway sandbox cannot have MUST
declare that need, and a second read-only lane MUST run exactly the remainder
against a real environment, so nothing is dropped by both lanes.

### 13.6 Where gates run

**EV-33 (MUST).** The **primary** correctness gate MUST be local (a pre-push
hook), version-controlled so it updates with a pull rather than drifting
per-machine, **failing closed** when it cannot determine what changed.
⟨why⟩ A metered hosted gate exhausted its allowance and every job began
aborting in seconds with a billing message that, on a change-request page, is
nearly indistinguishable from a gate that ran and passed.

**EV-34 (MUST).** Hosted continuous integration MUST confirm the same thing on a
clean checkout and MUST invoke the *same* gate script, so both share one
definition of "the tests pass".

**EV-35 (MUST).** Test executors MAY differ for speed but MUST NOT differ in
coverage. Where an alternative executor cannot run a class of tests, that class
MUST be appended unconditionally. An explicit executor request MUST error rather
than silently downgrade.

**EV-36 (MUST).** Scoping levers MUST have documented **reach**, and a lever
that cannot narrow the build MUST say so. A scope derived from a filter pattern
MUST **over-approximate**, and the over-approximation MUST be verified.

**EV-37 (MUST).** A non-crate path or an unresolvable scope MUST fall back
**loudly** to the full scope. The gate must never silently under-cover.

**EV-38 (MUST).** Tests MUST require no accelerator, no network, and no model
weights. Deterministic inference stubs, in-memory storage with real text search,
and a deterministic simulated fleet MUST cover the functional surface.

---

## 14. D12 — Operations

### 14.1 State roots

**OP-1 (MUST).** Configuration and mutable state MUST live on a small number of
well-defined roots with a documented purpose each, at minimum:
- **per-checkout** — project identity, per-repository posture, working notes;
- **per-user** — the user's configuration, indexes, artefacts, sessions, logs;
- **platform data** — the machine's federation identity, deliberately
  platform-native so a graphical application and a command-line tool share it;
- **committed contracts** — versioned, reviewed declarations (layer policy, knob
  registry, ratchet baselines, concept register, use-case contract, model
  selection, source catalogue, lint budgets), each with a stated writer (human or
  machine only) and a stated enforcer.

**OP-2 (MUST).** Ratchet baselines MUST be **machine-written only**, updated
through an explicit command, never edited by hand.

**OP-3 (MUST).** A machine identity MUST be independent of any membership and
MUST survive every membership change (FE-3).

**OP-4 (MUST).** A property of the **machine** ("this node serves remote
callers") MUST NOT be stored per-membership, because it is decided before any
membership exists.

**OP-5 (MUST).** Legacy layouts MUST be migrated on first start, idempotently.

### 14.2 Lifecycle

**OP-6 (MUST).** The background service MUST be installable as a supervised
system service on each supported platform.

**OP-7 (MUST).** The graphical application MUST be able to run the service as a
**supervised child process** rather than in-process, so a fatal fault in the
numerical layer leaves a recoverable interface (X-DG-6).

**OP-8 (MUST).** The child SHOULD be the same executable re-entered with an
argument, so there is no second artefact to keep in sync and no extra bytes in
the bundle.

**OP-9 (MUST).** Falling back to the in-process mode MUST be surfaced as an
event the interface renders, never silent, with a user-reachable reconnect.

**OP-10 (INVARIANT).** **Shutdown must be explicit, not incidental.** The exit
path MUST signal the child and **await** its reaping within a bounded budget
before the parent leaves.
⟨why⟩ The parent exits by a path that runs no destructors (deliberately, to
dodge a shutdown fault in a native library), so drop-based cleanup never fires.
Without an explicit shutdown the child was orphaned on every quit, then aborted
when it next logged — its output handles died with the parent, the logging layer
reported the failed write on the equally-dead error channel, that panicked, and
the panic handler's own write panicked again: nested panic, abort, and a crash
report on every voluntary quit. A panic handler MUST therefore write through a
non-panicking path and emit no logging event.

**OP-11 (MUST).** Every start-up fallback path MUST stop the child before
returning, so "unsupervised" cannot mean "still running and racing for the port".

**OP-12 (MUST).** First-run setup MUST end in the supervised configuration, so a
fresh install is supervised from its first post-setup minute.

**OP-13 (MUST).** A partial configuration reload MUST rebuild only what changed,
with a declared table of field → action, and MUST report `restart required` for
fields it cannot swap. Where a restart is required, the surface SHOULD perform it
through the platform service manager.

### 14.3 Attach mode

**OP-14 (MUST).** Where a graphical application and a background service both
want the same port, the application MUST **probe** at start-up and, on success,
attach: inference through a remote provider, mutations over the control plane,
and configuration changes pushed as a reload.

**OP-15 (INVARIANT).** In attach mode the application MUST read **its own**
configured state root. It MUST NOT adopt another process's data location at
boot.
⟨why⟩ That location also holds the user's entire conversation history.

### 14.4 Health and diagnostics

**OP-16 (MUST).** A first-stop diagnostic command MUST exist covering, at
minimum: engine, model, federation, peers, knowledge, disk, and stability — with
an unreachable probe rendering **unknown** (X-EH-2).

**OP-17 (MUST).** A liveness endpoint's actual path MUST be documented, and any
health check MUST distinguish a not-found response from a healthy one. A shell
idiom that treats any response as success is non-conforming.

**OP-18 (MUST).** Custom diagnostic channels MUST be enrolled in the deployed
service's filter, or they are dark. Enrolment MUST be pinned by a test that
renders a real event through a real subscriber.

**OP-19 (MUST).** Long jobs MUST be observable from outside the process that
started them.

### 14.5 Resource discipline

**OP-20 (MUST).** A memory ceiling that terminates the process MUST default off
and be opt-in for supervised deployments; a soft warning stays on.

**OP-21 (MUST).** Background work MUST yield to foreground work, bounded
(X-ST-4).

**OP-22 (MUST).** Where a shared resource can be contended (a build lock, a
single rebuild permit, an accelerator), acquisition MUST be visible and hangs
MUST be detected by a watchdog with a loud record.

### 14.6 Deployment

**OP-23 (MUST).** A shared air-gapped deployment MUST be supported with: written
rationale, an operator-facing brief, a **line-by-line audit of every outbound
call and its kill switch**, hand-written configuration files, service units, a
route allow-list, and separate packaging, installation, and acceptance scripts.

**OP-24 (MUST).** The egress audit MUST be read before any claim that the system
makes no outbound connections (X-PV-2).

---

# PART IV — NON-FUNCTIONAL REQUIREMENTS

## 15. Non-functional requirements

### 15.1 Performance envelopes

These are the shapes the system must have. Absolute numbers depend on hardware;
the **shapes** are requirements.

**NF-1 (BAR).** Per-turn retrieval wall time MUST be flat, within noise, across
at least a 10× range of installed corpus count (ST-32).

**NF-2 (BAR).** Conversation context carried forward MUST be constant with
respect to thread length (RT-51).

**NF-3 (BAR).** A grounded turn's verification cost MUST be bounded by the
drafter's already-budgeted evidence, and the verification prompt MUST be strictly
smaller than the drafting prompt (GR-4).

**NF-4 (BAR).** Start-up MUST NOT require embedding static classifier exemplars
(RT-28). **BAR:** first launch on a compute-only embedding path must not add
minutes.

**NF-5 (BAR).** The build-correctness gate MUST complete in tens of seconds warm
for a full workspace. A gate slow enough to skip is a gate nobody runs.

**NF-6 (BAR).** The composed quality suite's lean tier MUST finish in well under
an hour; the full tier is a release gate.

**NF-7 (BAR).** Index maintenance MUST cost only a metadata read per corpus per
cycle in the common case (ST-13).

**NF-8 (BAR).** A pre-flight placement plan MUST be instant even for a
hundreds-of-gigabytes model, because it reads metadata only (FE-73).

**NF-9 (SHOULD).** Editor-facing prediction MUST be fast enough not to be
noticed. **BAR:** p95 well under the typical inter-keystroke interval for the
deterministic lane.

### 15.2 Scale targets

**NF-10.** Thousands of installed corpora on one node.
**NF-11.** Hundreds of concurrent principals on one multi-tenant host, fairly
scheduled.
**NF-12.** Tens of federation members with epidemic convergence.
**NF-13.** Models several times larger than any single node's accelerator.

### 15.3 Portability

**NF-14 (MUST).** At least three desktop operating systems and two mobile
platforms, with graceful degradation where an accelerator is absent.
**NF-15 (MUST).** No test may require an accelerator, network, or model weights
(EV-38).
**NF-16 (MUST).** Deterministic components MUST produce identical output across
platforms.

### 15.4 Security posture

**NF-17 (MUST).** A written threat model MUST enumerate every listener, its
default binding, its authentication, and the honest gaps — and MUST be updated in
the same change that alters any of them.
**NF-18 (MUST).** No credential may be logged, and no credential may be
reconstructible from a diagnostic report.
**NF-19 (MUST).** Every trust decision MUST be re-derivable from what the
receiving side can *prove*, never from what the sending side *claims* (FE-16,
FE-93).

### 15.5 Documentation as contract

**NF-20 (MUST).** The system MUST maintain a document stating what IS, separate
from a document recording how it came to be. The former is a contract: every
claim must be verifiable against the code at the commit it appears in, and a
subsystem change MUST update its entry in the same change.
**NF-21 (MUST).** A deterministic gate MUST verify that every path the governing
documents cite resolves (CI-17).
**NF-22 (MUST).** Deferred work MUST be **ledgered**, so the next engineer
inherits a to-do list rather than a surprise. A large file or a documented gap
without an entry is a defect; with an entry it is sequenced work.
**NF-23 (MUST).** Completed entries MUST move to the history record and be
dropped from the ledger, so the ledger holds only live deferrals.

---

# PART V — ACCEPTANCE

## 16. How a rebuild is judged

A rebuild that reproduces the feature list and fails the following is **not** a
rebuild of this system.

### 16.1 The honesty acceptance suite

**A-1.** For every requirement in §2.1 (X-EH), the rebuild MUST demonstrate a
**failing case before the guard and a passing case after** — the failure must
have been watched to fail.

**A-2.** Drive the system with a sealed corpus and a bank whose fairness
contract is enforced at load. It MUST answer the answerable with citations, and
abstain on the absent, and the two MUST be scored on separate red lines
(EV-16 … EV-19).

**A-3.** Remove a corpus's readiness mid-run. The answer MUST name the lost
source and its reason (ST-36 … ST-40), and a turn that lost nothing MUST render
byte-identically.

**A-4.** Ask an authoritative-domain question of a surface that can install the
corpus but does not register its authoritative tool. The rebuild MUST NOT answer
ungrounded (GR-31).

**A-5.** Replay frozen transcripts through the real deciders and confirm that no
citation is demoted for a reason that is an artefact of prompt rendering
(GR-19).

### 16.2 The single-decider acceptance suite

**A-6.** For each of: storage layout, readiness, eligibility-and-disclosure,
preview-and-execution, per-item attributes, tool declaration, scoring
composition, and authority discovery — demonstrate exactly one implementation and
an automated check that fails when a second appears.

**A-7.** Add a new intent, a new tool, a new extractor, a new transport class,
and a new record stream. Each MUST require no edit to any dispatch site (X-EX-5),
and the intent MUST fail to build until every attribute column is supplied.

### 16.3 The failure-mode acceptance suite

**A-8.** Kill the numerical engine mid-decode. The control plane MUST survive,
the interface MUST offer reconnect, and the crash MUST be recorded locally
(IN-10, OP-7).

**A-9.** Crash-loop a subordinate whose start-up cost exceeds any plausible
window. The ceiling MUST engage (X-DG-7, X-DG-8).

**A-10.** Quit the application. No orphan, no crash report, bounded wait
(OP-10).

**A-11.** Present a public dial string with no credential. Every surface it can
reach MUST be one a stranger may have (FE-34 … FE-38).

**A-12.** As an authenticated member, attempt to mint a guest credential on a
peer's node. It MUST be refused (FE-35, FE-42).

**A-13.** Drive foreground activity at half the yield window indefinitely.
Background work MUST resume within the compiled deferral cap and MUST announce
the override (X-ST-4, FE-108).

**A-14.** Rotate the admission credential on a mixed fleet. It MUST refuse, and
the refusal MUST distinguish the two populations (FE-15).

### 16.4 The measurement acceptance suite

**A-15.** Every gate in the rebuild MUST be able to report all four verdicts, and
each MUST have been **watched failing** (X-EH-2, X-EH-7).

**A-16.** Negative controls MUST gate, and a *survived* mutation MUST be treated
as a defect in the suite (EV-24 … EV-26).

**A-17.** The use-case census MUST report zero live steps that assert nothing
(EV-28 … EV-30).

**A-18.** Every calibrated threshold MUST have: a bank containing abstention
cases, a per-case attribution, a rival identification, and a dated drift baseline
pinned to an exact model artefact (RT-14 … RT-26).

**A-19.** A benchmark record MUST be able to state what its number was for
(FE-90) and under what conditions it was taken (FE-91), and MUST refuse to serve
a peer's number as the reader's own (FE-93).

### 16.5 The half-the-code test

The rebuild is invited to be smaller. The following are the places where the
original system's size is **not** accidental, and where a smaller rebuild should
be checked for a missing requirement rather than praised:

- the four-verdict discipline and its plumbing (X-EH-2);
- the door set and provenance typing (X-PR-1 … X-PR-4);
- the passage → structure join and its repair path (ST-23 … ST-27);
- calibration tooling and its refusal to write constants (RT-14 … RT-27);
- the grounding ladder's evidence-universe unification (GR-2, GR-3);
- authority binding at the answer exit (GR-27 … GR-34);
- the three trust surfaces of one router (FE-36);
- per-device fit as one decider with fitting rows included (FE-74, FE-75);
- benchmark validity guards and the served-slot predicate (FE-86, FE-87);
- negative controls and the contract census (EV-24 … EV-32).

A rebuild that is half the size **and** passes §16.1–§16.4 has found real
redundancy. A rebuild that is half the size and fails any of them has found the
requirements it deleted.

---

## 17. Out of scope and deliberately deferred

The following are known, deliberate absences in the reference system. A rebuild
MAY address them; it MUST NOT be judged for not addressing them.

**OS-1.** Model training or fine-tuning.
**OS-2.** Any hosted or centrally-operated component; accounts; billing.
**OS-3.** True isolation of untrusted third-party sandboxed applications
(FE-131) — deferred pending a non-privileged bridge.
**OS-4.** End-to-end encryption of raw inter-process compute traffic between
distributed compute participants (FE-32). It requires a shared network and MUST
NOT be claimed as encrypted.
**OS-5.** Heterogeneous ports across federation members: the reference system
assumes uniform ports and states the assumption.
**OS-6.** Multi-model dispatch on the embedding endpoint; batched embedding.
**OS-7.** Per-turn quality prediction from a throughput rate card — filed as a
**measured regression**, not as unfinished work (FE-67).
**OS-8.** A ranking objective with units in production — blocked on the missing
signal it needs, not on the switch (FE-61, FE-69).
**OS-9.** Span-level provenance demotion in the user interface where the
admission signal is unavailable — the claim-level ledger is what ships.
**OS-10.** A curation overlay surviving re-extraction on the graph inspector;
forward-compatible fields exist.

---

# APPENDIX A — Vocabulary

Terms are defined so requirements can be stated precisely. Naming is not
prescribed.

| Term | Definition |
|---|---|
| **Corpus** | A named, self-describing body of indexed content with authoritative metadata, built by one source specification |
| **Passage** | The retrievable unit of a corpus; the thing a citation points into |
| **Source specification** | The declarative document configuring a whole acquisition pipeline for one source |
| **Catalogue** | The registry of published source specifications, with a bundled offline snapshot |
| **Shard** | A corpus holding a contiguous identifier range; structurally identical to a whole corpus |
| **Provenance** | The required, non-defaultable pair (acquired vs manufactured, verbatim vs summary) carried by every passage |
| **Door** | One of the small enumerable code paths permitted to stamp *acquired* provenance |
| **Seal** | A closed body of evidence a released answer is verified against |
| **Join** | The mapping from stored passages to the structural units of their source documents |
| **Enrichment** | Any derivation over an indexed corpus producing structure the raw index does not carry |
| **Assertion graph** | A typed graph of entities, claims, events, positions and relations extracted per document |
| **Summary tier** | A hierarchical tree of recursively-clustered summaries over passages |
| **Residency slot** | A named model-loading position by role (fast, primary, code, embedding) |
| **Capability claim** | A model's published statement of one kind of work it does well |
| **Workload requirement** | What a call site declares it needs, resolved by a scheduler against claims |
| **Intent** | A member of the closed classification of what a turn is asking for |
| **Move kind** | Whether a policy decision commits, proposes, or asks |
| **Axis** | One classification dimension with its own exemplars, scoring, and calibrated gate |
| **Gate (classification)** | A (minimum similarity, minimum margin) pair deciding whether an axis fires or abstains |
| **Grounding gate** | The verification ladder standing between a draft and its release |
| **Marking** | Releasing an audited draft with failed claims flagged, rather than re-synthesising |
| **Authority domain** | A corpus for which a declared tool is the authoritative source of figures |
| **Trust ring** | A closed voluntary set of federation members sharing inference and knowledge |
| **Member** | A node holding a ring's participation credential |
| **Guest** | A non-member holding a time-limited, scope-bounded grant |
| **Traffic class** | The category of peer traffic that decides which transport carries it |
| **Transport seam** | The single resolver of (peer, traffic class) → ordered endpoints |
| **Decision record** | The full log of one routing decision: candidates, scores, stamped inputs, exclusions, verdict |
| **Contribution ledger** | The replicated append-only record of work done for others; no balance, no ranking |
| **Activity ledger** | The non-replicated record of work done for oneself |
| **Work atlas** | Cross-machine awareness of what other agents and humans are currently working on |
| **Decision record store** | The queryable, replicated store of decisions, invariants and failed attempts |
| **Lane** | One measurable dimension of the composed quality suite, carrying a tier |
| **Ratchet baseline** | A machine-written, shrink-only count that a gate compares against |
| **Four verdicts** | passed / failed / could-not-judge / never-ran |
| **One decider** | The requirement that a rule have exactly one implementation |
| **Glassbox** | The property that a decision is visible at a diagnostic level without a code change |

---

# APPENDIX B — Requirements by risk

If schedule forces sequencing, this is the order in which omission is most
expensive to retrofit. Items higher in the list constrain the shape of
everything below them.

**Tier 0 — unretrofittable.** Choosing wrongly here means a rewrite, not a fix.

1. Provenance as a required non-defaultable property with a closed door set
   (X-PR-1 … X-PR-4).
2. The four-verdict discipline everywhere a check reports (X-EH-2).
3. Absence as a typed value; never defaulted (X-EH-1).
4. One decider per rule, with mechanical detection of a second (X-SD-1).
5. Closed sets exhaustive, open sets registries (X-EX-1, X-EX-2).
6. The passage → structure join, present from first ingest (ST-23).
7. Per-item total attribute records with no default (X-SD-5).
8. Trust surfaces as distinct bindings of one router (FE-36).

**Tier 1 — expensive to retrofit.**

9. The evidence-universe unification inside the grounding ladder (GR-2, GR-3).
10. Structural enforcement over instruction, everywhere a model is asked
    (X-ST-2).
11. The retrieval pipeline as data, with one shared evidence head (ST-28,
    ST-29).
12. Calibration tooling that writes no constant (RT-14 … RT-19).
13. The decision record and its replay split (FE-56 … FE-60).
14. Negative controls that gate (EV-24 … EV-26).
15. Idempotency by content-derived key (RT-36 … RT-38).

**Tier 2 — cheap to add later, expensive to have wrong.**

16. Stage and call attribution with residuals (X-OB-3, GR-49 … GR-53).
17. Index maintenance, gated and pruned (ST-11 … ST-15).
18. Unavailability disclosure (ST-36 … ST-40).
19. Fairness gates sharing one policy core (FE-96 … FE-108).
20. Measurement records carrying their own pre-image and conditions (FE-90,
    FE-91).
21. The support surface's three layers (UI-30 … UI-34).
22. Journal and telemetry as a generic, content-incapable mechanism (X-PV-7,
    IN-28).

---

*End of specification.*
