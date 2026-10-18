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
text (§2.11), a host states the **largest install** it reads, so a client
sizes an inline library before it sends one (§2.12), and a span says
**where it stands** in its source, such as the headings it stands under
(§2.13) or the turn of a recipe document declared a **transcript** it was
spoken in (§2.14). Verify can find a claim's candidates **by meaning**,
name its specifics by kind, and say which figure a source states otherwise
(§2.15). For an install that allows it, a document's DOI or ISBN fills in
its **record**, under what the recipe declared (§2.16). A request can say
who waits on it, so a person's typing is **served first** over a client's
background work (§2.17).

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
| `knowledge:sentence_queue` | a sentence reply on the `passages` route says where each waiting scoped corpus stands in the host's embedding queue: embedding now, or queued behind which corpora with their pending counts (§2.11) | `knowledge:sentences` |
| `knowledge:locator` | every span carries `locator` where the extraction that stored its text found one, a plain text's heading paths by §2.13's rule | `knowledge:range` |
| `evidence:verify_sentences` | verify's candidates are §2.11's sentences, each claim names its specifics by kind, the fast tier's literal miss is a hint, and a contradicted passage names what differs (§2.15) | `evidence:verify`, `knowledge:sentences` |
| `ingest:lookups` | for an install that allows it, a document's identifiers are looked up and its record merged under its declared metadata, listed on progress and on its hits (§2.16) | `ingest:documents`, `ingest:recipe` |
| `ingest:transcripts` | a recipe `[[document]]` takes `format = "transcript"`, read by turn, each turn's start and speaker the locator of its spans (§2.14) | `knowledge:locator`, `ingest:recipe` |
| `knowledge:priority` | knowledge search, and verify where offered, take `priority`; while an `interactive` request's model work is in flight a `background` request starts none (§2.17) | none |

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
| `not a transcript` | — | a document's `failed` reason (§2.9): declared `format = "transcript"`, in neither shape (§2.14) |

A passage whose stored text or record cannot be read gives v0.5 §2.5's reason
in `span_absent` (`texts not stored`, `text not stored`, `document
unreadable`), or an opaque one for a store error.

The span absences are in `oicp_types::evidence::reasons` (`SPAN_ABSENCES`),
shared by verify's passages and search's hits; `scorer unavailable` is in
`oicp_types::knowledge::reasons`; `install too large` is in
`oicp_types::ingest::reasons`; `not a transcript` is in
`oicp_types::transcript::reasons`; the rest are in `oicp_types::verify::reasons`.

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
                          sentence_queue: Option<Vec<SentencePlace>>, // iff passages and knowledge:sentence_queue
                          truncated: Option<Truncated> }    // the scan stopped at the host's ceiling
Scorer          { name: SentenceScorer, model: String }
SentenceScorer  = "cosine" | "rerank"
SentenceRoute   = "index" | "passages"
Truncated       { scanned: u64, in_scope: u64 }
SentencePlace   { corpus: String, pending: u64, place: IndexPlace,
                  behind: Vec<QueuedCorpus> }   // empty iff place = "embedding"
IndexPlace      = "embedding" | "queued"
QueuedCorpus    { corpus: String, pending: Option<u64> }
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
- **What is not the work's.** A host yields no sentence from text a
  distributor wrapped around the work, so neither a search nor verify's
  candidates (§2.15) quote it as the work's word; the stored text, its name
  and every span into it are unchanged. The reference host leaves out a
  Project Gutenberg wrapper (`sentence_index/wrapper.rs`): the header
  through its `*** START OF … GUTENBERG …` line and the licence from its
  `*** END OF` line, a sentence reaching across a marker keeping its part
  in the work. Quoted as a source's word, that header was most of the
  `contradicted` flags in zoracite's K2 run.
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
- **Queue.** A host embeds its corpora one at a time, first in first out, so
  a library installed behind another waits for it, and `sentences_pending`
  alone cannot say for how long or why. On a host advertising
  `knowledge:sentence_queue`, a `passages` reply carries `sentence_queue`:
  one `SentencePlace` for each scoped corpus with a sentence not yet
  embedded, in the order the host embeds them. `pending` is that corpus's
  own count of sentences not embedded, scoped or not, the work its place
  waits on. `place` is `embedding` for the corpus the host is embedding at
  the moment of the reply, or that nothing is ahead of, with `behind`
  empty, and `queued` for one that
  waits, with `behind` naming the corpora embedded before it, the one
  embedding now first, in the order the host embeds them, each with its own
  `pending`; `pending` is absent for a corpus the host has queued and not
  yet opened to count. At most one corpus is `embedding`, and a corpus that
  has finished is named in no `behind`. The queue is the host's, read when
  the reply is built, never a client's inference from counts. A corpus
  placed `embedding` and then `index` on later replies is the sequence a
  client expects; a client reads a reply without the field as no claim about
  the queue, which is what a host not advertising the feature sends.
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
see.

