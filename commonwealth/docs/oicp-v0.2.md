# Open Inference Capabilities Protocol (OICP)

**Version:** 0.2.0
**Status:** Draft
**License:** CC0 (public domain dedication)

---

## Abstract

This specification defines a protocol for capability negotiation between inference clients (applications that consume language model completions) and inference providers (services that run language models and host knowledge indexes). It provides a shared vocabulary for expressing what a client needs and what a provider offers, enabling capability-aware routing without coupling clients to specific model names, architectures, or provider implementations.

OICP is transport-agnostic. It defines JSON schemas for capability descriptions, inference requirements, and knowledge availability, intended to be embedded in HTTP request/response bodies or any other message format. The reference integration is an extension to the OpenAI chat completions API.

OICP addresses a gap between existing protocols. MCP (Model Context Protocol) standardizes tool access. A2A (Agent-to-Agent) standardizes agent collaboration. The OpenAI API standardizes inference transport. None of these provide a vocabulary for expressing "I need a model strong at analysis" or "I have a model strong at code" or "I host an indexed Wikipedia corpus." OICP fills this gap.

---

## 1. Design Principles

**Small.** The full specification fits in one document. Implementing it is a weekend of work. If the spec is too complex to implement in a weekend, it has failed.

**Stable.** The capability vocabulary changes slowly. Adding a capability is a minor version bump. Removing one is a major version bump and should essentially never happen.

**Ignorance-safe.** Unrecognized capabilities, fields, and extensions are ignored, not rejected. Old clients work with new providers and vice versa. No version negotiation is required.

**Neither side owns it.** This specification is standalone with a public domain license. No project has privileged influence over its evolution.

**Additive.** OICP improves routing when both sides support it and changes nothing when they don't. A client sending OICP requirements to a non-OICP provider works normally — the provider ignores unknown fields. A provider serving OICP capabilities to a non-OICP client works normally — the client ignores the extra metadata.

---

## 2. Capability Vocabulary

### 2.1 Capability Domains

A **capability** is a broad domain of competence.

| ID | Name | Description |
|----|------|-------------|
| `general` | General Reasoning | Broad knowledge, common-sense reasoning, question answering, summarization. The baseline capability. |
| `code` | Code Generation | Writing, reviewing, debugging, and explaining source code across programming languages. |
| `analysis` | Analysis & Research | Synthesizing information from multiple sources, evaluating evidence, structured argumentation, academic research. |
| `math` | Mathematics | Formal mathematics, proofs, numerical computation, statistical reasoning. |
| `creative` | Creative Writing | Fiction, poetry, narrative, humor, stylistic range, voice consistency. |
| `instruction` | Instruction Following | Precisely following complex, multi-step, or constrained instructions. Format compliance, schema adherence, structured output. |
| `multilingual` | Multilingual | Competence in languages other than English. |
| `vision` | Vision | Understanding images, diagrams, screenshots, and other visual inputs. |
| `long_context` | Long Context | Maintaining coherence and recall over very long inputs (>32k tokens). |

### 2.2 Proficiency Levels

Each capability is rated on a five-point ordinal scale:

| Level | Value | Meaning |
|-------|-------|---------|
| **None** | 0 | No meaningful competence. |
| **Basic** | 1 | Handles simple tasks. Frequent errors on moderate tasks. Roughly a small (<3B) general-purpose model. |
| **Moderate** | 2 | Handles moderate tasks competently. Struggles with expert-level work. Roughly a mid-size (7-14B) model. |
| **Strong** | 3 | Handles most tasks well, including many expert-level tasks. Roughly a large (30-70B) or specialized model. |
| **Exceptional** | 4 | State-of-the-art or near it. Competitive with the best available models. |

These are subjective assessments, not benchmark scores. The scale is intentionally coarse. When in doubt, rate lower.

### 2.3 Capability Profile

A **capability profile** is a map from capability IDs to proficiency levels. Capabilities not listed are implicitly level 0.

