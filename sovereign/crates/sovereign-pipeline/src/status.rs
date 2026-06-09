// SPDX-License-Identifier: AGPL-3.0-or-later
//! Read-only status reporter — pulls from the worklist DB and prints
//! a human-friendly summary. Works whether or not a driver is alive.
//!
//! The driver emits the same numbers as periodic log lines while it
//! runs; `pipeline status` is for after-the-fact triage and for the
//! daytime check-in when the night job has paused.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::worklist::Worklist;

#[derive(Debug, Clone)]
pub struct StatusReport {
    pub recipe_id: String,
    pub pending: u64,
    pub claimed: u64,
    pub done: u64,
    pub failed: u64,
    pub total: u64,
    pub done_last_hour: u64,
    pub failure_buckets: std::collections::BTreeMap<String, u64>,
}

impl StatusReport {
    pub fn render(&self) -> String {
        let pct_done = if self.total == 0 {
            0.0
        } else {
            self.done as f64 * 100.0 / self.total as f64
        };
        let rate = self.done_last_hour as f64;
        let remaining = self.pending + self.claimed;
        let eta = if rate > 0.0 {
            let hours = remaining as f64 / rate;
            if hours < 1.0 {
                format!("{}m", (hours * 60.0).round() as u64)
            } else if hours < 48.0 {
                format!("{:.1}h", hours)
            } else {
                format!("{:.1}d", hours / 24.0)
            }
        } else if remaining == 0 {
            "0m".into()
        } else {
            "?".into()
        };
        let mut out = String::new();
        out.push_str(&format!("recipe: {}\n", self.recipe_id));
        out.push_str(&format!(
            "  done    {:>6} / {} ({:.1}%)\n",
            self.done, self.total, pct_done
        ));
        out.push_str(&format!("  pending {:>6}\n", self.pending));
        out.push_str(&format!("  claimed {:>6}\n", self.claimed));
        out.push_str(&format!("  failed  {:>6}\n", self.failed));
        out.push_str(&format!("  rate    {:>6.1} / hr (last 60m)\n", rate));
        out.push_str(&format!("  eta     {:>6}\n", eta));
        if !self.failure_buckets.is_empty() {
            out.push_str("  failure buckets:\n");
            for (bucket, count) in &self.failure_buckets {
                out.push_str(&format!("    {:<14} {}\n", bucket, count));
            }
        }
        out
    }
}

pub fn report(worklist: &Worklist, recipe_id: &str) -> crate::worklist::Result<StatusReport> {
    let stats = worklist.stats(recipe_id)?;
    let since = unix_now() - 3600;
    let done_last_hour = worklist.completed_since(recipe_id, since)?;
    Ok(StatusReport {
        recipe_id: recipe_id.to_string(),
        pending: stats.pending,
        claimed: stats.claimed,
        done: stats.done,
        failed: stats.failed,
        total: stats.total,
        done_last_hour,
        failure_buckets: stats.failure_buckets,
    })
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_includes_recipe_and_counts() {
        let mut wl = Worklist::open_in_memory().unwrap();
        wl.seed("r", ["a", "b", "c"]).unwrap();
        let claimed = wl.claim("r", "drv", 1, 60).unwrap();
        wl.ack_success("r", &claimed[0]).unwrap();
        let r = report(&wl, "r").unwrap();
        let rendered = r.render();
        assert!(rendered.contains("recipe: r"));
        assert!(rendered.contains("done         1 / 3"));
        assert!(rendered.contains("pending      2"));
    }

    #[test]
    fn render_lists_failure_buckets() {
        let mut wl = Worklist::open_in_memory().unwrap();
        wl.seed("r", ["x"]).unwrap();
        let c = wl.claim("r", "drv", 1, 60).unwrap();
        wl.ack_failure("r", &c[0], "boom", "vram_thrash", 1)
            .unwrap();
        let r = report(&wl, "r").unwrap();
        let rendered = r.render();
        assert!(rendered.contains("failure buckets"));
        assert!(rendered.contains("vram_thrash"));
    }
}
