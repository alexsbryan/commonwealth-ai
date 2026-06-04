//! UAP disposition taxonomy + date-conditioned era mask (UFO.md
//! "Disposition taxonomy" + era-handling open decision #3, resolved
//! here as the **date-conditioned label mask**).
//!
//! Lives in `sovereign-eval` rather than the recipe dir because it is a
//! typed contract shared by BOTH consumers: the bench classifier (which
//! constrains the model's output to the era-possible label set) and the
//! scorer (which reads the confusion matrix against that same set). Both
//! depend on `sovereign-eval`, so one home, zero new dependencies.

use serde::{Deserialize, Serialize};

/// The 12-category disposition taxonomy — the classifier's label set
/// and the confusion-matrix axis. Unifies Blue Book's historical
/// categories with AARO's modern ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Disposition {
    Astronomical,
    Aircraft,
    Balloon,
    Satellite,
    UasDrone,
    Bird,
    Atmospheric,
    SensorArtifact,
    Hoax,
    OtherIdentified,
    InsufficientData,
    Unidentified,
}

/// Declaration order == canonical confusion-matrix axis order.
const ALL: &[Disposition] = &[
    Disposition::Astronomical,
    Disposition::Aircraft,
    Disposition::Balloon,
    Disposition::Satellite,
    Disposition::UasDrone,
    Disposition::Bird,
    Disposition::Atmospheric,
    Disposition::SensorArtifact,
    Disposition::Hoax,
    Disposition::OtherIdentified,
    Disposition::InsufficientData,
    Disposition::Unidentified,
];

impl Disposition {
    /// SCREAMING_SNAKE_CASE token — the exact string used in
    /// `gold_labels.jsonl`, the fixture `disposition` field, the
    /// classifier's grammar enum, and the confusion-matrix axis.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Disposition::Astronomical => "ASTRONOMICAL",
            Disposition::Aircraft => "AIRCRAFT",
            Disposition::Balloon => "BALLOON",
            Disposition::Satellite => "SATELLITE",
            Disposition::UasDrone => "UAS_DRONE",
            Disposition::Bird => "BIRD",
            Disposition::Atmospheric => "ATMOSPHERIC",
            Disposition::SensorArtifact => "SENSOR_ARTIFACT",
            Disposition::Hoax => "HOAX",
            Disposition::OtherIdentified => "OTHER_IDENTIFIED",
            Disposition::InsufficientData => "INSUFFICIENT_DATA",
            Disposition::Unidentified => "UNIDENTIFIED",
        }
    }

    /// Parse a taxonomy token. `None` for anything outside the 12.
    pub fn parse(s: &str) -> Option<Self> {
        ALL.iter().copied().find(|d| d.as_str() == s)
    }

    /// First year this category is physically possible. `None` = always
    /// valid. Soft thresholds (UAS_DRONE / SENSOR_ARTIFACT) are tunable;
    /// each is documented inline.
    pub const fn era_floor_year(&self) -> Option<i32> {
        match self {
            // Sputnik (the first artificial satellite).
            Disposition::Satellite => Some(1957),
            // Uncrewed aircraft become a meaningful civilian phenomenon
            // in the 2010s (UFO.md: "meaningfully post-2010s").
            Disposition::UasDrone => Some(2010),
            // Modern digital-sensor era — lens flare / compression
            // artifact / radar-processing anomalies (UFO.md: "modern
            // sensor era"). Soft threshold.
            Disposition::SensorArtifact => Some(1991),
            _ => None,
        }
    }
}

/// Every category as a token, in canonical axis order — the scorer's
/// default axis and the bench's "full" label set.
pub fn all_categories() -> Vec<String> {
    ALL.iter().map(|d| d.as_str().to_string()).collect()
}

/// The era-conditioned label mask: the subset of categories possible in
/// `year`, in canonical order. Used by the bench classifier to constrain
/// the grammar enum (so the model can't predict "Starlink" for a 1952
/// case) and by the scorer as the confusion-matrix axis.
pub fn era_mask(year: i32) -> Vec<String> {
    ALL.iter()
        .filter(|d| d.era_floor_year().is_none_or(|floor| year >= floor))
        .map(|d| d.as_str().to_string())
        .collect()
}

/// The union era mask over a set of case years — the axis when a whole
/// split spans eras. For the all-Blue-Book slice this drops
/// SATELLITE/UAS_DRONE/SENSOR_ARTIFACT; once a single modern case is
/// present they reappear.
pub fn era_mask_union(years: impl IntoIterator<Item = i32>) -> Vec<String> {
    let max_year = years.into_iter().max();
    match max_year {
        Some(y) => era_mask(y),
        None => all_categories(),
    }
}

/// Parse the year out of a `"YYYY-MM-DD"` date (the fixture `date`
/// field). The only date arithmetic the bench needs — avoids a chrono
/// dependency in this crate.
pub fn year_of(date: &str) -> Option<i32> {
    date.get(0..4)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trips_all() {
        for d in ALL {
            assert_eq!(Disposition::parse(d.as_str()), Some(*d));
        }
        assert_eq!(Disposition::parse("UAS_DRONE"), Some(Disposition::UasDrone));
        assert_eq!(Disposition::parse("nonsense"), None);
    }

    #[test]
    fn era_mask_1952_excludes_anachronistic() {
        let mask = era_mask(1952);
        assert!(!mask.contains(&"SATELLITE".to_string()));
        assert!(!mask.contains(&"UAS_DRONE".to_string()));
        assert!(!mask.contains(&"SENSOR_ARTIFACT".to_string()));
        assert!(mask.contains(&"ASTRONOMICAL".to_string()));
        assert_eq!(mask.len(), 9);
    }

    #[test]
    fn era_mask_2020_is_full() {
        assert_eq!(era_mask(2020).len(), 12);
    }

    #[test]
    fn satellite_floor_is_1957() {
        assert!(!era_mask(1956).contains(&"SATELLITE".to_string()));
        assert!(era_mask(1957).contains(&"SATELLITE".to_string()));
    }

    #[test]
    fn era_mask_union_takes_widest() {
        // A span with one modern case opens the full set.
        let mask = era_mask_union([1952, 1955, 2019]);
        assert_eq!(mask.len(), 12);
        // An all-old span stays narrow.
        assert_eq!(era_mask_union([1952, 1955]).len(), 9);
        // Empty → full (defensive).
        assert_eq!(era_mask_union(std::iter::empty()).len(), 12);
    }

    #[test]
    fn year_of_parses_fixture_dates() {
        assert_eq!(year_of("1952-08-14"), Some(1952));
        assert_eq!(year_of("2019-05-24"), Some(2019));
        assert_eq!(year_of("bad"), None);
    }
}
