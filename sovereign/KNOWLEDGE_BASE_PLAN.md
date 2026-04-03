# Knowledge Base: Implementation Plan

The local knowledge base transforms Sovereign from "a chat interface to a local model" into "local AI reasoning over curated knowledge, privately, with memory, for free." The knowledge base is the product. Web search is the premium add-on.

This plan builds on existing infrastructure: the RAG pipeline (document chunking, embedding, FTS5), the `dyn StateStore` (same `store_chunks` / `search_documents` methods), and the `web_search` tool (which becomes the web fallback tier). No new search index format. No new storage abstraction. The corpus is a large collection of documents with a `source_type` tag flowing through the existing pipeline.

**Full specification:** [KNOWLEDGE_BASE_FEAT.md](KNOWLEDGE_BASE_FEAT.md)

---

## Dependency Graph

```
Phase 1: Types + Schema Foundation
  │
Phase 2: Corpus Registry + Parsers
  │
Phase 3: Corpus Manager (download, install, update)
  │
Phase 4: Coverage-Aware Search Pipeline
  │
Phase 5: Setup Wizard + Desktop UI
  │
Phase 6: Memory Tuning + Polish
```

Phases are sequential — each builds on the previous. Phase 1 is the only one that touches core types; the rest are additive. Estimated total: 2-3 weeks.

---

## Existing Infrastructure (reused, not rebuilt)

| Component | Location | How it's used |
|---|---|---|
| `DocumentChunk` struct | `sovereign-core/src/types.rs:350` | Extended with `source_type` field |
| RAG chunking | `sovereign-tools/src/rag/chunk.rs` | Same chunking strategy for corpus content |
| RAG ingestion | `sovereign-tools/src/rag/ingest.rs` | Pattern for corpus ingestion pipeline |
| `StateStore::store_chunks()` | `sovereign-core/src/traits.rs:109` | Stores corpus chunks alongside user documents |
| `StateStore::search_documents()` | `sovereign-core/src/traits.rs:110` | Searches corpus + user docs + web results uniformly |
| FTS5 `documents_fts` | `sovereign-store/src/migrations.rs` | Text search across all document types |
| `KnowledgeTool` | `sovereign-tools/src/knowledge.rs` | Becomes the local search tier in the pipeline |
| `WebSearchTool` | `sovereign-tools/src/web/mod.rs` | Becomes the web fallback tier |
| `InferenceProvider::embed()` | `sovereign-core/src/traits.rs:18` | Embeds corpus chunks for vector search |

---

## Phase 1: Types + Schema Foundation

**Delivers:** The type system and database schema that all subsequent phases build on. No behavioral changes yet — existing tests pass, existing tools work unchanged.

### Files Modified

- `crates/sovereign-core/src/types.rs` — Add `SourceType` enum, `SearchMethod` enum, `SourceOrigin` enum, `CoverageDecision` enum, `SearchBudget` struct, `CoverageAssessment` struct. Add `source_type` field to `DocumentChunk` (default `UserDocument` for backward compatibility).
- `crates/sovereign-store/src/migrations.rs` — Add `corpus_state` table, add `source_type TEXT DEFAULT 'user'` and `corpus_id TEXT` columns to `documents` table.
- `crates/sovereign-store/src/sqlite.rs` — Update `store_chunks()` and `search_documents()` to read/write `source_type` and `corpus_id`. Add `corpus_state` CRUD methods.
- `crates/sovereign-store/src/postgres.rs` — Same changes for Postgres.
- `crates/sovereign-store/src/memory.rs` — Same changes for InMemoryStateStore.
- `crates/sovereign-core/src/traits.rs` — Add `StateStore` methods: `save_corpus_state()`, `get_corpus_state()`, `list_corpus_states()`, `delete_corpus_state()`, `get_search_budget()`, `update_search_budget()`.
- `crates/sovereign-core/tests/core_tests.rs` — Update `MockStore` with new method stubs.

### New Types

