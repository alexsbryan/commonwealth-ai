// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared types passed between the five pipeline stages.
//!
//! These are the wire-shape the Curator emits and the Drafter +
//! Presenter consume. They double as the JSON-schema target the
//! Curator's grammar-constrained Fast-slot completion is bound to,
//! so additions here flow straight into the Curator's structured
//! output and into the Drafter's prompt assembly without separate
//! plumbing changes.

use serde::{Deserialize, Serialize};

/// Alias for the candidate chunk shape the Retriever produces.
/// `corpus_engine::ScoredChunk` already carries content + provenance
/// + score; the pipeline doesn't need a wrapper, just a stable name
/// per the plan's vocabulary so reading the curator code lines up
/// with the spec.
pub type RetrievedChunk = corpus_engine::ScoredChunk;

/// One section of the Drafter's response skeleton. The Curator
/// authors these from the question's intent and the candidate
/// chunks; the Drafter then expands each section into prose,
/// constrained to its `target_tokens` budget. Section order is
/// significant — the Drafter generates sections in order, top to
/// bottom.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkeletonSection {
    /// User-facing label. Rendered as a heading in the final
    /// output ("Compatibilist position", "Where they disagree").
    pub label: String,
    /// One-sentence prompt to the Drafter explaining what this
    /// section should accomplish — *not* a header text. Drives
    /// the Drafter's per-section system instruction so it stays
    /// inside the Curator's plan rather than freelancing.
    pub purpose: String,
    /// Indices into [`CuratedPackage::kept_chunks`] for the
    /// chunks this section should draw from. Stored as `usize`
    /// indices rather than re-embedding chunks so an LLM-emitted
    /// payload is small and the Drafter can deduplicate references.
    pub chunk_refs: Vec<usize>,
    /// Per-section completion-token cap. The sum across sections
    /// must be ≤ the parent request's `max_tokens`; the Curator
    /// is responsible for keeping that invariant. Drafter enforces
    /// the cap on its own emission.
    pub target_tokens: u32,
}

impl SkeletonSection {
    /// Trivial single-section skeleton used by the
    /// [`should_curate`](crate::pipeline::should_curate) bypass —
    /// the curator skips Fast-slot work for short candidate sets
    /// and intents that don't need structured planning.
    pub fn passthrough(chunk_count: usize, target_tokens: u32) -> Self {
        Self {
            label: String::new(),
            purpose: "Pass-through (bypass): Drafter generates a single \
                      flat response over all retrieved chunks."
                .to_string(),
            chunk_refs: (0..chunk_count).collect(),
            target_tokens,
        }
    }
}

/// Whether the curated package is enough to draft a substantive
/// answer. The variants drive the glass-box honesty short-circuit
/// in `run_team_pipeline`: `Insufficient` skips the Drafter
/// entirely and routes the Presenter to an honest *"I don't have
/// grounding for this"* message — the alternative being a
/// confident parametric fabrication, which the situated principle
/// explicitly names as the failure mode this exists to prevent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Sufficiency {
    /// Candidates address the question with reasonable coverage.
    /// Drafter proceeds normally.
    Sufficient,
    /// Candidates address part of the question; named gaps remain.
    /// Drafter proceeds and the Presenter surfaces the gaps as
    /// caveats so the user can decide whether to expand the search.
    Partial { gaps: Vec<String> },
    /// Candidates do not actually answer the question. Drafter is
    /// skipped; Presenter shapes a direct honest message offering
    /// the user a way forward (`suggested_action` is the offer
    /// surfaced in that message — e.g. "install <corpus>" or
    /// "answer from general knowledge with that caveat").
    Insufficient {
        reason: String,
        suggested_action: String,
    },
}

/// Per-section + ceiling token budget for the Drafter. The Curator
/// composes this from its own per-section targets so the Drafter
/// has a single value to clamp `max_tokens` against without having
/// to re-derive it from the skeleton.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DraftBudget {
    /// Hard ceiling on Drafter completion tokens. Equal to the
    /// caller's `max_tokens` (or the model's context budget if the
    /// caller supplied no cap) — the Drafter must not exceed.
    pub ceiling_tokens: u32,
    /// Sum of per-section `target_tokens`. Used as a soft target
    /// during generation; the Drafter aims for this and is allowed
    /// to overshoot up to `ceiling_tokens`.
    pub target_tokens: u32,
}

impl DraftBudget {
    /// Budget for the bypass / passthrough case: one section, full
    /// budget, no per-section subdivision.
    pub fn full(max_tokens: u32) -> Self {
        Self {
            ceiling_tokens: max_tokens,
            target_tokens: max_tokens,
        }
    }
}