The reference host serves this section from a sentence index
(`svrn/crates/sovereign-daemon/src/sentence_index.rs`, the route
`routes_knowledge_sentences.rs`). After an install, at the first sentence
search that names a corpus, and at its start for any corpus already indexed,
it splits each stored text with `sentence_ranges` and embeds every sentence
in the background on its embed slot, each vector cached by the sentence's
own bytes and stamped with the embedder's identity (its answer to a fixed
sentinel, as the router's exemplar cache keys), never by a text's name and
range; the vectors are kept in the corpus's directory (`_sentences.bin`) with
each sentence's address, so a restart reloads them. A vector is scored only
under the embedder that made it: a query whose embedder answers the sentinel
otherwise finds every sentence pending. Its data file
(`svrn/crates/sovereign-daemon/data/sentence_search.toml`) names `scorer`
(`cosine`), `rerank_candidates` (50), `scan_ceiling` (400,000 sentences),
`index_ceiling_chunks` (a corpus past 100,000 chunks is not sentence-indexed
and is listed in `corpora_unavailable`), `passage_hits` (20, the passages
route's search), `embed_batch` and `checkpoint_secs`. On the passages route
the sentences are those its passage hits' ranges overlap; a hit with no range
contributes none. A sentence search naming no `corpora` reaches the corpora
already sentence-indexed there and starts no indexing, since a host may hold
thousands. Its `503` names a `detail` beside `error` and `scorer`. The rerank
scorer's model is the rerank slot's resident model.

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

### 2.13 Where a span stands

v0.5 §2.3's `Span` says where a range is by code points, which a client can
dereference but a reader cannot cite. It gains the place the extraction
found it: the field a citation's locator is filled from.

```
Span        { … (v0.5 fields) …,
              locator: Option<SpanLocator> }        // absent when the extraction found none
SpanLocator = {"kind": "page", "page": u32}                     // the 1-based page of the source
            | {"kind": "heading_path", "headings": [String]}    // outermost first, never empty
            | {"kind": "timestamp", "milliseconds": u64,        // a transcript turn's start (§2.14)
                                    "speaker": Option<String>}  // absent when the source names none
            | {"kind": "row", "row": u64}                       // the 1-based record of a file of many
```

- **Where its start stands.** A span's locator is where its `start` stands
  in its text, as the extraction that stored the text found it. Every span
  a host advertising `knowledge:locator` gives carries it: search hits and
  sentences (§2.8, §2.11), verify's passages (§2.6), alignments (v0.5
  §2.3). Two spans of one text that start in one place carry one locator.
- **Absent, never invented.** A span whose extraction found no place for
  it carries no `locator`; a heading path is never empty and a speaker is
  never guessed. A client reads absence as "nowhere named", never as the
  text's start.
- **A hint beside the address.** The range stays the span's address; the
  locator is what a reader cites it by. `SpanLocator` is a closed set: a
  kind outside it is a decode error, never a default, and adding one is a
  spec revision. `page` and `row` are for extractors that know them (a
  paginated source, a file of many records).
- **Plain text's heading path.** The plain-text extractor reads a text as
  paragraphs (runs of lines that are not whitespace alone), each with its
  whitespace runs read as single spaces. A paragraph is a heading when it
  opens with a part word (`BOOK`, `PART`, `CHAPTER`, `SECTION`, `VOLUME`,
  in capitals or with a capital initial), then a numeral (Arabic digits or
  capital Roman numerals) and a stop right after it, is at most 300 code
  points long, and opens no second part (a contents line names many, so it
  is no heading). Part words nest in the order the text's headings first
  use them: `BOOK I.` then `CHAPTER I.` then `PART I.` is three levels, and
  `PART I.` then `CHAPTER I.` is two the other way round. A heading closes
  every open heading at its level or deeper and opens its own, from its
  paragraph's first code point. A span's heading path is the headings open
  at its start, each as its paragraph reads; before a text's first
  heading, and in a text with none, there is none.