```rust
pub enum SourceType {
    UserDocument,
    Corpus { corpus_id: String },
    WebSearch { url: String },
}

pub enum SearchMethod {
    LocalOnly,
    LocalPlusWeb { reason: String },
    LocalOnlyIncomplete { reason: String },
    WebOnly { reason: String },
    NoResults { reason: String },
}

pub enum CoverageDecision {
    Sufficient,
    SupplementWithWeb { reason: String },
    RequiresWeb { reason: String },
}

pub enum SourceOrigin {
    Local { corpus: String, article_title: String },
    Web { url: String, domain: String },
    UserDocument { filename: String },
}

pub struct SearchBudget {
    pub backend: String,
    pub monthly_limit: u32,
    pub used_this_month: u32,
    pub reset_date: i64,
}

pub struct CorpusState {
    pub corpus_id: String,
    pub installed_at: i64,
    pub source_date: String,
    pub chunks_count: i64,
    pub index_size_mb: i64,
    pub last_updated: i64,
}
```

### Schema Addition

```sql
CREATE TABLE IF NOT EXISTS corpus_state (
    corpus_id       TEXT PRIMARY KEY,
    installed_at    INTEGER,
    source_date     TEXT,
    chunks_count    INTEGER,
    index_size_mb   INTEGER,
    last_updated    INTEGER
);

CREATE TABLE IF NOT EXISTS search_budget (
    backend         TEXT PRIMARY KEY,
    monthly_limit   INTEGER NOT NULL,
    used_this_month INTEGER NOT NULL DEFAULT 0,
    reset_date      INTEGER NOT NULL
);

-- Add to existing documents table:
-- (handled via ALTER TABLE for existing databases, or in CREATE for new)
ALTER TABLE documents ADD COLUMN source_type TEXT DEFAULT 'user';
ALTER TABLE documents ADD COLUMN corpus_id TEXT;
```

### Verification

1. All existing tests pass unchanged (new fields have defaults).
2. `DocumentChunk` with `source_type: SourceType::Corpus { corpus_id: "wikipedia".into() }` serializes and deserializes correctly.
3. `store_chunks()` with corpus-tagged chunks stores `source_type` and `corpus_id`.
4. `search_documents()` returns chunks with `source_type` populated.
5. `corpus_state` CRUD works in SQLite and in-memory stores.

---

## Phase 2: Corpus Registry + Parsers

**Delivers:** The corpus manifest and parsers that turn raw corpus dumps into `DocumentChunk` streams. No downloads yet — parsers work on local files for testing.

### Files Created

- `data/corpora.toml` — Corpus manifest (shipped with Sovereign, not compiled in).
- `crates/sovereign-tools/src/corpus/mod.rs` — Module root, `CorpusParser` trait.
- `crates/sovereign-tools/src/corpus/registry.rs` — `CorpusRegistry` loads and queries `corpora.toml`.
- `crates/sovereign-tools/src/corpus/wikipedia.rs` — `WikimediaDumpParser`: MediaWiki XML → chunks.
- `crates/sovereign-tools/src/corpus/stackexchange.rs` — `StackExchangeParser`: SE data dump XML → chunks, filtered by score.
- `crates/sovereign-tools/src/corpus/html_crawl.rs` — `HtmlCrawlParser`: polite crawl of HTML sites (SEP, CRS).
- `crates/sovereign-tools/src/corpus/gutenberg.rs` — `GutenbergParser`: Gutenberg catalog + text files → chunks.
- `crates/sovereign-tools/src/corpus/openalex.rs` — `OpenAlexParser`: JSONL abstracts → chunks.

### Files Modified

- `crates/sovereign-tools/src/lib.rs` — Add `pub mod corpus;`.
- `crates/sovereign-tools/Cargo.toml` — Add `quick-xml` (for MediaWiki/SE parsing), `flate2`/`bzip2` (decompression).

### CorpusParser Trait

```rust
pub trait CorpusParser: Send + Sync {
    fn parse(&self, source_path: &Path) -> Result<Box<dyn Iterator<Item = Result<DocumentChunk>>>>;
}
```

All parsers:
- Split at paragraph boundaries, max 512 tokens, 64-token overlap (reuse `sovereign-tools/src/rag/chunk.rs`).
- Set `source_type: SourceType::Corpus { corpus_id }`.
- Preserve metadata: article title, URL, section heading in the chunk content.
- Stream results — never load the entire corpus into memory.

### Verification

1. `WikimediaDumpParser` parses a 100-article test dump into chunks with correct `source_type` and metadata.
2. `StackExchangeParser` filters to `score >= 3` and produces question-answer paired chunks.
3. `HtmlCrawlParser` respects robots.txt and rate limits (1 req/sec).
4. `CorpusRegistry` loads `data/corpora.toml` and returns tier definitions.
5. All parsers produce chunks compatible with `StateStore::store_chunks()`.

---

