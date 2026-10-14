# Open Inference Capabilities Protocol (OICP) — v0.6

**Version:** 0.6.0 (draft)
**Status:** Draft, written in parts as each lands on the reference host. The
wire shape was approved by the operator 2026-10-12 (zoracite
`orders/s6-lean-route/order.md`, decision 1). Extends v0.5 additively; v0.5
remains the fallback path.
**License:** CC0 (public domain dedication)

---

## Abstract

v0.5 let a client hold a host to its sources for quotations. v0.6 does the
same for claims: **`verify`** judges a unit of text, claim by claim, against
the stored texts a caller may read, outside any conversation, and says for
each claim which rung decided and how far that rung is calibrated. Knowledge
search gains a **scope** (hits from named texts only) and a **range** on every
hit (where in its stored text it stands, §2.8), install progress says which
of a library's **documents** were read (§2.9), a client can **correct** a
verify decision so the host keeps the case it got wrong (§2.10), and search
can answer **sentences** ranked by meaning, each an address in its stored
text (§2.11), and a host states the **largest install** it reads, so a client
sizes an inline library before it sends one (§2.12).

As in v0.5, everything v0.6 adds is a serde-defaulted field, an optional
endpoint, or a feature string, and **feature presence, not the version
string, gates behaviour**. The types are in
`shared/crates/oicp-types/src/verify.rs` and, for search,
`shared/crates/oicp-types/src/knowledge.rs`.

## 2. The evidence extension, continued

### 2.1 Advertisement (additions)

| feature | meaning | requires |
|---|---|---|
| `evidence:verify` | `knowledge.evidence.verify_endpoint` judges a unit at the tiers `verify_tiers` names (§2.6) | `evidence:text`, `verify_endpoint`, a non-empty `verify_tiers` |
| `knowledge:scoped` | knowledge search takes `documents`, the text names its hits must be cut from (§2.8) | `knowledge:document` |
| `knowledge:range` | every knowledge-search hit carries `span`, its range of its stored text, or names why not in `span_absent` (§2.8) | `knowledge:document`, `evidence:text` |
| `evidence:verify_corrections` | `knowledge.evidence.verify_correction_endpoint` takes a client's correction of a verify decision (§2.10) | `evidence:verify`, `verify_correction_endpoint` |
| `ingest:documents` | install progress lists an install from its spawn, and each document it read once it has returned (§2.9) | `ingest:v1` |
| `knowledge:sentences` | knowledge search takes `granularity: "sentence"` and answers sentences, ranked by the scorer `knowledge.sentence_scorer` names (§2.11) | `knowledge:range`, `knowledge:scoped`, `knowledge.sentence_scorer` |

```rust
EvidenceEndpoints {
    … (v0.5 fields) …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verify_endpoint: Option<String>,   // e.g. "/oicp/v1/verify"; present iff evidence:verify
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    verify_tiers: Vec<VerifyTier>,     // the tiers it serves; empty iff verify_endpoint is absent
}
```

### 2.6 Verify

```
POST {verify_endpoint}
Request:  VerifyRequest  { unit: String,
                           context: Option<String>,      // text around the unit; read, never judged
                           corpora: Vec<String>,         // default []: every corpus the caller may read
                           tier: VerifyTier,             // "fast" | "checker" | "judge"
                           span_context: Option<u32> }   // default 32
Response: VerifyResponse { claims: Vec<VerifiedClaim>,
                           verifier: String,
                           corpora: Vec<CorpusTexts>,
                           corpora_unavailable: Vec<Unavailable> }

VerifiedClaim { start: u64, end: u64,            // code points into the unit
                exact: String,                   // == the unit's [start, end)
                class: Checkability,             // "checkable" | "not_checkable"
                verdict: VerifyVerdict,
                evidence: Vec<JudgedPassage>,
                specifics_absent: Vec<String>,   // fast tier: specifics no passage carries
                rung: String,
                score: Option<f32>,
                operating_point: String }        // "uncalibrated" until a bench measures the cell
JudgedPassage { corpus_id: String,
                span: Option<Span>,              // v0.5 §2.3's Span
                span_absent: Option<String>,     // why span is absent
                verdict: VerifyVerdict,
                score: Option<f32> }
VerifyVerdict = {"kind": "supported" | "contradicted" | "unsupported" | "abstained"}
              | {"kind": "could_not_judge", "reason": String}
```

