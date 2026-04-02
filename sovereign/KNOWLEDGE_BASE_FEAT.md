# Task: Local Knowledge Base as Tier 0 Search

**Priority:** High — this is the free-tier search experience. Without it, Sovereign has no search capability unless the user provides an API key.
**Crates:** `sovereign-core` (types), `sovereign-tools` (pipeline), `sovereign-store` (corpus storage), `sovereign-desktop` (setup wizard)
**Estimated effort:** 2-3 weeks for one engineer. The pipeline logic is the hard part. Corpus ingestion is mechanical.
**Dependencies:** The RAG pipeline (document chunking, embedding, FTS5) must already be functional. This task builds on it, not beside it.

---

## Context

Sovereign currently has a RAG pipeline for user-uploaded documents and a `web_search` tool that requires a paid API key (Tavily, Brave). There is no middle ground — users without an API key have no search capability beyond the model's parametric knowledge, which is unreliable for facts, dates, and specific claims.

This task introduces a **local knowledge base**: a curated corpus of freely available, high-quality reference sources that Sovereign indexes locally using the existing RAG infrastructure. It requires no API key, no network connection, and no ongoing cost. It becomes the first source the search pipeline consults for every query, with web search as a fallback for what the local corpus can't cover.

This is not a separate subsystem. It flows through the existing `dyn StateStore` (same `store_chunks` / `search_documents` methods), the existing Embed slot (same embedding model), and the existing `knowledge` tool (same retrieval interface). The new pieces are: corpus acquisition, a coverage assessment step, and a search pipeline that tries local first and falls back to web.

---

## Part 1: Corpus Definition and Acquisition

### 1.1 Corpus Registry

Define the available corpora as a manifest. This is a data file shipped with Sovereign, not compiled into the binary.

Create `data/corpora.toml`:

```toml
[meta]
version = "0.1.0"
# Corpora are grouped into tiers for the setup wizard.
# Each corpus can belong to multiple tiers.

# ─── Individual Corpora ──────────────────────────────────

[corpus.wikipedia]
name = "Wikipedia"
description = "6.8M English articles. General knowledge, history, science, biographies."
source = "https://dumps.wikimedia.org/enwiki/latest/enwiki-latest-pages-articles.xml.bz2"
format = "wikimedia_dump"
size_compressed_gb = 22
size_indexed_gb = 55     # After chunking + embeddings
update_frequency = "biweekly"
license = "CC-BY-SA-4.0"
tiers = ["essential", "research", "technical", "full"]

[corpus.sep]
name = "Stanford Encyclopedia of Philosophy"
description = "Peer-reviewed articles on philosophical concepts, thinkers, and debates."
source = "https://plato.stanford.edu/"  # Requires polite crawling; no bulk dump
format = "html_crawl"
size_compressed_gb = 0.2
size_indexed_gb = 0.5
update_frequency = "quarterly"
license = "CC-BY-NC-ND-4.0"
tiers = ["research", "full"]
notes = "License restricts commercial use. Sovereign is not commercial. Users distributing Sovereign commercially should exclude this corpus."

[corpus.stackexchange]
name = "Stack Exchange"
description = "Q&A across programming, statistics, science, and 170+ topics."
source = "https://archive.org/details/stackexchange"
format = "stackexchange_dump"
size_compressed_gb = 85
size_indexed_gb = 40    # After filtering to top-voted answers only
update_frequency = "quarterly"
license = "CC-BY-SA-4.0"
tiers = ["technical", "full"]
filter = "score >= 3"   # Only index answers with 3+ upvotes

[corpus.gutenberg]
name = "Project Gutenberg"
description = "70,000+ public domain books. Literature, history, philosophy, science."
source = "https://www.gutenberg.org/robot/harvest"
format = "gutenberg_catalog"
size_compressed_gb = 15
size_indexed_gb = 25
update_frequency = "monthly"
license = "Public Domain"
tiers = ["full"]

[corpus.openalex]
name = "OpenAlex Abstracts"
description = "Titles, abstracts, and citations for 250M+ scholarly works."
source = "https://openalex.org/data-dump"
format = "openaccess_jsonl"
size_compressed_gb = 30
size_indexed_gb = 45     # Titles + abstracts only, last 15 years
update_frequency = "monthly"
license = "CC0"
tiers = ["research", "full"]
filter = "year >= 2010, has_abstract = true"

[corpus.crs_reports]
name = "Congressional Research Service Reports"
description = "Non-partisan policy analysis from the US Congress."
source = "https://www.everycrsreport.com/"
format = "html_crawl"
size_compressed_gb = 2
size_indexed_gb = 4
update_frequency = "monthly"
license = "Public Domain (US Government)"
tiers = ["research", "full"]

# ─── Tier Definitions ────────────────────────────────────

[tier.essential]
name = "Essential"
description = "General knowledge and facts. Works offline."
corpora = ["wikipedia"]
total_indexed_gb = 55

[tier.research]
name = "Research"
description = "Academic concepts, scholarly discovery, policy analysis."
corpora = ["wikipedia", "sep", "openalex", "crs_reports"]
total_indexed_gb = 105

[tier.technical]
name = "Technical"
description = "Programming, statistics, engineering Q&A."
corpora = ["wikipedia", "stackexchange"]
total_indexed_gb = 95

[tier.full]
name = "Full"
description = "All available knowledge bases."
corpora = ["wikipedia", "sep", "stackexchange", "gutenberg", "openalex", "crs_reports"]
total_indexed_gb = 170
```