## Phase 3: Corpus Manager

**Delivers:** Download, install, update, and remove corpora. Background progress reporting for the UI.

### Files Created

- `crates/sovereign-tools/src/corpus/manager.rs` — `CorpusManager` struct: `install_corpus()`, `remove_corpus()`, `update_corpus()`, `installed()`, `check_for_updates()`.

### Files Modified

- `crates/sovereign-tools/src/corpus/mod.rs` — Export `CorpusManager`.

### CorpusManager

```rust
pub struct CorpusManager {
    registry: CorpusRegistry,
    store: Arc<dyn StateStore>,
    inference: Arc<dyn InferenceProvider>,
    data_dir: PathBuf,
}
```

- `install_corpus(id, progress_callback)`: Download source file (with HTTP resume via `Range` header for large files). Parse with the appropriate `CorpusParser`. Embed each chunk. Store via `store_chunks()`. Record in `corpus_state` table. Progress callback receives `CorpusProgress { phase, percent, chunks_processed, chunks_total }`.
- `remove_corpus(id)`: Delete chunks with matching `corpus_id` from documents table. Delete `corpus_state` row.
- `update_corpus(id)`: Compare `source_date` against remote. For Wikipedia: re-index only pages modified since last dump date. For others: full re-index.
- `check_for_updates()`: Called weekly by background task. Returns `Vec<CorpusUpdate>` for corpora with newer versions available.

### Download Strategy

Large corpus files (Wikipedia: 22GB compressed) need resume support:
- Use `reqwest` with `Range` header to resume interrupted downloads.
- Write to `{data_dir}/downloads/{corpus_id}.part`, rename on completion.
- Progress callback updates UI every 1MB.

### Verification

1. `install_corpus("wikipedia", ...)` with a test dump downloads, parses, embeds, and stores chunks.
2. `installed()` returns the corpus with correct chunk count and index size.
3. `remove_corpus("wikipedia")` deletes all chunks and state.
4. `install_corpus` with a pre-existing `.part` file resumes the download.
5. Progress callback fires at regular intervals during install.

---

## Phase 4: Coverage-Aware Search Pipeline

**Delivers:** The core behavioral change. The `web_search` tool becomes `search` — a unified tool that tries local corpus first, assesses coverage, and falls back to web only when needed.

### Files Created

- `crates/sovereign-tools/src/search.rs` — `SearchTool` struct: the unified search pipeline with query analysis, local search, coverage assessment, web fallback, merge-and-rerank, and synthesis.

### Files Modified

- `crates/sovereign-tools/src/web/mod.rs` — `WebSearchTool` becomes internal to the search module (web fallback tier, not a standalone tool).
- `crates/sovereign-tools/src/lib.rs` — Add `pub mod search;`.
- `crates/sovereign-core/src/types.rs` — Add `ExtractedSource` struct with `SourceOrigin`.
- `crates/sovereign-tools/src/knowledge.rs` — Refactored into the local search tier. May be merged into `search.rs` or kept as a helper.
- Server + CLI + Desktop `main.rs` — Register `SearchTool` instead of separate `WebSearchTool` + `KnowledgeTool`.

### Pipeline

```
User query
  → Stage 1: Query Analysis (Fast model)
      → intent, sub_queries, recency_need, needs_current_events
  → Stage 2: Local Corpus Search
      → embed query → vector search + FTS5 across corpus chunks
      → returns Vec<ScoredChunk> with source metadata
  → Stage 3: Coverage Assessment (Fast model)
      → Sufficient → synthesize from local sources only
      → SupplementWithWeb → targeted web queries for the gap
      → RequiresWeb → full web search
  → Stage 4: Synthesis (Primary model)
      → cited answer with SourceOrigin tags
      → SearchMethod logged to conversation metadata
```

### Coverage Assessment

The Fast model reads the top 5 local results and decides:
- **Sufficient**: Local results clearly answer the question (score > 0.85, comprehensive source).
- **SupplementWithWeb**: Local results cover the topic but miss a specific aspect. Generates targeted web queries for the gap only.
- **RequiresWeb**: No relevant local results, or query needs current events.

Short-circuits (no LLM call needed):
- `needs_current_events = true` → always `SupplementWithWeb` or `RequiresWeb`.
- No local results at all → always `RequiresWeb`.
- Top result score > 0.85 from Wikipedia/SEP → strong prior toward `Sufficient`.

### Budget Integration

