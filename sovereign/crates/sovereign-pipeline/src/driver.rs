//! The pipeline run-loop.
//!
//! ## Operational shape
//!
//! These jobs run at night and pause during the day, so failure,
//! retry, and resume-ability are **load-bearing**, not best-effort:
//!
//! - **Pause**: SIGTERM / SIGINT stops claiming new work, lets
//!   in-flight units finish, then exits cleanly. No in-flight loss.
//! - **Resume**: re-running the driver picks up where it left off.
//!   Pending rows are claimed first; abandoned `claimed` rows from
//!   a `SIGKILL`'d driver are swept back to `pending` on startup.
//! - **Daytime auto-pause**: if the recipe declares `active_hours`,
//!   the driver stops claiming when out-of-window, drains in-flight,
//!   then sleeps in 60-second ticks until the window opens again.
//! - **Failure isolation**: a unit that exhausts its retries lands
//!   in `failed` with a bucket tag, and the loop keeps going. One
//!   poisoned slug can't take down the run.
//!
//! ## Reporting
//!
//! A live one-line status is emitted every `STATUS_TICK_SECS`
//! (default 10s) via `tracing::info!`. Operators tail the driver
//! log to see throughput; `sovereign pipeline status` reads the
//! same numbers from the DB without needing the driver alive.

use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Local, Timelike};
use thiserror::Error;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::task::JoinSet;

use crate::adaptive::{outcome_from_bucket, AdaptiveConcurrency, Outcome};
use crate::classifier::{classify, ExecOutcome};
use crate::recipe::{parse_window, Recipe, Schedule};
use crate::worklist::{Worklist, WorklistError};