### 1.2 Corpus Ingestion Pipeline

Each corpus format needs a parser that produces `DocumentChunk` structs compatible with the existing RAG pipeline. Create `sovereign-tools/src/corpus/` with one module per format:

```rust
/// All corpus parsers produce the same output: a stream of DocumentChunks
/// that flow into `dyn StateStore::store_chunks()`.
pub trait CorpusParser: Send + Sync {
    /// Parse the corpus source into chunks. Streaming — yields chunks
    /// as they're parsed, doesn't load the entire corpus into memory.
    fn parse(&self, source_path: &Path) -> Result<Box<dyn Iterator<Item = Result<DocumentChunk>>>>;
}

pub struct WikimediaDumpParser;     // Parses MediaWiki XML dump
pub struct StackExchangeParser;      // Parses SE data dump XML, filters by score
pub struct HtmlCrawlParser;          // Politely crawls HTML sites (SEP, CRS)
pub struct GutenbergParser;          // Parses Gutenberg catalog and text files
pub struct OpenAlexParser;           // Parses JSONL abstracts dump
```

Each parser:
- Splits content into chunks at paragraph boundaries, max 512 tokens, 64-token overlap (same parameters as user document RAG).
- Preserves source metadata: corpus name, article title, URL, date, section heading.
- Tags each chunk with `source_type: CorpusChunk` to distinguish it from user-uploaded documents in the StateStore.

```rust
pub struct DocumentChunk {
    // ... existing fields ...

    /// Distinguishes corpus chunks from user documents.
    /// The search pipeline uses this to know which results
    /// came from the local corpus vs user documents vs web.
    pub source_type: SourceType,
}

pub enum SourceType {
    UserDocument,
    Corpus { corpus_id: String },  // "wikipedia", "sep", etc.
    WebSearch { url: String },
}
```

### 1.3 Corpus Manager

```rust
pub struct CorpusManager {
    registry: CorpusRegistry,       // Parsed from corpora.toml
    store: Arc<dyn StateStore>,
    inference: Arc<dyn InferenceProvider>,  // For embedding
    data_dir: PathBuf,              // Where downloaded corpus files live
}

impl CorpusManager {
    /// Download and index a corpus. Called during setup or on-demand.
    /// Shows progress via a callback (for the UI progress bar).
    pub async fn install_corpus(
        &self,
        corpus_id: &str,
        progress: impl Fn(CorpusProgress),
    ) -> Result<()> {
        // 1. Download source file (with resume support for large files).
        // 2. Parse with the appropriate CorpusParser.
        // 3. Embed each chunk via self.inference.embed().
        // 4. Store via self.store.store_chunks().
        // 5. Record installation state in corpus_state table.
    }

    /// Check for corpus updates and apply them.
    pub async fn update_corpus(&self, corpus_id: &str) -> Result<UpdateResult> {
        // Compare local version against remote.
        // For Wikipedia: check dump date vs last indexed date.
        // Download diff if available, otherwise full re-index.
    }

    /// Remove a corpus and its chunks from the store.
    pub async fn remove_corpus(&self, corpus_id: &str) -> Result<()>;

    /// List installed corpora and their status.
    pub fn installed(&self) -> Vec<CorpusStatus>;
}
```

### 1.4 StateStore Schema Addition

```sql
-- Track which corpora are installed and their state.
CREATE TABLE corpus_state (
    corpus_id       TEXT PRIMARY KEY,
    installed_at    INTEGER,
    source_date     TEXT,       -- Date of the source dump
    chunks_count    INTEGER,
    index_size_mb   INTEGER,
    last_updated    INTEGER
);

-- Add source_type to document chunks for filtering.
-- (If not already present — may require a migration.)
ALTER TABLE documents ADD COLUMN source_type TEXT DEFAULT 'user';
ALTER TABLE documents ADD COLUMN corpus_id TEXT;
```

---

## Part 2: Coverage-Aware Search Pipeline

This is the core behavioral change. The `web_search` tool currently goes straight to an external API. The new pipeline tries the local corpus first and only falls back to web search when local coverage is insufficient.

### 2.1 Rename and Restructure the Search Tool

The existing `web_search` tool becomes `search` — a unified tool that orchestrates local and web search. The name change matters for the user experience: the tool's descriptor says "Search" not "Web Search," because it searches the local corpus first.