`SearchBudget` is checked before web fallback:
- `BudgetPressure::Low` (>33% remaining): normal behavior.
- `BudgetPressure::Medium` (10-33%): only supplement for clear gaps, not ambiguous coverage.
- `BudgetPressure::High` (<10%): only use web when local coverage is clearly insufficient.
- Exhausted: no web search, tell user credits are spent.

### Tool Descriptor Change

```rust
ToolDescriptor {
    id: "search".to_string(),
    name: "Search".to_string(),
    description: "Search across local knowledge bases (Wikipedia, scholarly abstracts, \
                  encyclopedias, expert Q&A) and optionally the web. Local knowledge \
                  bases are always available. Web search requires an API key.".to_string(),
    parameters: /* same as current web_search */,
}
```

### Verification

1. Query a topic covered by installed corpus → `SearchMethod::LocalOnly`, cited synthesis from corpus sources.
2. Query a topic NOT in corpus → `SearchMethod::RequiresWeb` (or `NoResults` if no web backend).
3. Query a topic partially covered → `SearchMethod::LocalPlusWeb` with targeted web queries for the gap.
4. `SourceOrigin` tags propagate correctly into synthesis prompt and response citations.
5. Budget tracking: web search decrements `used_this_month`. Budget pressure changes coverage assessment behavior.
6. Query with `needs_current_events = true` → short-circuits to web without checking local corpus.

---

## Phase 5: Setup Wizard + Desktop UI

**Delivers:** Corpus tier selection in the setup wizard, background download with progress, knowledge base status in settings, source attribution in chat.

### Files Created

- `crates/sovereign-desktop/src/lib/setup/KnowledgeBaseSetup.svelte` — Corpus tier selection step.
- `crates/sovereign-desktop/src/lib/setup/WebSearchSetup.svelte` — Optional web search API key step.
- `crates/sovereign-desktop/src/lib/components/KnowledgeStatus.svelte` — Knowledge base status panel in settings.
- `crates/sovereign-desktop/src/lib/components/SourceAttribution.svelte` — Source tags on messages.

### Files Modified

- `crates/sovereign-desktop/src/lib/setup/SetupWizard.svelte` — Add corpus and web search steps between persona selection and completion.
- `crates/sovereign-desktop/src/lib/components/SettingsPanel.svelte` — Embed `KnowledgeStatus`.
- `crates/sovereign-desktop/src/lib/components/MessageBubble.svelte` — Show `SourceAttribution` when sources present.
- `crates/sovereign-desktop/src-tauri/src/commands.rs` — Add `install_corpus`, `remove_corpus`, `list_corpora`, `get_corpus_progress` commands.
- `crates/sovereign-desktop/src-tauri/src/state.rs` — Add `CorpusManager` to `AppState`.

### Setup Wizard Flow

```
Step 1: Persona selection (existing)
Step 2: Model selection (existing)
Step 3: Knowledge base tier selection (NEW)
         → Essential / Research / Technical / Full
         → Shows disk space required, available space
         → Pre-checks recommended tier for persona
Step 4: Web search API key (NEW, optional)
         → Frames as supplementary, not required
         → "Offline knowledge is enough for now"
Step 5: Complete → background downloads begin
```

Downloads happen asynchronously after setup completes. The user can start chatting immediately. Knowledge bases become available as they finish indexing. Tauri events report progress.

### Settings Panel

```
Knowledge Bases
───────────────────────────────────────
Wikipedia          6.8M articles    ✓ Indexed
OpenAlex           Installing...    ████░░ 62%
Stack Exchange     Not installed    [Add]

Web Search
───────────────────────────────────────
Tavily             847 / 1,000 credits remaining
                   Resets April 1
```

### Verification

1. Setup wizard shows corpus tier step after model selection.
2. Research persona pre-selects Research tier.
3. Background download shows progress bar in settings.
4. Completed corpus appears as "Indexed" in settings.
5. Message bubbles show source attribution ("Sources: Wikipedia (2), OpenAlex (1)").
6. Web search budget shown in settings with remaining credits.

---

## Phase 6: Memory Tuning + Polish

**Delivers:** Skill-level memory decay overrides, updated planner templates for knowledge-first planning, and the first-conversation greeting.

### Files Modified

