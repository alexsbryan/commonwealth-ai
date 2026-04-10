use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Distribution of capability types requested during a time window.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapabilityDistribution {
    pub code_fraction: f32,
    pub analysis_fraction: f32,
    pub general_fraction: f32,
}

/// Category of capability demand (for histogram tracking).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityCategory {
    Code,
    Analysis,
    General,
}

/// Predicts usage patterns based on historical request data.
/// Groups requests by (weekday, hour) and tracks the distribution
/// of capability types to preemptively load the right models.
pub struct UsagePredictor {
    /// (weekday 0-6, hour 0-23) → count per category.
    counts: HashMap<(u8, u8), HashMap<CapabilityCategory, u64>>,
}

impl UsagePredictor {
    pub fn new() -> Self {
        Self {
            counts: HashMap::new(),
        }
    }

    /// Record a request for the given time window and capability category.
    pub fn record_request(&mut self, weekday: u8, hour: u8, category: CapabilityCategory) {
        let slot = self.counts.entry((weekday, hour)).or_default();
        *slot.entry(category).or_insert(0) += 1;
    }

    /// Get the predicted capability distribution for a given time window.
    /// Returns None if no data exists for that window.
    pub fn predict(&self, weekday: u8, hour: u8) -> Option<CapabilityDistribution> {
        let slot = self.counts.get(&(weekday, hour))?;
        let total: u64 = slot.values().sum();
        if total == 0 {
            return None;
        }

        let code = *slot.get(&CapabilityCategory::Code).unwrap_or(&0) as f32 / total as f32;
        let analysis = *slot.get(&CapabilityCategory::Analysis).unwrap_or(&0) as f32 / total as f32;
        let general = *slot.get(&CapabilityCategory::General).unwrap_or(&0) as f32 / total as f32;

        Some(CapabilityDistribution {
            code_fraction: code,
            analysis_fraction: analysis,
            general_fraction: general,
        })
    }

    /// Get the dominant capability category for a given time window.
    pub fn dominant_capability(&self, weekday: u8, hour: u8) -> Option<CapabilityCategory> {
        let dist = self.predict(weekday, hour)?;

        if dist.code_fraction >= dist.analysis_fraction
            && dist.code_fraction >= dist.general_fraction
        {
            Some(CapabilityCategory::Code)
        } else if dist.analysis_fraction >= dist.general_fraction {
            Some(CapabilityCategory::Analysis)
        } else {
            Some(CapabilityCategory::General)
        }
    }

    /// Total number of recorded requests.
    pub fn total_requests(&self) -> u64 {
        self.counts.values().flat_map(|s| s.values()).sum()
    }

    /// Number of time slots with data.
    pub fn populated_slots(&self) -> usize {
        self.counts.len()
    }
}

impl Default for UsagePredictor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_predictor_returns_none() {
        let predictor = UsagePredictor::new();
        assert!(predictor.predict(1, 9).is_none());
        assert!(predictor.dominant_capability(1, 9).is_none());
        assert_eq!(predictor.total_requests(), 0);
    }

    #[test]
    fn record_and_predict() {
        let mut predictor = UsagePredictor::new();

        // Monday 9am: mostly code.
        for _ in 0..8 {
            predictor.record_request(1, 9, CapabilityCategory::Code);
        }
        for _ in 0..2 {
            predictor.record_request(1, 9, CapabilityCategory::General);
        }

        let dist = predictor.predict(1, 9).unwrap();
        assert!((dist.code_fraction - 0.8).abs() < 0.01);
        assert!((dist.general_fraction - 0.2).abs() < 0.01);
        assert!((dist.analysis_fraction - 0.0).abs() < 0.01);
    }

    #[test]
    fn dominant_capability_code() {
        let mut predictor = UsagePredictor::new();
        for _ in 0..10 {
            predictor.record_request(1, 9, CapabilityCategory::Code);
        }
        for _ in 0..3 {
            predictor.record_request(1, 9, CapabilityCategory::General);
        }

        assert_eq!(
            predictor.dominant_capability(1, 9),
            Some(CapabilityCategory::Code)
        );
    }

    #[test]
    fn dominant_capability_analysis() {
        let mut predictor = UsagePredictor::new();
        for _ in 0..7 {
            predictor.record_request(4, 20, CapabilityCategory::Analysis);
        }
        for _ in 0..3 {
            predictor.record_request(4, 20, CapabilityCategory::Code);
        }

        assert_eq!(
            predictor.dominant_capability(4, 20),
            Some(CapabilityCategory::Analysis)
        );
    }

    #[test]
    fn different_time_slots_independent() {
        let mut predictor = UsagePredictor::new();
        predictor.record_request(1, 9, CapabilityCategory::Code);
        predictor.record_request(1, 20, CapabilityCategory::Analysis);

        let morning = predictor.predict(1, 9).unwrap();
        assert!((morning.code_fraction - 1.0).abs() < 0.01);

        let evening = predictor.predict(1, 20).unwrap();
        assert!((evening.analysis_fraction - 1.0).abs() < 0.01);

        assert_eq!(predictor.populated_slots(), 2);
    }

    #[test]
    fn total_requests_count() {
        let mut predictor = UsagePredictor::new();
        predictor.record_request(0, 0, CapabilityCategory::Code);
        predictor.record_request(0, 0, CapabilityCategory::Code);
        predictor.record_request(1, 12, CapabilityCategory::General);

        assert_eq!(predictor.total_requests(), 3);
    }

    #[test]
    fn capability_distribution_serde() {
        let dist = CapabilityDistribution {
            code_fraction: 0.6,
            analysis_fraction: 0.3,
            general_fraction: 0.1,
        };
        let json = serde_json::to_string(&dist).unwrap();
        let back: CapabilityDistribution = serde_json::from_str(&json).unwrap();
        assert!((back.code_fraction - 0.6).abs() < 0.001);
    }
}
