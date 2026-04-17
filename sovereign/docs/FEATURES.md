# Features

Deep-dive on the behaviour users interact with beyond the three-command quick start. For first-time-user docs, see the [README](../README.md).

← [back to README](../README.md)

## Dual-model routing

A small, fast model handles simple questions in under a second. Complex requests route to a larger primary model that loads on demand and auto-unloads after 60 seconds of idle. `sovereign setup` installs both slots; the embedding slot rounds out the set for RAG.

## Multi-step task execution

Complex requests decompose into step DAGs. The executor runs steps in parallel where possible, handles branching, retries tool failures with backoff, and replans if a step fails.

Steps can be configured with:
- **Best-of-N sampling** — Generate multiple candidates and select the best via LLM judge, majority vote, or tool verification.
- **Evaluation passes** — Closed-loop self-correction that checks output quality and retries with feedback.
- **Adaptive compute** — Difficulty estimation adjusts token budgets, sampling, and evaluation per step.

## Tools

| Tool | Description |
|---|---|
| **Search** | Searches local knowledge bases + optional web. FTS5 + coverage assessment |
| **Web Fetch** | Downloads and extracts content from web pages |
| **Shell** | Runs commands on your machine (requires approval) |
| **Document** | Ingests and summarizes local documents (RAG) |

Tools retry on transient failures (timeout, rate limit) with configurable backoff.

## Memory

Working memory is compressed every message. Long-term memories are extracted when conversations end and retrieved via full-text search in future conversations. Skills can configure per-skill decay rates and prune thresholds.

## Response provenance

Every response carries structured metadata about how it was produced:
- Which intent the router classified
- What knowledge bases were searched and how many chunks matched
- Which inference backend generated the response
- OICP capability match quality
- Token count and latency

The desktop app shows this as a collapsible provenance bar on each message. This helps users understand why an answer might be incomplete ("search found 0 chunks in SEP") and report problems effectively.

## Skills

Skills are TOML files that configure routing, planning, synthesis prompts, memory rules, and inference requirements.

**Bundled skills** (in `skills/`):

| Skill | Description |
|---|---|
| **Research & Analysis** | Multi-source research with citations. Knowledge-first planning. 5%/month memory decay |
| **Code Review** | Structured code analysis. Privacy: local-only |
| **Personal Assistant** | Task management and organization |
| **Inner Work** | Reflective companion for personal psychological work. Always local-only |

**Writing a skill:** see [DEVELOPMENT.md — Adding a skill](DEVELOPMENT.md#adding-a-skill) for the full TOML schema.

Skills carry trust metadata: `signature` and `signed_by` fields enable distinguishing community-reviewed, author-signed, and unsigned skills.

## OICP — Open Inference Capabilities Protocol

Skills declare capability requirements (code analysis proficiency, minimum context window, privacy constraints). When multiple backends are available, Sovereign routes to the best match. The `inner-work` skill declares `privacy = "local_only"` — its data never leaves your machine even if remote backends are configured.

See [`docs/specs/oicp.md`](specs/oicp.md) for the full protocol specification.