- `crates/sovereign-core/src/memory.rs` — Support per-skill decay rate and prune threshold overrides from `SkillInferenceConfig`.
- `crates/sovereign-core/src/skills.rs` — Add `confidence_decay_per_month` and `prune_threshold` to `MemoryToml` / `Skill`.
- `skills/research-analyst/skill.toml` — Update templates for knowledge-first planning (`literature_survey`, `concept_explanation`, `methodology_help`). Add memory decay overrides (5%/month, prune at 0.1).
- `crates/sovereign-core/src/executor.rs` — When interpolating search results into synthesis prompts, include `SearchMethod` source provenance note.
- `crates/sovereign-desktop/src/lib/components/ChatView.svelte` — Research persona greeting mentions available knowledge bases.

### Research Persona Templates

```toml
[[planner.templates]]
name = "literature_survey"
trigger = "User wants to know what has been published on a topic"
steps = """
1. Search local knowledge bases for the topic, focusing on
   OpenAlex abstracts and Wikipedia context.
2. Assess coverage: does the local corpus show the landscape?
3. If sufficient, synthesize a survey with citations.
4. If gaps exist, supplement with web search if available.
5. Present: key authors, central debates, methods, open questions.
"""

[[planner.templates]]
name = "concept_explanation"
trigger = "User wants to understand a concept, theory, or framework"
steps = """
1. Search local knowledge bases, prioritizing Stanford
   Encyclopedia of Philosophy and Wikipedia.
2. Synthesize: what it is, who originated it, related concepts,
   main critiques.
3. Web search only if concept is very recent (post-2024).
"""
```

### Verification

1. Research-analyst memories decay at 5%/month (not default 10%).
2. Planner generates knowledge-first plans with `search` tool steps.
3. Synthesis prompt includes source provenance note based on `SearchMethod`.
4. Research persona greeting mentions available knowledge bases by name and count.

---

## Risk Summary

| Phase | Risk | Mitigation |
|---|---|---|
| **1** (Types) | Schema migration on existing databases with data | Use `ALTER TABLE ... ADD COLUMN` with defaults; test migration on populated DB |
| **2** (Parsers) | Wikipedia XML dump is 22GB compressed — parser must stream | Use `quick-xml` streaming reader, never load full document into memory |
| **2** (Parsers) | HTML crawl (SEP, CRS) may break on site changes | Defensive parsing, graceful skip on parse failure, log warnings |
| **3** (Manager) | 22GB+ downloads fail mid-transfer | HTTP Range resume; `.part` file survives restarts |
| **3** (Manager) | Embedding 6.8M articles takes days on CPU | Skip embeddings for corpus chunks; rely on FTS5 text search (already proven for documents/messages). Add vector embeddings as optional enhancement later. |
| **4** (Pipeline) | Small models unreliable at coverage assessment | Heuristic short-circuits for obvious cases (current events → web, high-score local → sufficient). LLM only handles the gray zone. |
| **4** (Pipeline) | Web search budget exhaustion degrades experience | Clear messaging: "credits used up, answering from local knowledge." Never silently fail. |
| **5** (UI) | 55GB+ indexed corpus fills user's disk | Show available disk space in wizard. Warn if insufficient. Don't auto-install Full tier. |

---

## Corpus Priority Order

Index these in this order. Each is independently useful:

1. **Wikipedia** (55GB indexed) — Ship first. Broadest coverage. Makes Sovereign meaningfully better than a bare local model for factual queries.
2. **OpenAlex abstracts** (45GB) — The killer corpus for researchers. "What has been published on X?" returns titles, authors, abstracts, citations.
3. **Stanford Encyclopedia of Philosophy** (0.5GB) — Tiny download, enormous value per byte. Deep, peer-reviewed explanations.
4. **Stack Exchange** (40GB, filtered) — Expert Q&A across 170+ communities.
5. **OpenStax textbooks** (~2GB) — Free CC-licensed textbooks for when users need to learn outside their field.
6. **Congressional Research Service** (4GB) — Nonpartisan US policy analysis.
7. **Project Gutenberg** (25GB) — Primary texts. Essential for humanities, optional for everyone else.

---

## The Core Principle

The knowledge base is not a fallback because web search costs money. The knowledge base is the product. News and current events are the premium add-on. Knowledge and intelligence are the free tier.

Every design decision flows from this: the tool is called "Search" not "Web Search." The setup wizard shows knowledge bases before web search. The coverage assessment spends web credits only when local coverage is genuinely insufficient. The user sees "Sources: Wikipedia (3)" and trusts that the answer comes from verified sources, not hallucination.