const STATUS_TICK_SECS: u64 = 10;
const CLAIM_BATCH_MIN: u32 = 1;

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("worklist: {0}")]
    Worklist(#[from] WorklistError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("recipe: {0}")]
    Recipe(#[from] crate::recipe::RecipeError),
    #[error("shutdown channel closed unexpectedly")]
    ShutdownClosed,
}

pub type Result<T> = std::result::Result<T, DriverError>;

/// Outcome the driver returns when its main loop exits.
#[derive(Debug, Clone, Default)]
pub struct RunSummary {
    pub started_at_unix: i64,
    pub finished_at_unix: i64,
    pub succeeded: u64,
    pub failed: u64,
    pub pending_remaining: u64,
    pub paused: bool,
}

/// Driver configuration — mostly pulled from the recipe but the
/// caller controls runtime knobs (DB path, driver id, shutdown).
pub struct DriverConfig {
    pub driver_id: String,
    pub status_tick: Duration,
}

impl Default for DriverConfig {
    fn default() -> Self {
        Self {
            driver_id: uuid::Uuid::new_v4().to_string(),
            status_tick: Duration::from_secs(STATUS_TICK_SECS),
        }
    }
}

/// A shutdown signal — flip `requested` to true and the driver will
/// stop claiming new work, wait for in-flight to drain, and return.
#[derive(Clone, Default)]
pub struct Shutdown {
    pub requested: Arc<std::sync::atomic::AtomicBool>,
}

impl Shutdown {
    pub fn request(&self) {
        self.requested
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    pub fn requested(&self) -> bool {
        self.requested.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Run a single recipe to completion (or until shutdown).
pub async fn run_recipe(
    recipe: Recipe,
    worklist: Arc<Mutex<Worklist>>,
    cfg: DriverConfig,
    shutdown: Shutdown,
) -> Result<RunSummary> {
    let recipe_id = recipe.recipe.id.clone();
    let started_at = unix_now();

    // 1) Seed the worklist. Idempotent — safe to call on every run.
    {
        let keys = recipe.load_keys()?;
        let mut wl = worklist.lock().await;
        let inserted = wl.seed(&recipe_id, keys)?;
        if inserted > 0 {
            tracing::info!(recipe = %recipe_id, inserted, "seeded worklist");
        }
        // 2) Sweep abandoned claims from any prior crashed driver.
        let swept = wl.sweep_expired_leases(&recipe_id)?;
        if swept > 0 {
            tracing::info!(recipe = %recipe_id, swept, "swept expired leases");
        }
    }

    let mut summary = RunSummary {
        started_at_unix: started_at,
        ..Default::default()
    };

    let adaptive = Arc::new(AdaptiveConcurrency::new(recipe.dispatch.concurrency.max(1)));
    let mut in_flight: JoinSet<UnitResult> = JoinSet::new();
    let mut last_status = Instant::now();

    loop {
        // ── Shutdown / schedule gate ────────────────────────────
        let stop_claiming = shutdown.requested() || !schedule_allows_claiming(&recipe.schedule);

        // ── Top up in-flight up to effective concurrency ────────
        if !stop_claiming {
            let target = adaptive.effective() as usize;
            let want = target.saturating_sub(in_flight.len());
            if want >= CLAIM_BATCH_MIN as usize {
                let claimed: Vec<String> = {
                    let mut wl = worklist.lock().await;
                    wl.claim(
                        &recipe_id,
                        &cfg.driver_id,
                        want as u32,
                        recipe.dispatch.lease_secs,
                    )?
                };
                for key in claimed {
                    let command = recipe.enrich.command.clone();
                    let timeout = Duration::from_secs(recipe.enrich.timeout_secs);
                    let custom = recipe.enrich.failure_classifier.clone();
                    let recipe_for_task = recipe_id.clone();
                    in_flight.spawn(async move {
                        let outcome = exec_one(&command, &key, timeout).await;
                        UnitResult {
                            recipe_id: recipe_for_task,
                            key,
                            outcome,
                            custom,
                        }
                    });
                }
            }
        }

        // ── Reap completions ────────────────────────────────────
        if in_flight.is_empty() {
            if stop_claiming {
                // Out of work *and* we were asked to stop — done.
                summary.paused = shutdown.requested();
                break;
            }
            // No work to do and not paused: either the recipe is
            // genuinely empty/finished, or we're between batches.
            let stats = worklist.lock().await.stats(&recipe_id)?;
            if stats.pending == 0 && stats.claimed == 0 {
                break;
            }
            // Otherwise: brief sleep before retrying. This is the
            // case where another driver holds the rest of the work
            // and we have nothing to claim right now.
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }

        // Wait for either: next completion, or a periodic tick to
        // re-evaluate schedule + status.
        let tick = tokio::time::sleep(cfg.status_tick);
        tokio::pin!(tick);

        tokio::select! {
            res = in_flight.join_next() => {
                if let Some(joined) = res {
                    match joined {
                        Ok(unit) => {
                            handle_completion(&unit, &recipe, &worklist, &mut summary, &adaptive).await?;
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "in-flight task panicked");
                        }
                    }
                }
            }
            _ = &mut tick => {
                // periodic — falls through to status emit below
            }
        }

        // ── Periodic status line ────────────────────────────────
        if last_status.elapsed() >= cfg.status_tick {
            emit_status(&recipe_id, &worklist, &summary, in_flight.len(), &adaptive).await?;
            last_status = Instant::now();
        }
    }

    // Final sync of pending count for the summary.
    let final_stats = worklist.lock().await.stats(&recipe_id)?;
    summary.pending_remaining = final_stats.pending + final_stats.claimed;
    summary.finished_at_unix = unix_now();
    Ok(summary)
}

struct UnitResult {
    recipe_id: String,
    key: String,
    outcome: ExecResult,
    custom: Vec<crate::recipe::ClassifierRule>,
}

#[derive(Debug)]
enum ExecResult {
    Success,
    TimedOut { combined: String },
    Exited { code: Option<i32>, combined: String },
}

async fn handle_completion(
    unit: &UnitResult,
    recipe: &Recipe,
    worklist: &Arc<Mutex<Worklist>>,
    summary: &mut RunSummary,
    adaptive: &Arc<AdaptiveConcurrency>,
) -> Result<()> {
    let mut wl = worklist.lock().await;
    match &unit.outcome {
        ExecResult::Success => {
            wl.ack_success(&unit.recipe_id, &unit.key)?;
            summary.succeeded += 1;
            adaptive.record(Outcome::Success);
            tracing::info!(recipe = %unit.recipe_id, key = %unit.key, "unit success");
        }
        ExecResult::TimedOut { combined } => {
            let bucket = classify(ExecOutcome::Timeout, &unit.custom);
            let state = wl.ack_failure(
                &unit.recipe_id,
                &unit.key,
                combined,
                bucket,
                recipe.dispatch.max_attempts,
            )?;
            if matches!(state, crate::worklist::State::Failed) {
                summary.failed += 1;
            }
            adaptive.record(outcome_from_bucket(bucket));
            tracing::warn!(
                recipe = %unit.recipe_id,
                key = %unit.key,
                bucket,
                ?state,
                "unit timed out"
            );
        }
        ExecResult::Exited { code, combined } => {
            let bucket = classify(
                ExecOutcome::Exit {
                    code: *code,
                    combined_output: combined,
                },
                &unit.custom,
            );
            let state = wl.ack_failure(
                &unit.recipe_id,
                &unit.key,
                combined,
                bucket,
                recipe.dispatch.max_attempts,
            )?;
            if matches!(state, crate::worklist::State::Failed) {
                summary.failed += 1;
            }
            adaptive.record(outcome_from_bucket(bucket));
            tracing::warn!(
                recipe = %unit.recipe_id,
                key = %unit.key,
                code = ?code,
                bucket,
                ?state,
                "unit failed"
            );
        }
    }
    Ok(())
}

async fn exec_one(command_template: &str, key: &str, timeout: Duration) -> ExecResult {
    let cmd = command_template.replace("{key}", key);
    let child = match Command::new("/bin/sh")
        .arg("-c")
        .arg(&cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return ExecResult::Exited {
                code: None,
                combined: format!("spawn failed: {e}"),
            };
        }
    };

    let fut = child.wait_with_output();
    let result = if timeout.is_zero() {
        fut.await.map_err(|e| e.to_string())
    } else {
        match tokio::time::timeout(timeout, fut).await {
            Ok(r) => r.map_err(|e| e.to_string()),
            Err(_elapsed) => {
                return ExecResult::TimedOut {
                    combined: format!(
                        "exceeded timeout of {}s for command: {cmd}",
                        timeout.as_secs()
                    ),
                };
            }
        }
    };

    match result {
        Ok(out) => {
            if out.status.success() {
                ExecResult::Success
            } else {
                let combined = format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr),
                );
                ExecResult::Exited {
                    code: out.status.code(),
                    combined,
                }
            }
        }
        Err(e) => ExecResult::Exited {
            code: None,
            combined: format!("wait failed: {e}"),
        },
    }
}

