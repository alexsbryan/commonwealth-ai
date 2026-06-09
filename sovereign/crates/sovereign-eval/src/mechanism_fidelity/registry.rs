// SPDX-License-Identifier: AGPL-3.0-or-later
//! The class registry — the single place the orchestrator resolves a
//! `--class <id>` to a [`ReasoningClass`] implementation.

use super::class::ReasoningClass;
use super::classes::aggregation::AggregationThreshold;
use super::classes::attribution::AttributionSupport;
use super::classes::wealth_tax::WealthTaxRelocation;

/// All registered classes. The orchestrator's `--class` resolves against
/// this; a characterization run iterates it to fill a model's card.
pub fn registry() -> Vec<Box<dyn ReasoningClass>> {
    vec![
        Box::new(WealthTaxRelocation),
        Box::new(AttributionSupport),
        Box::new(AggregationThreshold),
    ]
}

/// Resolve a class by id, or `None` if unknown.
pub fn by_id(id: &str) -> Option<Box<dyn ReasoningClass>> {
    registry().into_iter().find(|c| c.id() == id)
}

/// The ids of all registered classes (for help text / iteration).
pub fn class_ids() -> Vec<&'static str> {
    registry().iter().map(|c| c.id()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_are_registered_and_resolvable() {
        assert!(class_ids().contains(&"wealth_tax_relocation"));
        assert!(class_ids().contains(&"attribution_support"));
        assert!(class_ids().contains(&"aggregation_threshold"));
        assert!(by_id("wealth_tax_relocation").is_some());
        assert!(by_id("attribution_support").is_some());
        assert!(by_id("aggregation_threshold").is_some());
        assert!(by_id("nonsense").is_none());
    }

    #[test]
    fn class_ids_are_unique() {
        let mut ids = class_ids();
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n, "every registered class needs a distinct id");
    }
}
