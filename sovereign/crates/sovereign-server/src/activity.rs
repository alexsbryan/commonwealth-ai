//! Activity level tracking for the inference availability signal.
//!
//! `ActivityReporter` watches for file changes from the watcher coordinator
//! and reports the current activity level to Commonwealth via HTTP so the
//! gossip layer can tell peers "this node is busy, route inference elsewhere".
//!
//! Levels and their availability weights:
//!
//!   Hot  (tests running / heavy edits) → 0.20  (almost unavailable)
//!   Warm (recent activity)             → 0.65
//!   Cool (some time since activity)    → 0.85
//!   Idle (no activity)                 → 1.00  (fully available)
//!
//! The decay loop transitions Hot→Warm after 60s, Warm→Cool after 2m,
//! Cool→Idle after 5m, measured from the last file-change event.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use corpus_engine::ActivityCallback;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityLevel {
    Hot,
    Warm,
    Cool,
    Idle,
}

impl ActivityLevel {
    #[allow(dead_code)]
    fn availability(self) -> f32 {
        match self {
            ActivityLevel::Hot  => 0.20,
            ActivityLevel::Warm => 0.65,
            ActivityLevel::Cool => 0.85,
            ActivityLevel::Idle => 1.00,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            ActivityLevel::Hot  => "hot",
            ActivityLevel::Warm => "warm",
            ActivityLevel::Cool => "cool",
            ActivityLevel::Idle => "idle",
        }
    }
}

struct ActivityState {
    level: ActivityLevel,
    last_activity: Instant,
    last_report: Option<ActivityLevel>,
}

pub struct ActivityReporter {
    commonwealth_url: String,
    state: Arc<Mutex<ActivityState>>,
}

impl ActivityReporter {
    pub fn new(commonwealth_url: String) -> Self {
        Self {
            commonwealth_url,
            state: Arc::new(Mutex::new(ActivityState {
                level: ActivityLevel::Idle,
                last_activity: Instant::now(),
                last_report: None,
            })),
        }
    }

    /// Spawn the background decay loop. Returns a handle (dropping it does nothing
    /// — the loop runs until the process exits).
    pub fn start_decay_loop(self: &Arc<Self>) {
        let reporter = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(15)).await;
                reporter.tick_decay().await;
            }
        });
    }

    pub(crate) async fn tick_decay(&self) {
        let (current_level, elapsed) = {
            let s = self.state.lock().expect("activity state poisoned");
            (s.level, s.last_activity.elapsed())
        };

        let target = match current_level {
            ActivityLevel::Hot  if elapsed >= Duration::from_secs(60)  => ActivityLevel::Warm,
            ActivityLevel::Warm if elapsed >= Duration::from_secs(120) => ActivityLevel::Cool,
            ActivityLevel::Cool if elapsed >= Duration::from_secs(300) => ActivityLevel::Idle,
            other => other,
        };

        if target != current_level {
            self.transition(target, "decay").await;
        }
    }

    async fn transition(&self, target: ActivityLevel, reason: &str) {
        let already_reported = {
            let mut s = self.state.lock().expect("activity state poisoned");
            if s.level == target {
                return;
            }
            let prev = s.level;
            s.level = target;
            tracing::debug!(
                from = prev.as_str(),
                to = target.as_str(),
                reason,
                "activity level transition"
            );
            s.last_report == Some(target)
        };

        if already_reported {
            return;
        }

        self.report(target, reason).await;
        let mut s = self.state.lock().expect("activity state poisoned");
        s.last_report = Some(target);
    }

    async fn report(&self, level: ActivityLevel, reason: &str) {
        let url = format!("{}/internal/node/activity", self.commonwealth_url);
        let body = serde_json::json!({
            "level": level.as_str(),
            "reason": reason,
        });
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .expect("reqwest client");
        match client.post(&url).json(&body).send().await {
            Ok(resp) if resp.status().as_u16() == 204 => {
                tracing::debug!(level = level.as_str(), "activity reported to Commonwealth");
            }
            Ok(resp) => {
                tracing::warn!(
                    level = level.as_str(),
                    status = resp.status().as_u16(),
                    "activity report got unexpected status"
                );
            }
            Err(e) => {
                tracing::debug!(
                    level = level.as_str(),
                    error = %e,
                    "activity report failed (daemon may not be running)"
                );
            }
        }
    }
}