```rust
pub struct SearchTool {
    corpus_manager: Arc<CorpusManager>,
    store: Arc<dyn StateStore>,
    inference: Arc<dyn InferenceProvider>,
    web_backend: Option<SearchBackend>,  // None if no API key configured
    domain_reputation: DomainReputation,
}
```

### 2.2 The Pipeline

```rust
impl SearchTool {
    async fn execute_search(
        &self,
        query: &str,
        skill_config: &ToolPreferences,
    ) -> Result<SearchResult> {

        // ── Stage 1: Query Understanding ──────────────────────
        let analysis = self.analyze_query(query).await?;
        // Returns: QueryAnalysis { intent, sub_queries, recency_need,
        //          source_preference, needs_current_events: bool }

        // ── Stage 2: Local Corpus Search ──────────────────────
        let local_results = self.search_local_corpus(
            &analysis.sub_queries,
            query,
        ).await?;
        // Uses existing RAG: embed query → vector search + FTS5
        // across all installed corpus chunks.
        // Returns Vec<ScoredChunk> with source metadata.

        // ── Stage 3: Coverage Assessment ──────────────────────
        let coverage = self.assess_coverage(
            query,
            &analysis,
            &local_results,
        ).await?;

        match coverage.decision {
            CoverageDecision::Sufficient => {
                // Local corpus covers this query. No web search needed.
                Ok(SearchResult {
                    sources: self.local_to_sources(local_results),
                    search_method: SearchMethod::LocalOnly,
                })
            }

            CoverageDecision::SupplementWithWeb { reason } => {
                // Local corpus has partial coverage.
                // Web search fills the gap.
                if let Some(backend) = &self.web_backend {
                    let web_results = self.web_search(
                        &analysis,
                        &coverage.suggested_web_queries,
                        backend,
                    ).await?;

                    // Merge local and web results, deduplicate, rerank.
                    let merged = self.merge_and_rerank(
                        query, local_results, web_results
                    ).await?;

                    Ok(SearchResult {
                        sources: merged,
                        search_method: SearchMethod::LocalPlusWeb { reason },
                    })
                } else {
                    // No web backend configured. Use local results
                    // and note the gap in the response.
                    Ok(SearchResult {
                        sources: self.local_to_sources(local_results),
                        search_method: SearchMethod::LocalOnlyIncomplete { reason },
                    })
                }
            }

            CoverageDecision::RequiresWeb { reason } => {
                // Local corpus has no relevant coverage.
                if let Some(backend) = &self.web_backend {
                    let web_results = self.web_search(
                        &analysis,
                        &analysis.sub_queries,
                        backend,
                    ).await?;

                    Ok(SearchResult {
                        sources: web_results,
                        search_method: SearchMethod::WebOnly { reason },
                    })
                } else {
                    Ok(SearchResult {
                        sources: vec![],
                        search_method: SearchMethod::NoResults { reason },
                    })
                }
            }
        }
    }
}
```

### 2.3 Coverage Assessment

This is the critical intelligence in the pipeline. The Fast model reads the local results and determines whether they answer the user's question.

```rust
impl SearchTool {
    async fn assess_coverage(
        &self,
        query: &str,
        analysis: &QueryAnalysis,
        local_results: &[ScoredChunk],
    ) -> Result<CoverageAssessment> {
        // Short-circuit: if the query explicitly needs current events
        // or very recent information, local corpus is insufficient
        // by definition.
        if analysis.needs_current_events {
            return Ok(CoverageAssessment {
                decision: CoverageDecision::SupplementWithWeb {
                    reason: "Query requires current information that the local knowledge base may not have.".into(),
                },
                suggested_web_queries: analysis.sub_queries.clone(),
            });
        }

        // Short-circuit: if no local results at all, go to web.
        if local_results.is_empty() {
            return Ok(CoverageAssessment {
                decision: CoverageDecision::RequiresWeb {
                    reason: "No relevant results in local knowledge base.".into(),
                },
                suggested_web_queries: vec![],
            });
        }

        // Short-circuit: if top local result has very high relevance
        // score (>0.85) and comes from a comprehensive source
        // (Wikipedia article, SEP entry), likely sufficient.
        if local_results[0].score > 0.85 {
            // Still ask the Fast model to confirm, but bias toward Sufficient.
        }

        // General case: ask the Fast model.
        let top_results_summary: String = local_results.iter()
            .take(5)
            .map(|r| format!(
                "[{}] {}: {}",
                r.chunk.corpus_id.as_deref().unwrap_or("unknown"),
                r.chunk.title.as_deref().unwrap_or("untitled"),
                &r.chunk.content[..200.min(r.chunk.content.len())]
            ))
            .collect::<Vec<_>>()
            .join("\n\n");

        let prompt = format!(
            r#"A user asked: "{query}"

I found these results in the local knowledge base:

{top_results_summary}

Assess whether these results adequately answer the question.
Respond with exactly one JSON object:
{{
  "coverage": "full" | "partial" | "none",
  "missing": "what information the results don't cover (if partial)",
  "web_queries": ["targeted query to fill the gap (if partial)"]
}}"#
        );

        let request = CompletionRequest::new(&prompt)
            .with_speed(Speed::Fast);
        let response = self.inference.complete(&request).await?;
        let parsed: CoverageResponse = serde_json::from_str(&response.text)?;

        Ok(match parsed.coverage.as_str() {
            "full" => CoverageAssessment {
                decision: CoverageDecision::Sufficient,
                suggested_web_queries: vec![],
            },
            "partial" => CoverageAssessment {
                decision: CoverageDecision::SupplementWithWeb {
                    reason: parsed.missing.unwrap_or_default(),
                },
                suggested_web_queries: parsed.web_queries.unwrap_or_default(),
            },
            _ => CoverageAssessment {
                decision: CoverageDecision::RequiresWeb {
                    reason: parsed.missing.unwrap_or_default(),
                },
                suggested_web_queries: vec![],
            },
        })
    }
}
```