`oicp_types::locator` holds the type and the rule (`HeadingOutline`), so a
host's extractor and a test host read headings by one implementation. The
reference host advertises `knowledge:locator`: its ingest keeps each text's
heading paths (`HeadingOutline::placed`), or a transcript's turns, beside
the stored text, and every span reads its locator from them.

### 2.14 Transcripts

An interview or a meeting is cited by when it was said and who said it.
A recipe document declares it is a transcript; the host stores its words
and puts each turn's start and speaker on the spans cut from it.

```toml
[[document]]
name = "interview-2026-03-02"
format = "transcript"        # DocumentFormat, a closed set; absent, the recipe's extractor reads it
source = "interview.txt"     # or the text inline
```

- **Two shapes, detected.** A source that is a JSON object with a
  `segments` array is Whisper's JSON: each segment, its `text` trimmed,
  is a turn starting at its `start` seconds, with no speaker. Any other
  source is a paragraph transcript: a header line opens each turn, and the
  lines after it, up to the next header, are the turn. A header line holds
  one timestamp, first or last on the line, and otherwise a speaker:
  `HARBOUR MASTER  00:00:04`, `Interviewer (00:00:11)`, `[00:00:15]
  HARBOUR MASTER:`, or a timestamp alone for a turn with no speaker. A
  timestamp is `H:MM:SS` (any number of hours) or `M:SS`/`MM:SS`, with an
  optional `.` or `,` fraction of up to three digits, optionally in `()` or
  `[]` and followed by `:` or `,`. A speaker is at most six words, each
  opening with a capital, a digit or a bracket, with `:` `-` `–` `—` `|`
  trimmed from its ends, and does not end a sentence; a line that is not
  of that shape ("We left at 10:30") is words of the turn.
- **The words are stored.** The stored text is the turns' words, never a
  header line, so a quotation from it is the speaker's bytes. A paragraph
  transcript keeps each turn's lines as written, trimmed, the turns joined
  by a blank line; lines before its first header are kept as the text's
  opening, under no turn. Whisper's segments are joined by one space, so a
  sentence may run across segments.
- **The locator is the turn.** Every span cut from a transcript carries
  `{"kind": "timestamp", "milliseconds": …, "speaker": …}` (§2.13): the
  start of the turn its `start` falls in, so a sentence across Whisper
  segments takes the first one's time. `speaker` is the header's, absent
  for a timestamp alone and always absent for Whisper's JSON, whose
  segments name none: never guessed. A span in a paragraph transcript's
  opening carries no locator.
- **Neither shape.** A document declared a transcript that is in neither
  shape (no header line, or a `segments` entry without a `start` or a
  `text`) is not stored: §2.9 lists it `failed` with `not a transcript`,
  and it is never read as plain text instead. A `format` outside the set
  is a recipe that does not load, `400`.

`oicp_types::transcript` holds the set and the rule (`TurnOutline::read`),
so a host's extractor and a test host read transcripts by one
implementation. The reference host advertises `ingest:transcripts`;
WebVTT, SRT and DOCX are later members of the set, each read into the same
locator.

### 2.15 Verify by meaning

§2.6's reference rungs look for a claim's numbers and names as strings, and
its fast tier settles a claim on a literal miss: "3.2%" against a source
reading "3.2 per cent" is `unsupported`. A host advertising
`evidence:verify_sentences` verifies the way §2.11 searches instead: a
claim's candidates are found by meaning, its specifics are named by kind,
and the fast tier's miss is a hint, never a verdict.

```
VerifiedClaim     { … (§2.6 fields) …,
                    specifics: Vec<ClaimSpecific> }    // code points of the unit, inside the claim
JudgedPassage     { … (§2.6 fields) …,
                    differing: Vec<DifferingSpecific> } // on a contradicted passage, and only there
ClaimSpecific     { kind: "figure" | "date" | "quantity" | "name",
                    start: u64, end: u64, exact: String }
DifferingSpecific { claim: ClaimSpecific,              // in the unit
                    source: ClaimSpecific }            // in the passage's stored text
```

- **Candidates are sentences.** A claim's candidates are the sentences of
  the request's corpora that §2.11's search returns for the claim, by the
  advertised scorer: one retrieval path for search and verify. Every
  passage judged is such a sentence and carries its `span`.
- **The floor.** A claim no sentence reaches the host's relevance floor
  for is `unsupported` with `evidence: []`, at every tier: nothing in the
  library is about it. The floor is in the host's data file, never in its
  code, and `verifier` names it, so a client's cached verdicts change with
  it (§2.6, identity).
