# Open Inference Capabilities Protocol (OICP)

**Version:** 0.1.0-draft **Status:** Draft **License:** CC0 (public domain dedication)

---

## Abstract

This specification defines a protocol for capability negotiation between inference clients (applications that consume language model completions) and inference providers (services that run language models). It provides a shared vocabulary for expressing what a client needs from a model and what a provider's available models offer, enabling capability-aware routing without coupling clients to specific model names, architectures, or provider implementations.

OICP is transport-agnostic. It defines JSON schemas for capability descriptions and requirements, intended to be embedded in HTTP headers, request bodies, or any other message format. The reference integration is an extension to the OpenAI chat completions API, but any transport can carry these schemas.

---

## 1. Design Principles

**Small.** The full specification fits in one document. Implementing it is a weekend of work for either side. If the spec is too complex to implement in a weekend, it has failed.

**Stable.** The capability vocabulary changes slowly. Adding a capability is a minor version bump. Removing one is a major version bump and should essentially never happen. Clients and providers should be able to implement OICP once and not touch it for a year.

**Ignorance-safe.** Unrecognized capabilities are ignored, not rejected. A client requesting a capability that a provider doesn't understand is not an error — the provider simply doesn't factor that capability into its model selection. A provider advertising a capability that a client doesn't understand is not an error — the client ignores it. This means old clients work with new providers and vice versa.

**Neither side owns it.** This specification is not part of any client or provider project. It is a standalone document with a public domain license. Anyone can implement it. No project has privileged influence over its evolution.

---

## 2. Capability Vocabulary

### 2.1 Capability Domains

A **capability** is a broad domain of competence. The initial vocabulary:

|ID|Name|Description|
|---|---|---|
|`general`|General Reasoning|Broad knowledge, common-sense reasoning, question answering, summarization. The baseline capability — every model has some level of this.|
|`code`|Code Generation|Writing, reviewing, debugging, and explaining source code across programming languages.|
|`analysis`|Analysis & Research|Synthesizing information from multiple sources, evaluating evidence, structured argumentation, academic and professional research.|
|`math`|Mathematics|Formal mathematics, proofs, numerical computation, statistical reasoning.|
|`creative`|Creative Writing|Fiction, poetry, narrative, humor, stylistic range, voice consistency.|
|`instruction`|Instruction Following|Precisely following complex, multi-step, or constrained instructions. Format compliance, schema adherence, structured output.|
|`multilingual`|Multilingual|Competence in languages other than English.|
|`vision`|Vision|Understanding images, diagrams, screenshots, and other visual inputs.|
|`long_context`|Long Context|Maintaining coherence and recall over very long inputs (>32k tokens).|

### 2.2 Proficiency Levels

Each capability is rated on a five-point ordinal scale:

|Level|Value|Meaning|
|---|---|---|
|**None**|0|The model has no meaningful competence in this domain.|
|**Basic**|1|Can handle simple tasks in this domain. Makes frequent errors on moderate tasks. Roughly equivalent to a small (<3B) general-purpose model's performance on specialized benchmarks.|
|**Moderate**|2|Handles moderate tasks competently. Struggles with expert-level work. Roughly equivalent to a mid-size (7-14B) general-purpose model.|
|**Strong**|3|Handles most tasks well, including many expert-level tasks. Roughly equivalent to a large (30-70B) model or a specialized model of any size.|
|**Exceptional**|4|State-of-the-art or near it. Competitive with the best available models on benchmarks and real-world tasks in this domain.|

These are subjective assessments, not benchmark scores. The scale is intentionally coarse — the difference between levels should be obvious to a human evaluator without running formal benchmarks. When in doubt, rate lower.

### 2.3 Capability Profile

A **capability profile** is a map from capability IDs to proficiency levels. Capabilities not listed are implicitly level 0 (None).

```json
{
  "general": 3,
  "code": 4,
  "instruction": 3,
  "math": 3,
  "analysis": 2
}
```

This profile describes a model that excels at code, is strong at general reasoning, instruction following, and math, moderate at analysis, and has no listed proficiency in creative writing, multilingual tasks, vision, or long context.

### 2.4 Extending the Vocabulary

New capabilities may be added in minor version increments (0.2.0, 0.3.0, etc.). To propose a new capability:

