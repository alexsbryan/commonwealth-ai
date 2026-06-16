// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pure check functions for the deterministic rungs 1–3 (Extract / Filter /
//! Chunk). Each is `(stage output, …) -> StageResult` with no I/O and no
//! judgment beyond the printed threshold.

use corpus_engine::harness::{
    coverage, doc_id, recipe_hash, CaptureManifest, ChunkOutput, CoverageUnit, EnrichOutput,
    ExtractOutput, FilterOutput, IndexOutput, StageOutputs,
};
use corpus_engine::Recipe;

use super::declaration::Declaration;
use super::{config_hash, CheckId, EvidenceItem, HarnessRun, Locus, StageResult, Status, Verdict};

/// Assemble the deterministic rungs (Acquire → Index) into a `HarnessRun`.
/// `sample_id` comes from the frozen capture (fixed across the iterate loop);
/// `recipe_hash` is computed from the CURRENT recipe under test — so an edit to
/// the TOML changes the run identity while the sample stays fixed (I1).
pub fn run_deterministic(
    manifest: &CaptureManifest,
    recipe: &Recipe,
    outputs: &StageOutputs,
    enrich: Option<&EnrichOutput>,
    decl: &Declaration,
) -> HarnessRun {
    let mut stages = vec![
        check_acquire(manifest),
        check_extract(&outputs.extract, recipe, decl),
        check_filter(&outputs.filter, recipe),
        check_chunk(&outputs.chunk, recipe),
        check_index(&outputs.index, recipe),
    ];
    // Rung 6 only when the author opted into enrichment and atoms were produced.
    if let Some(e) = enrich {
        stages.push(check_enrich(e));
    }
    HarnessRun {
        sample_id: manifest.sample_id.clone(),
        recipe_hash: recipe_hash(recipe),
        stages,
    }
}

/// Rung 6 — Enrich link-integrity. Every atom evidence id resolves to a real
/// chunk; every cited quote is a verbatim substring of its chunk. (The numeric
/// audit's SSOT lives in the runtime layer and targets synthesized prose, not
/// atoms — deferred rather than duplicated here.)
pub fn check_enrich(out: &EnrichOutput) -> StageResult {
    let verdicts = vec![
        Verdict {
            check: CheckId::EnrichLinkIntegrity,
            status: if out.unresolved.is_empty() {
                Status::Pass
            } else {
                Status::Fail
            },
            expected: "every atom evidence id resolves to a real chunk".into(),
            observed: format!(
                "{} unresolved of {} refs across {} atoms",
                out.unresolved.len(),
                out.refs,
                out.atoms
            ),
            evidence: out
                .unresolved
                .iter()
                .take(5)
                .map(|m| EvidenceItem {
                    locus: Locus::Atom(m.atom_id.clone()),
                    excerpt: format!("cites chunk {} — does not resolve", m.chunk_id),
                })
                .collect(),
        },
        Verdict {
            check: CheckId::EnrichLinkIntegrity,
            status: if out.non_verbatim.is_empty() {
                Status::Pass
            } else {
                Status::Fail
            },
            expected: "every cited quote is a verbatim substring of its chunk".into(),
            observed: format!("{} non-verbatim of {} refs", out.non_verbatim.len(), out.refs),
            evidence: out
                .non_verbatim
                .iter()
                .take(5)
                .map(|m| EvidenceItem {
                    locus: Locus::Atom(m.atom_id.clone()),
                    excerpt: format!("chunk {}: {:?}", m.chunk_id, m.detail),
                })
                .collect(),
        },
    ];
    StageResult {
        stage: "Enrich".into(),
        config_hash: String::new(),
        cache_hit: false,
        verdicts,
    }
}

/// Rung 5 — Acquire integrity (recorded at capture, then frozen — I3). At least
/// one document, none empty; the evidence is a sample of the frozen source
/// files with sizes and content-type hints, plus the acquirer display.
pub fn check_acquire(m: &CaptureManifest) -> StageResult {
    let n = m.docs.len();
    let empty: Vec<&str> = m
        .docs
        .iter()
        .filter(|d| d.empty)
        .map(|d| d.doc_id.as_str())
        .collect();
    let verdicts = vec![
        Verdict {
            check: CheckId::AcquireIntegrity,
            status: if n > 0 { Status::Pass } else { Status::Fail },
            expected: "acquires ≥1 document".into(),
            observed: format!("{n} docs from {}", m.acquirer),
            evidence: m
                .source_files
                .iter()
                .take(3)
                .map(|f| EvidenceItem {
                    locus: Locus::Doc(f.rel_path.clone()),
                    excerpt: format!(
                        "{} bytes{}",
                        f.bytes,
                        f.content_type
                            .as_deref()
                            .map(|c| format!(", {c}"))
                            .unwrap_or_default()
                    ),
                })
                .collect(),
        },
        Verdict {
            check: CheckId::AcquireIntegrity,
            status: if empty.is_empty() {
                Status::Pass
            } else {
                Status::Fail
            },
            expected: "no empty documents".into(),
            observed: format!("{} empty of {n}", empty.len()),
            evidence: empty
                .iter()
                .take(5)
                .map(|id| EvidenceItem {
                    locus: Locus::Doc((*id).to_string()),
                    excerpt: "document extracted with empty content".into(),
                })
                .collect(),
        },
    ];
    StageResult {
        stage: "Acquire".into(),
        config_hash: String::new(),
        cache_hit: false,
        verdicts,
    }
}

