# Open Inference Capabilities Protocol (OICP) — v0.4

**Version:** 0.4.0
**Status:** Draft (extends v0.3; v0.3 remains the fallback path)
**License:** CC0 (public domain dedication)

---

## Abstract

v0.4 makes a host's *constraint machinery* and *knowledge plane* discoverable
enough that a client built only against "OICP manifest + OpenAI-compatible HTTP"
can run the full workflow / recipe-authoring stack against any conforming host —
without linking that host's implementation.

v0.3 standardized *routing*: which model serves which kind-of-work well. It
assumed an OpenAI-compatible HTTP substrate underneath but said nothing about
which request *features* that substrate honours, how a client discovers a host's
embedding space precisely enough to share a corpus, or how a client asks a host
to ingest a corpus. Those three gaps are exactly what a decoupled client trips
over. v0.4 closes them, additively.

Everything v0.4 adds is a serde-defaulted field or an optional endpoint. A v0.4
manifest with no new fields populated deserializes and behaves as a v0.3 manifest;
a v0.3 client never sees the new fields; a v0.4 client against a v0.3 host reads
"no features advertised" and degrades to the documented baseline. **Feature
presence, not the version string, gates behaviour** — a client MUST NOT hard-fail
on an unrecognized `oicp_version`.

---

## 1. Motivation

The reference implementation is being decomposed: the workflow engine and the
recipe-authoring toolchain are moving into a package that depends on the rest of
the system through nothing but this protocol. For that package to run recipes and
workflows against a host it did not compile against, three things that are today
implicit in a shared binary must become explicit on the wire:

1. **Which constraint fields the host honours.** The reference host accepts
   `response_format: {type: json_schema}`, a proprietary `lark_grammar` body
   field, sampler allow-lists, and a `think_budget`. A generic OpenAI-compatible
   endpoint may honour only `response_format: {type: json_object}`, or ignore
   constraints entirely and return prose. A client that cannot *discover* which
   of these hold has to either assume the richest (and get malformed output from
   weaker hosts) or assume the poorest (and waste the capability of rich hosts).
   Today this is resolved by operator configuration; it should be discovered.

2. **The host's exact embedding space.** Two nodes may share a corpus only if
   they embed bit-identically. v0.3's `EmbedModelInfo` carries `model_id`,
   `dimensions`, `pooling`, `normalization` — but the reference embedder also
   prepends a query-side *instruction prefix* before embedding, and that prefix
   is part of the embedding space. A client that reconstructs the query embedding
   without it produces vectors in a different space. The prefix must be
   advertised.

3. **How to ask a host to ingest a corpus.** The `recipe:` workflow step installs
   a corpus by calling the reference host's *internal* control plane
   (`POST /internal/corpus/install`), which is perimeter-trusted, loopback-shaped,
   and not part of any published protocol. A decoupled client — and any
   third-party host — needs a first-class, authenticated, manifest-advertised
   ingest surface.

A secondary motivation: enabling *other* implementations. A protocol a second
party can implement against needs its capability surface discoverable and its
conformance testable. v0.4 plus the companion conformance suite is what turns
"an internal vocabulary two crates share" into "a protocol a third party can
target."

## 2. Feature advertisement

`ProviderManifest` gains a provider-level `features` list:

```rust
ProviderManifest {
    … (v0.3 fields) …
    /// v0.4: request-level capabilities this host honours. Empty (the
    /// serde default, and the absence-on-the-wire shape) means "v0.3
    /// host" — the client assumes only baseline OpenAI-compat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    features: Vec<String>,
}
```

**Provider-level, not per-model.** Constraint enforcement in the reference host
lives in the sampler layer — one grammar engine, applied to every model the host
serves. Advertising `features` per model would repeat identical strings N times
and imply a per-model precision the implementation does not have. A per-model
`features` override, with *absent = inherit provider*, is **reserved for a future
revision** for hosts that front genuinely heterogeneous backends; v0.4 clients
MUST read `features` at the provider level.

### 2.1 Registered feature strings

Feature strings are matched exactly (byte-for-byte). Extension features carry the
`x:` prefix, mirroring the capability-hint convention (§2.1 of v0.3); a host MAY
advertise `x:`-prefixed features and clients MUST preserve unknown feature strings
verbatim (never reject a manifest for carrying one).

