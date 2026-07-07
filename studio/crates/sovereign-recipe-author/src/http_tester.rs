// SPDX-License-Identifier: AGPL-3.0-or-later
//! HTTP [`RecipeTester`] — drives a recipe through a remote OICP host's
//! `/oicp/v1/recipe/test` endpoint (v0.4 §5.4) instead of an in-process
//! `CorpusEngine`. This is what a standalone studio consumer (one that does
//! NOT link corpus-engine) uses to test recipes: it ships the recipe TOML over
//! the wire and reconstructs the authoring tools' [`RecipeTestOutcome`] from
//! the host's [`RecipeTestReport`].
//!
//! ## Documented lossiness (the wire is coarser than the in-process outcome)
//!
//! The wire report is a per-stage projection, so relative to the monolith-side
//! `CorpusEngineRecipeTester` the HTTP path:
//!
//! - **drops structured `section_misses`** — the wire flattens them into the
//!   chunk stage's free-text `misses` (and `nearby_text` is gone), so
//!   [`RecipeTestOutcome::section_misses`] is empty here. The seam's own trait
//!   doc anticipates exactly this ("the eventual HTTP tester … accepts that as
//!   a documented behavior change").
//! - **uses the weaker `ok` verdict for `passed`** — the wire `ok` is
//!   "validated clean AND produced chunks", not the strict
//!   `TestReport::passed()` (which also demands extraction ≥ 80%, no
//!   over-limit chunks, and every test query hitting). The wire doesn't carry
//!   those signals.
//! - **ignores install-time `parameters`** — the endpoint tests with none.
//!
//! These are the "HTTP cutover is an explicit behavior change" the seam
//! (`sovereign_contracts::recipe::testing`) calls out. The validation
//! error/warning split and the extraction rate — the signals the tools lean on
//! most — DO survive.

use std::path::Path;

use async_trait::async_trait;

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::oicp::{RecipeTestOptions, RecipeTestReport, RecipeTestRequest};
use sovereign_contracts::recipe::testing::{
    ExtractionOutcome, RecipeTestOutcome, RecipeTestParams, RecipeTester, ValidationOutcome,
};

/// A [`RecipeTester`] that POSTs to a remote OICP host's recipe-test endpoint.
pub struct HttpRecipeTester {
    client: reqwest::Client,
    /// Fully-resolved endpoint, e.g. `http://127.0.0.1:9741/oicp/v1/recipe/test`.
    endpoint: String,
    /// Bearer token for a non-loopback host; `None` on loopback.
    bearer: Option<String>,
}

impl HttpRecipeTester {
    /// Build against a daemon `base_url` — either the root
    /// (`http://127.0.0.1:9741`) or a `/v1`-shaped URL (the `/v1` suffix is
    /// stripped, since the OICP routes live at the daemon root). `bearer` is
    /// the auth token for a non-loopback host.
    pub fn new(base_url: &str, bearer: Option<String>) -> Self {
        let root = base_url.trim_end_matches('/');
        let root = root.strip_suffix("/v1").unwrap_or(root);
        Self {
            client: reqwest::Client::new(),
            endpoint: format!("{root}/oicp/v1/recipe/test"),
            bearer,
        }
    }

    /// Map the protocol [`RecipeTestParams`] onto the wire [`RecipeTestOptions`].
    /// A `sample_size` of `0` (validation-only) is expressed as `offline`: the
    /// endpoint reads `offline` as "no network acquisition" and sets its
    /// internal sample size to 0.
    fn wire_options(params: &RecipeTestParams) -> RecipeTestOptions {
        let validate_only = params.sample_size == 0;
        RecipeTestOptions {
            offline: params.offline || validate_only,
            sample_limit: if validate_only {
                None
            } else {
                Some(params.sample_size as u32)
            },
        }
    }
}

/// Reconstruct the authoring tools' [`RecipeTestOutcome`] from the wire
/// [`RecipeTestReport`]. Pure (no I/O) so the projection is unit-tested; the
/// stage names mirror the daemon's `map_test_report`.
fn outcome_from_wire(report: &RecipeTestReport) -> RecipeTestOutcome {
    // The `validate` stage carries the error/warning split: `misses` = errors,
    // `sample` = advisory warnings.
    let validation = report
        .stages
        .iter()
        .find(|s| s.name == "validate")
        .map(|s| ValidationOutcome {
            errors: s.misses.clone(),
            warnings: s.sample.clone(),
        })
        .unwrap_or_default();

    // The `extract` stage: docs_in = records attempted, docs_out = succeeded.
    let extraction = report
        .stages
        .iter()
        .find(|s| s.name == "extract")
        .map(|s| {
            let attempted = s.docs_in as usize;
            let succeeded = s.docs_out as usize;
            ExtractionOutcome {
                records_attempted: attempted,
                records_succeeded: succeeded,
                extraction_rate: if attempted == 0 {
                    0.0
                } else {
                    succeeded as f32 / attempted as f32
                },
            }
        });

    RecipeTestOutcome {
        validation,
        extraction,
        // Structured section misses don't survive the wire — see module docs.
        section_misses: Vec::new(),
        // The wire's end-to-end verdict; weaker than `TestReport::passed()`.
        passed: report.ok,
    }
}