/// Rung 1 — Extract field-coverage. Every declared field must be present in
/// ≥ `min_coverage` of docs (or source files, for section extractors).
pub fn check_extract(out: &ExtractOutput, recipe: &Recipe, decl: &Declaration) -> StageResult {
    let mut verdicts = Vec::new();
    for c in coverage(&recipe.extract, &out.docs, &out.section_misses, out.source_files) {
        let ratio = if c.total == 0 {
            0.0
        } else {
            c.found as f64 / c.total as f64
        };
        let pass = ratio >= decl.min_coverage;
        let unit = match c.unit {
            CoverageUnit::Docs => "docs",
            CoverageUnit::Files => "files",
        };
        let status = if pass {
            Status::Pass
        } else if c.required {
            Status::Fail
        } else {
            Status::Warn
        };
        verdicts.push(Verdict {
            check: CheckId::ExtractCoverage,
            status,
            expected: format!(
                "{}: present in ≥{:.0}% of {unit}",
                c.label,
                decl.min_coverage * 100.0
            ),
            observed: format!("found in {}/{} {unit}", c.found, c.total),
            evidence: c
                .misses
                .iter()
                .take(5)
                .map(|m| EvidenceItem {
                    locus: Locus::Doc(m.doc_id.clone()),
                    excerpt: m.nearby_text.clone().unwrap_or_default(),
                })
                .collect(),
        });
    }
    // A doc that failed to extract at all is its own (soft) signal.
    if !out.errors.is_empty() {
        verdicts.push(Verdict {
            check: CheckId::ExtractCoverage,
            status: Status::Warn,
            expected: "all sampled docs extract without error".into(),
            observed: format!(
                "{} extraction error(s) across {} attempted",
                out.errors.len(),
                out.attempted
            ),
            evidence: out
                .errors
                .iter()
                .take(5)
                .map(|e| EvidenceItem {
                    locus: Locus::Doc("<extraction error>".into()),
                    excerpt: e.clone(),
                })
                .collect(),
        });
    }
    StageResult {
        stage: "Extract".into(),
        config_hash: config_hash(&recipe.extract),
        cache_hit: false,
        verdicts,
    }
}

/// Rung 2 — Filter kept/dropped. Keeps ≥1; a declared filter that drops 0 docs
/// is a Warn (shown, not a hard fail) — usually a misconfiguration.
pub fn check_filter(out: &FilterOutput, recipe: &Recipe) -> StageResult {
    let kept = out.kept.len();
    let dropped = out.dropped.len();
    let status = if !out.active {
        Status::Pass
    } else if kept == 0 {
        Status::Fail
    } else if dropped == 0 {
        Status::Warn
    } else {
        Status::Pass
    };
    let observed = if out.active {
        format!("kept {kept}, dropped {dropped}  ({})", out.descriptions.join("; "))
    } else {
        format!("no filter declared; {kept} docs pass through")
    };
    StageResult {
        stage: "Filter".into(),
        config_hash: config_hash(&recipe.filters),
        cache_hit: false,
        verdicts: vec![Verdict {
            check: CheckId::FilterKept,
            status,
            expected: "keeps ≥1 doc; a declared filter that drops 0 is flagged".into(),
            observed,
            evidence: out
                .dropped
                .iter()
                .take(3)
                .map(|d| EvidenceItem {
                    locus: Locus::Doc(doc_id(d)),
                    excerpt: "dropped by filter".into(),
                })
                .collect(),
        }],
    }
}