The coverage assessment also generates *targeted* web queries when supplementing. Instead of re-running the user's full question against the web, it identifies the specific gap — "the local results cover Rodrik's trilemma concept but not empirical critiques from the last 2 years" — and generates a web query that targets only the gap. This means the web search credit is spent on the one thing the local corpus can't provide, not on re-fetching information that's already available locally.

### 2.4 Source Attribution

Every source in the synthesis prompt is tagged with its origin:

```rust
pub struct ExtractedSource {
    pub title: String,
    pub content: String,
    pub origin: SourceOrigin,
    pub relevance_score: f32,
}

pub enum SourceOrigin {
    /// From the local knowledge base. Includes corpus name.
    /// Rendered as: [Wikipedia: "Industrial policy"] or [SEP: "Social Norms"]
    Local { corpus: String, article_title: String },

    /// From web search. Includes URL for citation.
    /// Rendered as: [Source: economist.com, "Rodrik's Rethinking..."]
    Web { url: String, domain: String },

    /// From user's personal documents.
    /// Rendered as: [Your document: "Notes on Rodrik.pdf"]
    UserDocument { filename: String },
}
```

The synthesis prompt presents sources with their origin clearly labeled. The model's citations in the response distinguish between "according to Wikipedia" (always-available, verifiable) and "according to a recent article in The Economist" (web-sourced, may be paywalled). The user can see the provenance of every claim.

---

## Part 3: Synthesis Integration

### 3.1 The Executor's Interaction with Search Results

The search tool returns `SearchResult` including `search_method`. The Executor interpolates this into the subsequent Reason step. The synthesis prompt should include a note about source provenance when results are mixed:

```rust
// In the Executor, when building the synthesis prompt:
let source_note = match &search_result.search_method {
    SearchMethod::LocalOnly => {
        "All sources below are from your local knowledge base. \
         For the latest developments on this topic, consider \
         enabling web search in Settings."
    }
    SearchMethod::LocalPlusWeb { reason } => {
        format!(
            "Sources below are from both your local knowledge base \
             and web search. Web sources are marked with their URL. \
             Web search was used because: {reason}"
        ).as_str()
    }
    SearchMethod::LocalOnlyIncomplete { reason } => {
        format!(
            "Sources are from your local knowledge base only. \
             Additional information may be available via web search \
             (not currently configured). Gap: {reason}"
        ).as_str()
    }
    SearchMethod::WebOnly { .. } => {
        "Sources below are from web search."
    }
    SearchMethod::NoResults { reason } => {
        format!(
            "No results were found for this query. {reason} \
             Try rephrasing or enabling web search in Settings."
        ).as_str()
    }
};
```

This transparency is important. When the system answers from Wikipedia alone, the user should know that. When the system supplemented with web search, the user should know why. When the system couldn't find anything, the user should know the gap. Never silently fail.

### 3.2 SearchMethod in the Response Metadata

The `SearchMethod` is logged to the StateStore alongside the conversation message. This enables:

- Analytics: what fraction of queries are fully served by local corpus? (Validates the "40-50% of queries" estimate.)
- Budget tracking: how many web search credits were consumed and why?
- Quality feedback: if a user re-asks a question in a way that implies the first answer was inadequate, the system can check whether the original answer was local-only and suggest web search supplementation.

---

## Part 4: Setup Wizard Integration

### 4.1 Corpus Selection in First Run

After the persona selection step (Research, Personal Assistant, Developer), add a knowledge base step:

```
"Choose your local knowledge bases:"

┌─────────────────────────────────────────────────────┐
│  Essential (22 GB download, ~55 GB indexed)         │
│  Wikipedia — general knowledge, facts, history      │
│  ☑ Recommended for all users                        │
├─────────────────────────────────────────────────────┤
│  Research (+30 GB download, ~105 GB total)           │
│  + Stanford Encyclopedia of Philosophy              │
│  + OpenAlex scholarly abstracts                     │
│  + Congressional Research Service reports           │
│  ☑ Recommended for Research persona                 │
├─────────────────────────────────────────────────────┤
│  Technical (+65 GB download, ~95 GB total)           │
│  + Stack Exchange Q&A                               │
│  ☐ Recommended for Developer persona                │
├─────────────────────────────────────────────────────┤
│  Full (all of the above, ~170 GB indexed)            │
│  + Project Gutenberg books                          │
│  ☐                                                  │
└─────────────────────────────────────────────────────┘

Downloads happen in the background. You can start using
Sovereign immediately — knowledge bases become available
as they finish indexing.
```