- **Claims.** A host splits the unit into claims and returns every one, in
  unit order, each with its exact text. How it splits is its own (the
  reference host takes sentences). A claim the host does not check is
  `not_checkable` and `abstained`, never `supported`.
- **Verdicts.** `VerifyVerdict` is a closed set; an unknown kind is an error,
  never a default. `supported` and `contradicted` each rest on at least one
  passage with that verdict. `unsupported` says nothing judged supports the
  claim, which is not that the claim is false. `could_not_judge` names its
  reason.
- **Tiers.** `fast` uses no model. `checker` judges with one. `judge` is
  reserved for a stronger judge that quotes before deciding. A tier the host
  does not list in `verify_tiers` is refused with `422 tier not offered`.
- **Evidence.** Each passage judged is named with its own verdict and, where
  it is an exact slice of its stored text, its `span`, which dereferences
  through `text_endpoint` as v0.5 §2.3's spans do. A passage that is not an
  exact slice carries `span_absent: "not an exact slice"` instead of a range.
- **Calibration.** `operating_point` is `uncalibrated` until a bench has
  measured the rung's cell, and a client MUST NOT treat an uncalibrated
  verdict as a licence for more than a warning.
- **Identity.** `verifier` changes whenever the same request could be judged
  differently (a rung, a threshold, a model). A client keys cached verdicts on
  it together with each corpus's `texts_digest`.
- **Scope.** Every requested corpus lands in exactly one of `corpora` and
  `corpora_unavailable`, as in v0.5 §2.3. A checkable claim with no corpus to
  read is `could_not_judge: no corpus read`.

**The reference host's rungs** (`svrn/crates/sovereign-daemon/src/routes_oicp_verify.rs`):

