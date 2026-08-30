// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-symbol code-intelligence enrichment — the storage-agnostic generation core.
//!
//! Given a code symbol (a function) and its source body, produce an
//! *intent-forced* plain-English summary plus the questions it answers,
//! keyed by a content-hash of the body. This is the validated bridge from a
//! conceptual ("no function keywords") question to the right symbol:
//! retrieval matches the summary + questions, then the SCIP call-graph traces
//! from there. See `sovereign/docs/specs/CODE_INTEL_CHAT.md`.
//!
//! Design (SOLID, single-responsibility, dependency-injected):
//!  - [`SymbolMeta`] + [`SymbolEnrichment`] are plain data.
//!  - [`enrich_symbol`] is the unit of work: `(meta, body) -> enrichment`,
//!    pure given the injected [`ChatCompletionFn`]. It does NO file IO, NO
//!    SCIP access, and bakes in NO storage decision — those are later slices,
//!    so this core is identical whether the result lands as Atlas atoms or
//!    chunk-index rows.
//!  - [`enrich_symbols_incremental`] is the *patchable* batch driver: it
//!    re-generates only symbols whose body-hash is absent from the prior
//!    cache. A rename, a move, or a caller-only change leaves the body-hash
//!    untouched, so nothing re-generates — per-commit cost equals the number
//!    of changed function *bodies* (spec §3.4), not the corpus.
//!
//! The summary *style* is load-bearing: the prompt forces user-vocabulary
//! ("another machine in the cluster"), never code jargon ("peer", "node"). A
//! jargon summary fails retrieval; the user-voiced one wins (spec §3.2). The
//! prompt input here is code-only — identical to the input validated in the
//! 172-function scale run — so this is not coached to any eval.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use crate::enrichment::pipeline::prompts::load_or_baked;
use crate::enrichment::pipeline::types::{ChatCompletionFn, ChatPrompt};
use crate::error::{Error, Result};

/// SCIP-sourced symbol enumeration (slice 2) — gated on `treesitter`, the
/// feature that pulls in `corpus-engine-scip` (matches `atlas::code_walk`).
#[cfg(feature = "treesitter")]
pub mod scip_source;

/// Storage: write per-symbol enrichments as searchable chunks (slice 3, Path B).
pub mod store;

/// The composed enrichment pass (slice 4): enumerate -> summarize -> index,
/// patchable via a body-hash sidecar cache. Gated on `treesitter`.
#[cfg(feature = "treesitter")]
pub mod pass;

// Inc 2 slice 2a — the call-graph *trace* builder lives in the lean
// `corpus-engine-scip` crate (`corpus_engine_scip::trace`), NOT here: it reads
// the call graph via SQL over `scip_graph.db` and needs none of the tree-sitter
// grammars this crate's `treesitter` feature pulls in. Homing it there lets the
// chat runtime depend on the read API without dragging the parser into every
// build. See `corpus-engine-scip/src/trace.rs`.

/// Phase id carried on every code-intel prompt, so the chat client can route
/// bulk symbol summarization to a fast/short model when the operator has
/// declared a per-phase override. See [`ChatPrompt::phase_id`].
pub const PHASE_ID: &str = "code_intel_symbol";

/// Output-token budget: one SUMMARY sentence plus two short ASKS. Matches the
/// budget used in the validated scale run (spec §5).
const MAX_OUTPUT_TOKENS: u32 = 160;

/// Low temperature — factual descriptions, not creative prose.
const TEMPERATURE: f32 = 0.2;

/// The output contract, enforced by grammar rather than asked for in prose.
///
/// ARCH §7.6 — never ask a model to guarantee what code can enforce. The
/// prompts used to end "Output EXACTLY this shape and nothing else"; measured
/// 2026-08-30 on this host's fast slot (Qwopus3.5-4B), that produced 0 usable
/// responses in 5 configurations — the model reasons aloud and the 160-token
/// budget is gone before any label appears. `/no_think` and
/// `chat_template_kwargs` are both dropped by the compatible path, so neither
/// is a fix. `refactor_cmd::label_model` hit the identical wall (13/13 parse
/// failures) and solved it this way; this is the same solution, one crate over.
fn response_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "summary": {"type": "string"},
            "asks": {"type": "array", "items": {"type": "string"},
                     "minItems": 2, "maxItems": 2}
        },
        "required": ["summary", "asks"],
        "additionalProperties": false
    })
}

const SUMMARY_LABEL: &str = "summary:";
const ASKS_LABEL: &str = "asks:";

/// The intent-forced system prompt. Overridable on disk via
/// `$SOVEREIGN_PROMPT_DIR/code_intel/symbol_enrichment_system.md` (glassbox:
/// tune the lever without a rebuild — see `pipeline::prompts`).
static SYMBOL_ENRICHMENT_SYSTEM: LazyLock<&'static str> = LazyLock::new(|| {
    load_or_baked(
        "code_intel/symbol_enrichment_system.md",
        include_str!("prompts/symbol_enrichment_system.md"),
    )
});

