//! Safety enforcement for corpus acquisition.
//!
//! Hardcoded rules, not configurable by recipes:
//! - robots.txt compliance on all web crawls
//! - Minimum 1-second delay between requests to the same domain
//! - Honest User-Agent header
//! - Web crawl scope: link_pattern must share domain of seed_urls
//! - Download size warning: actual > 1.5x estimated triggers callback

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// User-Agent string for all HTTP requests.
pub const USER_AGENT: &str = "CorpusEngine/0.1 (+https://sovereign.dev/corpus-engine)";

/// Minimum delay between requests to the same domain.
pub const MIN_REQUEST_DELAY: Duration = Duration::from_secs(1);

/// Rate limiter that enforces per-domain request delays.
pub struct RateLimiter {
    last_request: Mutex<HashMap<String, Instant>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            last_request: Mutex::new(HashMap::new()),
        }
    }

    /// Wait if necessary to respect the rate limit for a domain.
    pub async fn wait_for_domain(&self, domain: &str) {
        let sleep_duration = {
            let map = self.last_request.lock().unwrap();
            if let Some(last) = map.get(domain) {
                let elapsed = last.elapsed();
                if elapsed < MIN_REQUEST_DELAY {
                    Some(MIN_REQUEST_DELAY - elapsed)
                } else {
                    None
                }
            } else {
                None
            }
        };

        if let Some(d) = sleep_duration {
            tokio::time::sleep(d).await;
        }

        let mut map = self.last_request.lock().unwrap();
        map.insert(domain.to_string(), Instant::now());
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Validate that a crawl link pattern stays within the seed URL's domain.
pub fn validate_crawl_scope(seed_urls: &[String], link_pattern: &str) -> Result<()> {
    for seed in seed_urls {
        let seed_domain = extract_domain(seed);
        let pattern_domain = extract_domain(link_pattern);

        if !pattern_domain.is_empty() && seed_domain != pattern_domain {
            return Err(Error::Safety(format!(
                "Crawl scope violation: link_pattern domain '{}' does not match \
                 seed_url domain '{}'. Crawls must stay within the seed domain.",
                pattern_domain, seed_domain,
            )));
        }
    }
    Ok(())
}

/// Check if a download size is suspiciously large compared to the estimate.
pub fn check_download_size(actual_bytes: u64, estimated_gb: f64) -> Option<String> {
    if estimated_gb <= 0.0 {
        return None;
    }
    let estimated_bytes = (estimated_gb * 1_073_741_824.0) as u64;
    let threshold = (estimated_bytes as f64 * 1.5) as u64;

    if actual_bytes > threshold {
        Some(format!(
            "Download is {:.1} GB, which is more than 1.5x the estimated {:.1} GB. \
             This may indicate an unexpected data format or source change.",
            actual_bytes as f64 / 1_073_741_824.0,
            estimated_gb,
        ))
    } else {
        None
    }
}

fn extract_domain(url: &str) -> String {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    without_scheme
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_scope_same_domain() {
        assert!(validate_crawl_scope(
            &["https://plato.stanford.edu/entries/".into()],
            "https://plato.stanford.edu/entries/*",
        )
        .is_ok());
    }

    #[test]
    fn validate_scope_different_domain() {
        assert!(validate_crawl_scope(
            &["https://plato.stanford.edu/entries/".into()],
            "https://evil.com/*",
        )
        .is_err());
    }

    #[test]
    fn validate_scope_relative_pattern() {
        // Relative patterns (no domain) are always ok.
        assert!(validate_crawl_scope(&["https://example.com/pages/".into()], "/pages/*",).is_ok());
    }

    #[test]
    fn download_size_within_bounds() {
        assert!(check_download_size(20_000_000_000, 22.0).is_none());
    }

    #[test]
    fn download_size_exceeds_threshold() {
        // 40GB actual vs 22GB estimated = 1.8x > 1.5x threshold
        assert!(check_download_size(40_000_000_000, 22.0).is_some());
    }

    #[test]
    fn download_size_zero_estimate() {
        assert!(check_download_size(1_000_000, 0.0).is_none());
    }

    #[test]
    fn extract_domain_works() {
        assert_eq!(extract_domain("https://example.com/path"), "example.com");
        assert_eq!(extract_domain("http://foo.bar:8080/x"), "foo.bar");
        assert_eq!(extract_domain("/relative/path"), "");
    }

    #[tokio::test]
    async fn rate_limiter_enforces_delay() {
        let limiter = RateLimiter::new();

        let start = Instant::now();
        limiter.wait_for_domain("example.com").await;
        limiter.wait_for_domain("example.com").await;
        let elapsed = start.elapsed();

        // Second request should have waited ~1 second.
        assert!(elapsed >= Duration::from_millis(900));
    }
}