```json
{
  "general": 3,
  "code": 4,
  "instruction": 3,
  "math": 3,
  "analysis": 2
}
```

### 2.4 Extending the Vocabulary

New capabilities may be added in minor version increments. Requirements for a new capability:

1. Represents a genuinely distinct axis of model competence, not a subcategory of an existing capability.
2. At least two distinct model families have meaningfully different proficiency levels on the proposed capability.
3. Ratable by a human evaluator without running a benchmark suite.

Implementations MUST ignore capability IDs they don't recognize.

---

## 3. Client Requirements Schema

A client expresses what it needs from an inference call using an **inference requirement**.

```json
{
  "oicp_version": "0.2.0",
  "capabilities": {
    "required": {
      "code": 2
    },
    "preferred": {
      "code": 3,
      "instruction": 3
    }
  },
  "context": {
    "min_tokens": 8192,
    "preferred_tokens": 32768
  },
  "performance": {
    "latency": "interactive"
  },
  "privacy": {
    "sharding": "local_only"
  },
  "request_id": "step-4-synthesis"
}
```

### 3.1 Fields

**`oicp_version`** (string, required): The version of this specification the client implements.

**`capabilities`** (object, optional): Capability requirements.

- **`required`** (object, optional): Minimum proficiency levels. The provider MUST NOT select a model with proficiency below any required threshold. If no model meets all required thresholds, the provider SHOULD return an error rather than silently serving an inadequate model.

- **`preferred`** (object, optional): Desired proficiency levels for scoring and ranking among models that meet the required thresholds.

The distinction matters. A research tool might require `analysis: 2` (don't serve a model that can't do analysis at all) and prefer `analysis: 3` (if a strong analysis model is available, use it). The required floor prevents bad matches. The preferred scores guide selection among adequate options.

If `capabilities` is omitted entirely, the provider selects based on its own default criteria.

**`context`** (object, optional): Context window requirements.

- **`min_tokens`** (integer, optional): Provider MUST NOT select a model with a context window smaller than this value.
- **`preferred_tokens`** (integer, optional): Provider SHOULD prefer models at or above this value but MAY serve a smaller-window model if it is significantly better on the requested capabilities.

**`performance`** (object, optional): Performance constraints.

- **`latency`** (string, optional): One of:
  - `"interactive"` — Optimize for time-to-first-token. May sacrifice model quality for speed.
  - `"throughput"` — Optimize for model capability. May tolerate higher latency.
  - `"background"` — No latency requirement. Provider may queue the request.
  - `"best_effort"` (default) — Provider uses its own judgment.

**`privacy`** (object, optional): Privacy constraints.

- **`sharding`** (string, optional): One of:
  - `"local_only"` (default) — The provider MUST NOT distribute inference across multiple nodes. The request must be served on a single machine. Intermediate activations must not traverse a network. If the provider cannot serve the request on a single node, it SHOULD return an error rather than silently sharding.
  - `"mesh_allowed"` — The provider MAY distribute inference across multiple nodes for better model quality. The client accepts that intermediate activations will traverse the network between nodes.

The default is `"local_only"`. This is a deliberate design choice: privacy is the default, not something the client has to remember to request. Clients that want distributed inference explicitly opt in.

**`request_id`** (string, optional): A client-provided identifier echoed back in the response for correlation. Useful when the client has multiple concurrent requests to the same provider and needs to match responses to requests.

### 3.2 Matching Semantics

A model **satisfies** a requirement if:

1. For every capability in `required`, the model's proficiency is ≥ the required level.
2. The model's context window is ≥ `context.min_tokens` (if specified).
3. The provider can serve the model without violating the `privacy.sharding` constraint.

A model's **match score** against a requirement is computed from the `preferred` capabilities. The scoring algorithm is not specified — providers may use weighted sums, geometric means, or any other method, and may factor in their own constraints (VRAM, topology, cost). The specification requires only that higher proficiency in preferred capabilities produces a higher score.

---

## 4. Provider Manifest Schema

A provider advertises what it offers using a **provider manifest**.