The persona selection pre-checks the recommended tier. The user can override. Downloads and indexing run in the background — the first conversation can happen before any corpus is fully indexed. As corpora complete indexing, the search pipeline starts including them automatically.

### 4.2 Web Search Configuration

Immediately after corpus selection:

```
"Web search (optional):"

Your knowledge bases cover many questions offline. For current
events, recent publications, and specialized topics, web search
fills the gaps.

Tavily offers 1,000 free searches/month. No credit card needed.
Setup takes 2 minutes.

  [Get Tavily key] → opens tavily.com in browser
  [Enter API key: __________ ]
  [Skip — I'll use offline knowledge only]
```

Frame web search as supplementary, not primary. The local corpus is the default. Web search is the upgrade.

---

## Part 5: Corpus Updates

### 5.1 Background Update Checks

The `CorpusManager` checks for corpus updates weekly (configurable). Wikipedia dumps are released biweekly. OpenAlex and Stack Exchange update quarterly.

```rust
impl CorpusManager {
    /// Called by a background task on a weekly schedule.
    pub async fn check_for_updates(&self) -> Vec<CorpusUpdate> {
        let mut updates = vec![];
        for corpus in self.installed() {
            if let Some(newer) = self.check_remote_version(&corpus.id).await {
                updates.push(CorpusUpdate {
                    corpus_id: corpus.id.clone(),
                    current_date: corpus.source_date.clone(),
                    available_date: newer,
                    download_size_gb: self.estimate_update_size(&corpus.id),
                });
            }
        }
        updates
    }
}
```

Updates are offered to the user, not applied automatically:

```
"A newer Wikipedia dump is available (March 15, 2026).
 Download and update? (22 GB)"
 [Update now]  [Later]  [Don't ask again for this corpus]
```

Indexing an update is incremental where possible. For Wikipedia, the dump includes page revision dates; only pages modified since the last indexed dump need re-embedding. For Stack Exchange, only new answers need processing. This reduces update time from hours to minutes for most corpora.

---

## Part 6: Search Budget Tracking

### 6.1 Budget State

```rust
pub struct SearchBudget {
    pub backend: String,         // "tavily", "brave"
    pub monthly_limit: u32,      // 1000 for Tavily free
    pub used_this_month: u32,
    pub reset_date: NaiveDate,   // First of next month
}
```

Stored in `dyn StateStore`. Updated after every web search call. Queried by the coverage assessment step to factor cost into decisions.

### 6.2 Budget-Aware Decisions

When credits are low (<10% remaining), the coverage assessment becomes more conservative about recommending web search:

```rust
// In assess_coverage:
let budget = self.store.get_search_budget().await?;
let budget_pressure = if budget.remaining() < budget.monthly_limit / 10 {
    BudgetPressure::High  // Be conservative with web search
} else if budget.remaining() < budget.monthly_limit / 3 {
    BudgetPressure::Medium
} else {
    BudgetPressure::Low
};

// Under high budget pressure, only supplement with web search
// if local coverage is clearly insufficient, not just partial.
```

When credits are exhausted, the system tells the user plainly:

```
"Your monthly web search credits are used up. I can still answer
 from your local knowledge bases. For web search, credits reset
 on [date], or you can upgrade your plan at tavily.com."
```

---

## Testing

### Unit tests

- Each `CorpusParser` produces valid `DocumentChunk` structs from sample data.
- `CoverageAssessment` returns `Sufficient` when local results score > 0.85 for a factual query.
- `CoverageAssessment` returns `SupplementWithWeb` when local results are partial and a gap is identifiable.
- `CoverageAssessment` returns `RequiresWeb` when query needs current events (`needs_current_events = true`).
- Search budget tracking correctly decrements and resets monthly.
- Budget pressure correctly makes coverage assessment more conservative.
- Merge-and-rerank correctly deduplicates local and web results covering the same topic.

### Integration tests

- Install Wikipedia corpus from a small test dump (100 articles). Verify chunks are stored with `source_type: Corpus { corpus_id: "wikipedia" }`.
- Query a topic covered by the test dump. Verify the pipeline returns `SearchMethod::LocalOnly` and produces cited synthesis.
- Query a topic NOT in the test dump. Verify the pipeline returns `SearchMethod::RequiresWeb` (or `NoResults` if no web backend).
- Query a topic partially covered. Verify the pipeline returns `SearchMethod::LocalPlusWeb` with targeted web queries for the gap.
- Verify `SourceOrigin` tags propagate correctly into the synthesis prompt and response citations.

### Manual validation