/// The same lever for TYPES. A struct has no behaviour to describe, and the
/// function prompt's "anchor on what it RETURNS" is meaningless for one — asked
/// that way, the model answers about the prompt instead of the code ("This
/// function is not present in the provided code; only a `Label` struct
/// definition was given...").
static TYPE_ENRICHMENT_SYSTEM: LazyLock<&'static str> = LazyLock::new(|| {
    load_or_baked(
        "code_intel/type_enrichment_system.md",
        include_str!("prompts/type_enrichment_system.md"),
    )
});

/// Which prompt a symbol is asked with.
///
/// Two, not one, and the split is load-bearing: measured on this graph
/// 2026-08-24, the enrichable population was 61,706 symbols of which only
/// 32,739 (53%) were callable. The other 47% — 4,945 types, 3,164 modules and
/// 20,858 non-callable terms — were all being handed a prompt that says "ONE
/// function" six times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// Free functions, methods, trait methods — things with behaviour.
    Callable,
    /// Structs, enums, traits, aliases — things that represent, not act.
    Type,
}

impl PromptKind {
    /// Short, stable id. Appears in the cache key, so it must never be
    /// re-spelled casually — a changed id silently invalidates that slice.
    pub fn id(self) -> &'static str {
        match self {
            PromptKind::Callable => "fn",
            PromptKind::Type => "ty",
        }
    }

    fn system(self) -> &'static str {
        match self {
            PromptKind::Callable => *SYMBOL_ENRICHMENT_SYSTEM,
            PromptKind::Type => *TYPE_ENRICHMENT_SYSTEM,
        }
    }

    fn noun(self) -> &'static str {
        match self {
            PromptKind::Callable => "FUNCTION",
            PromptKind::Type => "TYPE",
        }
    }
}

/// Bump when a prompt's TEXT changes in a way that should re-generate.
///
/// # Why this exists, and why it is per-kind
///
/// The cache was keyed on `body_hash` ALONE, so a prompt improvement was
/// invisible: every existing summary kept the old prompt's output forever and
/// no re-run could dislodge it. `store.rs` records this exact bug being found
/// and fixed for `RENDER_VERSION` — the rendering half — but the GENERATION
/// half kept the defect.
///
/// It is keyed per-kind on purpose, and that is the cost mitigation. Bumping
/// the type prompt re-generates ~4,945 type summaries, not all 37,684. Had
/// this landed after a full corpus run instead of before one, fixing a prompt
/// would have meant regenerating everything — measured at 3.5s/symbol under
/// load, that is the difference between an hour and a day and a half.
pub const PROMPT_VERSION: u32 = 2;

/// The cache identity for one symbol: body, prompt, and prompt version.
///
/// Everything that can change the OUTPUT belongs in the key. A key that names
/// only the input describes a cache that cannot be corrected.
pub fn cache_key(kind: PromptKind, body: &str) -> String {
    format!("{}/{}v{PROMPT_VERSION}", body_hash(body), kind.id())
}

/// Identity + location of a code symbol to enrich. Sourced from the SCIP
/// graph (`SymbolRow`) in a later slice; defined here decoupled so the
/// generator carries no dependency on the SCIP reader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolMeta {
    /// Display name, e.g. `select_route`.
    pub name: String,
    /// Fully-qualified SCIP descriptor, e.g.
    /// `sovereign_mesh::peer_inference::MeshInferenceProvider::select_route`.
    pub qualified_name: String,
    /// Source file (corpus-relative), e.g.
    /// `crates/sovereign-mesh/src/peer_inference.rs`.
    pub file_path: String,
    /// 1-based inclusive line span of the symbol definition.
    pub line_start: u32,
    pub line_end: u32,
    /// Source language, e.g. `rust`.
    pub language: String,
}

/// The enrichment produced for one symbol: the user-voiced summary, the
/// questions it answers, and the body content-hash that keys it for
/// incremental re-generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolEnrichment {
    pub meta: SymbolMeta,
    /// `blake3(body)[..16]` — the patchability key. Same body => same hash =>
    /// the prior summary is reused; a body edit => new hash => one
    /// re-generation. Matches the `atoms.rs::short_hash` / chunk
    /// `content_hash` convention.
    pub body_hash: String,
    /// `{body_hash}/{prompt}v{version}` — the full identity this summary was
    /// generated under. Defaulted when absent so a pre-versioning cache file
    /// still loads; such entries simply miss and re-generate once, which is
    /// the correct behaviour for a summary whose prompt is unknown.
    #[serde(default)]
    pub cache_key: String,
    /// One plain-English sentence on the real-world job the symbol does.
    pub summary: String,
    /// Plain-English questions a user might ask that this answers (the
    /// conceptual->symbol bridge; maps onto the Atlas `Question`-atom shape).
    pub asks: Vec<String>,
}

