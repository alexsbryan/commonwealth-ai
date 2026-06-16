// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic authoring harness — the *mechanism* layer.
//!
//! Runs the real ingest stages over a FROZEN sample and emits typed,
//! judgment-free observations. The *policy* layer (Pass/Fail verdicts) lives
//! in `sovereign-eval::authoring_harness`, which consumes what this module
//! produces. See `sovereign/docs/specs/AUTHORING_HARNESS.md`.
//!
//! Invariants this module upholds:
//! - **I3** — acquisition runs exactly once, here, at sample [`capture`]; the
//!   bytes are content-addressed and never re-fetched during iteration.
//! - **I1** — [`sample_id`] is stable over the frozen content (inputs are
//!   sorted), and [`CaptureManifest`] keeps the timestamp in the sidecar, not
//!   in any value a verdict is derived from.
//! - **I2** — capture (and the runner) go through the same `acquire_source` /
//!   `make_extractor` / [`crate::engine::chunk_doc`] the production ingest
//!   uses; nothing is reimplemented.

pub mod enrich;
pub mod field_coverage;
pub mod frozen_sample;
pub mod miss;
pub mod runner;
pub mod stage_output;

pub use enrich::{check_evidence, verify_atoms, verify_atoms_at, EnrichMiss, EnrichOutput};
pub use field_coverage::{
    coverage, declared_fields, doc_id, CoverageUnit, FieldCoverage, FieldDecl, FieldProbe,
};
pub use frozen_sample::{capture, CaptureManifest, CapturedDoc, CapturedFile, FrozenSample};
pub use miss::FieldMiss;
pub use runner::HarnessRunner;
pub use stage_output::{ChunkOutput, ExtractOutput, FilterOutput, IndexOutput, StageOutputs};

use sha2::{Digest, Sha256};

use crate::recipe::Recipe;

/// Lower-hex SHA-256 of `bytes` — the harness's content-addressing primitive
/// (the same hash the `asset_store` uses for blob identity).
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Stable content id for a frozen sample: SHA-256 over the **sorted** set of
/// per-file SHA-256 hashes. Order-independent, so the same frozen bytes always
/// yield the same id regardless of acquire/walk order — the spine of I1's
/// "same `(sample_id, recipe)` → byte-identical `HarnessRun`".
pub fn sample_id(mut file_hashes: Vec<String>) -> String {
    file_hashes.sort();
    file_hashes.dedup();
    sha256_hex(file_hashes.join("\n").as_bytes())
}

/// Stable hash of the recipe-as-authored. Mirrors the
/// `filters::compute_signature` pattern (canonical serde_json → SHA-256).
/// `Recipe::resolved_parameters` is `#[serde(skip)]`, so install-time
/// parameter values do not perturb the hash — it identifies the TOML, not a
/// particular install.
pub fn recipe_hash(recipe: &Recipe) -> String {
    let bytes = serde_json::to_vec(recipe).unwrap_or_default();
    sha256_hex(&bytes)
}
