// SPDX-License-Identifier: AGPL-3.0-or-later
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use commonwealth_core::ids::NodeId;

/// State of a graceful departure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DepartureState {
    /// Departure announced, countdown started.
    Announced,
    /// Scheduler is rebalancing plans to exclude this node.
    Rebalancing,
    /// Draining in-flight requests on the old plan.
    Draining,
    /// Safe to stop all processes.
    Complete,
}

/// The default departure countdown — the window the mesh gets to rebalance
/// away from this node before its processes stop. Named once, so the value
/// `GracefulDeparture::new` uses and the value a caller passes explicitly
/// cannot drift apart (ARCH §10.6).
pub const DEFAULT_COUNTDOWN: Duration = Duration::from_secs(30);

/// Manages a graceful departure — a countdown during which the mesh
/// rebalances so there are zero 503s.
pub struct GracefulDeparture {
    pub node_id: NodeId,
    pub announced_at: Instant,
    pub countdown: Duration,
    pub state: DepartureState,
}

impl GracefulDeparture {
    /// Start a new graceful departure with the default 30-second countdown.
    pub fn new(node_id: NodeId) -> Self {
        Self {
            node_id,
            announced_at: Instant::now(),
            countdown: DEFAULT_COUNTDOWN,
            state: DepartureState::Announced,
        }
    }

    /// Start with a custom countdown duration.
    pub fn with_countdown(node_id: NodeId, countdown: Duration) -> Self {
        Self {
            node_id,
            announced_at: Instant::now(),
            countdown,
            state: DepartureState::Announced,
        }
    }

    /// How much time has elapsed since the departure was announced.
    pub fn elapsed(&self) -> Duration {
        self.announced_at.elapsed()
    }

    /// How much time remains in the countdown.
    pub fn remaining(&self) -> Duration {
        self.countdown.saturating_sub(self.elapsed())
    }

    /// Whether the countdown has fully elapsed and it's safe to stop.
    pub fn is_ready_to_stop(&self) -> bool {
        self.state == DepartureState::Complete || self.elapsed() >= self.countdown
    }

    /// Advance to the next state in the departure sequence.
    /// Returns the new state.
    pub fn advance(&mut self) -> DepartureState {
        self.state = match self.state {
            DepartureState::Announced => DepartureState::Rebalancing,
            DepartureState::Rebalancing => DepartureState::Draining,
            DepartureState::Draining => DepartureState::Complete,
            DepartureState::Complete => DepartureState::Complete,
        };
        self.state
    }

    /// Get the current state.
    pub fn state(&self) -> DepartureState {
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_departure_is_announced() {
        let dep = GracefulDeparture::new(NodeId::from_u128(1));
        assert_eq!(dep.state(), DepartureState::Announced);
        assert!(!dep.is_ready_to_stop());
    }

    #[test]
    fn advance_through_states() {
        let mut dep = GracefulDeparture::new(NodeId::from_u128(1));

        assert_eq!(dep.advance(), DepartureState::Rebalancing);
        assert_eq!(dep.advance(), DepartureState::Draining);
        assert_eq!(dep.advance(), DepartureState::Complete);
        // Stays at Complete.
        assert_eq!(dep.advance(), DepartureState::Complete);
    }

    #[test]
    fn complete_is_ready_to_stop() {
        let mut dep = GracefulDeparture::new(NodeId::from_u128(1));
        dep.advance(); // Rebalancing
        dep.advance(); // Draining
        dep.advance(); // Complete
        assert!(dep.is_ready_to_stop());
    }

    #[test]
    fn short_countdown_ready_immediately() {
        let dep = GracefulDeparture::with_countdown(NodeId::from_u128(1), Duration::from_millis(0));
        assert!(dep.is_ready_to_stop());
    }

    #[test]
    fn remaining_decreases() {
        let dep = GracefulDeparture::with_countdown(NodeId::from_u128(1), Duration::from_secs(30));
        // Remaining should be close to 30s (within a few ms of test execution).
        let remaining = dep.remaining();
        assert!(remaining <= Duration::from_secs(30));
        assert!(remaining >= Duration::from_secs(29));
    }

    #[test]
    fn departure_state_serde_roundtrip() {
        for state in [
            DepartureState::Announced,
            DepartureState::Rebalancing,
            DepartureState::Draining,
            DepartureState::Complete,
        ] {
            let json = serde_json::to_string(&state).unwrap();
            let back: DepartureState = serde_json::from_str(&json).unwrap();
            assert_eq!(state, back);
        }
    }
}