/// A symbol plus its current source body — the input to a batch run.
#[derive(Debug, Clone)]
pub struct SymbolSource {
    pub meta: SymbolMeta,
    pub body: String,
    /// Which prompt this symbol is asked with. Decided by the enumerator from
    /// the SCIP descriptor, which is the reliable signal — the graph's `kind`
    /// column is not (see `is_enrichable_kind`).
    pub kind: PromptKind,
}

/// Glassbox counts for one incremental batch — the patchability cost model
/// made observable (per-commit cost = `regenerated`, not `total`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct IncrementalReport {
    pub total: usize,
    /// Body-hash already present in the prior cache — summary reused, no model
    /// call (rename / move / caller-only change land here).
    pub reused: usize,
    /// Body new or changed — one model call spent.
    pub regenerated: usize,
    /// Generation or parse failure — logged and skipped (never poisons the set).
    pub failed: usize,
}

/// `blake3` 16-hex of the symbol body. Matches the hashing convention used for
/// atom ids (`enrichment/atlas/atoms.rs::short_hash`) and chunk `content_hash`
/// (`engine/reindex.rs`), so a symbol's key is stable and comparable across
/// the pipeline.
pub fn body_hash(body: &str) -> String {
    kernel_types::ContentHash::of_str(body).short()
}

/// Extract a symbol's body from source text, starting at the 0-based `line_start`.
///
/// rust-analyzer's SCIP export omits function *enclosing ranges*, so `line_end` is
/// unreliable — frequently `< line_start` — which previously collapsed the body to
/// the single signature line (`fn name(`) and starved the summarizer into
/// name-only confabulation. We instead recover the real body by **brace-matching**
/// from `line_start`: accumulate lines until the `{`…`}` depth returns to zero
/// after the first `{`. For a braceless symbol (a trait-method declaration, a
/// `const`) we fall back to the original `line_end`-bounded span. Capped at
/// `MAX_BODY_LINES`. SCIP records line numbers 0-based, so editor line N is `N-1`.
pub fn extract_body(content: &str, line_start: i32, line_end: i32) -> String {
    let lines: Vec<&str> = content.lines().collect();
    extract_body_from_lines(&lines, line_start, line_end)
}

/// Body extraction over PRE-SPLIT lines. Whole-corpus enumeration calls this with
/// a per-file line vector split ONCE (in `enumerate_from_rows`'s file cache) —
/// `extract_body` above re-split the entire file on every symbol, which is
/// O(functions × file_len) per file and dominated the whole-corpus enumerate
/// (~22k functions, big files re-split hundreds of times). Generic over the line
/// element so the cache can hold owned `String`s and `&str` callers still work.
pub fn extract_body_from_lines<S: AsRef<str>>(
    lines: &[S],
    line_start: i32,
    line_end: i32,
) -> String {
    let start = line_start.max(0) as usize;
    if start >= lines.len() {
        return String::new();
    }
    let cap = (start + MAX_BODY_LINES).min(lines.len());
    let (mut depth, mut seen_brace) = (0i32, false);
    for (i, l) in lines.iter().enumerate().take(cap).skip(start) {
        let l = l.as_ref();
        depth += l.matches('{').count() as i32 - l.matches('}').count() as i32;
        seen_brace |= l.contains('{');
        if seen_brace && depth <= 0 {
            return join_lines(&lines[start..=i]);
        }
    }
    if seen_brace {
        // Opened but never balanced within the cap — return the capped window.
        return join_lines(&lines[start..cap]);
    }
    // Braceless symbol (or the plain-text test path): fall back to the original
    // `line_end`-bounded inclusive span.
    let end = (line_end.max(line_start) as usize)
        .min(start + MAX_BODY_LINES)
        .min(lines.len() - 1);
    join_lines(&lines[start..=end])
}

