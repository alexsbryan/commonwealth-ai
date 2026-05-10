//! Token-bucket rate limiter for the HTTP API acquirer.
//!
//! Each acquirer instance owns one [`TokenBucket`] sized by the
//! recipe's `rate_limit_per_second`. Every outbound request awaits a
//! token before firing. Burst capacity equals the per-second rate
//! (clamped to `[1.0, 10.0]`) so a brief idle period lets a few
//! back-to-back requests through without blocking — this matches the
//! way most rate-limited APIs are tolerant of bursts up to a small
//! ceiling.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

/// Token bucket. Cheap to clone — internally an `Arc<Mutex<…>>` so
/// concurrent tasks share the same accounting.
#[derive(Clone)]
pub struct TokenBucket {
    inner: Arc<Mutex<Inner>>,
    rate_per_second: f32,
    capacity: f32,
}

struct Inner {
    tokens: f32,
    last_refill: Instant,
}

impl TokenBucket {
    /// Construct a bucket that admits `rate_per_second` requests
    /// per second on average. `rate_per_second` <= 0 produces a
    /// no-op bucket (all `wait()` calls return immediately).
    pub fn new(rate_per_second: f32) -> Self {
        let rate = rate_per_second.max(0.0);
        let capacity = if rate <= 0.0 {
            1.0
        } else {
            rate.clamp(1.0, 10.0)
        };
        Self {
            inner: Arc::new(Mutex::new(Inner {
                tokens: capacity,
                last_refill: Instant::now(),
            })),
            rate_per_second: rate,
            capacity,
        }
    }

    /// No-op bucket — every `wait()` returns immediately. Used by
    /// the acquirer when the recipe sets `rate_limit_per_second =
    /// None`.
    pub fn unlimited() -> Self {
        Self::new(0.0)
    }

    /// Block until a token is available, then consume it. Safe to
    /// call concurrently — concurrent waiters serialise behind the
    /// internal mutex.
    pub async fn wait(&self) {
        if self.rate_per_second <= 0.0 {
            return;
        }
        loop {
            // Refill what's accumulated since the last check, then
            // try to consume a token.
            let wait_for_secs = {
                let mut inner = self.inner.lock().await;
                let now = Instant::now();
                let elapsed = (now - inner.last_refill).as_secs_f32();
                inner.tokens =
                    (inner.tokens + elapsed * self.rate_per_second).min(self.capacity);
                inner.last_refill = now;
                if inner.tokens >= 1.0 {
                    inner.tokens -= 1.0;
                    return;
                }
                // Time until the next token: (1 - tokens) / rate.
                ((1.0 - inner.tokens) / self.rate_per_second).max(0.001)
            };
            tokio::time::sleep(Duration::from_secs_f32(wait_for_secs)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn unlimited_bucket_does_not_block() {
        let b = TokenBucket::unlimited();
        // 100 immediate waits should complete with no advance of the
        // (paused) clock.
        for _ in 0..100 {
            b.wait().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn bucket_throttles_after_capacity_exhausted() {
        // Rate 2 r/s → capacity 2 (clamp lower bound is 1.0, upper
        // 10.0). Two immediate waits succeed; the third should sleep
        // ~500ms.
        let b = TokenBucket::new(2.0);
        b.wait().await;
        b.wait().await;
        let before = tokio::time::Instant::now();
        b.wait().await;
        let elapsed = before.elapsed();
        // Allow a generous window — the test runtime may round.
        assert!(
            elapsed >= Duration::from_millis(450),
            "third wait should have throttled, elapsed = {elapsed:?}"
        );
    }
}