/// Rung 3 — Chunk degeneracy + size. Count > 0, no empty chunks, sizes within
/// the declared bound, not collapsed to a single chunk across multiple docs.
pub fn check_chunk(out: &ChunkOutput, recipe: &Recipe) -> StageResult {
    let n = out.chunks.len();
    let doc_count = out.per_doc_counts.len();
    let empty = out.chunks.iter().filter(|c| c.trim().is_empty()).count();
    let sizes: Vec<usize> = out.chunks.iter().map(|c| c.chars().count()).collect();
    let max_len = sizes.iter().copied().max().unwrap_or(0);
    let min_len = sizes.iter().copied().min().unwrap_or(0);
    let bounded = out.declared_max_chars != usize::MAX;
    let over = if bounded {
        sizes.iter().filter(|&&s| s > out.declared_max_chars).count()
    } else {
        0
    };
    let collapsed = n == 1 && doc_count > 1;

    let mut verdicts = vec![
        verdict(
            n > 0,
            "produces ≥1 chunk",
            &format!("{n} chunks from {doc_count} docs"),
            vec![],
        ),
        verdict(
            empty == 0,
            "no empty chunks",
            &format!("{empty} empty of {n}"),
            vec![],
        ),
    ];
    if bounded {
        let evidence = out
            .chunks
            .iter()
            .enumerate()
            .max_by_key(|(_, c)| c.chars().count())
            .filter(|_| over > 0)
            .map(|(i, c)| EvidenceItem {
                locus: Locus::Chunk(i.to_string()),
                excerpt: c.chars().take(200).collect(),
            })
            .into_iter()
            .collect();
        verdicts.push(verdict(
            over == 0,
            &format!("all chunks ≤ {} chars", out.declared_max_chars),
            &format!("{over} over limit (largest {max_len}, smallest {min_len})"),
            evidence,
        ));
    }
    verdicts.push(verdict(
        !collapsed,
        "not collapsed to a single chunk",
        &if collapsed {
            "all docs collapsed into one chunk".to_string()
        } else {
            format!("sizes {min_len}..{max_len} across {doc_count} docs")
        },
        vec![],
    ));

    StageResult {
        stage: "Chunk".into(),
        config_hash: config_hash(&recipe.chunk),
        cache_hit: false,
        verdicts,
    }
}

/// Rung 4 — Index round-trip + model-match. The index builds and opens; the
/// declared embed model is recorded; a deterministically-chosen rare token,
/// FTS-queried, returns its source chunk. Model-free (FTS keyword path).
pub fn check_index(out: &IndexOutput, recipe: &Recipe) -> StageResult {
    let mut verdicts = vec![Verdict {
        check: CheckId::IndexRoundtrip,
        status: if out.built { Status::Pass } else { Status::Fail },
        expected: "index builds and opens".into(),
        observed: if out.built {
            "built + opened (FTS, model-free)".into()
        } else {
            format!("build failed: {}", out.error.as_deref().unwrap_or("unknown"))
        },
        evidence: Vec::new(),
    }];

    if out.built {
        let model_ok = out.model_declared == out.model_recorded;
        verdicts.push(Verdict {
            check: CheckId::IndexRoundtrip,
            status: if model_ok { Status::Pass } else { Status::Fail },
            expected: format!(
                "index records the declared embed model '{}'",
                out.model_declared
            ),
            observed: format!("recorded '{}'", out.model_recorded),
            evidence: Vec::new(),
        });

        match &out.token {
            Some(tok) => verdicts.push(Verdict {
                check: CheckId::IndexRoundtrip,
                status: if out.roundtrip_ok {
                    Status::Pass
                } else {
                    Status::Fail
                },
                expected: format!("rare token \"{tok}\" (FTS) returns its source chunk"),
                observed: format!(
                    "{} hit(s); source chunk {}",
                    out.hit_count,
                    if out.roundtrip_ok {
                        "returned"
                    } else {
                        "NOT returned"
                    }
                ),
                evidence: out
                    .source_preview
                    .iter()
                    .map(|p| EvidenceItem {
                        locus: Locus::Chunk("source".into()),
                        excerpt: p.clone(),
                    })
                    .collect(),
            }),
            None => verdicts.push(Verdict {
                check: CheckId::IndexRoundtrip,
                status: Status::Warn,
                expected: "a rare token for the round-trip".into(),
                observed: "no suitable token in the sample (chunks too short?)".into(),
                evidence: Vec::new(),
            }),
        }
    }

    StageResult {
        stage: "Index".into(),
        config_hash: config_hash(&recipe.index),
        cache_hit: false,
        verdicts,
    }
}

/// A binary Pass/Fail verdict (the chunk degeneracy checks). Stages that need a
/// third `Warn` state build their `Verdict` directly.
fn verdict(pass: bool, expected: &str, observed: &str, evidence: Vec<EvidenceItem>) -> Verdict {
    Verdict {
        check: CheckId::ChunkDegeneracy,
        status: if pass { Status::Pass } else { Status::Fail },
        expected: expected.to_string(),
        observed: observed.to_string(),
        evidence,
    }
}
