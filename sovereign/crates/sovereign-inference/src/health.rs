// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct HealthTracker {
    latency_ewma_ms: AtomicU64,
    error_count: AtomicU32,
    request_count: AtomicU32,
    last_success: AtomicU64,
    last_failure: AtomicU64,
    available: AtomicBool,
}

impl HealthTracker {
    pub fn new() -> Self {
        Self {
            latency_ewma_ms: AtomicU64::new(0),
            error_count: AtomicU32::new(0),
            request_count: AtomicU32::new(0),
            last_success: AtomicU64::new(0),
            last_failure: AtomicU64::new(0),
            available: AtomicBool::new(true),
        }
    }

    pub fn record_success(&self, latency_ms: u64) {
        self.request_count.fetch_add(1, Ordering::Relaxed);

        // EWMA with alpha = 0.3
        let prev = self.latency_ewma_ms.load(Ordering::Relaxed);
        let new = if prev == 0 {
            latency_ms
        } else {
            ((prev as f64 * 0.7) + (latency_ms as f64 * 0.3)) as u64
        };
        self.latency_ewma_ms.store(new, Ordering::Relaxed);

        self.last_success.store(now_unix(), Ordering::Relaxed);
        self.available.store(true, Ordering::Relaxed);
    }

    pub fn record_failure(&self) {
        self.request_count.fetch_add(1, Ordering::Relaxed);
        self.error_count.fetch_add(1, Ordering::Relaxed);
        self.last_failure.store(now_unix(), Ordering::Relaxed);

        // Mark unavailable after 3 consecutive failures
        if self.error_count.load(Ordering::Relaxed) >= 3 {
            self.available.store(false, Ordering::Relaxed);
        }
    }

    pub fn is_healthy(&self) -> bool {
        self.available.load(Ordering::Relaxed)
    }

    /// Returns a score from 0.0 (dead) to 1.0 (perfect).
    pub fn health_score(&self) -> f64 {
        if !self.is_healthy() {
            return 0.0;
        }

        let total = self.request_count.load(Ordering::Relaxed) as f64;
        if total == 0.0 {
            return 1.0; // no data, assume healthy
        }

        let errors = self.error_count.load(Ordering::Relaxed) as f64;
        let error_rate = errors / total;

        (1.0 - error_rate).max(0.0)
    }

    pub fn latency_ms(&self) -> u64 {
        self.latency_ewma_ms.load(Ordering::Relaxed)
    }

    /// Reset error count (e.g., after a successful health probe).
    pub fn reset_errors(&self) {
        self.error_count.store(0, Ordering::Relaxed);
        self.available.store(true, Ordering::Relaxed);
    }

    /// Current consecutive error count.
    pub fn error_count(&self) -> u32 {
        self.error_count.load(Ordering::Relaxed)
    }
}

impl Default for HealthTracker {
    fn default() -> Self {
        Self::new()
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