/// Join body lines (bounded to the extracted window, never the whole file).
fn join_lines<S: AsRef<str>>(lines: &[S]) -> String {
    lines
        .iter()
        .map(|s| s.as_ref())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Max body lines extracted per symbol — bounds the prompt and caps any leftover
/// mis-span. Real functions rarely exceed this; a longer one is truncated, which
/// still conveys intent for the summary.
const MAX_BODY_LINES: usize = 400;

/// Which SCIP symbol kinds get a per-symbol intent summary. We *want* functions
/// and methods — but rust-analyzer's SCIP export leaves nearly every Rust
/// function/method as `unknown` (and labels some methods `trait`); it reliably
/// tags only `enum`/`module`/`class`/`struct`/`type`/`variable`/`const`/`field`.
/// So allow-listing `function`/`method` skips almost the entire codebase
/// (verified on commonwealth-ai: §4 targets `select_route`, `gate_answer`,
/// `handle_message_stream_with_classification` are labelled `trait`/`unknown`).
/// We INVERT: enrich anything that is NOT a reliably-labelled non-callable. The
/// body-length gate + (post exporter-fix) real body spans drop the residue. The
/// precise signal is the call graph — a symbol present as a `refs.caller_symbol`
/// has a body and calls things — a future full-corpus slice can switch to that.
pub fn is_enrichable_kind(kind: &str) -> bool {
    !matches!(
        kind,
        "enum" | "module" | "class" | "struct" | "type" | "variable" | "const" | "field"
    )
}

/// Compose the chat prompt for one symbol. Pure + deterministic, so it is
/// unit-testable without a model. The system message carries the accuracy rules +
/// output format; the user message carries the function's full name, its file, and
/// its code. The name + file are a domain anchor — without them the model judged a
/// body like `blast_radius` from the bare name and invented "machines in a
/// cluster"; with them it stays in the right domain.
pub fn compose_symbol_prompt(qualified_name: &str, file_path: &str, body: &str) -> ChatPrompt {
    compose_symbol_prompt_for(PromptKind::Callable, qualified_name, file_path, body)
}

/// Compose the prompt for one symbol, asked as the right kind of thing.
pub fn compose_symbol_prompt_for(
    kind: PromptKind,
    qualified_name: &str,
    file_path: &str,
    body: &str,
) -> ChatPrompt {
    let user = format!(
        "{}: {qualified_name}\nFILE: {file_path}\n\nCODE:\n{body}",
        kind.noun()
    );
    ChatPrompt::new(kind.system(), user)
        .with_phase_id(PHASE_ID)
        .with_temperature(TEMPERATURE)
        .with_max_output_tokens(MAX_OUTPUT_TOKENS)
        .with_response_schema("code_intel_entry", response_schema())
}

/// Case-insensitive (ASCII) substring search returning the byte offset in the
/// *original* string. The labels are ASCII, so the returned offset is always a
/// char boundary — safe to slice at even when the surrounding text is UTF-8.
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    (0..=h.len() - n.len()).find(|&i| (0..n.len()).all(|j| h[i + j].eq_ignore_ascii_case(&n[j])))
}

/// Parse the model's `SUMMARY:` / `ASKS:` response into `(summary, asks)`.
/// Lenient: tolerates missing labels, asks on one or several lines, list
/// markers, and surrounding quotes. Never panics; an empty summary is
/// surfaced as an error by [`enrich_symbol`].
pub fn parse_symbol_response(text: &str) -> (String, Vec<String>) {
    let t = text.trim();
    // Grammar-constrained responses are JSON. Try that first: the label parser
    // below would find `"summary":` INSIDE the object and hand back the rest of
    // the blob. The label path stays for entries generated before PROMPT_VERSION
    // 2 and for any provider that ignores the schema.
    if t.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
            let summary = v.get("summary").and_then(|x| x.as_str()).unwrap_or("");
            if !summary.trim().is_empty() {
                let asks = v
                    .get("asks")
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|q| q.as_str())
                            .map(|q| clean(q))
                            .filter(|q| !q.is_empty())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                return (clean(summary), asks);
            }
        }
    }
    let s_at = find_ci(t, SUMMARY_LABEL);
    let a_at = find_ci(t, ASKS_LABEL);
    let (sl, al) = (SUMMARY_LABEL.len(), ASKS_LABEL.len());

    let (summary_raw, asks_raw): (&str, &str) = match (s_at, a_at) {
        (Some(s), Some(a)) if a > s => (&t[s + sl..a], &t[a + al..]),
        (Some(s), Some(a)) => (&t[s + sl..], &t[a + al..s]), // asks before summary
        (Some(s), None) => (&t[s + sl..], ""),
        (None, Some(a)) => (&t[..a], &t[a + al..]),
        (None, None) => (t, ""),
    };
    (clean(summary_raw), split_asks(asks_raw))
}

/// Trim whitespace and a single layer of surrounding quotes.
fn clean(s: &str) -> String {
    s.trim().trim_matches('"').trim().to_string()
}

/// Strip a leading list marker (`-`, `*`, bullet, `1.`, `2)`) and quotes.
fn strip_marker(s: &str) -> &str {
    let s = s
        .trim()
        .trim_start_matches(['-', '*', '\u{2022}', '\u{00b7}'])
        .trim();
    let s = s
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .trim_start_matches(['.', ')'])
        .trim();
    s.trim_matches('"').trim()
}

/// Split the ASKS block into individual questions. Prefers `?` boundaries
/// (re-appending the mark); falls back to one-per-line.
fn split_asks(raw: &str) -> Vec<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    if raw.contains('?') {
        raw.split('?')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| format!("{}?", strip_marker(p)))
            .collect()
    } else {
        raw.lines()
            .map(|l| strip_marker(l).to_string())
            .filter(|l| !l.is_empty())
            .collect()
    }
}