| rung | tier | decides |
|---|---|---|
| `class/no-specifics` | any | a claim carrying no number, date or name is `not_checkable` |
| `fast/value-presence` | fast | each specific of the claim is looked for in the passages lexical search returns for the claim and for the specific; one found in none is `unsupported`, named in `specifics_absent`; otherwise `abstained`, because presence is not support |
| `checker/chunk-support` | checker | the in-turn gate's per-claim register judges each of the claim's best three lexical candidates in turn (the reference host's `CHECKER_PASSAGES`, named in `verifier` as `passages=3`); support at or above 0.5 on any is `supported`, otherwise `unsupported`. The register says support or not, so this rung never says `contradicted` |

The reference host serves `fast` wherever `verify` is mounted, and `checker`
only on the listener that holds its daemon's inference; elsewhere `checker`
claims are `could_not_judge: no checker`.

A checker call the host's model queue refuses for its predicted wait is
backpressure, not a verdict: the host answers the whole unit `503` with a
`Retry-After` header and the body every shed carries (`{"error": {"message":
"host busy: …", "type": "server_error", "code": "local_queue_full"},
"reason": "local_queue_full", "retry_after_secs": N}`), stops judging the
unit, and keeps nothing of it in the journal (§2.10). The client retries the
unit after `retry_after_secs`. `checker did not answer` is kept for a checker
that was asked and failed.

### 2.7 Named refusals and reasons (additions)

| reason | status | when |
|---|---|---|
| `empty unit` | 400 | the unit is empty or whitespace |
| `tier not offered` | 422 | the requested tier is not in `verify_tiers` |
| `local_queue_full` | 503 | the checker's model queue shed a call; the body is the shed body, with `Retry-After` (§2.6) |
| `no checker` | — | a verdict's reason: the tier needs a model this listener does not hold |
| `checker did not answer` | — | a verdict's reason: the checker gave no verdict on any passage |
| `no corpus read` | — | a verdict's reason: no requested corpus could be read |
| `not an exact slice` | — | a passage's `span_absent`: its text is not a slice of its stored text |
| `passage names no text` | — | a passage's `span_absent`: it names no stored text |
| — | 400 | a search's `documents` names something that is not a text name (§2.8) |
| `scorer unavailable` | 503 | the sentence scorer the host's data file names cannot run here; the body names it (§2.11) |
| `install too large` | 413 | an install body past the host's `max_install_bytes`; the body names the limit (§2.12) |

A passage whose stored text or record cannot be read gives v0.5 §2.5's reason
in `span_absent` (`texts not stored`, `text not stored`, `document
unreadable`), or an opaque one for a store error.

The span absences are in `oicp_types::evidence::reasons` (`SPAN_ABSENCES`),
shared by verify's passages and search's hits; `scorer unavailable` is in
`oicp_types::knowledge::reasons`; `install too large` is in
`oicp_types::ingest::reasons`; the rest are in `oicp_types::verify::reasons`.

### 2.8 Scoped and ranged search

The manifest's existing `search_endpoint` (v0.3 §6) gains one request field
and two hit fields. A host advertising neither feature ignores them, so a
client sends `documents` only to a host advertising `knowledge:scoped`.

```
KnowledgeSearchRequest { … (v0.4 fields) …,
                         documents: Vec<String> }    // default []: any text; else text names (64 lowercase hex)
KnowledgeResult        { … (v0.5 fields) …,
                         span: Option<Span>,         // v0.5 §2.3's Span, into the text `document` names
                         span_absent: Option<String> }
```

- **Scope.** Under a non-empty `documents`, every hit is cut from one of the
  named texts; a hit that names no text is never returned. A host applies the
  scope before it cuts its top `limit`, so a scope never thins its own hits.
  An entry that is not a text name is refused with `400`, never searched as
  no filter. The platform scopes by text name only: a citekey, a file name or
  a source id is a client's to map to a name.
- **Range.** Each hit carries `span` exactly when its text, any title header
  the host's chunker added removed, is an exact slice of its stored text; the
  span then dereferences through `text_endpoint` to its `exact`. Otherwise the
  hit names why in `span_absent`: its document's absence reason when it has
  no `document` (v0.5 §3), else `not an exact slice` or, for a text that
  cannot be read, v0.5 §2.5's reason. Never both fields, never neither.
- **Peers.** A mesh host forwards `documents` to its peers and holds their
  hits to the same scope, dropping any from an unnamed text, so a peer that
  predates the field cannot widen it.

The reference host (`svrn/crates/sovereign-daemon/src/oicp_evidence.rs`)
scopes inside the index (`CorpusIndex::search_in_texts`, a filter on each
chunk's `text_sha256`) and locates each hit at serve time with the locator
verify's passages use (`span_of`). Chunks cut as exact slices at ingest are
later work; until then some hits name `not an exact slice`.

### 2.9 Install progress by document

v0.4 §5.2's progress entry gains a document list.

```
CorpusIngestProgress { … (v0.4 fields) …,
                       documents: Vec<IngestDocument>,     // default []: until the install returns
                       documents_unlisted: Option<u64> }   // read past the host's cap; the list is partial
IngestDocument = { source: Option<String>,                 // the document's name in its recipe
                   "state": "ready",  text_sha256: Option<String> }
               | { source: Option<String>,
                   "state": "failed", reason: String }
```

- **From the spawn.** An install that answered `spawned: true` is in the
  very next poll's `progress`, `pending` until the host has more to say, so
  a client never mistakes "not started yet" for "already installed" (v0.4
  §5.3 rule 4's grace window becomes unnecessary for such a host).
- **By document.** Once the install has returned, its entry lists each
  document it read, in the order read: `ready` with its text's name (the
  name `evidence:text` reads it by), or `failed` with why. A document is
  named as its recipe names it: an inline `[[document]]` by its `name`, a
  file by its path relative to the acquired root. One unreadable file is one
  `failed` entry beside the others' `ready`; it does not fail the install.
  `source` is absent only when the host's extractor could not say which
  document failed.
- **Absence.** A document the source yielded no text for (an empty file) is
  not read, so it is not listed; a client treats a library document that is
  not listed `ready` as not ready. The list is the last install's: a new
  attempt starts it again.
- **Held.** A corpus the host holds is listed, `complete`, with its last
  install's documents for as long as it holds it, across the host's own
  restarts. An install answered `spawned: false` is followed by an entry
  that says which documents are ready; v0.4 §5.3 rule 4's licence to list an
  already-installed corpus never does not extend to a host advertising
  `ingest:documents`.

The reference host records each outcome in its ingest loop (`corpus-engine`
`ingest_inner_with_skipset`, `IngestResult::documents`), lists at most
10,000 per install (`DOCUMENTS_LISTED_CAP`), and keeps the list with the
corpus (`_documents.json`, `corpus_index::corpus::Corpus::install_documents`).
A corpus it held from before that record existed is read again on its next
install rather than answered `spawned: false` with nothing to list.

### 2.10 Corrections

A host that keeps a journal of its verify decisions gives each decided claim
an `episode_id` (`VerifiedClaim.episode_id`), and takes a client's word that
one was wrong:

```
POST {verify_correction_endpoint}
Request:  VerifyCorrection      { episode_id: String,
                                  expected: "supported" | "contradicted" | "unsupported",
                                  witness: Option<WitnessRef> }
WitnessRef                      { corpus: String, text_sha256: String, start: u64, end: u64 }
Response: VerifyCorrectionReply { recorded: bool,
                                  witness: "verified" | "not_held" | "out_of_range" | "not_given" }
```

- **What is kept.** The host keeps the decision and the correction by
  reference: the episode, the verdicts, the rung, the score, the evidence by
  handle, the witness by name and range. Never the claim's words: the
  client keeps those by the `episode_id`, and decides whether to share them.
- **The witness is read back** before anything is kept: `verified` when a
  corpus the caller may read holds the text and the range lies inside it,
  `out_of_range` when it does not, `not_held` for a text the caller cannot
  reach (one answer for both, as v0.5 §2.5's). It says the address is real,
  not that the passage supports the claim.
- **`recorded: false`** when the journal is switched off or full for the
  day: a posture, not an error. A listener that keeps no journal refuses with
  `503 no journal`; an `episode_id` that is not one the host issues is
  `400 not an episode id`.
- `expected` may be `contradicted`, which no rung yet decides: a correction
  can name the case the verifier cannot.

The reference host (`routes_oicp_verify_journal`) keeps both in its
grounding journal under surface `oicp_verify` (`svrn journal grounding`),
beside the in-turn gate's decisions: one line per decided claim, with the
claim's specifics counted against the passages read (`specifics: {checked,
absent}`), so a claim unsupported with every specific present (its facts in
the sources, in no single passage) is a query over the journal. `fast`'s
`abstained` and a `not_checkable` claim decide nothing and keep nothing.
Only the operator listener keeps a journal.

### 2.11 Sentence search

The manifest's `search_endpoint` takes one more request field. At its
default it is the search §2.8 describes; at `sentence` the hits are sentences
of the stored texts, best first by meaning, each one an address a client can
cite.

```
KnowledgeManifest       { … ,
                          sentence_scorer: Option<SentenceScorer> }  // present iff knowledge:sentences
KnowledgeSearchRequest  { … (§2.8 fields) …,
                          granularity: Granularity }    // "passage" (default, never written) | "sentence"
KnowledgeSearchResponse { … ,
                          scorer: Option<Scorer>,       // present iff the request asked for sentences
                          route: Option<SentenceRoute>, // beside scorer
                          sentences_pending: Option<u64>,   // present iff route = "passages"
                          truncated: Option<Truncated> }    // the scan stopped at the host's ceiling
Scorer          { name: SentenceScorer, model: String }
SentenceScorer  = "cosine" | "rerank"
SentenceRoute   = "index" | "passages"
Truncated       { scanned: u64, in_scope: u64 }
```

- **Hits.** At `sentence`, `results` holds at most `limit` sentences (20 by
  default), best first. Each is a `KnowledgeResult` whose `content` is the
  sentence, whose `document` is its text's record, whose `score` is the
  scorer's, and whose `span` (§2.8) is its code-point range of that text,
  dereferencing byte-equal through `text_endpoint` to `content`. A sentence
  is cut from its stored text, so it always carries `span`, never
  `span_absent`.
- **Sentences.** How a host splits a text into sentences is its own, as
  verify's claims are (§2.6); a hit's `span` says where each one stands. The
  reference host splits with `sentence_ranges`
  (`svrn/crates/sovereign-core/src/runtime/grounding/specifics.rs`), the rule
  its verify claims are split by: a sentence ends at `.`, `!` or `?` and any
  closers after it (`"` `'` `)` `]` `”` `’`) when the text ends there, or
  when whitespace follows and then an uppercase letter, a digit, an opener
  (`"` `“` `(` `[`) or the end; never at a `.` after a lone capital (an
  initial). Each range is trimmed of whitespace; a text with no stop is one
  sentence, and whitespace alone is none.
- **Scope.** `corpora` and `documents` apply as in §2.8, before the top
  `limit` is cut: under a non-empty `documents` every sentence is from a
  named text, and a malformed name is `400`.
- **Scorers.** The scorer is the one the host's data file names, advertised
  as `knowledge.sentence_scorer`, from a closed set; a name outside it is a
  decode error, never a default, and adding one is a spec revision.
  `cosine` scores the query's embedding (in the embedder's query space, with
  its `query_instruction_prefix`) against each sentence's (document space),
  over every sentence in scope; it is the default. `rerank` takes cosine's
  top candidates, as many as the data file says, and reorders them through
  the host's rerank slot; the scores are the reranker's. Every reply names
  the advertised scorer and the model that scored (`scorer.model`: the
  embedder for `cosine`, the reranker for `rerank`). Scores are comparable
  only under one scorer and model; a client that keeps a threshold keys it
  on `scorer.name`.
- **Refusal, never substitution.** A host whose scorer cannot run (`rerank`
  with no rerank slot, `cosine` with no embedder) refuses the search `503`
  with `{"error": "scorer unavailable", "scorer": "<name>"}`, and never
  answers it with another scorer. This is deliberately not the precedent of
  the reference host's passage search, whose
  `CorpusIndex::search_with_rerank` quietly falls back to fusion order when
  its reranker is missing or fails.
- **Route.** `index` says every sentence of every scoped text is embedded
  and every one was scored. `passages` says some scoped text is still being
  indexed: the host ran the passage search for the same query and scope,
  took the sentences of its top hits, and ranked those by the same scorer;
  `sentences_pending` counts the scoped sentences not yet embedded. A client
  reads `passages` as "still indexing", not as a smaller library.
- **Ceiling.** On the `index` route the scan is exhaustive up to a ceiling of
  sentences per scope, kept in the host's data file. A scope past it is
  answered from `scanned` of its `in_scope` sentences and says so in
  `truncated`; a reply without `truncated` scored every sentence in scope.
  A scope is never cut short in silence.
- **Gating.** A client sends `granularity` only to a host advertising
  `knowledge:sentences`. A host that does not advertise it answers passages
  with no `scorer`, which is how a client knows the field was not honoured.
- **Locality.** Sentence search answers from the host's own corpora. A host
  does not merge a peer's sentences, whose scores come from another model.

Conformance holds a reply to the scorer it names, not to its arithmetic: a
host that names `rerank` while scoring by cosine is beyond what a client can
see. The reference host does not yet serve this section and does not
advertise `knowledge:sentences`; its data file will name the scorer,
`rerank`'s candidate count and the scan's ceiling.

### 2.12 Large installs

v0.5 §4.1's inline recipe is how a client that shares no disk with a host
installs a library, and it carries every document's text in the one install
request. So the largest library a client can install that way is the largest
install body the host reads, and the manifest states it.

```
IngestEndpoints { … (v0.4 fields) …,
                  max_install_bytes: Option<u64> }   // the largest install body the host reads
```

- **Stated.** `max_install_bytes` is the largest request body, counted in
  bytes as sent (the request's JSON, every escape included), that
  `install_endpoint` reads. A host that states it reads every install body up
  to it, whatever bound it puts on its other routes. The measure is the
  body, not the sum of the texts: escaping a text for TOML and then for JSON
  grows it, so a client sizes the request it is about to send.
- **Refusal by name.** A client refuses by name, before it sends, an install
  whose body is past the stated limit. A host answers one `413` with
  `{"error": "install too large", "max_install_bytes": <n>}`, and never reads
  it in part or installs a prefix of it.
- **Unstated.** A host that states no limit may refuse an install `413` at a
  bound of its own, with any body. A client reads that `413` as "too large
  for this host" and has no number to size against.
- **One request.** An install is one request; v0.6 has no install in parts.
  A library past a host's limit installs from a source the host reads itself
  (a recipe naming files on a disk it shares), or not at that host.
- **The recipe test.** `max_install_bytes` bounds the install route only. A
  host's recipe test (v0.4 §5.4) may read less; a client that dry-runs a
  library past the host's other bounds samples it first.

The reference host states 64 MiB (67,108,864 bytes,
`routes_oicp_ingest::MAX_INSTALL_BYTES`), set on its install route alone.
Every other route on its client surface, the recipe test included, keeps the
surface's 8 MiB (`sovereign_peer_wire::MAX_REQUEST_BODY_BYTES`), which until
then bounded install too and refused an inline library of 22.6 MB `413`. It
buffers an install whole before it reads the recipe, so the limit is also
the memory one install may hold before it is validated.

## 6. Conformance (additions)

| check | holds | red against |
|---|---|---|
| `evidence.verify` | the fixture's planted sentence comes back as claims whose `exact` is the unit's `[start, end)`; every verdict is one of the five; every passage's `span` dereferences to its `exact`; a tier outside `verify_tiers` is `422 tier not offered` | a passage `exact` one character off |
| `knowledge.range` | every hit for each fixture signature carries `span` or a `span_absent` from the set, never both; each span dereferences to its `exact`; at least one hit over the fixture's plain texts is ranged | a hit `exact` one character off |
| `evidence.verify_correction` | a decided claim of the fixture unit carries an `episode_id`; a correction whose witness is a span verify returned is `verified`, one running past its text `out_of_range`; a malformed episode is `400 not an episode id`. Writes two correction lines on a live host | a host that never reads the witness back |
| `ingest.documents` | once the fixture's install has returned, every fixture document is listed `ready` under the sha256 of its text, each with a `source`, none `failed`; and `ingest.recipe` holds that an install answering `spawned: true` is in the next poll | an entry listing no documents; an install missing from the first poll |
| `knowledge.scoped` | a query reaching several fixture texts, scoped to the second-ranked one, returns at least one hit and every hit from that text; a `documents` entry that is not a text name is `400` | a host that ignores `documents` |
| `ingest.install_limit` | with `ingest:recipe` and `max_install_bytes` stated, an install body of exactly that many bytes, a recipe that does not load padded to the size, is read: `400`, never `413`; one byte more is `413 install too large` naming the limit. A stated limit past 256 MiB is probed at 256 MiB, with nothing sent past it | a host stating a limit whose install route reads bodies only to its surface's 8 MiB |
| `knowledge.sentences` | each fixture signature, at `granularity: sentence` scoped to its own text, returns at least one sentence, at most `limit`, best first, every one from that text; each carries `span` whose `exact` is its `content` and whose range reads back byte-equal through `text_endpoint`; the reply names the advertised scorer and a model, and a route, with `sentences_pending` exactly on `passages`. A `503 scorer unavailable` naming the advertised scorer is could-not-judge | a sentence range one code point off; a sentence from outside the scope; a host advertising `rerank`, holding no rerank slot, that answers with cosine |

`manifest.features` checks §2.1's co-occurrence: `evidence:verify` iff
`verify_endpoint`, with a non-empty `verify_tiers`, and never without
`evidence:text`; `knowledge:scoped` and `knowledge:range` never without
`knowledge:document`, and `knowledge:range` never without `evidence:text`;
`ingest:documents` never without `ingest:v1`; `evidence:verify_corrections`
iff `verify_correction_endpoint`, and never without `evidence:verify`;
`knowledge:sentences` iff `knowledge.sentence_scorer`, and never without
`knowledge:range` and `knowledge:scoped`.

## 7. Backward compatibility

- Every v0.6 field is `#[serde(default)]` and skipped when empty, so a v0.5
  manifest reads into the v0.6 types and writes back unchanged.
- `OICP_VERSION` is still `"0.4.0"` in the reference types (v0.5 is approved
  but the constant was not moved); features gate behaviour either way.
- A v0.6 client against a v0.5 host sees no `evidence:verify` and finds no
  `verify_endpoint`; it sees no `knowledge:scoped`, and sends no `documents`,
  which such a host would ignore and answer unscoped.
- `max_install_bytes` is skipped when unstated, so a v0.5 manifest reads and
  writes back unchanged; a v0.6 client against a host that states none sends
  as a v0.5 client does and reads a `413` as too large.
- `granularity` is never written at its default, so a v0.6 client's passage
  search is the request a v0.5 host knew. A client sends `sentence` only to a
  host advertising `knowledge:sentences`; the reply's sentence fields are
  absent from every passage reply.