- **Specifics by kind.** Every checkable claim names its specifics, each
  an exact range of the unit inside the claim: a `figure` (a number with
  or without a unit, a percentage, a currency amount), a `date` (a year, a
  written date), a `quantity` (a number with a counted noun), a `name` (a
  run of name-shaped words). A number is written in digits or in words:
  `forty-eight thousand pins` is a quantity as `48,000 pins` is, its range
  the words as written, and the two are compared by value. An ordinal
  (`the first settlement`) is no number, nor is a lone `one`, more often a
  pronoun than a count. A number inside a citation is none of them:
  an author-date citation (`(Okafor 2019)`, `(Okafor, 2019, p. 4)`) or a
  citekey (`[@okafor2019]`). Nor is one inside a locator (`(page 14)`, `(p.
  3)`, `(§ 4)`, `(chapter 2)`). A figure inside any other parentheses is a
  specific: `(against 6.1 percent the year before)` names `6.1`. `ClaimSpecificKind` is a
  closed set.
- **The fast miss is a hint.** At `fast`, a claim a sentence reaches the
  floor for is `abstained`, and each of its specifics no candidate carries
  as written is named in `specifics_absent` for the checker to read. The
  fast tier says `unsupported` only below the floor, with no evidence, and
  never `supported` or `contradicted`.
- **Contradicted names what differs.** A candidate the claim is about that
  states one of the claim's figures, dates or quantities otherwise, and
  does not back the claim, is `contradicted`, and its `differing` pairs the
  claim's specific with the sentence's of the same kind. A figure or a
  quantity under a bound ("more than 50 years", "up to 6") is stated
  otherwise only by one no value of its range meets: "more than a hundred
  years" does not state "more than 50 years" otherwise, and does state
  "fewer than 50". A claim is `contradicted` only on such a passage. A client offering the source's
  figure as a fix offers it when exactly one pair differs. A claim whose
  candidates neither back it nor differ is `unsupported` with them, nearest
  first: the library discusses it and does not back it.

A host advertising `evidence:verify_sentences` verifies by this section;
§2.6's rung table is the reference host's before it. The reference host
advertises it (`svrn/crates/sovereign-daemon/src/routes_oicp_verify.rs`),
its floor `verify_floor` in its data file
(`svrn/crates/sovereign-daemon/data/sentence_search.toml`). Its rungs are
`class/no-specifics`, then `sentences/floor` (`unsupported`, no
evidence), then `fast/value-presence` as a hint (`abstained`), then
`checker/sentence-support` judging the best candidate sentences, with a
differing figure, date or quantity making `contradicted`; its specifics
finder (`sentence_specifics`, in
`svrn/crates/sovereign-core/src/runtime/grounding/specifics.rs`) splits its
numbers, in digits or in cardinal words, into the three kinds (a number in
words is never a date) and leaves citations and locators out.

### 2.16 Records by identifier

A library's documents often declare no more than an identifier: a DOI, an
ISBN. A host can fill in the rest from a resolver, but a lookup tells the
resolver what the client is reading, so it asks only for an install that
allows it, and never lets a resolver overrule what the recipe declared.

```
CorpusInstallRequest { … (v0.4 fields) …,
                       allow_lookups: bool }               // default false, never written at the default
IngestDocument (ready) { …, record: Option<MergedRecord> }  // §2.9
Document               { …, record: Option<MergedRecord> }  // v0.5's record of a text, on hits and spans
MergedRecord { csl: {String: Value},          // CSL-JSON: the declared fields, then the looked-up ones
               origins: {String: String},     // each field of csl: "declared", or the resolver's name
               lookups: [RecordLookup] }
RecordLookup { identifier: String,            // the CSL variable: "DOI", "ISBN"
               value: String,
               resolver: String,              // the resolver the host's data file names for it
               outcome: "resolved" | "unresolved" | "unreachable" | "not_allowed" }
```

- **Which documents.** A document whose declared metadata (its
  `metadata`, as the recipe or extractor declared it) carries an identifier the host has a resolver for has a `record`; any
  other has none. The host's data file maps each CSL variable to its
  resolver, so another resolver is a row there, not a code change.
- **Only when allowed.** A host sends a lookup only for an install whose
  request says `allow_lookups: true`. For any other install every lookup
  is `not_allowed`, nothing leaves the host, and the record is the
  declared metadata. The allowance is part of the install: the same
  recipe installed with the other allowance is a new install (`spawned:
  true`).
