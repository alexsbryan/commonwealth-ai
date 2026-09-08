// SPDX-License-Identifier: AGPL-3.0-or-later
//! The host's load average — ONE reader, shared by the check runner and the
//! lanes it drives.
//!
//! # Why this is here and not in either caller
//!
//! Two surfaces ask the same question and must get the same answer:
//!
//! - `sovereign-cli`'s `quality_check_cmd::Precondition::HostQuiet` decides
//!   whether a WALL-CLOCK lane is worth running at all.
//! - `sovereign-cli-llm`'s `quality_lane_cmd::chat_ask` decides whether its
//!   `per-stage ceilings` row can be judged, while the sixteen rows around it
//!   (route, gate outcome, both halves answered, useful) stay judgeable —
//!   those are claims about the ANSWER and a busy machine does not change
//!   them.
//!
//! Two implementations of one threshold is ARCH §10.6's smell exactly, and
//! this crate already hosts the other half of the lane protocol
//! ([`crate::lane_verdict`]), so it is where the pair converges.
//!
//! # Why a load average at all
//!
//! Because the bars are wall-clock and this host's speed moves under them.
//! Measured on the authoring machine, same binary, same bank, same bar:
//! primary-slot decode 50.7 tok/s at 1-minute load 3.7, 45.7 tok/s at load
//! 22, and 17.8 tok/s at load 32 — a 2.8x spread (note `d596639c`). A
//! latency verdict that ignores that is not measuring the code.
//!
//! # What it does NOT see
//!
//! A decode in flight on the daemon that costs this host no CPU — a request
//! served by a mesh peer. The daemon serves no queue-depth or slot-busy
//! field to read instead (`/status` carries `inference.resident[]` and
//! `process.rss_mb`; there is no queue route), so load is a CORRELATED
//! instrument, not an equivalent one. Callers say so in their own docs
//! rather than implying a completeness this cannot deliver (ARCH §18.3).

/// The host's 1-minute load average.
///
/// `None` where the platform reports none — and callers must treat that as
/// "cannot be judged" rather than "quiet". A guard that passes when it cannot
/// see is not a guard (ARCH §18.1).
pub fn load_average_1m() -> Option<f64> {
    let mut avg = [0f64; 3];
    // SAFETY: `getloadavg` writes at most `nelem` doubles into the buffer and
    // returns how many it actually wrote. The buffer holds 3 and we pass 3.
    let n = unsafe { libc::getloadavg(avg.as_mut_ptr(), 3) };
    if n >= 1 {
        Some(avg[0])
    } else {
        None
    }
}

/// Is this host quiet enough for a wall-clock verdict to mean anything?
///
/// Returns the observed load alongside the answer so a caller's
/// could-not-judge reason can NAME what it saw instead of restating the rule
/// — the difference between "the host was busy" and "1-minute load average
/// 32.30 is over the declared bound 4.0".
pub fn host_quiet(max_load: f64) -> HostQuietness {
    match load_average_1m() {
        Some(load) if load <= max_load => HostQuietness::Quiet { load },
        Some(load) => HostQuietness::Loaded { load, max_load },
        None => HostQuietness::Unknown,
    }
}

/// What [`host_quiet`] found. Three states, not two: a host that reports no
/// load average is not a quiet one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HostQuietness {
    /// At or under the declared bound.
    Quiet { load: f64 },
    /// Over it. Carries both numbers so the reason can name them.
    Loaded { load: f64, max_load: f64 },
    /// The platform reports no load average.
    Unknown,
}

impl HostQuietness {
    /// True only for [`HostQuietness::Quiet`].
    pub fn is_quiet(&self) -> bool {
        matches!(self, HostQuietness::Quiet { .. })
    }

    /// The could-not-judge reason, naming the load actually observed.
    /// `None` when the host IS quiet and there is nothing to explain.
    pub fn reason(&self) -> Option<String> {
        match self {
            HostQuietness::Quiet { .. } => None,
            HostQuietness::Loaded { load, max_load } => Some(format!(
                "1-minute load average {load:.2} is over the declared bound {max_load:.1} — \
                 a wall-clock bar on a contended host verifies nothing"
            )),
            HostQuietness::Unknown => {
                Some("this host reports no load average, so it cannot be shown quiet".to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bound is inclusive, and being OVER it is not the same state as
    /// having no reading at all.
    #[test]
    fn quietness_separates_loaded_from_unknown_and_names_the_load() {
        let quiet = HostQuietness::Quiet { load: 1.5 };
        assert!(quiet.is_quiet());
        assert_eq!(quiet.reason(), None);

        let loaded = HostQuietness::Loaded {
            load: 32.3,
            max_load: 4.0,
        };
        assert!(!loaded.is_quiet());
        let r = loaded.reason().expect("a loaded host owes a reason");
        assert!(r.contains("32.30"), "the reason must name the load: {r}");
        assert!(
            r.contains("4.0"),
            "and the bound it was judged against: {r}"
        );

        // The state a `bool` return would have collapsed into "loaded".
        let unknown = HostQuietness::Unknown;
        assert!(!unknown.is_quiet());
        assert!(unknown
            .reason()
            .expect("unknown owes a reason too")
            .contains("no load average"));
    }

    /// This host does report one, and a load average is never negative.
    #[test]
    fn the_load_average_reads_on_this_platform() {
        let l = load_average_1m().expect("macOS and Linux both report getloadavg");
        assert!(l >= 0.0, "a load average is never negative, got {l}");
    }
}