/// True when the recipe permits claiming new work right now.
fn schedule_allows_claiming(sched: &Option<Schedule>) -> bool {
    match sched {
        None => true,
        Some(s) => {
            let w = match parse_window(&s.active_hours) {
                Ok(w) => w,
                Err(_) => return true, // validate() caught this at load; be permissive
            };
            let now = Local::now();
            w.contains(now.hour() as u8, now.minute() as u8)
        }
    }
}

async fn emit_status(
    recipe_id: &str,
    worklist: &Arc<Mutex<Worklist>>,
    summary: &RunSummary,
    in_flight: usize,
    adaptive: &Arc<AdaptiveConcurrency>,
) -> Result<()> {
    let stats = worklist.lock().await.stats(recipe_id)?;
    let elapsed_secs = (unix_now() - summary.started_at_unix).max(1) as f64;
    let rate_per_hour = summary.succeeded as f64 * 3600.0 / elapsed_secs;
    let eta_str = eta_string(stats.pending + stats.claimed, rate_per_hour);
    tracing::info!(
        recipe = %recipe_id,
        in_flight,
        concurrency_eff = adaptive.effective(),
        concurrency_max = adaptive.configured(),
        pending = stats.pending,
        done = stats.done,
        failed = stats.failed,
        rate_per_hr = format!("{:.1}", rate_per_hour),
        eta = %eta_str,
        "status"
    );
    Ok(())
}