/// Generate the enrichment for one symbol. Pure given `chat`. Returns an
/// [`Error::Extraction`] if the model yields no usable summary (surfaced, not
/// silently stored, per the glassbox principle).
pub async fn enrich_symbol(
    chat: &ChatCompletionFn,
    meta: SymbolMeta,
    body: &str,
) -> Result<SymbolEnrichment> {
    enrich_symbol_as(chat, PromptKind::Callable, meta, body).await
}

/// Enrich one symbol, asked as the right kind of thing.
pub async fn enrich_symbol_as(
    chat: &ChatCompletionFn,
    kind: PromptKind,
    meta: SymbolMeta,
    body: &str,
) -> Result<SymbolEnrichment> {
    let prompt = compose_symbol_prompt_for(kind, &meta.qualified_name, &meta.file_path, body);
    let raw = (chat)(&prompt).await?;
    let (summary, asks) = parse_symbol_response(&raw);
    if summary.is_empty() {
        return Err(Error::Extraction(format!(
            "code_intel: empty summary for `{}` ({}B in; response head: {:?})",
            meta.name,
            body.len(),
            raw.chars().take(120).collect::<String>(),
        )));
    }
    Ok(SymbolEnrichment {
        body_hash: body_hash(body),
        cache_key: cache_key(kind, body),
        meta,
        summary,
        asks,
    })
}

/// Enrich a batch of symbols, reusing prior results whose body-hash is
/// unchanged — the patchable hot path. Returns the full current set (reused +
/// regenerated, in input order) and a glassbox [`IncrementalReport`].
///
/// `prior` is keyed by [`body_hash`]: content-addressed, so two symbols with
/// identical bodies share one summary, and a rename/move with an unchanged
/// body reuses it (only the `meta` is refreshed).
pub async fn enrich_symbols_incremental(
    chat: &ChatCompletionFn,
    symbols: Vec<SymbolSource>,
    prior: &HashMap<String, SymbolEnrichment>,
) -> (Vec<SymbolEnrichment>, IncrementalReport) {
    use futures::StreamExt;
    let total = symbols.len();

    // Partition: a body whose hash is already in `prior` reuses its summary with
    // no model call; new/changed bodies are enriched CONCURRENTLY below. Each
    // carries its input index so the output preserves input order regardless of
    // completion order.
    let mut indexed: Vec<(usize, SymbolEnrichment)> = Vec::new();
    let mut to_enrich: Vec<(usize, SymbolSource)> = Vec::new();
    for (i, src) in symbols.into_iter().enumerate() {
        // The KEY is body + prompt + prompt version. Keyed on the body alone,
        // a prompt fix could never dislodge a bad summary.
        let hash = cache_key(src.kind, &src.body);
        if let Some(prev) = prior.get(&hash) {
            // Body unchanged (hash-keyed) — reuse the summary, refresh meta in
            // case the symbol moved or was renamed with an identical body.
            let mut e = prev.clone();
            e.meta = src.meta;
            indexed.push((i, e));
        } else {
            to_enrich.push((i, src));
        }
    }
    let reused = indexed.len();

    // Bounded concurrency: a single-sequence serving slot simply queues these
    // (no harm), while a multi-seq slot (n_seq_max>1) turns them into real
    // parallelism — the bulk-pass speedup. Tunable via
    // SOVEREIGN_CODE_INTEL_CONCURRENCY (default 8).
    let conc = std::env::var("SOVEREIGN_CODE_INTEL_CONCURRENCY")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(8);
    let results: Vec<(usize, Option<SymbolEnrichment>)> =
        futures::stream::iter(to_enrich.into_iter().map(|(i, src)| {
            let chat = chat.clone();
            async move {
                match enrich_symbol_as(&chat, src.kind, src.meta.clone(), &src.body).await {
                    Ok(e) => (i, Some(e)),
                    Err(err) => {
                        tracing::warn!(
                            target: "enrichment.code_intel",
                            symbol = %src.meta.name,
                            file = %src.meta.file_path,
                            error = %err,
                            "symbol enrichment failed; skipping",
                        );
                        (i, None)
                    }
                }
            }
        }))
        .buffer_unordered(conc)
        .collect()
        .await;

    let mut regenerated = 0;
    let mut failed = 0;
    for (i, r) in results {
        match r {
            Some(e) => {
                regenerated += 1;
                indexed.push((i, e));
            }
            None => failed += 1,
        }
    }
    indexed.sort_by_key(|(i, _)| *i);
    let out: Vec<SymbolEnrichment> = indexed.into_iter().map(|(_, e)| e).collect();

    let mut report = IncrementalReport {
        total,
        ..Default::default()
    };
    report.reused = reused;
    report.regenerated = regenerated;
    report.failed = failed;

    tracing::info!(
        target: "enrichment.code_intel",
        total = report.total,
        regenerated = report.regenerated,
        reused = report.reused,
        failed = report.failed,
        concurrency = conc,
        "code_intel incremental enrichment complete",
    );
    (out, report)
}