- Install the full Wikipedia corpus. Time the ingestion (expect 4-8 hours depending on hardware). Verify index size matches estimates.
- Run 50 diverse queries spanning factual, conceptual, current events, and niche topics. Classify each as correctly-assessed-coverage vs misassessed. Target: >80% correct coverage decisions.
- Compare synthesis quality on factual queries between local-only (Wikipedia) and web-search. For well-covered topics, local should be competitive. For recent or niche topics, web should clearly win.

---

## What Not To Do

**Do not build a custom search index.** The existing RAG infrastructure (sqlite-vec for vectors, FTS5 for keywords) handles corpus search the same way it handles user documents. The corpus is just a large collection of documents with a `source_type` tag. No new index format, no new search algorithm.

**Do not bundle corpora in the installer.** The installer stays small (~30MB). Corpora download on demand after first launch. The user sees a progress bar, uses Sovereign for non-search tasks while downloading, and search quality improves as corpora finish indexing.

**Do not make web search feel like a premium upsell.** The messaging is: "Your knowledge base covers this" (capability) and "Web search can supplement with current information" (upgrade), not "You need web search for a good experience" (deficiency). The local corpus is the product. Web search is the enhancement.

**Do not skip the coverage assessment step.** It's tempting to always search both local and web for maximum quality. But this burns a web search credit on every query, defeats the budget management, and makes the free tier useless. The coverage assessment is what makes the free tier viable — it spends web credits only when local coverage is genuinely insufficient.

# Post-v0: Intelligence Over Knowledge

_Reframing Sovereign around its actual value proposition._

**Context:** You've shipped v0. The Runtime works. The five traits hold. The Executor walks DAGs. The UI has conversations. The RAG pipeline indexes user documents. The `web_search` tool exists but requires a paid API key. Without that key, Sovereign is a chat interface to a local model with no access to external knowledge. This is the gap.

**The user:** A curious PhD student on a ramen budget. Can't afford $20/month for ChatGPT or Perplexity. Doesn't want thesis research queries going to cloud providers. Needs to understand theoretical lineages, survey what's been published, bridge concepts across fields, and get unstuck on methodology — every day, for free, privately.

**The reframe:** Sovereign's value is not "local AI." Local AI alone is a worse ChatGPT. Sovereign's value is **local AI reasoning over curated knowledge, privately, with memory across sessions, for free.** The knowledge bases aren't a fallback because web search costs money. They're the product. News and current events are the premium add-on. Knowledge and intelligence are the free tier.

Everything below follows from this.

---

## 1. Ship the Knowledge Base System

**Ref:** The full knowledge base task has already been specified. This section is the priority ordering and the integration points that task didn't cover.

### Corpus priority order

Index these in this order. Each one is independently useful. Don't wait for all of them.

1. **Wikipedia** (55GB indexed). Ship first. Covers the broadest range of queries. The moment this is indexed, Sovereign can answer "what is X" and "who was Y" queries from a verified source rather than parametric knowledge. This single corpus makes Sovereign meaningfully more useful than a bare local model.

2. **OpenAlex abstracts** (45GB indexed, filtered to last 15 years with abstracts). Ship second. This is the killer corpus for the PhD persona. "What has been published on institutional design in common-pool resource management?" returns titles, authors, abstracts, and citation counts. The student discovers papers they didn't know existed. No Google Scholar account needed. No Semantic Scholar rate limits. Just local search over 250 million records.

3. **Stanford Encyclopedia of Philosophy** (0.5GB indexed). Ship third. Tiny download, enormous value per byte. Deep, authoritative, peer-reviewed explanations of every major concept in philosophy, political theory, ethics, epistemology, logic, and philosophy of science. For any student working in social sciences, humanities, or interdisciplinary fields, this fills gaps that Wikipedia only summarizes.

4. **Stack Exchange** (40GB indexed, filtered to score ≥ 3). Ship fourth. Cross Validated for statistics methodology. Stack Overflow for programming. Math Stack Exchange for proofs. Economics Stack Exchange for theory. 170+ communities of expert Q&A, already structured as question-answer pairs that the model can reason over directly.

5. **OpenStax textbooks** (~2GB indexed). Add to the corpus manifest. OpenStax publishes free, CC-licensed, peer-reviewed textbooks in economics, statistics, biology, physics, psychology, sociology, and more. When a student needs to understand something outside their specialization — "I need basic immunology for this interdisciplinary project" — a textbook chapter structured for learning is more useful than a reference article.

6. **Congressional Research Service reports** (4GB indexed). Nonpartisan US policy analysis. Valuable for anyone studying governance, policy, or political economy.

7. **Project Gutenberg** (25GB indexed). Primary texts. Mill, Kant, Darwin, Adam Smith, Marx. Essential for humanities. Optional for everyone else.

### Integration point: the search tool rename

The `web_search` tool becomes `search`. This is not cosmetic. The tool's descriptor — which the Router and Planner see — should read:

```rust
ToolDescriptor {
    id: "search".to_string(),
    name: "Search".to_string(),
    description: "Search across local knowledge bases (Wikipedia, scholarly abstracts, \
                  encyclopedias, expert Q&A) and optionally the web. Local knowledge \
                  bases are always available. Web search requires an API key.".to_string(),
    parameters: /* ... */,
}
```

When the Planner generates a DAG with a search step, it's reaching for *knowledge*, not specifically the web. The tool handles the local-first, web-fallback logic internally.

---

## 2. Rewrite the Setup Wizard

The current wizard is: download models → toggle capabilities → start chatting. The new wizard should make knowledge the headline.

### Flow

```
Step 1: "How will you use Sovereign?"
        [Research & Analysis]  [Personal Assistant]  [Developer]

Step 2: "Your AI comes with built-in knowledge."

        Sovereign includes curated, offline knowledge bases
        so your AI can reason over verified sources — not
        just guess from memory.

        Essential (22 GB download)
          Wikipedia — 6.8M articles on everything
          ☑ Recommended

        Research (add ~50 GB)
          + 250M scholarly abstracts (OpenAlex)
          + Stanford Encyclopedia of Philosophy
          + OpenStax textbooks
          + Congressional Research Service reports
          ☑ Recommended for Research persona

        Technical (add ~65 GB)
          + Stack Exchange expert Q&A
          ☐ Recommended for Developer persona

        Literature (add ~15 GB)
          + 70,000 public domain books
          ☐

        [Available disk space: 342 GB]

Step 3: "Downloading your knowledge base and AI model."
        [Progress: Wikipedia ████████░░ 78%]
        [Progress: AI model  ██████████ done]

        You can start using Sovereign now. Knowledge bases
        become available as they finish indexing.

Step 4: "Want web search for current events?" (optional)

        Your knowledge bases cover most research questions
        offline. For today's news, very recent papers, or
        specialized topics, web search fills the gap.

        Tavily: 1,000 free searches/month. No credit card.

        [Get a free key (2 min)] → opens tavily.com
        [Enter key: ________]
        [Skip — offline knowledge is enough for now]

Step 5: "Ready. Ask me anything."
```

Note the ordering: knowledge bases come before web search. Knowledge is the product. Web search is offered after, as optional. The language is "offline knowledge is enough for now" — not "you're missing out."

---

## 3. Adjust the Default Persona Configurations

### Research persona

Active skills: `research-analyst` (bundled).
Knowledge tier: Research (auto-selected in wizard).
Web search: prompted but not required.

The `research-analyst` skill's planner templates should be updated to reflect the knowledge-first pipeline:

```toml
[[planner.templates]]
name = "literature_survey"
trigger = "User wants to know what has been published or researched on a topic"
steps = """
1. Search local knowledge bases for the topic, focusing on
   OpenAlex abstracts and Wikipedia context.
2. Assess coverage: does the local corpus show the landscape
   of published work on this topic?
3. If coverage is sufficient, synthesize a survey of the field
   from local sources with citations.
4. If coverage has gaps (very recent work, niche subfield),
   supplement with web search if available.
5. Present findings as a structured overview: key authors,
   central debates, methodological approaches, and open
   questions — with sources cited.
"""

[[planner.templates]]
name = "concept_explanation"
trigger = "User wants to understand a concept, theory, or framework"
steps = """
1. Search local knowledge bases, prioritizing Stanford
   Encyclopedia of Philosophy and Wikipedia.
2. Synthesize an explanation that covers: what the concept is,
   who originated it, how it relates to adjacent concepts,
   and what the main critiques are.
3. Web search is almost never needed for this query type.
   Only supplement if the concept is very recent (post-2024)
   or the local corpus has no relevant results.
"""

[[planner.templates]]
name = "methodology_help"
trigger = "User is stuck on a statistical, technical, or research method"
steps = """
1. Search local knowledge bases, prioritizing Stack Exchange
   (Cross Validated for stats, relevant SE sites for other
   methods) and OpenStax textbooks.
2. Synthesize an explanation at the user's apparent level:
   if they're asking a basic question, start from fundamentals.
   If they're asking about edge cases, assume expertise.
3. Web search only if the method is very new or the local
   corpus has no coverage.
"""
```

These templates encode the knowledge-first philosophy at the planning level. The Planner doesn't default to web search. It defaults to the local corpus and only reaches for the web when it has a specific, identified gap.

### Personal assistant persona

Active skills: none (general-purpose).
Knowledge tier: Essential (Wikipedia only).
Web search: prompted.

For this persona, Wikipedia handles "what is," "when did," "who was" queries. The model's parametric knowledge handles conversational tasks. Web search is more useful here than for the research persona because personal assistant queries are more likely to be time-sensitive ("what's the weather," "what happened today").

### Developer persona

Active skills: none by default (user configures).
Knowledge tier: Technical (Wikipedia + Stack Exchange).
Web search: prompted.

Stack Exchange coverage of programming and technical Q&A is deep enough that many "how do I do X in Y" queries are answerable locally.

---