| feature | meaning |
|---|---|
| `constraint:json_schema` | `response_format: {type: "json_schema", json_schema: {...}}` is honoured with grammar-level enforcement — output is guaranteed to validate against the supplied schema. |
| `constraint:json_object` | `response_format: {type: "json_object"}` guarantees syntactically valid JSON (no schema conformance guarantee). |
| `constraint:lark` | the `lark_grammar` body field (a Lark grammar source string) is honoured; output is guaranteed to be in the grammar's language. Strictly more expressive than `constraint:json_schema`. |
| `constraint:allowlist:url` | the `url_allowlist` sampler constraint is honoured. |
| `constraint:allowlist:evidence_id` | the `evidence_id_allowlist` sampler constraint is honoured. |
| `constraint:allowlist:cmd_prefix` | the `cmd_prefix` / `assistant_prefix` sampler constraints are honoured. |
| `think_budget` | the `think_budget` body field (a reasoning-token cap) is honoured. |
| `oicp:request_properties` | the `oicp` request envelope (`InferenceRequirements`: `capability_hint`, `latency_class`, `context_tokens`, `max_output_tokens`) is consumed for routing. |
| `ingest:v1` | the §5 ingest extension (`install` + `progress`) is mounted; MUST co-occur with a populated `knowledge.ingest`. |
| `ingest:recipe_test` | the §5.4 recipe-test endpoint is mounted; MUST co-occur with `knowledge.ingest.test_endpoint`. |
| `model_fingerprint` | §6 fingerprints are populated on manifest models and echoed in response metadata. |

An unrecognized non-`x:` feature is preserved verbatim (a forward-standardization
cycle may define it); a client treats an unrecognized feature as *absent*
(it will not send a field it does not itself understand).

## 3. Constraint negotiation

A client MUST NOT send a constraint-bearing request field unless the host
advertises the corresponding feature. The single exception is
`response_format: {type: "json_object"}`, which a client MAY send speculatively to
a v0.3 or unknown host — it is the OpenAI baseline and a conforming
OpenAI-compatible host either honours or harmlessly ignores it.

A host MUST ignore request fields it does not understand — it MUST NOT reject a
request (e.g. with `400`) merely because it carries a `lark_grammar`, a
`think_budget`, or an `oicp` envelope it does not implement. This is what lets a
client speak richly to rich hosts and have the extra vocabulary fall away
silently against poorer ones.

### 3.1 The structured-output degrade ladder (normative)

A client that needs schema-conformant JSON output selects its mechanism in this
order, taking the first the host advertises:

1. `constraint:json_schema` — send `response_format: {type: json_schema, …}`;
   trust the output conforms.
2. `constraint:json_object` — send `response_format: {type: json_object}` **and**
   embed the schema in the prompt; parse the result and validate it
   client-side.
3. neither — embed the schema in the prompt; parse and validate client-side, with
   a bounded parse-and-repair retry.

`constraint:lark` is **not** a rung on this ladder — it is a distinct capability
for grammars that are not expressible as JSON Schema (alternations, regex
leaves, tool-call envelopes). When a client holds a Lark grammar and the host
does not advertise `constraint:lark`, the client either re-encodes it as JSON
Schema when that is possible, or falls to prompt-plus-client-side-validation.

This ladder replaces operator configuration: a host's advertised features *are*
the answer to "how hard should I constrain against this endpoint," and any
per-provider structured-output override becomes exactly that — an explicit
override of a value the protocol otherwise supplies.

## 4. Embed-model identity completeness

`EmbedModelInfo` gains the query-instruction prefix:

```rust
EmbedModelInfo {
    model_id:      String,
    dimensions:    usize,
    pooling:       PoolingStrategy,
    normalization: NormalizationStrategy,
    /// v0.4: instruction prefix prepended to *query* text (not
    /// document text) before embedding. Empty string = no prefix
    /// (also the v0.3-on-the-wire shape via serde default).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    query_instruction_prefix: String,
}
```

`String` (not `Option<String>`) preserves the type's `Eq`/`Hash` derivations and
keeps the compatibility test mechanical: **two nodes are embedding-compatible for
a shared corpus iff their `EmbedModelInfo` values compare equal — all five
fields, prefix included.**

The prefix is **asymmetric**: it applies to *query* embedding only. Corpus
documents are embedded from their raw text; a search query is embedded after the
prefix is prepended. A client reconstructing a query embedding for a federated
knowledge search MUST prepend the advertised prefix (or it produces a vector in a
different space and the search silently degrades).