// ── Inc 6: change-set between two cache snapshots ────────────

/// The set of symbols whose code-intel summary changed between two
/// `code_intel_cache.json` snapshots, keyed by their atlas **doc-anchor**
/// (== SCIP qualified_name, or `<file>#<name>` when SCIP is silent — the
/// exact anchor `atlas::code_walk::emit_entities` stamps on an item atom's
/// `first_appearance.chunk_id`). The Inc-6 patch upserts `changed` and drops
/// `removed`; both sets are doc-ids `apply_atom_delta` understands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodeIntelChangeSet {
    /// New or body-changed symbols → upsert their atom.
    pub changed: std::collections::BTreeSet<String>,
    /// Symbols present in `prior` but gone from `refreshed` → drop their atom.
    pub removed: std::collections::BTreeSet<String>,
}

impl CodeIntelChangeSet {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.removed.is_empty()
    }
    pub fn len(&self) -> usize {
        self.changed.len() + self.removed.len()
    }
}

/// The atlas doc-anchor for a code symbol: the SCIP qualified_name when
/// present, else `<file>#<name>`. Mirrors `atlas::code_walk::emit_entities`'
/// `doc_anchor` exactly, so a change-set keyed by these anchors lines up with
/// the live atlas's item doc-ids (and `code_intel::store::symbol_source_key`,
/// which is this string namespaced under `codeintel:`).
pub fn symbol_doc_anchor(meta: &SymbolMeta) -> String {
    if meta.qualified_name.is_empty() {
        format!("{}#{}", meta.file_path, meta.name)
    } else {
        meta.qualified_name.clone()
    }
}