- **Declared wins.** Every declared field is in `csl` unchanged, its
  origin `declared`. A looked-up field enters only where the metadata
  declares none, its origin the resolver's name. `origins` names exactly
  the fields of `csl`.
- **Named, never invented.** `unresolved` (the resolver does not know the
  identifier) and `unreachable` (it could not be asked, or did not answer)
  add no field. A client may install again later; the host may ask again
  then.
- **Listed.** The record is on progress beside its document once the
  install has returned (§2.9), whether or not a search has hit it, and on
  the document's `Document` wherever the host gives it (hits, spans): the
  same record in both places. It is kept with the install's document list.
- **Not identity.** The record is no part of a text's name or of a
  corpus's `texts_digest`; a lookup never changes a stored text.

The reference host does not advertise `ingest:lookups` yet. When it does,
its data file maps `DOI` to Crossref and `ISBN` to Open Library, and its
records keep CSL-JSON field names as the resolvers return them.

### 2.17 The writer's request first

A client that checks a draft while its writer types sends the host two kinds
of work at once: searches a person waits on, and checks nobody is watching.
On a host whose models share one accelerator, the checks' model work slows
every search the writer makes. A request can say which it is.

```
KnowledgeSearchRequest { …, priority: Option<RequestPriority> }   // never written when absent
VerifyRequest          { …, priority: Option<RequestPriority> }   // §2.6; never written when absent
RequestPriority = "interactive" | "background"
```

- **Who waits.** `interactive`: a person waits on the answer as they type.
  `background`: the client runs it on its own schedule, such as a draft's
  checks. A request without `priority` is neither: it is served as before
  this section, neither waiting nor waited for.
- **The writer's first.** A request's model work is what it asks of the
  host's models: the query's embedding, a verify's embeddings and its
  checker's completions. While an `interactive` request's model work is in
  flight, a `background` request starts none: each of its steps waits until
  no interactive model work is in flight, then starts. Work already started
  is not preempted, but a host may read a background step in parts and
  start no part while interactive work is in flight; a part already
  started finishes.
- **Bounded.** A background step waits at most the host's hold ceiling,
  then starts, so interactive requests in a row cannot starve it. The
  reference host's ceiling is 2 s.
- **When, never what.** The same request with either priority, or none, is
  answered the same; only when its model work starts differs. A step read
  in parts reads the same tokens in the same order, its parts fixed by its
  length alone, never by timing.
- **Refused, never defaulted.** A value outside the set is refused (`400`
  or `422`), never read as absent.
- **Seen.** The host says at debug, for each background step that waited,
  how long and behind how many interactive steps, and when the ceiling sent
  it on.