```json
{
  "oicp_version": "0.2.0",
  "provider": {
    "name": "Sunset District Co-op",
    "type": "mesh"
  },
  "models": [
    {
      "id": "qwen3-coder-30b-q4km",
      "base_model": "qwen3-coder-30b",
      "quantization": "Q4_K_M",
      "capabilities": {
        "general": 2,
        "code": 3,
        "instruction": 3,
        "math": 3
      },
      "context_tokens": 32768,
      "status": {
        "available": true,
        "loaded": true,
        "estimated_tokens_per_sec": 45.0,
        "estimated_ttft_ms": 1100
      }
    },
    {
      "id": "qwen3-30b-q4km",
      "base_model": "qwen3-30b",
      "quantization": "Q4_K_M",
      "capabilities": {
        "general": 3,
        "analysis": 3,
        "creative": 3,
        "code": 2,
        "instruction": 2
      },
      "context_tokens": 32768,
      "status": {
        "available": true,
        "loaded": false,
        "estimated_tokens_per_sec": 42.0,
        "estimated_ttft_ms": 1300,
        "estimated_load_time_sec": 45
      }
    }
  ],
  "knowledge": {
    "corpora": [
      {
        "id": "wikipedia",
        "name": "Wikipedia",
        "total_chunks": 6800000,
        "shards": 3,
        "replicas": 2,
        "fully_available": true,
        "last_updated": "2026-03-15"
      },
      {
        "id": "openalex",
        "name": "OpenAlex Abstracts",
        "total_chunks": 14200000,
        "shards": 5,
        "replicas": 1,
        "fully_available": true,
        "last_updated": "2026-01-01"
      }
    ],
    "search_endpoint": "/v1/knowledge/search"
  },
  "federation": {
    "peers": [
      {
        "name": "Mission District Co-op",
        "capabilities_url": "http://10.0.1.50:9741/oicp/v1/capabilities",
        "trust_level": "model_and_knowledge_sharing"
      }
    ]
  }
}
```

### 4.1 Fields

**`oicp_version`** (string, required): The version of this specification the provider implements.

**`provider`** (object, optional): Provider metadata. Informational only.

- **`name`** (string, optional): Human-readable provider name.
- **`type`** (string, optional): Provider type hint. One of `"local"`, `"mesh"`, `"cloud"`, `"hybrid"`. Informational only — clients MUST NOT make routing decisions based on this field.

**`models`** (array, required): Available models. Each entry:

- **`id`** (string, required): Provider-specific model identifier. Clients may use this for logging but SHOULD NOT hardcode dependencies on specific IDs.
- **`base_model`** (string, optional): The underlying model family, independent of quantization. Allows clients to recognize that `qwen3-30b-q4km` and `qwen3-30b-q8` are the same model at different quality points.
- **`quantization`** (string, optional): The quantization format applied to this model (e.g., `"Q4_K_M"`, `"Q8_0"`, `"F16"`). Absent for full-precision models or cloud-hosted models where quantization is not exposed. When present, the capability profile SHOULD reflect the model's capabilities *at this quantization level*, not at full precision.
- **`capabilities`** (object, required): The model's capability profile as defined in Section 2.3.
- **`context_tokens`** (integer, required): Maximum context window size in tokens.
- **`status`** (object, required): Current operational status.
  - **`available`** (boolean, required): Whether this model can be served (loaded or loadable with available resources).
  - **`loaded`** (boolean, required): Whether this model is currently in memory and ready immediately.
  - **`estimated_tokens_per_sec`** (number, optional): Estimated generation throughput.
  - **`estimated_ttft_ms`** (integer, optional): Estimated time-to-first-token.
  - **`estimated_load_time_sec`** (integer, optional): If not loaded, estimated time to load. Absent if loaded.

**`knowledge`** (object, optional): Knowledge index availability. Present when the provider hosts searchable knowledge bases.

