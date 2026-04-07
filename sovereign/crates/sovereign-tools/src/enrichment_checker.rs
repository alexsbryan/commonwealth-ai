//! `EnrichmentChecker` — health-checks enrichment coverage for every corpus.
//!
//! Only fires for corpora where `enrichment_enabled = true` in the index meta.

use std::sync::Arc;

use corpus_engine::CorpusEngine;
use sovereign_core::error::{Error, Result};
use sovereign_core::health::{
    Component, HealthCheckable, HealthIssue, HealthReport, RepairKind,
    RepairOutcome,
};

/// Coverage below this fraction triggers `LowEnrichmentCoverage`.
const COVERAGE_THRESHOLD: f32 = 0.80;

// ─── EnrichmentChecker ───────────────────────────────────────────────────────

pub struct EnrichmentChecker {
    engine: Arc<CorpusEngine>,
}

impl EnrichmentChecker {
    pub fn new(engine: Arc<CorpusEngine>) -> Self {
        Self { engine }
    }
}

// ── HealthCheckable impl ──────────────────────────────────────────────────────

impl HealthCheckable for EnrichmentChecker {
    fn component(&self) -> Component {
        Component::Enrichment("*".into())
    }

    fn check(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<HealthReport>> + Send + '_>>
    {
        Box::pin(async move {
            let indexes = self
                .engine
                .installed_indexes()
                .await
                .map_err(|e| Error::Other(Box::new(e)))?;

            let mut issues = Vec::new();

            for info in &indexes {
                if !info.enrichment_enabled {
                    continue;
                }
                let corpus_id = info.corpus_id.clone();
                let total = info.chunk_count;
                if total == 0 {
                    continue;
                }

                // ── Stale enrichment ────────────────────────────────────────
                if let Ok(index) = self.engine.open_index_for_corpus(&corpus_id).await {
                    let stale = index
                        .stale_claim_count()
                        .await
                        .map_err(|e| Error::Other(Box::new(e)))?;
                    if stale > 0 {
                        issues.push(HealthIssue::StaleEnrichment {
                            corpus_id: corpus_id.clone(),
                            stale_claim_count: stale,
                        });
                    }

                    // ── Orphaned enrichment ──────────────────────────────────
                    let orphans = index
                        .find_stale_claims(usize::MAX)
                        .await
                        .map_err(|e| Error::Other(Box::new(e)))?;
                    let orphan_count = orphans.len() as u64;
                    if orphan_count > 0 {
                        issues.push(HealthIssue::OrphanedEnrichment {
                            corpus_id: corpus_id.clone(),
                            orphan_claim_count: orphan_count,
                        });
                    }
                }

                // ── Coverage ────────────────────────────────────────────────
                if let Some(enriched) = info.enriched_chunks {
                    let coverage = enriched as f32 / total as f32;
                    if coverage < COVERAGE_THRESHOLD && enriched > 0 {
                        issues.push(HealthIssue::LowEnrichmentCoverage {
                            corpus_id: corpus_id.clone(),
                            enriched_chunks: enriched,
                            total_chunks: total,
                            coverage_pct: coverage * 100.0,
                            threshold_pct: COVERAGE_THRESHOLD * 100.0,
                        });
                    }
                }
            }

            let component = match indexes.len() {
                1 => Component::Enrichment(indexes[0].corpus_id.clone()),
                _ => Component::Enrichment("*".into()),
            };
            Ok(HealthReport::from_issues(component, issues))
        })
    }

    fn repair(
        &self,
        issue: &HealthIssue,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<RepairOutcome>> + Send + '_>,
    > {
        let issue = issue.clone();
        Box::pin(async move {
            match &issue {
                HealthIssue::StaleEnrichment { corpus_id, .. } | HealthIssue::OrphanedEnrichment { corpus_id, .. } => {
                    Ok(RepairOutcome::NeedsUserDecision {
                        question: format!(
                            "Corpus `{corpus_id}` has stale/orphaned enrichment data. \
                             Delete and re-extract claims for affected chunks?"
                        ),
                        options: vec![
                            sovereign_core::health::UserOption {
                                kind: RepairKind::RefreshEnrichment,
                                label: "Refresh enrichment".into(),
                                description: "Delete stale claims and re-run extraction.".into(),
                            },
                            sovereign_core::health::UserOption {
                                kind: RepairKind::Dismiss,
                                label: "Dismiss".into(),
                                description: "Ignore — stale claims may degrade search quality.".into(),
                            },
                        ],
                        consequence: "Re-extraction will use inference credits.".into(),
                    })
                }
                HealthIssue::LowEnrichmentCoverage { corpus_id, .. } => {
                    Ok(RepairOutcome::NeedsUserDecision {
                        question: format!(
                            "Corpus `{corpus_id}` has low enrichment coverage. \
                             Run full enrichment now?"
                        ),
                        options: vec![
                            sovereign_core::health::UserOption {
                                kind: RepairKind::RefreshEnrichment,
                                label: "Run full enrichment".into(),
                                description: "Extract claims for all un-enriched chunks.".into(),
                            },
                            sovereign_core::health::UserOption {
                                kind: RepairKind::Dismiss,
                                label: "Dismiss".into(),
                                description: "Ignore — epistemic search will be partial.".into(),
                            },
                        ],
                        consequence: "Full enrichment may take a long time and use inference credits.".into(),
                    })
                }
                _ => Err(Error::RepairNotSupported),
            }
        })
    }

    fn can_repair_autonomously(&self, _issue: &HealthIssue) -> bool {
        // All enrichment repairs require user authorisation (inference cost).
        false
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_threshold_constant() {
        assert!(COVERAGE_THRESHOLD > 0.0 && COVERAGE_THRESHOLD < 1.0);
    }
}