The reference host holds steps at its embedding slot and at the admission
of its generation slots, the two places its search and verify start model
work. Background steps waiting to enter one stage go one at a time, so at
most one stands between an interactive step and the stage it queues at:
background steps already queued at a stage would otherwise be served first.
A background step inside its stage holds no other back. It reads a
background completion's prompt in parts of 64 tokens and pauses between
them while an interactive step is running inside its stage, never for one
waiting on the slot the prompt holds, each pause within the ceiling:
a checker's prompt read whole held the accelerator for most of a second,
and an interactive embedding running beside it took up to nine times as
long as alone. A background embedding batch likewise lets the embedding
context go between its sub-batches and pauses while an interactive step is
in flight: held across a whole batch, it kept a writer's query embedding
waiting 300 to 540 ms. Where its embedding context computes on the
accelerator its generation slots share, it keeps a second context on the
CPU for an interactive request's embeddings: beside the primary's compute,
a writer's query embedding took 353 ms on the shared accelerator and under
50 ms on the CPU.

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
| `knowledge.sentence_queue` | installs two corpora of its own (`fixture/queue.json`, the second under another id), one behind the other, and searches both at sentence granularity until the host answers `index`; every `passages` reply carries `sentence_queue`, each placed corpus has a sentence pending, at most one is `embedding` and names nothing ahead, one `queued` names the corpus embedding now first, and a corpus of the request named ahead is itself placed in that reply. Needs `ingest:recipe`; an `index` reply at the first search is could-not-judge | a corpus queued behind another answered `embedding`; a corpus that has finished still named ahead |
| `knowledge.locator` | installs its own library (`fixture/headings.json`: a headed text with a contents line and a preface, and a text with no headings) and aligns each listed sentence verbatim; each span's `locator` is the heading path the rule gives, or absent where it gives none. Needs `ingest:recipe` and `evidence:align`, could-not-judge without | a span given the heading path of the part before the one it stands in; a locator invented for a span under no heading |
| `evidence.verify_sentences` | installs its own library (`fixture/claims.json`, a ledger of figure sentences) and verifies five units at each served tier: every specific is an exact range of the unit inside its claim; each listed figure, date or quantity is covered, a parenthesised one too; nothing inside `(Okafor 2019)` or `(page 14)` is a specific; the fast tier never says `supported` or `contradicted` and says `unsupported` only with no evidence; a unit restating the library's figure in another form is `abstained` at `fast` and `supported` at `checker`; another figure is `contradicted`, naming the pair, each side read back; a concept the library lacks is `unsupported` with no evidence. A checker that answers `could_not_judge` is could-not-judge | a claim settled from the fast tier's literal miss; a citation's year made a specific; a figure inside parentheses left out; a contradiction that names no differing specific |
| `ingest.lookups` | installs its own library (`fixture/records.json`: a document declaring a title and a DOI that resolves, one declaring a DOI that does not, one declaring none) allowing lookups, and again as another corpus not allowing them; reads each record off progress: every field has an origin, every declared field kept as `declared`, the resolving DOI `resolved`, the other `unresolved` with nothing taken, the undeclared document no looked-up field; every lookup of the second install `not_allowed` with nothing taken; with `knowledge:document`, a hit carries the record progress lists. A resolver `unreachable` is could-not-judge | a field invented for an identifier that did not resolve; a declared title overwritten by the looked-up one; a lookup sent for an install that did not allow it |
| `ingest.transcript` | installs its own library (`fixture/transcripts.json`: a paragraph transcript with three header shapes and an opening, Whisper's JSON with a sentence across segments, and a declared transcript in neither shape) and aligns each listed quotation verbatim; each span's `locator` is its turn's start and speaker, a Whisper segment's with none; with `ingest:documents`, the one in neither shape is listed `failed: not a transcript` and the others `ready`. Needs `evidence:align`, could-not-judge without | a header line read into the turn before it (the turn given the previous speaker); a speaker invented for a Whisper segment; a document in neither shape stored as plain text |
| `knowledge.priority` | each fixture signature searched with `priority: interactive`, with `background` and with none returns the same hits in the same order; with `evidence:verify`, the fixture unit verified at the first served tier with each priority returns the same verdicts; a priority outside the set is `400` or `422` | a host that answers differently by priority; a host that reads an unknown priority as absent |

`manifest.features` checks §2.1's co-occurrence: `evidence:verify` iff
`verify_endpoint`, with a non-empty `verify_tiers`, and never without
`evidence:text`; `knowledge:scoped` and `knowledge:range` never without
`knowledge:document`, and `knowledge:range` never without `evidence:text`;
`ingest:documents` never without `ingest:v1`; `evidence:verify_corrections`
iff `verify_correction_endpoint`, and never without `evidence:verify`;
`knowledge:sentences` iff `knowledge.sentence_scorer`, and never without
`knowledge:range` and `knowledge:scoped`; `knowledge:sentence_queue`
never without `knowledge:sentences`; `knowledge:locator` never
without `knowledge:range`; `ingest:transcripts` never without
`knowledge:locator` and `ingest:recipe`; `evidence:verify_sentences` never
without `evidence:verify` and `knowledge:sentences`; `ingest:lookups` never
without `ingest:documents` and `ingest:recipe`; `knowledge:priority` never without a knowledge plane.

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
- `locator` is skipped when absent, so a v0.5 span reads and writes back
  unchanged, and a v0.6 client reads a span from a host without
  `knowledge:locator` as one that names no place.
- `specifics` and `differing` are skipped when empty, so a §2.6 reply
  reads and writes back unchanged; a client reads their absence from a
  host without `evidence:verify_sentences` as "not named", not as none.
- `allow_lookups` is never written at its default and `record` is skipped
  when absent, so a v0.5 install request and a v0.5 document read and write
  back unchanged; a host without `ingest:lookups` ignores the allowance
  and sends no lookup.
- `priority` is never written when absent, so a v0.5 search or a §2.6
  verify request reads and writes back unchanged. A client sends it only
  to a host advertising `knowledge:priority`; a host without it may refuse
  the field or ignore it, serving the request as one without.
- A client sends `format = "transcript"` only to a host advertising
  `ingest:transcripts`; a host without it may refuse the recipe or read the
  document as plain text, its spans then carrying no turn.