/// Diff two `code_intel_cache.json` snapshots by `(symbol_source_key →
/// body_hash)` (the §3.4 patchability cost model): a symbol whose body_hash
/// changed — or that is new — is `changed`; one that vanished is `removed`.
/// Returns atlas doc-anchors ready for `code_walk::extract_atoms_for_symbols`
/// + `apply_atom_delta`. Keyed on the stable per-symbol source key (survives
/// body edits) so a body change is a hash-mismatch on the SAME key, not a
/// remove+add.
///
/// Per key we compare the SET of body_hashes, not a single value — a cache
/// written before the stale-eviction fix can carry more than one entry for a
/// symbol (old + new body_hash), in nondeterministic Vec order. Comparing
/// sets makes the diff order-insensitive and idempotent: identical snapshots
/// (the no-op re-run) always diff to nothing, regardless of duplicates.
pub fn diff_code_intel_caches(
    prior: &[SymbolEnrichment],
    refreshed: &[SymbolEnrichment],
) -> CodeIntelChangeSet {
    use std::collections::{BTreeSet, HashSet};
    // symbol_source_key → (doc_anchor, set of body_hashes seen for it).
    type Index = HashMap<String, (String, HashSet<String>)>;
    let index = |v: &[SymbolEnrichment]| -> Index {
        let mut m: Index = HashMap::new();
        for e in v {
            let entry = m
                .entry(store::symbol_source_key(&e.meta))
                .or_insert_with(|| (symbol_doc_anchor(&e.meta), HashSet::new()));
            entry.1.insert(e.body_hash.clone());
        }
        m
    };
    let prior_map = index(prior);
    let refreshed_map = index(refreshed);

    let mut changed = BTreeSet::new();
    for (key, (anchor, hashes)) in &refreshed_map {
        match prior_map.get(key) {
            Some((_, prior_hashes)) if prior_hashes == hashes => {} // unchanged
            _ => {
                changed.insert(anchor.clone()); // new or body-hash set changed
            }
        }
    }
    let mut removed = BTreeSet::new();
    for (key, (anchor, _)) in &prior_map {
        if !refreshed_map.contains_key(key) {
            removed.insert(anchor.clone());
        }
    }
    CodeIntelChangeSet { changed, removed }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn meta(name: &str) -> SymbolMeta {
        SymbolMeta {
            name: name.to_string(),
            qualified_name: format!("crate::{name}"),
            file_path: "src/x.rs".to_string(),
            line_start: 1,
            line_end: 9,
            language: "rust".to_string(),
        }
    }

    /// A fake injected provider: counts calls and returns a fixed response, so
    /// tests assert on plumbing + the incremental-skip behaviour without a model.
    fn fake_chat(resp: &'static str, calls: Arc<AtomicUsize>) -> ChatCompletionFn {
        Arc::new(move |_p: &ChatPrompt| {
            calls.fetch_add(1, Ordering::SeqCst);
            let r = resp.to_string();
            Box::pin(async move { Ok(r) })
        })
    }

    #[test]
    fn body_hash_is_stable_and_sensitive() {
        let a = body_hash("fn f() {}");
        assert_eq!(a, body_hash("fn f() {}"), "same body => same hash");
        assert_ne!(a, body_hash("fn f() { g() }"), "changed body => new hash");
        assert_eq!(a.len(), 16, "16 hex chars (matches short_hash convention)");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn extract_body_is_zero_based_inclusive() {
        let c = "L0\nL1\nL2\nL3";
        assert_eq!(extract_body(c, 1, 2), "L1\nL2", "0-based inclusive");
        assert_eq!(extract_body(c, 0, 0), "L0", "single line");
        assert_eq!(extract_body(c, 0, 3), c, "whole file");
        assert_eq!(extract_body(c, 2, 99), "L2\nL3", "end clamps to file");
    }

    #[test]
    fn enrichable_kinds_exclude_reliably_labelled_noncallables() {
        // Functions/methods are enrichable — but so are the `unknown`/`trait`
        // labels rust-analyzer (mis)assigns to real Rust functions, which is the
        // whole reason this is a deny-list rather than an allow-list.
        assert!(is_enrichable_kind("function"));
        assert!(is_enrichable_kind("method"));
        assert!(
            is_enrichable_kind("unknown"),
            "RA labels most Rust fns 'unknown'"
        );
        assert!(
            is_enrichable_kind("trait"),
            "RA labels some methods 'trait'"
        );
        // Reliably-labelled non-callables stay excluded.
        assert!(!is_enrichable_kind("struct"));
        assert!(!is_enrichable_kind("module"));
        assert!(!is_enrichable_kind("enum"));
        assert!(!is_enrichable_kind("field"));
    }

    #[test]
    fn parse_well_formed() {
        let (s, a) = parse_symbol_response(
            "SUMMARY: It decides where the request runs.\nASKS: Which machine handles it? What if it is down?",
        );
        assert_eq!(s, "It decides where the request runs.");
        assert_eq!(a, vec!["Which machine handles it?", "What if it is down?"]);
    }

    #[test]
    fn parse_tolerates_missing_asks_and_labels() {
        let (s, a) = parse_symbol_response("SUMMARY: Just a summary.");
        assert_eq!(s, "Just a summary.");
        assert!(a.is_empty());

        // No labels at all: whole text is the summary.
        let (s2, a2) = parse_symbol_response("It picks a model.");
        assert_eq!(s2, "It picks a model.");
        assert!(a2.is_empty());
    }

    #[test]
    fn parse_strips_list_markers_and_quotes() {
        let (s, a) = parse_symbol_response(
            "SUMMARY: \"Routes the work.\"\nASKS:\n1. Where does it go?\n2. What is the fallback?",
        );
        assert_eq!(s, "Routes the work.");
        assert_eq!(a, vec!["Where does it go?", "What is the fallback?"]);
    }

    #[test]
    fn parse_is_case_insensitive_on_labels() {
        let (s, a) = parse_symbol_response("summary: lower works.\nAsks: really? yes?");
        assert_eq!(s, "lower works.");
        assert_eq!(a, vec!["really?", "yes?"]);
    }

    #[tokio::test]
    async fn enrich_symbol_produces_enrichment() {
        let calls = Arc::new(AtomicUsize::new(0));
        let chat = fake_chat(
            "SUMMARY: It chooses which computer answers.\nASKS: Where does my request go? What if none are free?",
            calls.clone(),
        );
        let e = enrich_symbol(&chat, meta("select_route"), "fn select_route() {}")
            .await
            .expect("ok");
        assert_eq!(e.meta.name, "select_route");
        assert_eq!(e.summary, "It chooses which computer answers.");
        assert_eq!(e.asks.len(), 2);
        assert_eq!(e.body_hash, body_hash("fn select_route() {}"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn enrich_symbol_errors_on_empty_summary() {
        let calls = Arc::new(AtomicUsize::new(0));
        let chat = fake_chat("ASKS: only questions? no summary?", calls.clone());
        let r = enrich_symbol(&chat, meta("f"), "fn f() {}").await;
        assert!(matches!(r, Err(Error::Extraction(_))));
    }

    #[tokio::test]
    async fn incremental_skips_unchanged_bodies() {
        let calls = Arc::new(AtomicUsize::new(0));
        let chat = fake_chat("SUMMARY: does a thing.\nASKS: a? b?", calls.clone());

        // First pass: empty prior -> everything regenerates.
        let syms = vec![
            SymbolSource {
                kind: PromptKind::Callable,
                meta: meta("f"),
                body: "fn f() { 1 }".to_string(),
            },
            SymbolSource {
                kind: PromptKind::Callable,
                meta: meta("g"),
                body: "fn g() { 2 }".to_string(),
            },
        ];
        let prior = HashMap::new();
        let (set, rep) = enrich_symbols_incremental(&chat, syms.clone(), &prior).await;
        assert_eq!(rep.regenerated, 2);
        assert_eq!(rep.reused, 0);
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        // Build the cache from the first pass.
        let cache: HashMap<String, SymbolEnrichment> =
            set.into_iter().map(|e| (e.cache_key.clone(), e)).collect();

        // Second pass: f unchanged (reused, no call), g body edited (one call).
        calls.store(0, Ordering::SeqCst);
        let syms2 = vec![
            SymbolSource {
                kind: PromptKind::Callable,
                meta: meta("f"),
                body: "fn f() { 1 }".to_string(),
            },
            SymbolSource {
                kind: PromptKind::Callable,
                meta: meta("g"),
                body: "fn g() { 2 + 2 }".to_string(),
            },
        ];
        let (_set2, rep2) = enrich_symbols_incremental(&chat, syms2, &cache).await;
        assert_eq!(rep2.reused, 1, "f reused");
        assert_eq!(rep2.regenerated, 1, "g re-summarized");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "only the changed body cost a call"
        );
    }

    #[tokio::test]
    async fn incremental_rename_same_body_is_free() {
        let calls = Arc::new(AtomicUsize::new(0));
        let chat = fake_chat("SUMMARY: a job.\nASKS: x? y?", calls.clone());
        let body = "fn original() { work() }";
        let (set, _) = enrich_symbols_incremental(
            &chat,
            vec![SymbolSource {
                kind: PromptKind::Callable,
                meta: meta("original"),
                body: body.to_string(),
            }],
            &HashMap::new(),
        )
        .await;
        let cache: HashMap<String, SymbolEnrichment> =
            set.into_iter().map(|e| (e.cache_key.clone(), e)).collect();

        // Rename: identical body, new name -> reused, meta refreshed, zero calls.
        calls.store(0, Ordering::SeqCst);
        let (set2, rep) = enrich_symbols_incremental(
            &chat,
            vec![SymbolSource {
                kind: PromptKind::Callable,
                meta: meta("renamed"),
                body: body.to_string(),
            }],
            &cache,
        )
        .await;
        assert_eq!(rep.reused, 1);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "rename with same body is free"
        );
        assert_eq!(
            set2[0].meta.name, "renamed",
            "meta refreshed to the new name"
        );
        assert_eq!(set2[0].summary, "a job.", "summary carried over");
    }

    /// The trap this fix exists for: the cache was keyed on the body alone, so
    /// a prompt improvement could never dislodge a bad summary. The key must
    /// move when the prompt does — and must NOT move when only the body is the
    /// same but the kind differs.
    #[test]
    fn the_cache_key_separates_prompts_so_a_prompt_fix_can_take_effect() {
        let body = "fn f() { g(); }";
        let as_fn = cache_key(PromptKind::Callable, body);
        let as_ty = cache_key(PromptKind::Type, body);
        assert_ne!(
            as_fn, as_ty,
            "one body asked two ways must not share a cache entry"
        );
        assert!(as_fn.contains(&body_hash(body)), "{as_fn}");
        assert!(as_fn.ends_with(&format!("fnv{PROMPT_VERSION}")), "{as_fn}");
        // Same inputs, same key — the reuse path still works.
        assert_eq!(as_fn, cache_key(PromptKind::Callable, body));
    }

    fn enr_for(name: &str, body_hash: &str) -> SymbolEnrichment {
        SymbolEnrichment {
            meta: meta(name),
            body_hash: body_hash.to_string(),
            cache_key: String::new(),
            summary: format!("summary of {name}"),
            asks: vec![],
        }
    }

    #[test]
    fn change_set_detects_changed_new_and_removed() {
        // prior: f@h1, g@h2 ; refreshed: f@h1 (unchanged), g@h2b (body changed),
        // h@h3 (new). g removed-from-prior? no — gone is `x`.
        let prior = vec![enr_for("f", "h1"), enr_for("g", "h2"), enr_for("x", "hx")];
        let refreshed = vec![enr_for("f", "h1"), enr_for("g", "h2b"), enr_for("h", "h3")];
        let cs = diff_code_intel_caches(&prior, &refreshed);
        // f unchanged → absent; g body changed + h new → changed; x vanished → removed.
        // doc-anchor for meta(name) is the qualified_name `crate::<name>`.
        assert!(
            cs.changed.contains("crate::g"),
            "g body changed: {:?}",
            cs.changed
        );
        assert!(
            cs.changed.contains("crate::h"),
            "h is new: {:?}",
            cs.changed
        );
        assert!(
            !cs.changed.contains("crate::f"),
            "f unchanged must not appear"
        );
        assert_eq!(cs.changed.len(), 2);
        assert_eq!(
            cs.removed.iter().cloned().collect::<Vec<_>>(),
            vec!["crate::x"]
        );
    }

    #[test]
    fn change_set_empty_when_identical() {
        let snap = vec![enr_for("f", "h1"), enr_for("g", "h2")];
        let cs = diff_code_intel_caches(&snap, &snap);
        assert!(cs.is_empty(), "identical snapshots → no change: {cs:?}");
    }
}
