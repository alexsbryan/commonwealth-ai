# Open Inference Capabilities Protocol (OICP) — v0.5

**Version:** 0.5.0
**Status:** Draft for the operator's approval (extends v0.4 additively; v0.4 remains the fallback path)
**License:** CC0 (public domain dedication)

---

## Abstract

v0.5 lets a client hold a host to its sources. It adds an **evidence
extension**: a host keeps the text each ingested document's chunks are cut
from, names that text by the sha256 of its bytes, serves it back by name, and
aligns a quotation against it, reporting every difference at exact ranges.
Around it, v0.5 adds what the extension needs to be usable by a client that
shares nothing with the host but this protocol:

- search hits that carry their text's record, metadata included;
- an install that carries the recipe it installs;
- a named client, whose credential decides from any address;
- and the rule, already shipped, that a web page is never a local process.

Everything v0.5 adds is a serde-defaulted field, an optional endpoint, or a
feature string. As v0.4 §8 rules, **feature presence, not the version
string, gates behaviour**: a v0.5 manifest with no v0.5 fields populated
serializes byte-identically to a v0.4 one, and each addition below names the
feature a client checks before relying on it.

The types are in `shared/crates/oicp-types/src/evidence.rs` (the one schema).
The conformance suite is `cmnwlth/crates/oicp-conformance` (§6).

---

## 1. Motivation

A quotation is checkable only against something that exists. Before v0.5 a
host kept chunks: re-joined, overlapped and title-headed strings that are
slices of no stored text. So no reply could say where a quotation stands, and
no second machine could recheck that it does. v0.5 makes the text the
addressed thing, and every other addition is a read of it.

## 2. The evidence extension

### 2.1 Advertisement

| feature | meaning | requires |
|---|---|---|
| `evidence:text` | texts are stored and `knowledge.evidence.text_endpoint` reads them (§2.2) | `knowledge.evidence` |
| `evidence:align` | `knowledge.evidence.align_endpoint` aligns a quotation against stored texts (§2.3) | `evidence:text`, `align_endpoint` |
| `knowledge:document` | knowledge-search hits carry `document` (§3) | a knowledge plane |
| `ingest:recipe` | install accepts `recipe_toml` (§4) | `ingest:v1` |
| `auth:named_client` | a presented credential in the host's form decides from any address (§5.1) | — |

```rust
KnowledgeManifest {
    … (v0.4 fields) …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence: Option<EvidenceEndpoints>,
}

EvidenceEndpoints {
    text_endpoint:  String,          // e.g. "/oicp/v1/text"; present iff evidence:text
    #[serde(default, skip_serializing_if = "Option::is_none")]
    align_endpoint: Option<String>,  // e.g. "/oicp/v1/align"; present iff evidence:align
}
```

Endpoint values are paths relative to the manifest's origin, as
`search_endpoint` is.

### 2.2 Stored texts

**What a text is.** The string one extracted document's chunks are cut from,
after the host's content normalisation. A source whose extractor emits
sections has one text per section.

**Its name** is the sha256 of its UTF-8 bytes, as 64 lowercase hex
characters (`text_sha256`). A name is content, never a location: the same
text held by two corpora, or two hosts, has one name.

**Its record:**

```rust
Document {
    text_sha256: String,                     // the text's name
    extractor:   String,                     // non-empty, no whitespace; opaque
    source:      SourceRef {
        id:     String,                      // the host's id for the source: an attribute, never the name
        sha256: Option<String>,              // sha256 of the bytes the extractor read
    },
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metadata:    Option<serde_json::Value>,  // verbatim (§3)
}
```

