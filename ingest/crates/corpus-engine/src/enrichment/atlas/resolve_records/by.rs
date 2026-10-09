// SPDX-License-Identifier: AGPL-3.0-or-later
//! What made each link, with its precision (campaign ontology-layer C3). Kept
//! apart from `resolve_records.rs`, which sits in arch-gate's approach band.

use super::super::precision::{Precision, SourcePrecision};
use super::{Criterion, Decision, Held, Outcome};

impl Outcome {
    /// The sources of a link and their precisions (C3). A key the declaration
    /// says suffices links at 1.0 by construction; the model's argmax decides
    /// unmeasured; a weighed link carries each source that named it at the
    /// precision the recipe declares. Opening or refusing links nothing.
    pub fn by(&self, criterion: &Criterion) -> Vec<SourcePrecision> {
        match self {
            Outcome::Decided(Decision::Key { key, .. }) => vec![SourcePrecision::new(
                format!("identity_key:{key}"),
                Precision::Declared(1.0),
            )],
            Outcome::Decided(Decision::Cited { .. }) => vec![SourcePrecision::new(
                "model_partition",
                Precision::Unmeasured,
            )],
            Outcome::Decided(Decision::Selected { .. }) => vec![SourcePrecision::new(
                "model_choice",
                Precision::declared_or_unmeasured(criterion.model_choice),
            )],
            Outcome::Decided(Decision::Weighed { sources, .. })
            | Outcome::Held(Held { sources, .. }) => sources
                .iter()
                .map(|v| SourcePrecision::new(v.source, Precision::Declared(v.precision)))
                .collect(),
            Outcome::Decided(Decision::Opened { .. }) | Outcome::Refused(_) => Vec::new(),
        }
    }
}
