//! Background health-monitor loop.
//!
//! `HealthMonitor` is constructed in `bootstrap()` (sovereign-desktop) and
//! runs as a long-lived Tokio task.  It polls each registered
//! `HealthCheckable`, persists reports to the `StateStore`, and schedules or
//! executes repairs automatically when policy allows.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use dashmap::DashMap;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

use crate::error::Result;
use crate::health::{
    Component, HealthCheckable, HealthIssue, HealthReport, HealthStatus, PendingDecision,
    RepairKind, RepairOutcome, UserDecision, UserOption,
};
use crate::traits::StateStore;

// ─── MonitorConfig ───────────────────────────────────────────────────────────

/// Tuning parameters for the health-monitor loop.
#[derive(Debug, Clone)]
pub struct MonitorConfig {
    /// How often to run all checkers.
    pub check_interval: Duration,
    /// Idle time before the monitor considers initiating an autonomous repair.
    pub repair_grace_period: Duration,
    /// Maximum number of concurrent repairs.
    pub max_concurrent_repairs: usize,
    /// If `Some`, only run checks between these UTC hours (inclusive).
    pub maintenance_window_utc: Option<(u8, u8)>,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(300),     // 5 min
            repair_grace_period: Duration::from_secs(30), // 30 s
            max_concurrent_repairs: 2,
            maintenance_window_utc: None,
        }
    }
}

// ─── ActiveRepair ────────────────────────────────────────────────────────────

struct ActiveRepair {
    #[allow(dead_code)]
    started_at: SystemTime,
    #[allow(dead_code)]
    issue_tag: String,
}

// ─── HealthMonitor ───────────────────────────────────────────────────────────

/// Runs health checks and automated repairs in the background.
pub struct HealthMonitor {
    config: MonitorConfig,
    checkers: RwLock<Vec<Arc<dyn HealthCheckable>>>,
    store: Arc<dyn StateStore>,
    /// Key = component display name; guards against duplicate concurrent repairs.
    active_repairs: DashMap<String, ActiveRepair>,
    /// Decisions waiting for user input.
    pending_queue: Mutex<Vec<PendingDecision>>,
    /// Latest report per component (for in-process reads without hitting the store).
    latest_reports: DashMap<String, HealthReport>,
}

impl HealthMonitor {
    pub fn new(config: MonitorConfig, store: Arc<dyn StateStore>) -> Self {
        Self {
            config,
            checkers: RwLock::new(vec![]),
            store,
            active_repairs: DashMap::new(),
            pending_queue: Mutex::new(vec![]),
            latest_reports: DashMap::new(),
        }
    }

    /// Register a checker.  Must be called before `run()`.
    pub async fn register(&self, checker: Arc<dyn HealthCheckable>) {
        self.checkers.write().await.push(checker);
    }

    // ── Main loop ────────────────────────────────────────────────────────────