## 4. Update the Coverage Assessment Heuristics

The coverage assessment in the search tool needs heuristics tuned to the PhD use case.

### Queries that almost always resolve locally

- "What is [concept/theory/framework]" → Wikipedia + SEP
- "Who is [scholar/thinker]" → Wikipedia
- "Explain [statistical method/technique]" → Stack Exchange + OpenStax
- "What is the difference between [X] and [Y]" → Wikipedia + SEP
- "What has been published on [topic]" → OpenAlex
- "Summarize [classic text]" → Gutenberg + Wikipedia

For these patterns, the coverage assessment should have a strong prior toward `Sufficient` when local results score > 0.7. Don't waste a web credit confirming what the local corpus clearly covers.

### Queries that almost always need web

- "What happened with [X] this week/today/recently" → Web
- "Latest research on [X]" (where "latest" means < 6 months) → Web
- "What is [very new thing]" (released after corpus date) → Web
- "Current [price/status/position]" → Web

For these patterns, the coverage assessment should short-circuit to `RequiresWeb` based on the query analysis's temporal signals, without even checking the local corpus.

### The gray zone

- "Recent developments in [established field]" — local corpus has the field, web has the recent part. `SupplementWithWeb` with targeted queries for recency.
- "What do critics say about [X]" — Wikipedia has a neutral summary, web has the actual critical voices. `SupplementWithWeb` for specific critiques.
- "Compare [X] and [Y] approaches to [Z]" — depends entirely on whether X, Y, and Z are well-established or emerging.

These are where the Fast model's judgment in the coverage assessment earns its keep. The heuristic short-circuits handle the obvious cases cheaply. The LLM handles the ambiguous cases with a quick inference call.

---

## 5. Make Knowledge Base Status Visible

The user should always know what knowledge is available. Add to the settings panel:

```
Knowledge Bases
───────────────────────────────────────────
Wikipedia          6.8M articles    ✓ Indexed
OpenAlex           14.2M abstracts  ✓ Indexed
Stanford Enc.      1,800 entries    ✓ Indexed
Stack Exchange     Installing...    ████░░ 62%
OpenStax           Not installed    [Add]

Web Search
───────────────────────────────────────────
Tavily             847 / 1,000 credits remaining
                   Resets April 1
```

And in the conversation UI, when the system answers from the local corpus, a subtle indicator:

```
Sources: Wikipedia (2), OpenAlex (3), SEP (1)
```

When web search is used:

```
Sources: Wikipedia (1), tavily.com (2) — 2 web credits used
```

This transparency builds trust. The student sees that their AI is reasoning over real sources, not hallucinating. They see exactly when web credits are spent and why. They feel in control of their budget.

---

## 6. Memory System Tuning for Academic Work

The PhD student's memory needs are different from a general user's. Academic work has long arcs — a thesis topic persists for years. The memory system should be tuned for this.

### What to extract from research conversations

```toml
# In the research-analyst skill's memory rules
[memory]
extract_prompt_addendum = """
For research conversations, extract:
- Topics and fields the user is actively studying.
- Specific theories, frameworks, or models the user has engaged with.
- Scholars and authors the user has discussed or cited.
- Methodological approaches the user is using or considering.
- Connections the user has drawn between different ideas or fields.
- Open questions the user has articulated.

Do NOT extract:
- Summaries of what the knowledge base returned. The user can
  search again. Extract the user's OWN insights and connections.
- General facts. "Ostrom identified eight design principles" is
  in Wikipedia. "User is connecting Ostrom's boundary rules to
  open-source license enforcement" is a genuine insight worth
  remembering.
"""
```

### Confidence decay adjustment

The default memory confidence decay is 10% per month, pruned below 0.2. For academic memories, this is too aggressive — a thesis topic doesn't become less relevant after three months. Memories tagged with the `research-analyst` skill should decay at 5% per month and prune below 0.1. A student who researched Ostrom in January and comes back to it in June should find those memories intact.

The skill manifest gains a memory decay override:

```toml
[memory]
confidence_decay_per_month = 0.05
prune_threshold = 0.1
```

---

## 7. First Conversation Experience

After setup completes and Wikipedia finishes indexing (or even partially indexes), the first conversation should demonstrate the knowledge-first value immediately.

The system's greeting for the Research persona:

```
Ready. Your knowledge base includes 6.8 million Wikipedia
articles [and N scholarly abstracts, if OpenAlex is indexed].

Try asking me to explain a concept, survey what's been
published on a topic, or help you think through a
methodological question.
```

Not "ask me anything." Not "how can I help." A specific invitation that demonstrates what the knowledge base enables. The student's first query — "explain the difference between instrumental variable estimation and regression discontinuity design" — gets a synthesis drawn from Wikipedia's econometrics articles, Cross Validated answers, and an OpenStax statistics chapter. Cited, structured, accurate. The student immediately sees: this is different from ChatGPT. This has sources. This works offline. This is mine.

That first experience is what determines whether the student uses Sovereign tomorrow. Make it undeniable.