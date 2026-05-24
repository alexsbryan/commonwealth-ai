//! Source-recovery prompt fragments shared across typed-extension
//! consumers.
//!
//! Empirical driver: 2026-05-24 obsidian-vault bench surfaced that
//! the typed-extension pass over RAPTOR cluster summaries produced
//! technically-valid atoms whose names had been paraphrased away from
//! the source vocabulary ("PBM administrative fee expansion" instead
//! of "spread pricing"). RAPTOR's per-leaf summariser strips
//! distinctive verbatim phrases by design (its prompt forbids quote
//! marks); typed-extraction over those summaries inherits the loss.
//!
//! The discipline text in `source_recovery_discipline.md` is the
//! mitigation. Spliced into any LLM-pass user body that operates on
//! a RAPTOR summary, it instructs the model to PREFER verbatim
//! phrasings from the surfaced source excerpts over paraphrase from
//! the summary itself.
//!
//! Public surface:
//!
//! - [`SOURCE_RECOVERY_DISCIPLINE`] — the discipline text as loaded
//!   from the asset file.
//! - [`render_source_recovery_block`] — formats a slice of
//!   [`QuoteSpan`]s into the canonical "Verbatim source excerpts"
//!   markdown block + appends the discipline text. The exact shape
//!   produced by `sovereign_tools::typed_extension::pass`'s Pass A
//!   and Pass B user bodies.
//!
//! Asset, not const, per ARCH_PRINCIPLES §6.2: text an operator
//! might reasonably tune (the discipline phrasing has already been
//! iterated once and will likely be tuned again as more pipelines
//! consume it) belongs alongside its consumer rather than baked into
//! Rust.

/// Discipline text loaded from the asset file. Spliced verbatim into
/// the user body — see [`render_source_recovery_block`] for the
/// canonical splice.
pub const SOURCE_RECOVERY_DISCIPLINE: &str = include_str!("source_recovery_discipline.md");

/// Belt-and-braces truncation cap on any one verbatim excerpt
/// surfaced into a prompt. RAPTOR caps spans around full sentences;
/// this guards against a future summariser change emitting
/// multi-sentence spans that would balloon the prompt.
pub const SOURCE_RECOVERY_QUOTE_CHAR_CAP: usize = 320;

/// Render the canonical source-recovery block: a markdown
/// "Verbatim source excerpts" list followed by the discipline text.
///
/// Pass an empty slice to render the discipline text alone (no
/// excerpts header) — useful for callers that have no quote_spans
/// to surface but still want the naming discipline applied.
///
/// `excerpts` is a slice of verbatim source-sentence strings. The
/// renderer doesn't need chunk_id or character offsets for prompt
/// formatting — callers preserving chunk-level provenance (e.g.
/// `sovereign_tools::typed_extension`) thread the structured
/// citation through separate machinery
/// (`SourceCitation` + `apply_citation`).
///
/// `corpus-engine` deliberately does NOT depend on
/// `sovereign-core`, so this helper avoids `QuoteSpan` as a
/// parameter shape — `&[&str]` keeps the dep direction one-way per
/// ARCH §8.3.
///
/// Output shape:
///
/// ```markdown
/// **Verbatim source excerpts (sentences pulled from the source chunks underneath this cluster):**
///
/// > [0] First quote text here.
/// > [1] Second quote text here.
///
/// **Atom-naming discipline (load-bearing for source recovery):**
/// …discipline text…
/// ```
///
/// The exact wording of the "Verbatim source excerpts" header is
/// stable — its presence is asserted by the typed_extension unit
/// tests so a rename here will surface as a test failure rather
/// than a silent prompt drift.
pub fn render_source_recovery_block(excerpts: &[&str]) -> String {
    let mut body = String::new();
    if !excerpts.is_empty() {
        body.push_str(
            "**Verbatim source excerpts (sentences pulled from the source chunks underneath this cluster):**\n\n",
        );
        for (i, span) in excerpts.iter().enumerate() {
            let trimmed = truncate_quote(span);
            body.push_str(&format!("> [{i}] {trimmed}\n"));
        }
        body.push('\n');
    }
    body.push_str(SOURCE_RECOVERY_DISCIPLINE);
    body
}

fn truncate_quote(span: &str) -> String {
    let trimmed = span.trim();
    if trimmed.chars().count() <= SOURCE_RECOVERY_QUOTE_CHAR_CAP {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(SOURCE_RECOVERY_QUOTE_CHAR_CAP).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discipline_text_loads_from_asset() {
        // The asset is the contract — every consumer reading
        // SOURCE_RECOVERY_DISCIPLINE relies on these key tokens
        // being present. A gut-renaming in the markdown breaks the
        // splice silently otherwise.
        assert!(SOURCE_RECOVERY_DISCIPLINE.contains("Atom-naming discipline"));
        assert!(SOURCE_RECOVERY_DISCIPLINE.contains("Prefer verbatim phrasings"));
        assert!(SOURCE_RECOVERY_DISCIPLINE.contains("Do NOT invent prose names"));
        assert!(SOURCE_RECOVERY_DISCIPLINE.contains("Opposition labels are SHORT"));
        assert!(SOURCE_RECOVERY_DISCIPLINE.contains("Evidence labels lead with the distinctive token"));
        assert!(SOURCE_RECOVERY_DISCIPLINE.contains("primary_entities"));
    }

    #[test]
    fn render_surfaces_each_excerpt_with_index() {
        let body = render_source_recovery_block(&[
            "First quote text.",
            "Second quote text.",
        ]);
        assert!(body.contains("Verbatim source excerpts"));
        assert!(body.contains("> [0] First quote text."));
        assert!(body.contains("> [1] Second quote text."));
        assert!(body.contains("Atom-naming discipline"));
    }

    #[test]
    fn render_without_excerpts_emits_discipline_only() {
        let body = render_source_recovery_block(&[]);
        assert!(!body.contains("Verbatim source excerpts"));
        assert!(body.contains("Atom-naming discipline"));
    }

    #[test]
    fn render_truncates_overly_long_excerpts() {
        let long = "a".repeat(SOURCE_RECOVERY_QUOTE_CHAR_CAP + 50);
        let body = render_source_recovery_block(&[&long]);
        assert!(body.contains('…'));
    }
}