- **`corpora`** (array, required if `knowledge` is present): Available corpora.
  - **`id`** (string, required): Corpus identifier (e.g., `"wikipedia"`, `"openalex"`).
  - **`name`** (string, optional): Human-readable corpus name.
  - **`total_chunks`** (integer, required): Total number of indexed chunks in this corpus across all shards.
  - **`shards`** (integer, optional): Number of shards this corpus is split across. Absent if the corpus is hosted on a single node.
  - **`replicas`** (integer, optional): Number of copies of each shard. Higher means more resilient to node departure.
  - **`fully_available`** (boolean, required): Whether all shards of this corpus are currently reachable. `false` means search results may be incomplete.
  - **`last_updated`** (string, optional): ISO 8601 date of the most recent corpus update. Helps clients assess freshness.

- **`search_endpoint`** (string, required if `knowledge` is present): Relative URL path for the knowledge search API.

**`federation`** (object, optional): Discovery of peered providers. Present when this provider has trust relationships with other providers.

- **`peers`** (array, required if `federation` is present): Peered providers.
  - **`name`** (string, required): Human-readable name of the peered provider.
  - **`capabilities_url`** (string, required): URL where the peer's OICP manifest can be fetched. Clients may optionally follow these URLs to discover capabilities across a federation.
  - **`trust_level`** (string, optional): Describes the trust relationship. Informational. Example values: `"model_and_knowledge_sharing"`, `"full"`.

Clients are NOT required to follow federation links. The field enables optional cross-provider discovery but imposes no obligation.

### 4.2 Serving Endpoint

The provider manifest is served at:

```
GET /oicp/v1/capabilities
```

This endpoint returns the full provider manifest as JSON. Clients SHOULD poll periodically (recommended interval: 30 seconds) to maintain a current view. The endpoint MUST be lightweight — providers SHOULD cache the manifest and update it only when model or knowledge state changes.

---

## 5. Request-Level Requirements

### 5.1 OpenAI-Compatible API Extension

For providers serving the OpenAI chat completions API, requirements are included in the request body under an `oicp` key:

```json
POST /v1/chat/completions
{
  "messages": [
    {"role": "user", "content": "Compare Ostrom's design principles with Williamson's transaction cost framework"}
  ],
  "oicp": {
    "oicp_version": "0.2.0",
    "capabilities": {
      "required": {"analysis": 2, "general": 2},
      "preferred": {"analysis": 3, "general": 3}
    },
    "context": {"min_tokens": 16384},
    "performance": {"latency": "throughput"},
    "privacy": {"sharding": "mesh_allowed"},
    "request_id": "synthesis-step-4"
  }
}
```

Providers that implement OICP read the `oicp` key and use it for model selection. Providers that don't implement OICP ignore unknown keys (per the OpenAI API convention) and select models using their default logic.

### 5.2 Response Metadata

Providers that support OICP SHOULD include metadata about the serving model:

```json
{
  "id": "chatcmpl-...",
  "model": "qwen3-30b-q4km",
  "choices": [...],
  "oicp": {
    "model_capabilities": {
      "general": 3,
      "analysis": 3,
      "creative": 3,
      "code": 2,
      "instruction": 2
    },
    "quantization": "Q4_K_M",
    "match_quality": "full",
    "degraded_capabilities": null,
    "request_id": "synthesis-step-4"
  }
}
```

**`model_capabilities`** (object, optional): The capability profile of the model that served the request.

**`quantization`** (string, optional): The quantization of the serving model.

**`match_quality`** (string, optional): How well the serving model matched the requirements.
- `"full"` — All required thresholds met. Best available model for the preferred capabilities was selected.
- `"partial"` — All required thresholds met, but a better model for the preferred capabilities exists and was unavailable.
- `"degraded"` — One or more required thresholds were not met. The provider served the best available model but it falls below the client's minimum requirements.
- `"unmatched"` — No OICP requirements were provided; default model selection was used.

**`degraded_capabilities`** (object, optional): Present only when `match_quality` is `"degraded"`. Lists specifically which requirements were not met:

```json
{
  "degraded_capabilities": {
    "analysis": {"required": 3, "served": 2},
    "general": {"required": 3, "served": 2}
  }
}
```

This lets clients make informed decisions. A client receiving degradation on `analysis: 3 → 2` might add extra verification steps. A client receiving degradation on `code: 3 → 1` might fall back to a different provider entirely.

**`request_id`** (string, optional): The client-provided `request_id` echoed back for correlation.

---

## 6. Knowledge Search API

When a provider hosts searchable knowledge bases (indicated by the `knowledge` section in the manifest), clients can query them via the knowledge search endpoint.

### 6.1 Request

```json
POST /v1/knowledge/search
{
  "query_embedding": [0.123, -0.456, 0.789, ...],
  "query_text": "Ostrom design principles commons governance",
  "corpora": ["wikipedia", "openalex", "sep"],
  "limit": 20
}
```

**`query_embedding`** (array of floats, required): The query vector, computed by the client using its local embedding model. The provider uses this for vector similarity search.

**`query_text`** (string, required): The raw query text, used for keyword/full-text search alongside vector search. Also used for logging and diagnostics.

**`corpora`** (array of strings, optional): Which corpora to search. If omitted, all available corpora are searched.

**`limit`** (integer, optional): Maximum number of results to return. Default: 20.

### 6.2 Response

```json
{
  "results": [
    {
      "content": "Elinor Ostrom identified eight design principles...",
      "title": "Elinor Ostrom",
      "corpus_id": "wikipedia",
      "url": "https://en.wikipedia.org/wiki/Elinor_Ostrom",
      "score": 0.89,
      "metadata": {
        "section": "Design principles for managing a commons",
        "date": "2026-03-15"
      }
    }
  ],
  "corpora_searched": ["wikipedia", "openalex", "sep"],
  "corpora_unavailable": [],
  "total_chunks_searched": 21500000
}
```

**`results`** (array, required): Matching document chunks, ordered by descending relevance score.

Each result:
- **`content`** (string, required): The chunk text.
- **`title`** (string, optional): The document or article title.
- **`corpus_id`** (string, required): Which corpus this result comes from.
- **`url`** (string, optional): Source URL for citation.
- **`score`** (number, required): Relevance score. Scale is provider-defined but higher is always more relevant.
- **`metadata`** (object, optional): Additional metadata (section headings, publication dates, authors, etc.).

**`corpora_searched`** (array, required): Which corpora were actually searched (may differ from requested if some are unavailable).

**`corpora_unavailable`** (array, optional): Corpora that were requested but couldn't be searched (e.g., all shards offline).

**`total_chunks_searched`** (integer, optional): Total number of chunks across all searched corpora. Helps clients assess coverage.

### 6.3 Embedding Model Compatibility

The knowledge search API assumes the client's query embedding is compatible with the embeddings stored in the provider's index. In practice, this means the client and provider use the same embedding model. Sovereign's default embedding model (`qwen3-embedding-0.6b`) is the reference. Providers that use a different embedding model SHOULD advertise this in a future extension. For v0.2.0, the assumption is that all participants in a mesh use the same embedding model (configured at mesh level).

---

## 7. Community Capability Profiles

### 7.1 Profile Registry

A public repository of capability profiles for known models, organized by model family and quantization:

```
oicp-profiles/
├── qwen/
│   ├── qwen3-coder-30b.toml        # Full precision reference
│   ├── qwen3-coder-30b-Q8_0.toml
│   ├── qwen3-coder-30b-Q4_K_M.toml
│   ├── qwen3-coder-30b-Q2_K.toml
│   ├── qwen3-30b.toml
│   ├── qwen3-30b-Q4_K_M.toml
│   └── qwen3-14b-Q4_K_M.toml
├── deepseek/
│   └── ...
├── meta/
│   └── ...
└── README.md
```

### 7.2 Profile Format