#[async_trait]
impl RecipeTester for HttpRecipeTester {
    async fn test(
        &self,
        recipe_path: &Path,
        params: &RecipeTestParams,
    ) -> Result<RecipeTestOutcome> {
        // The trait takes a path (the in-process harness reads sidecars from
        // the recipe dir); the wire ships TOML, so read the source here.
        let recipe_toml = std::fs::read_to_string(recipe_path).map_err(|e| {
            Error::InvalidInput(format!(
                "could not read recipe at {}: {e}",
                recipe_path.display()
            ))
        })?;
        let req = RecipeTestRequest {
            recipe_toml,
            options: Self::wire_options(params),
        };

        let mut rb = self.client.post(&self.endpoint).json(&req);
        if let Some(ref tok) = self.bearer {
            rb = rb.header("Authorization", format!("Bearer {tok}"));
        }
        let resp = rb.send().await.map_err(|e| {
            Error::Execution(format!(
                "recipe-test POST to {} failed: {e}",
                self.endpoint
            ))
        })?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Execution(format!(
                "recipe-test endpoint {} returned {status}: {body}",
                self.endpoint
            )));
        }
        let report: RecipeTestReport = resp.json().await.map_err(|e| {
            Error::Execution(format!("could not parse recipe-test report: {e}"))
        })?;
        Ok(outcome_from_wire(&report))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_contracts::oicp::RecipeStageReport;

    fn stage(name: &str, docs_in: u32, docs_out: u32, misses: &[&str], sample: &[&str]) -> RecipeStageReport {
        RecipeStageReport {
            name: name.into(),
            docs_in,
            docs_out,
            misses: misses.iter().map(|s| s.to_string()).collect(),
            sample: sample.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn endpoint_strips_v1_suffix_and_appends_route() {
        let t = HttpRecipeTester::new("http://127.0.0.1:9741/v1", None);
        assert_eq!(t.endpoint, "http://127.0.0.1:9741/oicp/v1/recipe/test");
        let t = HttpRecipeTester::new("http://127.0.0.1:9741/", None);
        assert_eq!(t.endpoint, "http://127.0.0.1:9741/oicp/v1/recipe/test");
    }

    #[test]
    fn wire_options_zero_sample_is_validate_only_offline() {
        let o = HttpRecipeTester::wire_options(&RecipeTestParams {
            sample_size: 0,
            offline: false,
            ..Default::default()
        });
        assert!(o.offline, "sample_size 0 ⇒ validate-only ⇒ offline");
        assert_eq!(o.sample_limit, None);
    }

    #[test]
    fn wire_options_bounded_sample_passes_limit() {
        let o = HttpRecipeTester::wire_options(&RecipeTestParams {
            sample_size: 5,
            offline: false,
            ..Default::default()
        });
        assert!(!o.offline);
        assert_eq!(o.sample_limit, Some(5));
    }

    #[test]
    fn outcome_maps_validation_split_and_extraction_rate() {
        let report = RecipeTestReport {
            ok: true,
            stages: vec![
                stage("validate", 0, 0, &["missing url"], &["consider a glob"]),
                stage("extract", 10, 8, &["record 3: 404"], &[]),
                stage("chunk", 8, 40, &["a.html / Intro: not found"], &["chunk preview…"]),
            ],
        };
        let out = outcome_from_wire(&report);
        assert_eq!(out.validation.errors, vec!["missing url".to_string()]);
        assert_eq!(out.validation.warnings, vec!["consider a glob".to_string()]);
        let ext = out.extraction.expect("extract stage present");
        assert_eq!(ext.records_attempted, 10);
        assert_eq!(ext.records_succeeded, 8);
        assert!((ext.extraction_rate - 0.8).abs() < 1e-6);
        // Structured section misses are a documented wire loss.
        assert!(out.section_misses.is_empty());
        assert!(out.passed);
    }

    #[test]
    fn outcome_handles_validate_only_report() {
        // A validation-only run: just the `validate` stage, no extraction.
        let report = RecipeTestReport {
            ok: false,
            stages: vec![stage("validate", 0, 0, &["bad regex"], &[])],
        };
        let out = outcome_from_wire(&report);
        assert_eq!(out.validation.errors, vec!["bad regex".to_string()]);
        assert!(out.extraction.is_none());
        assert!(!out.passed);
    }

    #[test]
    fn outcome_extraction_rate_guards_zero_attempts() {
        let report = RecipeTestReport {
            ok: false,
            stages: vec![stage("extract", 0, 0, &[], &[])],
        };
        let out = outcome_from_wire(&report);
        let ext = out.extraction.unwrap();
        assert_eq!(ext.extraction_rate, 0.0);
    }
}