/// The Curator's output. Carried through the pipeline as the
/// Drafter's input; the Presenter sees the Drafter's draft +
/// (optionally) this package's `sufficiency` so it can shape an
/// honest message on the `Insufficient` short-circuit path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CuratedPackage {
    /// The chunks the Drafter is allowed to cite. Index space
    /// referenced by [`SkeletonSection::chunk_refs`]. 0..=8 typical;
    /// 0 only on the `Insufficient` path. The Curator picks these
    /// from the Retriever's candidate set (~20 chunks today) by
    /// clustering by position and dropping low-relevance noise.
    pub kept_chunks: Vec<RetrievedChunk>,
    /// Ordered sections the Drafter should fill. Length 0 only on
    /// `Insufficient`. The Drafter renders sections sequentially.
    pub skeleton: Vec<SkeletonSection>,
    /// Whether the package is enough to draft. Drives the
    /// glass-box short-circuit. See [`Sufficiency`].
    pub sufficiency: Sufficiency,
    /// Token budget for the Drafter. Composed from the skeleton's
    /// per-section targets.
    pub draft_budget: DraftBudget,
}

impl CuratedPackage {
    /// Bypass package used when [`should_curate`](crate::pipeline::should_curate)
    /// returns false. Forwards every candidate to the Drafter
    /// untouched, with a single passthrough skeleton section
    /// covering the full budget — the cheap path for SimpleQuery
    /// and tiny candidate sets where curation has no room to add
    /// value.
    pub fn passthrough(candidates: Vec<RetrievedChunk>, max_tokens: u32) -> Self {
        let chunk_count = candidates.len();
        Self {
            kept_chunks: candidates,
            skeleton: vec![SkeletonSection::passthrough(chunk_count, max_tokens)],
            sufficiency: Sufficiency::Sufficient,
            draft_budget: DraftBudget::full(max_tokens),
        }
    }

    /// Insufficient-grounding package: zero kept chunks, empty
    /// skeleton, the honest message routed to the Presenter via
    /// `Sufficiency::Insufficient`. Used when the Curator
    /// determines the candidates do not actually answer the
    /// question; the Drafter is skipped on this path.
    pub fn insufficient(reason: String, suggested_action: String, max_tokens: u32) -> Self {
        Self {
            kept_chunks: Vec::new(),
            skeleton: Vec::new(),
            sufficiency: Sufficiency::Insufficient {
                reason,
                suggested_action,
            },
            draft_budget: DraftBudget::full(max_tokens),
        }
    }

    /// Render this curated package as the Drafter's prompt body.
    /// Replaces the legacy "dump 13K chars of chunks" approach: the
    /// Drafter sees a structured task — labelled sections with
    /// per-section purpose + token budget, each pointing at the
    /// chunks it should draw from.
    ///
    /// The output is plain text with `<section>` markers around
    /// each section so a fine-tuned model could grammar-constrain
    /// its output by section, but the format is also legible to a
    /// vanilla model — the Drafter's system prompt teaches it to
    /// honour the section boundaries.
    ///
    /// Per the situated-team plan §2.4, this lives with
    /// [`CuratedPackage`] rather than as a free function in
    /// `commonwealth-knowledge::grounding` (the plan's original
    /// home for it) because the dep direction
    /// (`commonwealth-knowledge` has no `sovereign-core` dep) makes
    /// the shared type unavailable there. The intent matches: one
    /// formatter, alongside the type it formats.
    pub fn format_for_drafter(&self) -> String {
        if matches!(self.sufficiency, Sufficiency::Insufficient { .. }) {
            // The Drafter never sees this path — `run_team_pipeline`
            // short-circuits to the Presenter on Insufficient. We
            // return a sentinel so accidental formatting in tests
            // surfaces the missing short-circuit instead of feeding
            // the Drafter an empty package.
            return "<!-- curator: insufficient grounding; Drafter \
                    should be skipped on this path -->"
                .to_string();
        }

        let mut out = String::with_capacity(
            self.kept_chunks
                .iter()
                .map(|c| c.content.len())
                .sum::<usize>()
                + 1024,
        );

        // Chunk catalogue first — addressed by index inside each
        // <section>. Keeping it once at the top (rather than
        // inlined per section) avoids repeating chunk text when a
        // chunk is referenced by multiple sections.
        out.push_str("<chunks>\n");
        for (i, c) in self.kept_chunks.iter().enumerate() {
            let title = c.title.as_deref().unwrap_or("(untitled)");
            out.push_str(&format!(
                "  <chunk id=\"{i}\" corpus=\"{corpus}\" score=\"{score:.3}\" title=\"{title}\">\n",
                corpus = c.corpus_id,
                score = c.score,
                title = escape_xml_attr(title),
            ));
            out.push_str(&indent(&c.content, "    "));
            if !c.content.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("  </chunk>\n");
        }
        out.push_str("</chunks>\n\n");

        // Sections in order. The Drafter generates section bodies
        // one at a time, top-to-bottom, clamped to per-section
        // target_tokens.
        for (i, section) in self.skeleton.iter().enumerate() {
            let refs = section
                .chunk_refs
                .iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
                .join(",");
            out.push_str(&format!(
                "<section index=\"{i}\" budget=\"{budget}\" chunk_refs=\"{refs}\" \
                 label=\"{label}\">\n  <purpose>{purpose}</purpose>\n</section>\n",
                budget = section.target_tokens,
                label = escape_xml_attr(&section.label),
                purpose = escape_xml_text(&section.purpose),
            ));
        }

        // Sufficiency footer — surfaces partial gaps so the
        // Drafter can label them in-line if the section purpose
        // didn't already.
        if let Sufficiency::Partial { gaps } = &self.sufficiency {
            if !gaps.is_empty() {
                out.push_str("\n<gaps>\n");
                for gap in gaps {
                    out.push_str(&format!("  <gap>{}</gap>\n", escape_xml_text(gap)));
                }
                out.push_str("</gaps>\n");
            }
        }

        out
    }
}