A v0.4 host that publishes a `knowledge` section MUST populate `embed_model`
(v0.3's "`None` ⇒ exclude from collaborative ingestion" rule stays in force for
v0.3 hosts). The host is the authority for its own `normalization` and
`query_instruction_prefix`; a client MUST NOT guess these from a `/v1/models`
response for a v0.4 host. (Reconstruction-by-guessing remains a permissible
*fallback* only when talking to a v0.3 host that leaves `embed_model` absent.)

## 5. Ingest extension

An OICP host MAY expose a corpus-ingest surface. When it does, it advertises the
endpoints inside `KnowledgeManifest` and the corresponding features in
`ProviderManifest.features`:

```rust
KnowledgeManifest {
    … (v0.3 fields) …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ingest: Option<IngestEndpoints>,
}

IngestEndpoints {
    install_endpoint:  String,           // e.g. "/oicp/v1/corpus/install"
    progress_endpoint: String,           // e.g. "/oicp/v1/corpus/progress"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    test_endpoint:     Option<String>,   // §5.4; present iff feature ingest:recipe_test
}
```

Endpoint values are paths relative to the manifest's origin (the same convention
`search_endpoint` uses). Advertising `ingest` REQUIRES the `ingest:v1` feature;
advertising `test_endpoint` REQUIRES `ingest:recipe_test`.

### 5.1 Install

```
POST {install_endpoint}
Request:   CorpusInstallRequest { corpus_id: String,
                                  parameters: BTreeMap<String, serde_json::Value> }  // default {}
Response:  CorpusInstallResponse { corpus_id: String, spawned: bool }
```

Idempotent. `spawned = true` means a fresh ingest job started; `spawned = false`
means the corpus is already installed or an ingest for it is already running.
`503` when the host has no ingest engine; `400` on parameter validation failure.

### 5.2 Progress

```
GET {progress_endpoint}
Response:  CorpusProgressResponse { progress: BTreeMap<String, CorpusIngestProgress> }

CorpusIngestProgress {
    phase:    IngestPhase,           // Pending | Downloading | Embedding | Indexing
                                     //   | Optimizing | Enriching | Complete | Failed
    fraction: Option<f32>,           // [0,1], best-effort; absent when unknown
    detail:   Option<String>,        // human-readable; the error message when phase = Failed
}
```

The progress DTO is a **protocol type** — it deliberately does not embed any
implementation's internal progress enum, so a host may implement ingest without
linking the reference engine.

### 5.3 The poll state machine (normative)

After a `200` from install, a client polls `progress_endpoint`:

1. An entry for the `corpus_id` in a non-terminal phase ⇒ ingest in progress.
2. Phase `Complete` or `Failed` ⇒ terminal (`Failed` carries the reason in
   `detail`).
3. An entry that was observed and then *disappears* from the map ⇒ treat as
   complete (a host MAY evict terminal entries).
4. No entry for the `corpus_id` ever appearing within a grace window (a client
   SHOULD use **15 s**) ⇒ treat as already installed (the common `spawned=false`
   case where the corpus was present before the call).

A host MUST make rules 3 and 4 unambiguous: it MUST retain a terminal entry for
at least one client poll interval, OR guarantee that an already-installed corpus
never produces a progress entry at all.

### 5.4 Recipe test (optional)

A host MAY expose a dry-run recipe test — run the recipe's acquire→extract→
chunk stages over a small sample and return per-stage diagnostics, without
installing anything:

```
POST {test_endpoint}
Request:   RecipeTestRequest { recipe_toml: String,
                               options: RecipeTestOptions { sample_limit: Option<u32>,
                                                            offline: bool, .. } }
Response:  RecipeTestReport { stages: Vec<StageReport { name, docs_in, docs_out,
                                                        misses: Vec<String>,
                                                        sample: Vec<String> }>,
                              ok: bool }
```

`RecipeTestReport` is a protocol type (no implementation internals on the wire).
This is the network form of the authoring inner loop's "does this recipe work"
check; a client validates a recipe's *shape* locally against the published recipe
JSON Schema and reserves this endpoint for *behavioural* test-runs.

### 5.5 Auth

The ingest endpoints ride the host's standard client-facing auth posture: on the
reference host, loopback callers pass and non-loopback callers present a bearer
token (the same layer that guards `/v1/chat/completions`). They MUST NOT be
auth-exempt. A host MUST authenticate ingest at least as strongly as it
authenticates inference. Lifecycle operations beyond install (pause, cancel,
scope-expand) are **out of scope** for v0.4 (§9).

