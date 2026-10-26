// SPDX-License-Identifier: AGPL-3.0-or-later
//! `bench/lanes/engine-swap/conformance.toml`, read as the judge needs it:
//! each row's checks, which target pairs it compares, and the verdict that was
//! predicted before any run.

use std::path::Path;

use serde::Deserialize;

use super::check::Check;

/// The inventory.
#[derive(Debug, Default, Deserialize)]
pub struct Inventory {
    /// In-scope rows.
    #[serde(rename = "row", default)]
    pub rows: Vec<Row>,
    /// Rows that are gone, and why. Not judged and not cases: kept so a
    /// reader who greps a retired id finds the reason rather than silence
    /// (ARCH 6 — an absence is reported, never defaulted).
    #[serde(rename = "retired", default)]
    pub retired: Vec<Retired>,
}

/// A row that no longer exists, with its retirement on record.
#[derive(Debug, Deserialize)]
pub struct Retired {
    /// The id it had.
    pub id: String,
    /// The date the row was retired (`YYYY-MM-DD`).
    pub retired: String,
    /// Why, citing the caller census that retired it.
    pub why: String,
}

/// One row: a boundary behaviour, its checks and its prediction.
#[derive(Debug, Deserialize)]
pub struct Row {
    /// Stable id, `layer.name`.
    pub id: String,
    /// Every check must pass for the row to pass.
    pub judge: Vec<Check>,
    /// `(reference, target)` pairs this row compares. Empty means the
    /// reference target against every other target in the records.
    #[serde(default)]
    pub pairs: Vec<(String, String)>,
    /// The predicted verdict, as written before the first run.
    pub predict: Prediction,
}

/// A predicted verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Prediction {
    /// Predicted to pass.
    #[serde(rename = "pass")]
    Pass,
    /// Predicted to answer while dropping the request's meaning.
    #[serde(rename = "fail:silent")]
    Silent,
    /// Predicted to answer differently, not because a field was dropped.
    #[serde(rename = "fail:differs")]
    Differs,
    /// Predicted to refuse where the reference serves.
    #[serde(rename = "fail:refused")]
    Refused,
}

impl Inventory {
    /// Read and parse the inventory file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        let inventory: Self =
            toml::from_str(&text).map_err(|e| format!("parsing {}: {e}", path.display()))?;
        if let Some(row) = inventory.rows.iter().find(|r| r.judge.is_empty()) {
            return Err(format!("row {} names no check", row.id));
        }
        let mut seen = std::collections::BTreeSet::new();
        if let Some(row) = inventory.rows.iter().find(|r| !seen.insert(r.id.as_str())) {
            return Err(format!("row id {} appears twice", row.id));
        }
        let mut gone = std::collections::BTreeSet::new();
        if let Some(row) = inventory
            .retired
            .iter()
            .find(|r| !gone.insert(r.id.as_str()))
        {
            return Err(format!("retired row id {} appears twice", row.id));
        }
        if let Some(both) = gone.intersection(&seen).next() {
            return Err(format!(
                "row id {both} is both a live row and a retired one"
            ));
        }
        Ok(inventory)
    }
}