fn escape_xml_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_xml_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn indent(s: &str, prefix: &str) -> String {
    s.lines()
        .map(|l| format!("{prefix}{l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(idx: usize, content: &str) -> RetrievedChunk {
        RetrievedChunk {
            content: content.to_string(),
            title: Some(format!("Title {idx}")),
            url: None,
            corpus_id: "test".into(),
            score: 0.42 + idx as f32 * 0.01,
            metadata: Default::default(),
            chunk_id: None,
            source_doc_id: None,
            vector_distance: None,
        }
    }

    #[test]
    fn format_passthrough_renders_chunks_and_one_section() {
        let pkg = CuratedPackage::passthrough(vec![chunk(0, "alpha"), chunk(1, "bravo")], 512);
        let s = pkg.format_for_drafter();
        assert!(s.contains("<chunks>"));
        assert!(s.contains("alpha"));
        assert!(s.contains("bravo"));
        assert!(s.contains("<section index=\"0\""));
        assert!(s.contains("budget=\"512\""));
        assert!(s.contains("chunk_refs=\"0,1\""));
    }

    #[test]
    fn format_insufficient_returns_sentinel() {
        let pkg = CuratedPackage::insufficient(
            "Off-domain corpus".into(),
            "install philosophy".into(),
            512,
        );
        let s = pkg.format_for_drafter();
        assert!(s.contains("insufficient grounding"));
        assert!(s.contains("Drafter should be skipped"));
    }

    #[test]
    fn format_partial_emits_gaps_block() {
        let pkg = CuratedPackage {
            kept_chunks: vec![chunk(0, "alpha")],
            skeleton: vec![SkeletonSection {
                label: "Definition".into(),
                purpose: "Anchor on the term.".into(),
                chunk_refs: vec![0],
                target_tokens: 200,
            }],
            sufficiency: Sufficiency::Partial {
                gaps: vec!["historical context".into(), "criticisms".into()],
            },
            draft_budget: DraftBudget {
                ceiling_tokens: 512,
                target_tokens: 200,
            },
        };
        let s = pkg.format_for_drafter();
        assert!(s.contains("<gaps>"));
        assert!(s.contains("historical context"));
        assert!(s.contains("criticisms"));
    }

    #[test]
    fn format_escapes_xml_metacharacters_in_titles_and_purposes() {
        let mut chunk = chunk(0, "content");
        chunk.title = Some(r#"weird "title" with <tags> & ampersands"#.into());
        let pkg = CuratedPackage {
            kept_chunks: vec![chunk],
            skeleton: vec![SkeletonSection {
                label: r#"label "with" quotes"#.into(),
                purpose: "purpose with <foo> & bar".into(),
                chunk_refs: vec![0],
                target_tokens: 100,
            }],
            sufficiency: Sufficiency::Sufficient,
            draft_budget: DraftBudget {
                ceiling_tokens: 256,
                target_tokens: 100,
            },
        };
        let s = pkg.format_for_drafter();
        // No raw `"` in attribute values, no raw `<` outside our
        // own tag openings.
        assert!(s.contains("&quot;title&quot;"));
        assert!(s.contains("&lt;tags&gt;"));
        assert!(s.contains("&lt;foo&gt;"));
        assert!(s.contains("&amp;"));
    }
}
