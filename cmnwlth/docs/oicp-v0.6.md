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
of a library's **documents** were read (§2.9), and a client can **correct** a
verify decision so the host keeps the case it got wrong (§2.10).

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

A passage whose stored text or record cannot be read gives v0.5 §2.5's reason
in `span_absent` (`texts not stored`, `text not stored`, `document
unreadable`), or an opaque one for a store error.

The span absences are in `oicp_types::evidence::reasons` (`SPAN_ABSENCES`),
shared by verify's passages and search's hits; the rest are in
`oicp_types::verify::reasons`.

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

## 6. Conformance (additions)

| check | holds | red against |
|---|---|---|
| `evidence.verify` | the fixture's planted sentence comes back as claims whose `exact` is the unit's `[start, end)`; every verdict is one of the five; every passage's `span` dereferences to its `exact`; a tier outside `verify_tiers` is `422 tier not offered` | a passage `exact` one character off |
| `knowledge.range` | every hit for each fixture signature carries `span` or a `span_absent` from the set, never both; each span dereferences to its `exact`; at least one hit over the fixture's plain texts is ranged | a hit `exact` one character off |
| `evidence.verify_correction` | a decided claim of the fixture unit carries an `episode_id`; a correction whose witness is a span verify returned is `verified`, one running past its text `out_of_range`; a malformed episode is `400 not an episode id`. Writes two correction lines on a live host | a host that never reads the witness back |
| `ingest.documents` | once the fixture's install has returned, every fixture document is listed `ready` under the sha256 of its text, each with a `source`, none `failed`; and `ingest.recipe` holds that an install answering `spawned: true` is in the next poll | an entry listing no documents; an install missing from the first poll |
| `knowledge.scoped` | a query reaching several fixture texts, scoped to the second-ranked one, returns at least one hit and every hit from that text; a `documents` entry that is not a text name is `400` | a host that ignores `documents` |

`manifest.features` checks §2.1's co-occurrence: `evidence:verify` iff
`verify_endpoint`, with a non-empty `verify_tiers`, and never without
`evidence:text`; `knowledge:scoped` and `knowledge:range` never without
`knowledge:document`, and `knowledge:range` never without `evidence:text`;
`ingest:documents` never without `ingest:v1`; `evidence:verify_corrections`
iff `verify_correction_endpoint`, and never without `evidence:verify`.

## 7. Backward compatibility

- Every v0.6 field is `#[serde(default)]` and skipped when empty, so a v0.5
  manifest reads into the v0.6 types and writes back unchanged.
- `OICP_VERSION` is still `"0.4.0"` in the reference types (v0.5 is approved
  but the constant was not moved); features gate behaviour either way.
- A v0.6 client against a v0.5 host sees no `evidence:verify` and finds no
  `verify_endpoint`; it sees no `knowledge:scoped`, and sends no `documents`,
  which such a host would ignore and answer unscoped.