1. The capability must represent a genuinely distinct axis of model competence — not a subcategory of an existing capability. "Rust programming" is not a capability; it's a facet of `code`. "Audio understanding" would be a valid new capability if multimodal audio models become common.
2. At least two distinct model families must have meaningfully different proficiency levels on the proposed capability (otherwise it doesn't discriminate and isn't useful for routing).
3. The capability must be ratable by a human evaluator without running a benchmark suite. If you need to run SWE-Bench to distinguish levels, the capability is too fine-grained.

Implementations MUST ignore capability IDs they don't recognize. This is the core extensibility mechanism — a provider can advertise a capability that was added in OICP 0.3.0, and a client implementing OICP 0.1.0 will simply not use it for matching. No errors, no version negotiation.

---

## 3. Client Requirements Schema

A client expresses what it needs from an inference call using an **inference requirement**:

```json
{
  "oicp_version": "0.1.0",
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
  }
}
```

### 3.1 Fields

**`oicp_version`** (string, required): The version of this specification the client implements.

**`capabilities`** (object, optional): Capability requirements. Contains two sub-objects:

- **`required`** (object, optional): Minimum proficiency levels. The provider MUST NOT select a model with a proficiency level below any required threshold. If no model meets all required thresholds, the provider SHOULD return an error rather than silently serving an inadequate model.
    
- **`preferred`** (object, optional): Desired proficiency levels, used for scoring and ranking among models that meet the required thresholds. Higher proficiency in preferred capabilities makes a model a better match.
    