```toml
# oicp-profiles/qwen/qwen3-coder-30b-Q4_K_M.toml

[model]
name = "Qwen3 Coder 30B"
family = "qwen3"
parameters = "30B"
quantization = "Q4_K_M"
repo = "Qwen/Qwen3-Coder-30B-GGUF"

[capabilities]
general = 2
code = 3
instruction = 3
math = 3
analysis = 2
creative = 1
multilingual = 2

[notes]
code = "Strong on HumanEval and SWE-Bench. Broad language coverage. Q4_K_M shows slight degradation on complex multi-file reasoning vs Q8."
general = "Reasonable common-sense reasoning but optimized for code at the expense of breadth."
quantization_impact = "Q4_K_M degrades code from 4 to 3 on complex reasoning tasks. Simpler code tasks are unaffected."
```

### 7.3 Profile Governance

- Contributions via pull requests with brief justifications.
- Disputed ratings (where experienced users disagree by ≥2 levels) documented with both positions.
- Per-quantization profiles capture the real-world experience that quantization affects capability — a model rated `code: 4` at full precision might deserve `code: 3` at Q4_K_M.
- Providers may reference profiles as defaults and override based on local experience.

### 7.4 Provider Usage

```toml
# In a provider's model configuration
[[models]]
repo = "Qwen/Qwen3-Coder-30B-GGUF"
quant = "Q4_K_M"
oicp_profile = "qwen/qwen3-coder-30b-Q4_K_M"

# Optional overrides
[models.capability_overrides]
code = 2  # "We find it performs below community rating at our context lengths"
```

---

## 8. Versioning

OICP follows semantic versioning:

- **Patch** (0.2.x): Clarifications, examples. No schema changes.
- **Minor** (0.x.0): New capabilities, new optional fields. Backward-compatible.
- **Major** (x.0.0): Breaking changes. Should be extremely rare.

Implementations declare support via `oicp_version`. Cross-version compatibility is ensured by the ignorance-safety principle — unknown fields are ignored, not rejected.

### Changes from 0.1.0 to 0.2.0

- Added `privacy` field to client requirements with `local_only` as default.
- Added `quantization` and `base_model` fields to provider model entries.
- Added `knowledge` section to provider manifest for corpus advertisement.
- Added `federation` section to provider manifest for peer discovery.
- Added `degraded_capabilities` detail to response metadata.
- Added `request_id` to client requirements and response metadata for correlation.
- Added `corpora_unavailable` to knowledge search response.
- Defined the knowledge search API (Section 6).
- Restructured community profiles to be per-quantization.

---

## 9. Reference Implementation Notes

### For Clients (e.g., Sovereign)

The minimum viable OICP client:

1. Include an `oicp` key in completion requests with capability requirements.
2. Optionally poll `/oicp/v1/capabilities` for provider state.
3. Optionally query `/v1/knowledge/search` for knowledge retrieval.

A skill system maps naturally to OICP: each skill declares capability and privacy requirements, and the executor attaches them to outgoing requests per step.

Per-step requirements enable mixed-privacy conversations: a plan that retrieves public knowledge from the mesh (`sharding: mesh_allowed`) and then does private reflection locally (`sharding: local_only`) uses different OICP requirements on different steps of the same plan.

### For Providers (e.g., Commonwealth)

The minimum viable OICP provider:

1. Serve `/oicp/v1/capabilities` with models and optionally knowledge corpora.
2. Read the `oicp` key from incoming completion requests for model selection.
3. Use `required` capabilities as a filter and `preferred` capabilities for scoring.
4. Include `oicp` metadata in responses.
5. Optionally serve `/v1/knowledge/search` for knowledge queries.
6. Enforce `privacy.sharding` constraints — never shard a `local_only` request.

A mesh scheduler factors OICP into model selection alongside VRAM, latency, and topology constraints. The OICP score is one input, not the only input. Caching the OICP-to-model resolution for the current model portfolio keeps routing overhead sub-millisecond.

### For Neither

OICP does not specify:

- How models are loaded, sharded, or managed.
- How providers discover each other or coordinate internally.
- How clients plan, route, or orchestrate tasks.
- How knowledge bases are ingested, chunked, or embedded.
- How capability profiles are benchmarked or validated.
- Any authentication, authorization, or payment mechanism.
- The embedding model used for knowledge search (assumed to be shared between client and provider).