## 6. Model identity and fingerprint

Cache correctness for model-dependent artifacts (e.g. cached enrichment phases)
requires knowing *which* model produced a result. v0.4 makes model identity
precise on both the manifest and the response:

- The `model` field of a completion response MUST be the **resolved concrete
  model id** — the id after any alias or routing-pipeline rewrite, not the alias
  the client sent.
- `ProviderModel` gains `fingerprint: Option<String>` — an opaque token that MUST
  change whenever the served weights, quantization, or chat template change. A
  recommended synthesis is `"{id}@{quantization}#{size_gb}"` or a content-hash
  prefix; the value is opaque to clients.
- `OicpResponseMeta` gains `model_fingerprint: Option<String>` — the same token,
  echoed per response.

Both fields are serde-defaulted and gated by the `model_fingerprint` feature. A
client SHOULD include `(model, fingerprint)` in cache keys for model-dependent
artifacts when a fingerprint is present, falling back to `model` alone otherwise.

## 7. Context-length discoverability

`ProviderModel.context_tokens` is the normative source of a model's context
window. v0.4 tightens two obligations:

- A host MUST populate `context_tokens` truthfully per model — not a fixed
  constant across models.
- For every claim, `claim.max_context ≤ model.context_tokens`.

A client SHOULD read `context_tokens` from the manifest rather than assuming a
fixed window. Extending `/v1/models` to carry a context window is **out of
scope** — that surface stays vanilla-OpenAI; the manifest is the OICP channel for
this fact.

## 8. Backward compatibility

- Every field v0.4 adds is `#[serde(default)]` with `skip_serializing_if` for the
  empty case. A v0.4 manifest with `features: []`, no `knowledge.ingest`, no
  fingerprints, and empty prefixes serializes byte-identically to a v0.3 manifest.
- `OICP_VERSION` becomes `"0.4.0"`. Clients MUST NOT hard-fail on an unrecognized
  version; **feature presence, not the version string, gates behaviour.**
- A v0.3 client never sees the new fields (they serialize away when empty) and
  never calls the new endpoints.
- A v0.4 client against a v0.3 host reads `features: []` → baseline degrade ladder;
  `embed_model` absent → excluded from collaborative ingestion (the existing
  rule); no `knowledge.ingest` → the client falls back to any host-internal
  install path only when it is itself loopback-local to that host.
- Unknown / `x:`-prefixed feature strings are preserved verbatim.

## 9. Non-goals

Explicitly deferred, to keep the contract small and honest:

- **Cancellation of in-flight inference.** Timeout is the only backstop; there is
  no cooperative cancel on the completion contract.
- **Ingest lifecycle beyond install** — pause, cancel, scope-expand. These remain
  host-internal control-plane operations.
- **Streaming (SSE) ingest progress.** Progress is poll-only.
- **Per-model `features` overrides.** Reserved for a future revision.
- **crates.io publication of the reference types.** The wire schema is specified
  here and in `oicp-types`; independent publication is a later step.

## 10. Conformance

A host claims v0.4 conformance by passing the companion conformance suite
(`oicp-conformance`), which exercises each surface this document specifies against
a live host URL: manifest well-formedness and claim invariants, feature-string
validity, embed-model completeness, the structured-output degrade ladder at each
advertised level, embedding bit-compatibility (including the query prefix), the
knowledge-search shape, and — when advertised — the ingest install/progress state
machine and the recipe-test endpoint. Feature-gated checks *skip* (they are not
failures) when the corresponding feature is not advertised, so a minimal
OpenAI-compatible host and a full reference host are both conformant at their
respective feature levels.

---

## 11. What v0.4 does not change

- The v0.3 `CapabilityClaim`, request property fields, scheduling priority order,
  and the composed reference scorer (`score_with_adjustments`) are unchanged.
- The `ProviderManifest` top-level shape gains only the additive `features` field.
- The knowledge-search API (§6 of v0.3 / v0.2) is unchanged.
- The privacy model (`ShardingPrivacy`, default `LocalOnly`) is unchanged.

See the implementation in `oicp-types/src/lib.rs`, the previous protocol version
in [`oicp-v0.3.md`](./oicp-v0.3.md), and the extraction it enables in
`sovereign/SYSTEM_OVERVIEW.md`.