#[async_trait]
impl ActivityCallback for ActivityReporter {
    async fn on_files_changed(&self) {
        let current = {
            let mut s = self.state.lock().expect("activity state poisoned");
            s.last_activity = Instant::now();
            let prev = s.level;
            s.level = ActivityLevel::Hot;
            prev
        };

        if current != ActivityLevel::Hot {
            self.report(ActivityLevel::Hot, "files_changed").await;
            let mut s = self.state.lock().expect("activity state poisoned");
            s.last_report = Some(ActivityLevel::Hot);
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::ActivityCallback;

    fn reporter() -> Arc<ActivityReporter> {
        // Point at a port that doesn't exist — HTTP errors are silently ignored.
        Arc::new(ActivityReporter::new("http://127.0.0.1:1".to_string()))
    }

    #[test]
    fn activity_level_availability_values() {
        assert!((ActivityLevel::Hot.availability() - 0.20).abs() < 1e-6);
        assert!((ActivityLevel::Warm.availability() - 0.65).abs() < 1e-6);
        assert!((ActivityLevel::Cool.availability() - 0.85).abs() < 1e-6);
        assert!((ActivityLevel::Idle.availability() - 1.00).abs() < 1e-6);
    }

    #[test]
    fn activity_level_as_str_values() {
        assert_eq!(ActivityLevel::Hot.as_str(), "hot");
        assert_eq!(ActivityLevel::Warm.as_str(), "warm");
        assert_eq!(ActivityLevel::Cool.as_str(), "cool");
        assert_eq!(ActivityLevel::Idle.as_str(), "idle");
    }

    #[test]
    fn availability_order_is_monotonically_increasing() {
        assert!(ActivityLevel::Hot.availability() < ActivityLevel::Warm.availability());
        assert!(ActivityLevel::Warm.availability() < ActivityLevel::Cool.availability());
        assert!(ActivityLevel::Cool.availability() < ActivityLevel::Idle.availability());
    }

    #[tokio::test]
    async fn on_files_changed_transitions_to_hot() {
        let r = reporter();
        {
            let s = r.state.lock().unwrap();
            assert_eq!(s.level, ActivityLevel::Idle, "should start idle");
        }

        r.on_files_changed().await;

        let s = r.state.lock().unwrap();
        assert_eq!(s.level, ActivityLevel::Hot, "on_files_changed must transition to Hot");
    }

    #[tokio::test]
    async fn on_files_changed_idempotent_when_already_hot() {
        let r = reporter();
        r.on_files_changed().await; // → Hot, last_report = Some(Hot)
        let report_after_first = r.state.lock().unwrap().last_report;

        r.on_files_changed().await; // already Hot — should not change last_report
        let report_after_second = r.state.lock().unwrap().last_report;

        assert_eq!(
            report_after_first, report_after_second,
            "calling on_files_changed twice must not update last_report the second time"
        );
    }

    #[tokio::test]
    async fn decay_hot_to_warm_after_grace_period() {
        let r = reporter();
        r.on_files_changed().await; // → Hot, last_activity = now

        // Manually wind back last_activity to simulate 61s elapsed.
        {
            let mut s = r.state.lock().unwrap();
            s.last_activity = Instant::now() - Duration::from_secs(61);
        }

        r.tick_decay().await;

        let s = r.state.lock().unwrap();
        assert_eq!(s.level, ActivityLevel::Warm, "Hot must decay to Warm after 60s");
    }

    #[tokio::test]
    async fn decay_does_not_advance_before_threshold() {
        let r = reporter();
        r.on_files_changed().await; // → Hot, last_activity = now

        // Only 30 seconds elapsed — should remain Hot.
        {
            let mut s = r.state.lock().unwrap();
            s.last_activity = Instant::now() - Duration::from_secs(30);
        }

        r.tick_decay().await;

        let s = r.state.lock().unwrap();
        assert_eq!(s.level, ActivityLevel::Hot, "Hot must not decay before 60s threshold");
    }
}