fn eta_string(remaining: u64, rate_per_hour: f64) -> String {
    if remaining == 0 {
        return "0m".into();
    }
    if rate_per_hour <= 0.0 {
        return "?".into();
    }
    let hours = remaining as f64 / rate_per_hour;
    if hours < 1.0 {
        format!("{}m", (hours * 60.0).round() as u64)
    } else if hours < 48.0 {
        format!("{:.1}h", hours)
    } else {
        format!("{:.1}d", hours / 24.0)
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::Recipe;

    fn echo_recipe(keys: &[&str]) -> Recipe {
        let keys_toml = keys.iter().map(|k| format!("\"{k}\"")).collect::<Vec<_>>().join(", ");
        let toml = format!(
            r#"
[recipe]
id = "echo-test"

[source]
type = "inline"
keys = [{keys_toml}]

[enrich]
command = "echo {{key}}"
timeout_secs = 5

[dispatch]
max_attempts = 2
lease_secs = 30
concurrency = 2
"#
        );
        Recipe::from_toml(&toml).unwrap()
    }

    #[tokio::test]
    async fn drives_inline_recipe_to_completion() {
        let wl = Arc::new(Mutex::new(Worklist::open_in_memory().unwrap()));
        let r = echo_recipe(&["alpha", "beta", "gamma"]);
        let cfg = DriverConfig {
            driver_id: "test-drv".into(),
            status_tick: Duration::from_millis(50),
        };
        let summary = run_recipe(r, wl.clone(), cfg, Shutdown::default())
            .await
            .unwrap();
        assert_eq!(summary.succeeded, 3);
        assert_eq!(summary.failed, 0);
        let stats = wl.lock().await.stats("echo-test").unwrap();
        assert_eq!(stats.done, 3);
    }

    #[tokio::test]
    async fn failed_command_retries_and_then_lands_in_failed() {
        let wl = Arc::new(Mutex::new(Worklist::open_in_memory().unwrap()));
        let toml = r#"
[recipe]
id = "fail-test"

[source]
type = "inline"
keys = ["x"]

[enrich]
command = "false # {key}"
timeout_secs = 5

[dispatch]
max_attempts = 2
lease_secs = 30
concurrency = 1
"#;
        let r = Recipe::from_toml(toml).unwrap();
        let cfg = DriverConfig {
            driver_id: "test-drv".into(),
            status_tick: Duration::from_millis(50),
        };
        let summary = run_recipe(r, wl.clone(), cfg, Shutdown::default())
            .await
            .unwrap();
        assert_eq!(summary.succeeded, 0);
        assert_eq!(summary.failed, 1);
        let stats = wl.lock().await.stats("fail-test").unwrap();
        assert_eq!(stats.failed, 1);
        // attempts == 2 (max), state == failed, bucket = unknown
        assert_eq!(*stats.failure_buckets.get("unknown").unwrap_or(&0), 1);
    }

    #[tokio::test]
    async fn timeout_classifies_as_timeout_bucket() {
        let wl = Arc::new(Mutex::new(Worklist::open_in_memory().unwrap()));
        let toml = r#"
[recipe]
id = "to-test"

[source]
type = "inline"
keys = ["x"]

[enrich]
command = "sleep 5 # {key}"
timeout_secs = 1

[dispatch]
max_attempts = 1
lease_secs = 30
concurrency = 1
"#;
        let r = Recipe::from_toml(toml).unwrap();
        let cfg = DriverConfig {
            driver_id: "test-drv".into(),
            status_tick: Duration::from_millis(50),
        };
        let summary = run_recipe(r, wl.clone(), cfg, Shutdown::default())
            .await
            .unwrap();
        assert_eq!(summary.failed, 1);
        let stats = wl.lock().await.stats("to-test").unwrap();
        assert_eq!(*stats.failure_buckets.get("timeout").unwrap_or(&0), 1);
    }

    #[tokio::test]
    async fn shutdown_request_drains_and_exits() {
        // Recipe with a longer-running command; flip shutdown after
        // the first claim and confirm we exit without claiming the
        // second.
        let wl = Arc::new(Mutex::new(Worklist::open_in_memory().unwrap()));
        let toml = r#"
[recipe]
id = "drain-test"

[source]
type = "inline"
keys = ["a", "b"]

[enrich]
command = "sleep 1 # {key}"
timeout_secs = 5

[dispatch]
max_attempts = 1
lease_secs = 30
concurrency = 1
"#;
        let r = Recipe::from_toml(toml).unwrap();
        let cfg = DriverConfig {
            driver_id: "test-drv".into(),
            status_tick: Duration::from_millis(50),
        };
        let shutdown = Shutdown::default();
        let shutdown_clone = shutdown.clone();
        let wl_clone = wl.clone();
        let h = tokio::spawn(async move { run_recipe(r, wl_clone, cfg, shutdown_clone).await });
        // Give the driver time to claim and start the first unit,
        // then flip shutdown.
        tokio::time::sleep(Duration::from_millis(300)).await;
        shutdown.request();
        let summary = h.await.unwrap().unwrap();
        // First unit completes; second is left pending (or maybe
        // claimed depending on timing — we only check that we did
        // not finish everything).
        assert!(summary.paused, "summary should report paused=true");
        let stats = wl.lock().await.stats("drain-test").unwrap();
        assert!(stats.pending + stats.claimed >= 1, "second unit not consumed");
    }

    #[test]
    fn eta_under_an_hour_renders_minutes() {
        assert_eq!(eta_string(50, 100.0), "30m");
    }

    #[test]
    fn eta_zero_remaining_is_zero() {
        assert_eq!(eta_string(0, 0.0), "0m");
    }

    #[test]
    fn eta_zero_rate_is_unknown() {
        assert_eq!(eta_string(1, 0.0), "?");
    }
}