The distinction between `required` and `preferred` matters. A coding tool might require `code: 2` (the model must be at least moderate at code — don't serve a poetry model) and prefer `code: 4` (if a specialized coding model is available, use it). A research tool might require `analysis: 2` and prefer `analysis: 3, general: 3`. The required floor prevents bad matches; the preferred scores guide selection among adequate options.

If `capabilities` is omitted entirely, the provider selects based on its own default criteria (typically: largest available model).

**`context`** (object, optional): Context window requirements.

- **`min_tokens`** (integer, optional): The provider MUST NOT select a model with a context window smaller than this value.
- **`preferred_tokens`** (integer, optional): The provider SHOULD prefer models with context windows at or above this value, but may serve a model with a smaller window if no larger-context model is available or if the larger model is significantly worse on the requested capabilities.

**`performance`** (object, optional): Performance constraints.

- **`latency`** (string, optional): One of:
    - `"interactive"` — The client is in a real-time conversation loop. The provider should optimize for time-to-first-token, potentially at the cost of model quality (e.g., selecting a model that fits on fewer nodes with lower cross-node latency).
    - `"throughput"` — The client wants the best possible output quality and can tolerate higher latency. The provider should optimize for model capability, even if this means sharding across more nodes or selecting a larger, slower model.
    - `"background"` — The client has no latency requirement. The provider may queue the request and serve it when capacity is available. This is appropriate for batch processing, pre-computation, or tasks where the user is not waiting.
    - `"best_effort"` (default if omitted) — The provider uses its own judgment to balance latency and quality.

### 3.2 Matching Semantics

A model **satisfies** a requirement if:

1. For every capability listed in `required`, the model's proficiency level is ≥ the required level.
2. The model's context window is ≥ `context.min_tokens` (if specified).

A model's **match score** against a requirement is computed from the `preferred` capabilities. The scoring algorithm is not specified — providers may use weighted sums, geometric means, or any other method. The specification only requires that higher proficiency in preferred capabilities produces a higher score, and that the provider selects the highest-scoring model among those that satisfy the requirement.

This intentional underspecification of scoring allows providers to factor in their own constraints (VRAM, node topology, cost) alongside the client's preferences. A provider might slightly prefer a model with lower capability scores if it's already loaded and avoids a 45-second model swap. That's a legitimate provider-side optimization that the client shouldn't constrain.

---

## 4. Provider Capabilities Schema

A provider advertises what it offers using a **provider manifest**:

```json
{
  "oicp_version": "0.1.0",
  "provider": {
    "name": "Sunset District Co-op",
    "type": "mesh"
  },
  "models": [
    {
      "id": "qwen3-coder-30b-q4km",
      "capabilities": {
        "general": 2,
        "code": 4,
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
  ]
}
```

### 4.1 Fields

**`oicp_version`** (string, required): The version of this specification the provider implements.

**`provider`** (object, optional): Provider metadata. Informational only — clients MUST NOT use this for routing decisions.

- **`name`** (string, optional): Human-readable provider name.
- **`type`** (string, optional): Provider type hint. One of `"local"`, `"mesh"`, `"cloud"`, `"hybrid"`. Informational only.

**`models`** (array, required): Available models.

Each model entry contains:

- **`id`** (string, required): A provider-specific model identifier. Clients may use this for logging or display, but SHOULD NOT hard-code dependencies on specific model IDs. The capability profile is the routing-relevant information, not the ID.
- **`capabilities`** (object, required): The model's capability profile as defined in Section 2.3.
- **`context_tokens`** (integer, required): Maximum context window size in tokens.
- **`status`** (object, required): Current operational status.
    - **`available`** (boolean, required): Whether this model can currently be served, either because it's loaded or because it can be loaded with available resources.
    - **`loaded`** (boolean, required): Whether this model is currently in memory and ready to serve requests immediately.
    - **`estimated_tokens_per_sec`** (number, optional): Estimated generation throughput at current capacity.
    - **`estimated_ttft_ms`** (integer, optional): Estimated time-to-first-token in milliseconds.
    - **`estimated_load_time_sec`** (integer, optional): If `loaded` is false, estimated time to load this model into memory. Absent if `loaded` is true.

### 4.2 Serving Endpoint

The provider manifest is served at:

```
GET /oicp/v1/capabilities
```

This endpoint returns the full provider manifest as JSON. Clients poll this endpoint periodically (recommended interval: 30 seconds) to maintain a current view of provider capabilities. The endpoint MUST be lightweight — providers SHOULD cache the manifest and update it when model state changes, not recompute it on every request.

The manifest endpoint is separate from the inference endpoint. A provider can serve the manifest at `/oicp/v1/capabilities` and inference at `/v1/chat/completions` (OpenAI-compatible). The two APIs are complementary, not coupled.

---

## 5. Request-Level Requirements

Clients attach inference requirements to individual completion requests. The transport mechanism depends on the API protocol being used.

### 5.1 OpenAI-Compatible API Extension

For providers serving the OpenAI chat completions API, requirements are included in the request body under an `oicp` key:

```json
POST /v1/chat/completions
{
  "messages": [
    {"role": "user", "content": "Implement a JWT refresh token rotation in Rust using Axum"}
  ],
  "oicp": {
    "capabilities": {
      "required": {"code": 2},
      "preferred": {"code": 3, "instruction": 3}
    },
    "context": {"min_tokens": 8192},
    "performance": {"latency": "interactive"}
  }
}
```

Providers that implement OICP read the `oicp` key and use it for model selection. Providers that don't implement OICP ignore unknown keys (as the OpenAI API spec requires) and select models using their default logic. This means OICP-aware clients work with non-OICP providers — they just don't get capability-aware routing.

### 5.2 Response Metadata

Providers that support OICP SHOULD include metadata about the model that actually served the request:

```json
{
  "id": "chatcmpl-...",
  "model": "qwen3-coder-30b-q4km",
  "choices": [...],
  "oicp": {
    "model_capabilities": {
      "general": 2,
      "code": 4,
      "instruction": 3,
      "math": 3
    },
    "match_quality": "full"
  }
}
```

**`model_capabilities`** (object, optional): The capability profile of the model that served the request.

**`match_quality`** (string, optional): How well the serving model matched the client's requirements.

- `"full"` — All required thresholds met, and the best available model for the preferred capabilities was selected.
- `"partial"` — All required thresholds met, but a better model for the preferred capabilities exists and was unavailable (not loaded, insufficient capacity).
- `"degraded"` — One or more required thresholds were not met. The provider served the best available model but it falls below the client's minimum requirements. The client should treat the response with appropriate skepticism.
- `"unmatched"` — No OICP requirements were provided; the provider used default model selection.

This response metadata lets clients adapt. A client receiving `"degraded"` for a code task might add extra validation steps or fall back to a different provider. A client receiving `"full"` can trust the response as coming from an appropriate model. The metadata is advisory — clients MAY ignore it.

---

## 6. Community Capability Profiles

Model capability profiles are subjective assessments. To reduce the burden on individual providers and improve consistency, OICP encourages community-maintained capability profiles.

### 6.1 Profile Registry

A **profile registry** is a public repository of capability profiles for known models. The canonical registry is a git repository containing TOML files:

```
oicp-profiles/
├── qwen/
│   ├── qwen3-coder-30b.toml
│   ├── qwen3-30b.toml
│   └── qwen3-14b.toml
├── deepseek/
│   ├── deepseek-v3.toml
│   └── deepseek-coder-v2.toml
├── meta/
│   └── llama4-maverick.toml
└── README.md
```

Each profile file:

```toml
# oicp-profiles/qwen/qwen3-coder-30b.toml

[model]
name = "Qwen3 Coder 30B"
family = "qwen3"
parameters = "30B"
repo = "Qwen/Qwen3-Coder-30B-GGUF"

[capabilities]
general = 2
code = 4
instruction = 3
math = 3
analysis = 2
creative = 1
multilingual = 2

# Optional: notes explaining the ratings.
[notes]
code = "Top-tier on HumanEval, SWE-Bench. Strong across Python, Rust, TypeScript, Go."
creative = "Can generate creative text but with limited stylistic range. Not its purpose."
general = "Reasonable common-sense reasoning but optimized for code at the expense of breadth."
```

### 6.2 Profile Governance

The profile registry accepts community contributions via pull requests. To maintain quality:

- Ratings are supported by brief justifications (the `[notes]` section).
- Controversial ratings (where experienced users disagree by ≥ 2 levels) are noted as disputed, with both positions documented.
- Profiles are versioned — when a model is updated (e.g., a fine-tune improves its reasoning), the profile is updated with a changelog.
- The registry is descriptive, not prescriptive. Providers may use registry profiles as defaults and override them based on their own evaluation. A provider who believes Qwen3-Coder-30B deserves `code: 3` instead of `code: 4` is free to advertise that.

### 6.3 Provider Usage

Providers can reference registry profiles rather than manually rating every model:

```toml
# In Commonwealth's mesh config
[[mesh.models.available]]
repo = "Qwen/Qwen3-Coder-30B-GGUF"
quant = "Q4_K_M"
oicp_profile = "qwen/qwen3-coder-30b"  # Pulls ratings from the registry

# Optional: override specific ratings
[mesh.models.available.capability_overrides]
code = 3  # "We find it's not quite top-tier at Q4 quantization"
```

This keeps the common case easy — most providers will use the community profiles as-is — while allowing per-deployment overrides for quantization effects, fine-tuned variants, or honest disagreement with the community ratings.

---

## 7. Versioning

OICP follows semantic versioning:

- **Patch** (0.1.x): Clarifications, typo fixes, examples. No schema changes.
- **Minor** (0.x.0): New capabilities added to the vocabulary. New optional fields added to schemas. All changes are backward-compatible — old clients and providers continue to work.
- **Major** (x.0.0): Breaking changes to schemas or semantics. Should be extremely rare. The goal is for OICP 1.0.0 to be the last major version for many years.

Implementations declare which version they support via `oicp_version`. A provider implementing 0.2.0 will receive requests from clients implementing 0.1.0 — the provider simply ignores the client's older vocabulary and the client ignores any new capabilities in the provider's manifest. Version negotiation is unnecessary because the ignorance-safety principle (Section 1) ensures cross-version compatibility.

---

## 8. Reference Implementation Notes

### For Clients (e.g., Sovereign)

The minimum viable OICP client:

1. Include an `oicp` key in completion requests with `preferred` capabilities.
2. Optionally poll `/oicp/v1/capabilities` to discover available models.
3. Ignore the response's `oicp` metadata if you don't need it.

A skill system maps naturally to OICP requirements: each skill declares its capability preferences in its manifest, and the client's executor attaches them to outgoing requests.

### For Providers (e.g., Commonwealth)

The minimum viable OICP provider:

1. Serve `/oicp/v1/capabilities` with model capability profiles.
2. Read the `oicp` key from incoming completion requests.
3. Use `required` capabilities as a filter and `preferred` capabilities for scoring.
4. Include `oicp` metadata in responses.

A mesh scheduler factors OICP requirements into model selection alongside its own constraints (VRAM, latency, topology). The OICP score is one input to the scheduling decision, not the only input.

### For Neither

OICP does not specify:

- How models are loaded, sharded, or managed.
- How providers discover each other or coordinate.
- How clients plan, route, or orchestrate tasks.
- How capability profiles are benchmarked or validated.
- Any authentication, authorization, or payment mechanism.

These are the domain of the projects that implement OICP, not of the protocol itself. OICP is the language they speak at the boundary. Everything else is their own business.