    /// Run the monitor until `shutdown` is cancelled.
    ///
    /// The first cycle is deferred by `check_interval` so launch
    /// stays quiet — `tokio::time::interval` would otherwise fire
    /// the first tick immediately and the monitor would kick off a
    /// repair (notably FTS rebuild) the moment the desktop process
    /// starts, competing with the user's first interaction for the
    /// fast slot. We trust startup-time validators (embed-dim probe
    /// in `bootstrap`) to catch the issues that genuinely matter
    /// before any user input; ongoing drift detection is what this
    /// loop is for, and ongoing drift can wait one cycle.
    pub async fn run(&self, shutdown: CancellationToken) {
        let start = tokio::time::Instant::now() + self.config.check_interval;
        let mut interval = tokio::time::interval_at(start, self.config.check_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if self.in_maintenance_window() {
                        self.run_cycle().await;
                    }
                }
                _ = shutdown.cancelled() => {
                    tracing::info!("HealthMonitor shutting down.");
                    break;
                }
            }
        }
    }

    // ── Single cycle ─────────────────────────────────────────────────────────

    async fn run_cycle(&self) {
        let checkers = self.checkers.read().await.clone();
        for checker in &checkers {
            match checker.check().await {
                Err(e) => {
                    tracing::warn!(
                        component = %checker.component().display_name(),
                        error = %e,
                        "health check failed"
                    );
                }
                Ok(report) => {
                    self.handle_report(checker.as_ref(), report).await;
                }
            }
        }
    }

    async fn handle_report(&self, checker: &dyn HealthCheckable, report: HealthReport) {
        let key = report.component.display_name();
        self.latest_reports.insert(key.clone(), report.clone());

        if let Err(e) = self.store.save_health_report(&report).await {
            tracing::warn!(error = %e, "failed to persist health report");
        }

        if report.status == HealthStatus::Healthy {
            return;
        }

        let concurrent = self.active_repairs.len();
        if concurrent >= self.config.max_concurrent_repairs {
            return;
        }

        for issue in &report.issues {
            if self.active_repairs.contains_key(&key) {
                break; // one repair per component at a time
            }

            if checker.can_repair_autonomously(issue) {
                self.spawn_repair(checker, issue, &key).await;
            } else {
                self.maybe_surface_decision(checker.component(), issue)
                    .await;
            }
        }
    }

    async fn spawn_repair(&self, checker: &dyn HealthCheckable, issue: &HealthIssue, key: &str) {
        // Mark as active before await to prevent race
        self.active_repairs.insert(
            key.to_string(),
            ActiveRepair {
                started_at: SystemTime::now(),
                issue_tag: issue.tag().to_string(),
            },
        );

        match checker.repair(issue).await {
            Ok(RepairOutcome::Resolved) => {
                tracing::info!(
                    component = key,
                    issue = issue.tag(),
                    "repair resolved issue"
                );
            }
            Ok(RepairOutcome::PartialProgress { detail }) => {
                tracing::info!(
                    component = key,
                    issue = issue.tag(),
                    detail,
                    "repair made partial progress"
                );
            }
            Ok(RepairOutcome::Failed { reason }) => {
                tracing::warn!(
                    component = key,
                    issue = issue.tag(),
                    reason,
                    "repair failed"
                );
            }
            Ok(RepairOutcome::NeedsUserDecision {
                question,
                options,
                consequence,
            }) => {
                self.surface_decision(
                    checker.component().clone(),
                    issue.clone(),
                    question,
                    options,
                    consequence,
                )
                .await;
            }
            Err(e) => {
                tracing::warn!(component = key, error = %e, "repair returned error");
            }
        }

        self.active_repairs.remove(key);
    }

    async fn maybe_surface_decision(&self, component: Component, issue: &HealthIssue) {
        let pending = self.pending_queue.lock().await;
        // De-duplicate: don't re-surface if already pending for this component+issue.
        if pending
            .iter()
            .any(|d: &PendingDecision| d.matches(&component, issue))
        {
            return;
        }
        drop(pending); // release lock before async call

        // Surface a generic "authorise repair?" decision.
        let question = format!(
            "The {} subsystem has issue `{}`. Authorise automatic repair?",
            component.display_name(),
            issue.tag()
        );
        let options = vec![UserOption {
            kind: RepairKind::Dismiss,
            label: "Dismiss".into(),
            description: "Ignore this issue for now.".into(),
        }];
        self.surface_decision(
            component,
            issue.clone(),
            question,
            options,
            "No repair will run without your authorisation.".into(),
        )
        .await;
    }

    async fn surface_decision(
        &self,
        component: Component,
        issue: HealthIssue,
        question: String,
        options: Vec<UserOption>,
        consequence: String,
    ) {
        let now = SystemTime::now();
        let surfaced_at_secs = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let decision = PendingDecision {
            id: None,
            component,
            issue,
            question,
            options,
            consequence,
            surfaced_at_secs,
            surfaced_at: Some(now),
        };

        if let Err(e) = self.store.save_pending_decision(&decision).await {
            tracing::warn!(error = %e, "failed to persist pending decision");
        }

        self.pending_queue.lock().await.push(decision);
    }

    // ── Public query API ─────────────────────────────────────────────────────

    /// Get the most recent cached report for a component, without re-running the check.
    pub fn latest_report(&self, component: &Component) -> Option<HealthReport> {
        self.latest_reports
            .get(&component.display_name())
            .map(|r| r.clone())
    }

    /// All decisions currently awaiting user input.
    pub async fn pending_queue(&self) -> Vec<PendingDecision> {
        self.pending_queue.lock().await.clone()
    }

    /// Apply a user decision; removes the pending entry and marks it resolved in the store.
    pub async fn apply_user_decision(&self, decision: UserDecision) -> Result<()> {
        self.store
            .resolve_pending_decision(decision.decision_id, decision.chosen)
            .await?;

        let mut pending = self.pending_queue.lock().await;
        let resolved_id = decision.decision_id;
        pending.retain(|d: &PendingDecision| d.id != Some(resolved_id));
        Ok(())
    }

    // ── Maintenance window ───────────────────────────────────────────────────

    fn in_maintenance_window(&self) -> bool {
        match self.config.maintenance_window_utc {
            None => true,
            Some((start_h, end_h)) => {
                use std::time::UNIX_EPOCH;
                let secs = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let hour = ((secs % 86400) / 3600) as u8;
                hour >= start_h && hour <= end_h
            }
        }
    }
}