`source.sha256` is `null` exactly when the document is a record inside a file
of many (a JSONL line, a CSV row, a dump entry), whose own bytes were never a
file. It is serialized as `null`, never omitted, so the absence is stated.
The reference host's `extractor` is the recipe's `[extract]` tag (with a
custom extractor's kind) `@` its engine version.

**Units.** Every range is a half-open `[start, end)` count of Unicode code
points into a text. **Every reply that carries a range also carries the exact
string it names**, so a client in any language checks a range by comparing
strings, without knowing how the host stores text.

**Reading a text:**

```
GET {text_endpoint}/{text_sha256}?start=&end=&context=&corpus=
Response: TextSlice { document: Document, start: u64, end: u64,
                      text: String, before: String, after: String }
```

- With no range, the reply is the whole text: `start = 0`, `end` its length
  in code points, `before` and `after` empty. A client caches it by its name.
- With a range (either bound may be given; `start` defaults to 0, `end` to
  the length), `text` is exactly the text's `[start, end)`, and `before` and
  `after` carry up to `context` code points on each side (default 32).
- `corpus` is a hint for where to look. Without it, the host searches every
  corpus the caller may read. A text held only outside what the caller may
  read is `text not held`, the same answer as a text held nowhere.
- Several documents can share one text: identical texts from different
  sources share a name. `TextSlice.document` is the record with the lowest
  `(corpus_id, source.id, ordinal)` among those the caller may read.
- The route is guarded like every other client route (§5).

A client checks a read by hashing: `sha256(whole text) == text_sha256`.

**Existing corpora.** A corpus built before its host stored texts has none
until it is reingested. Its documents answer `texts not stored` by name,
never an empty result. A recipe may opt out of storing texts; its documents
then answer `text not stored`.

### 2.3 Alignment

```
POST {align_endpoint}
Request:  AlignRequest  { quote: String,
                          corpora: Vec<String>,   // default []: every corpus the caller may read
                          limit:   Option<u32>,   // default 5
                          context: Option<u32> }  // default 32
Response: AlignResponse { alignments: Vec<Alignment>,
                          aligner: String,
                          corpora: Vec<CorpusTexts { corpus_id: String, texts_digest: String }>,
                          corpora_unavailable: Vec<Unavailable { corpus_id: String, reason: String }> }

Alignment  { span: Span, differences: Vec<Difference>, coverage: f32 }
Span       { corpus_id: String,            // where it was found: a location, never the name
             document: Document,
             start: u64, end: u64,         // code points into the text
             exact: String,                // == the text's [start, end)
             prefix: String, suffix: String }  // up to `context` code points either side
Difference { kind: DifferenceKind,
             quote:  [u64; 2],             // code points into the request's quote
             source: [u64; 2] }            // code points into the text
```

`DifferenceKind` is a closed set, snake_case on the wire. An unknown kind is
an error, never a default.

| kind | `quote` range | `source` range |
|---|---|---|
| `substituted` | the quotation's words | the source's words they replace |
| `added` | words the source does not have | empty, at the point they were added |
| `omitted` | empty, at the point of the omission | words dropped with no ellipsis |
| `elided` | the ellipsis (`…`, `...` or `. . .`) | the stretch it stands for |
| `bracketed` | the square-bracketed unit, with the word characters it touches (`[T]he`) | the stretch it stands for, possibly empty (`[sic]`) |

- `differences` is in quotation order and is empty when the quotation is
  verbatim under the host's matching normalisation. The reference
  normalisation (`norm_v0`) is NFC; then whitespace runs, curly quotes,
  dashes, the ellipsis character and emphasis markers folded; then a word
  hyphenated across a line end joined. Case is **not** folded: the host
  reports wording and case, and whether a case-only difference matters is the
  client's call.
- `coverage` is the quotation's tokens matched exactly over its tokens, in
  `[0, 1]`. A host returns only alignments at or above its own floor, best
  first.
- `aligner` names the aligner and its normalisation (`align/1 norm/0` on the
  reference host). It changes whenever the same input could align
  differently, so a client keys cached alignments on it.
- How a host picks candidate texts is its own (the reference host takes the
  top chunks of its lexical search over the quotation). The shape of what it
  returns is not.
- Every requested corpus appears in exactly one of `corpora` (aligned
  against, with its digest) and `corpora_unavailable` (with a reason, such as
  `texts not stored`).

### 2.4 The library digest

`CorpusTexts.texts_digest` is sha256, as 64 lowercase hex, over these bytes:
one line per stored text of the corpus,

```
<text_sha256> <source sha256, or - for a record> <extractor>\n
```

the distinct lines sorted bytewise and concatenated. It changes exactly when
a text, a source or an extractor changes, and never when chunking or
embedding does. It is opaque to a client, which keys caches on it. A host
MUST derive it exactly so, so that two hosts holding the same library agree.
The reference derivation of the bytes is
`oicp_types::evidence::texts_digest_preimage`.

### 2.5 Named refusals

Every refusal names its reason in the ingest routes' body shape,
`{"error": "<reason>"}`. The reasons are matched exactly, and any other is
opaque. The constants are in `oicp_types::evidence::reasons`.

| reason | status | when |
|---|---|---|
| `text not held` | 404 | no corpus the caller may read holds a text of that name |
| `texts not stored` | 404 | the corpus keeps no texts (built before, or opted out) |
| `text not stored` | 404 | this document's text was not stored |
| `range outside text` | 400 | `start > end`, or `end` past the text's length |
| `corpus not held` | — | in `corpora_unavailable`: a requested corpus the caller may not read, or the host does not hold |

In an align reply, a requested corpus that cannot be read is listed in
`corpora_unavailable` with its reason (`corpus not held` or `texts not stored`,
for two) rather than failing the request. `corpus not held` is one answer for
both of its cases, so a reply never tells a caller which corpora exist beyond
its reach.

## 3. Documents on search hits

With `knowledge:document`, every `KnowledgeResult` carries
`document: Option<Document>`: the record of the text the hit was cut from,
read through the hit's text name. `document.metadata` **supersedes** the v0.2
`metadata` map, whose `HashMap<String, String>` cannot carry the opaque JSON
a source declares. The field stays for compatibility, and the reference host
leaves it empty. A peer that predates the field sends no `document`.

Metadata is the extractor's, unless the recipe declares it for a source (§4.1).
Declared metadata replaces the extractor's for that source. It is parsed for
validity and returned verbatim.

## 4. Install with a recipe

```
POST {install_endpoint}
Request:   CorpusInstallRequest { corpus_id: String,
                                  parameters: BTreeMap<String, Value>,   // default {}
                                  recipe_toml: Option<String> }          // v0.5
Response:  CorpusInstallResponse { corpus_id: String, spawned: bool,
                                   recipe_sha256: Option<String> }       // v0.5
```

- With `recipe_toml`, the host installs that recipe under `corpus_id` in
  place of a recipe it knows by that id. A client MUST NOT send `recipe_toml`
  unless the host advertises `ingest:recipe`. A v0.4 host ignores the field
  it does not know (v0.4 §3) and would install its own recipe of that id.
- The recipe is validated at the host's one load boundary. **A recipe or
  parameters that do not validate are `400`**, with the reason in the body,
  never `200 spawned: false`.
- `recipe_sha256` is sha256, 64 lowercase hex, of the `recipe_toml` bytes,
  present whenever the request carried them. The host stamps it on the
  installed index.
- **Idempotence:** the same `(corpus_id, recipe_sha256)` again is
  `spawned: false`. A different recipe for an installed corpus reingests it.
- Progress follows v0.4 §5.2-5.3 unchanged. Install is guarded like the other
  ingest routes (v0.4 §5.5); with `auth:named_client`, a named client
  installs as itself.

### 4.1 Recipe format additions

v0.5 adds two blocks to the reference host's recipe format. A host
advertising `ingest:recipe` accepts both, because a client that shares no
disk with it installs a library this way.

**Inline documents.** `[acquire] type = "inline"` takes the recipe's
`[[document]]` blocks as its documents:

```toml
[acquire]
type = "inline"

[[document]]
name = "okafor2019"
text = '''…the source's words…'''
metadata = '''{"id": "okafor2019", "type": "article-journal"}'''
```

An inline document's source bytes are the UTF-8 bytes of its `text`, so its
`source.sha256` is their sha256, and its `source.id` is its `name`.

**Declared metadata for file sources.** In a recipe that reads files, a
`[[document]]` block names one by `source`, relative to the source root, and
declares its metadata:

```toml
[[document]]
source   = "sources/okafor2019.pdf"
metadata = '''{"id": "okafor2019", "type": "article-journal"}'''
```

In both forms `metadata` is a JSON string, parsed for validity and stored
verbatim. The normative example of a whole recipe is the conformance
fixture, `cmnwlth/crates/oicp-conformance/fixture/library.recipe.toml`
(§6.1).

## 5. Who is asking

### 5.1 Named clients (`auth:named_client`)

A host's credentials have one recognizable form. On the reference host that
is `svrn_` followed by 64 lowercase hex characters.

1. **A presented credential in that form decides, from any address.** It
   verifies, and resolves to its principal, or it is a `401`, from loopback
   too. A bearer not in that form is no credential of this host. From a
   remote peer it is a `401`. From a local peer whose host grants loopback the
   owner's trust, it counts as absent. Credentials issued before the form
   (no prefix) still verify.
2. **The principal carries the client's name.** The host attributes a named
   client's requests, MCP calls included, to that name in its logs.
   Revocation takes effect on the next request. Whether loopback grants the
   owner's trust is the host's declared posture (`owner` or `none`), never
   inferred from which credentials exist.
3. **Every client route sits behind the same check, `/mcp` included, and
   owner-only routes check for the owner.** A named client on loopback is not
   the owner, so it cannot mint credentials or grants.

Rule 2's attribution is visible only in the host's own log, so it is the
host's test to keep, not the suite's.

### 5.2 Local peers (every host; no feature)

A host that trusts a local process more than a remote caller must not extend
that trust to a web page: a browser on the owner's machine connects from
loopback, and a page served under a name that resolves to 127.0.0.1 is, to the
browser, the host's own origin. MCP's Streamable HTTP transport requires the
same check ("Servers MUST validate the `Origin` header", revision 2025-06-18).
This rule changes no wire a client relies on, so it gates on no feature and
every host owes it.

On the reference host (`shared/crates/host-kit/src/locality.rs`), a request is
local only when all four hold:

1. its peer address is loopback;
2. its `Host`, when present, names loopback: `localhost`, or an IP literal
   that is loopback, with or without a port;
3. its `Origin`, when present, is the request's own: its authority equals the
   `Host` (an `Origin` of `null` is never anyone's own);
4. its `Sec-Fetch-Site`, when present, is `same-origin` or `none`. This
   catches a cross-site `GET`, which carries no `Origin`.

A loopback request failing 3 or 4 is refused by name,
`403 {"error": "cross-origin", "origin": <what said so>}`. That is neither a
`401` nor a `local-only` refusal, so the operator can tell them apart. A
request failing 2 is not local, and needs a credential like any remote
caller. No route grants a foreign origin `Access-Control-Allow-Origin`. Routes
that are public to every caller (the reference host's `/status`, `/health`
and `/oicp/v1/capabilities`) stay public: they were never local trust.

The owner's own tools send none of the browser headers: curl and reqwest send
`Host` alone, and Node's `fetch` adds `sec-fetch-mode: cors` and nothing this
rule reads. A browser client that is not same-origin would be declared to the
host and present a credential. No such client exists, so v0.5 defines no
declaration for one.

## 6. Conformance

A host claims v0.5 conformance by passing `oicp-conformance` at each feature
it advertises. Each check below is gated on its feature except
`auth.local_peer`, which runs against every host and fails the run when red.
Each was watched red against a host fake that breaks exactly its law,
`cmnwlth/crates/oicp-conformance/src/fake_host.rs`.

| check | holds | red against |
|---|---|---|
| `ingest.recipe` | a recipe that does not parse is `400`; the fixture installs and reaches a terminal phase; `recipe_sha256` is the sha256 of the bytes sent; an identical second install is `spawned: false` | `200 spawned: false` on a bad recipe |
| `ingest.recipe_test` | the fixture's dry run reports `acquire`, `extract` and `chunk` in that order, each with output, and `ok` (owed since v0.4 §10) | a report missing a stage |
| `knowledge.document` | each fixture document's declared metadata returns verbatim on its hits, and every hit carries a well-formed record | emptied metadata |
| `evidence.text` | every span from align, and every name from search, dereferences: the whole text hashes to its name, `[start, end)` of it is the span's `exact`, a range read returns `exact` with its context; `text not held` and `range outside text` are named | `exact` one character off |
| `evidence.align` | the fixture's sentence aligns verbatim at coverage 1; with one word changed, exactly one `substituted` at that word's ranges; every reply names its `aligner` and a `texts_digest` that is stable and equals §2.4 over the fixture documents' records | a reply without `texts_digest` |
| `auth.named_client` | an unissued credential in the host's form is `401` on every protected route and `/mcp`, from loopback too; `--named-token` is admitted; `--revoked-token` is `401` | the loopback-first resolver |
| `auth.local_peer` | from loopback, a foreign `Origin`, `Sec-Fetch-Site: cross-site` or a foreign `Host` is never admitted as local, on every protected OICP route and `/mcp`; where the client is trusted as local the first two are `403 cross-origin`; no foreign origin is granted `Access-Control-Allow-Origin` | an address-only guard; a permissive CORS layer |

`knowledge.search` also requires, with `knowledge:document`, that every hit
decodes as a `KnowledgeResult` carrying a well-formed `document`.
`manifest.features` checks the co-occurrence rules of §2.1.

### 6.1 The fixture library

The suite installs one small library, three short documents with metadata on
two and one sentence it plants a changed word in. It installs the library
with `ingest:recipe` and the inline form of §4.1, because a host need not
share the suite's disk. The documents are data
(`cmnwlth/crates/oicp-conformance/fixture/library.json`), and the rendered
recipe is committed beside them as the normative example of the inline form,
`cmnwlth/crates/oicp-conformance/fixture/library.recipe.toml`. On a local host,
`--fixture-dir <dir>` writes the documents there as files and installs a
recipe that reads them, with declared metadata, instead. The evidence checks
identify a fixture document by `source.sha256` equal to the sha256 of its
text. If the library cannot be installed, each check that reads it reports
that it could not judge, and why.

### 6.2 Running it

```
oicp-conformance --host <url> [--token <bearer>]
                 [--fixture-dir <dir>]                      # local host only
                 [--named-token <bearer>] [--revoked-token <bearer>]
                 [--bogus-token <bearer>]                   # default: svrn_ + 64 hex
```

## 7. Backward compatibility

- Every v0.5 field is `#[serde(default)]` and skipped when empty. A v0.4
  payload, manifest or hit or install, reads into the v0.5 types and writes
  back unchanged. The committed v0.4 wire fixture is pinned so.
- `OICP_VERSION` becomes `"0.5.0"` when this draft is approved. Until then the
  reference types carry `"0.4.0"`, and features gate behaviour either way.
- A v0.5 client against a v0.4 host sees none of the five features. It finds
  no `knowledge.evidence`, reads hits without `document`, and installs only by
  id.
- The `metadata` map on `KnowledgeResult` stays, empty on the reference host
  once it advertises `knowledge:document`.

## 8. Non-goals

- **Chunks as exact slices of their text, and ranges on search hits.** Spans
  come from alignment in v0.5. A hit carries its text's name, not its range.
- **An MCP projection of these operations.** If one is wanted, its schemas are
  generated from `oicp-types`, never written a second time.
- **Text roots and inclusion proofs** over stored texts. The text store is
  where they would attach.
- **A declaration for cross-origin browser clients** (§5.2).
- **Request tracing headers** (`traceparent`).

## 9. What v0.5 does not change

The v0.4 manifest, constraint negotiation, embed-model identity, the ingest
progress state machine, model fingerprints and the knowledge-search request are
unchanged. See the previous version, [`oicp-v0.4.md`](./oicp-v0.4.md).