These are the domain of the projects that implement OICP. OICP is the language they speak at the boundary.

---

## Appendix A: JSON Schema (Informative)

### Client Requirements

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "type": "object",
  "properties": {
    "oicp_version": {"type": "string"},
    "capabilities": {
      "type": "object",
      "properties": {
        "required": {"type": "object", "additionalProperties": {"type": "integer", "minimum": 0, "maximum": 4}},
        "preferred": {"type": "object", "additionalProperties": {"type": "integer", "minimum": 0, "maximum": 4}}
      }
    },
    "context": {
      "type": "object",
      "properties": {
        "min_tokens": {"type": "integer", "minimum": 0},
        "preferred_tokens": {"type": "integer", "minimum": 0}
      }
    },
    "performance": {
      "type": "object",
      "properties": {
        "latency": {"type": "string", "enum": ["interactive", "throughput", "background", "best_effort"]}
      }
    },
    "privacy": {
      "type": "object",
      "properties": {
        "sharding": {"type": "string", "enum": ["local_only", "mesh_allowed"], "default": "local_only"}
      }
    },
    "request_id": {"type": "string"}
  },
  "required": ["oicp_version"]
}
```

### Provider Manifest

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "type": "object",
  "properties": {
    "oicp_version": {"type": "string"},
    "provider": {
      "type": "object",
      "properties": {
        "name": {"type": "string"},
        "type": {"type": "string", "enum": ["local", "mesh", "cloud", "hybrid"]}
      }
    },
    "models": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "id": {"type": "string"},
          "base_model": {"type": "string"},
          "quantization": {"type": "string"},
          "capabilities": {"type": "object", "additionalProperties": {"type": "integer", "minimum": 0, "maximum": 4}},
          "context_tokens": {"type": "integer"},
          "status": {
            "type": "object",
            "properties": {
              "available": {"type": "boolean"},
              "loaded": {"type": "boolean"},
              "estimated_tokens_per_sec": {"type": "number"},
              "estimated_ttft_ms": {"type": "integer"},
              "estimated_load_time_sec": {"type": "integer"}
            },
            "required": ["available", "loaded"]
          }
        },
        "required": ["id", "capabilities", "context_tokens", "status"]
      }
    },
    "knowledge": {
      "type": "object",
      "properties": {
        "corpora": {
          "type": "array",
          "items": {
            "type": "object",
            "properties": {
              "id": {"type": "string"},
              "name": {"type": "string"},
              "total_chunks": {"type": "integer"},
              "shards": {"type": "integer"},
              "replicas": {"type": "integer"},
              "fully_available": {"type": "boolean"},
              "last_updated": {"type": "string"}
            },
            "required": ["id", "total_chunks", "fully_available"]
          }
        },
        "search_endpoint": {"type": "string"}
      },
      "required": ["corpora", "search_endpoint"]
    },
    "federation": {
      "type": "object",
      "properties": {
        "peers": {
          "type": "array",
          "items": {
            "type": "object",
            "properties": {
              "name": {"type": "string"},
              "capabilities_url": {"type": "string"},
              "trust_level": {"type": "string"}
            },
            "required": ["name", "capabilities_url"]
          }
        }
      }
    }
  },
  "required": ["oicp_version", "models"]
}
```

### Response Metadata

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "type": "object",
  "properties": {
    "model_capabilities": {"type": "object", "additionalProperties": {"type": "integer", "minimum": 0, "maximum": 4}},
    "quantization": {"type": "string"},
    "match_quality": {"type": "string", "enum": ["full", "partial", "degraded", "unmatched"]},
    "degraded_capabilities": {
      "type": "object",
      "additionalProperties": {
        "type": "object",
        "properties": {
          "required": {"type": "integer"},
          "served": {"type": "integer"}
        }
      }
    },
    "request_id": {"type": "string"}
  }
}
```