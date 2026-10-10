// SPDX-License-Identifier: AGPL-3.0-or-later
//! What made each link, with its precision (campaign ontology-layer C3). Kept
//! apart from `resolve_records.rs`, which sits in arch-gate's approach band.

use super::super::precision::{Precision, SourcePrecision};
use super::{Decision, Held, Outcome};

impl Outcome {
    /// The sources of a link and their precisions (C3). A key the declaration
    /// says suffices links at 1.0 by construction; a partition answer's link is
    /// unmeasured; a weighed link carries each source that agreed with it at
    /// its precision as estimated on this corpus. Opening or refusing links
    /// nothing.
    pub fn by(&self) -> Vec<SourcePrecision> {
        match self {
            Outcome::Decided(Decision::Key { key, .. }) => vec![SourcePrecision::new(
                format!("identity_key:{key}"),
                Precision::Declared(1.0),
            )],
            Outcome::Decided(Decision::Cited { .. }) => vec![SourcePrecision::new(
                "model_partition",
                Precision::Unmeasured,
            )],
            Outcome::Decided(Decision::Weighed { sources, .. })
            | Outcome::Held(Held { sources, .. }) => sources
                .iter()
                .map(|v| SourcePrecision::new(v.source.clone(), Precision::Estimated(v.precision)))
                .collect(),
            Outcome::Decided(Decision::Opened { .. }) | Outcome::Refused(_) => Vec::new(),
        }
    }
}
