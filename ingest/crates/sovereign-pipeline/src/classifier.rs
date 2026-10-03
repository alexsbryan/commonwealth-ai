// SPDX-License-Identifier: AGPL-3.0-or-later
//! Classify a failed exec into a coarse bucket.
//!
//! Failure buckets are how the operator decides what to do next:
//!
//! - `timeout`                    — the command outlived `timeout_secs`. Usually
//!                                  a model stall or a wedged peer. Bumping
//!                                  once is reasonable; twice is a bug.
//! - `vram_thrash`                — GPU OOM (`out of memory`, `hipMalloc`,
//!                                  `cudaMalloc`). Capacity bug; reduce slots
//!                                  or quant.
//! - `gpu_vulkan`                 — Vulkan runtime error that isn't pure OOM
//!                                  (`VK_ERROR_*`, `ggml_vulkan ... error`).
//!                                  Backend-attributable.
//! - `gpu_rocm`                   — ROCm runtime error that isn't pure OOM
//!                                  (`HIP error`, `rocBLAS`, `ggml-cuda: ...
//!                                  error`). Backend-attributable.
//! - `inference_json_parse`       — the LLM produced JSON that didn't pass
//!                                  the constraint (`response contained no
//!                                  recognisable JSON object`). This is the
//!                                  primary signal for grammar / sampling
//!                                  regressions and is the bucket to watch
//!                                  when comparing inference backends.
//! - `inference_5xx`              — daemon HTTP 5xx response to a
//!                                  `/v1/chat/completions` call. Often peer
//!                                  overload or an internal model crash;
//!                                  retry usually wins.
//! - `daemon_down`                — `enrich` ran while the daemon was not
//!                                  reachable (restart, oom-killed). The
//!                                  attempt aborts before any real work.
//! - `refused`                    — connect-level rejection (`Connection
//!                                  refused`). Cousin of `daemon_down` but
//!                                  fires on connect-time only.
//! - `model_missing`              — pinned model not loaded.
//! - `stale_cache`                — phase-N cache exists but was produced by
//!                                  a different pipeline (`cache has no
//!                                  <key> payloads`). App-state, not load.
//! - `mismatch`                   — bad input (404 slug, schema fail).
//! - `phase_failed`               — generic atlas-pipeline phase failure
//!                                  (`error: phase N (atlas) failed`) not
//!                                  caught by anything narrower.
//! - `build_step_failed`          — broadest fallback for `enrich build`
//!                                  failures (`error: step \`X\` exited`).
//! - `unknown`                    — nothing matched. If `unknown` grows in
//!                                  the histogram, add a rule.
//!
//! ## Backend comparison
//!
//! When evaluating a backend switch (e.g. ROCm → Vulkan), the buckets
//! that can actually move are `inference_json_parse`, `inference_5xx`,
//! `vram_thrash`, `gpu_vulkan`, `gpu_rocm`. App-state buckets like
//! `stale_cache` and operational buckets like `daemon_down` should be
//! stable across backends — a delta in those is unrelated noise.
//!
//! ## Ordering
//!
//! Built-in rules are applied first, in declaration order — most
//! specific first. Once any rule matches, that bucket wins. Custom
//! rules from the recipe run only if no built-in matched, then the
//! default `unknown` bucket.

use crate::recipe::ClassifierRule;

#[derive(Debug, Clone, Copy)]
pub enum ExecOutcome<'a> {
    Timeout,
    Exit {
        code: Option<i32>,
        combined_output: &'a str,
    },
}

pub fn classify(outcome: ExecOutcome<'_>, custom: &[ClassifierRule]) -> &'static str {
    match outcome {
        ExecOutcome::Timeout => "timeout",
        ExecOutcome::Exit {
            code,
            combined_output,
        } => {
            if code == Some(124) {
                return "timeout";
            }
            for rule in builtin_rules() {
                if rule.matches(combined_output) {
                    return rule.bucket;
                }
            }
            for rule in custom {
                if regex_match(&rule.stderr_pattern, combined_output) {
                    // Leak-once into a small set so we can return
                    // `&'static str`; the recipe is loaded once per
                    // process so the set stays bounded.
                    return leak_bucket(&rule.bucket);
                }
            }
            "unknown"
        }
    }
}

struct BuiltinRule {
    bucket: &'static str,
    pattern: &'static str,
}

impl BuiltinRule {
    fn matches(&self, hay: &str) -> bool {
        regex_match(self.pattern, hay)
    }
}

fn builtin_rules() -> &'static [BuiltinRule] {
    // Order is significant: first match wins. Specific patterns must
    // appear before generic catch-alls (phase_failed, build_step_failed).
    &[
        // --- GPU subsystem ---------------------------------------------
        BuiltinRule {
            bucket: "vram_thrash",
            // OOM is its own bucket — distinct from runtime-error buckets
            // below — because the operator response differs (reduce
            // slots/quant vs. retry).
            pattern: r"(?i)out of memory|cudaMalloc|HIP out of memory|hipMalloc",
        },
        BuiltinRule {
            bucket: "gpu_vulkan",
            // VK_ERROR_* are the canonical Vulkan failure codes;
            // ggml_vulkan logs surface non-OOM Vulkan issues through to
            // the daemon's 5xx body.
            pattern: r"(?i)VK_ERROR_[A-Z_]+|ggml_vulkan[^\n]*(error|fail)",
        },
        BuiltinRule {
            bucket: "gpu_rocm",
            // `hipError` excludes hipMalloc (handled above as vram_thrash);
            // rocBLAS and ggml-cuda errors round out the ROCm surface.
            pattern: r"(?i)\bHIP error\b|rocBLAS\b|ggml-cuda[^\n]*error",
        },
        // --- Inference layer -------------------------------------------
        BuiltinRule {
            bucket: "inference_json_parse",
            // The LLM produced output that the constraint enforcer
            // couldn't decode as a JSON object. Root cause for the
            // downstream `extract` retry-exhaustion that shows up in
            // many tasks.
            pattern: r"(?i)response contained no recognisable JSON|parse error: Serialization error.*phase \d+",
        },
        BuiltinRule {
            bucket: "inference_5xx",
            // 5xx from the daemon's chat-completions endpoint. Usually
            // peer overload or internal model crash.
            pattern: r"(?i)/v1/chat/completions[^\n]*?\b5\d{2}\b|/v1/chat/completions[^\n]*(server error|internal error)",
        },
        // --- Operational -----------------------------------------------
        BuiltinRule {
            bucket: "daemon_down",
            // `enrich` checks the daemon before touching real work; the
            // `is not responding` message comes from that pre-flight.
            pattern: r"daemon is not responding|daemon (was )?not reachable",
        },
        BuiltinRule {
            bucket: "refused",
            pattern: r"(?i)Connection refused|connect failed|ECONNREFUSED",
        },
        BuiltinRule {
            bucket: "model_missing",
            pattern: r"(?i)no model named|model not found|model_missing",
        },
        // --- App state -------------------------------------------------
        BuiltinRule {
            bucket: "stale_cache",
            // Phase-N cache file exists but was produced by a different
            // pipeline so the keys it advertises don't include what the
            // current pipeline needs.
            pattern: r"(?i)cache has no .+ payloads|cache version mismatch|incompatible cache format",
        },
        BuiltinRule {
            bucket: "mismatch",
            pattern: r"(?i)404 Not Found|invalid slug|unknown category",
        },
        // --- Generic catch-alls ----------------------------------------
        BuiltinRule {
            bucket: "phase_failed",
            // Atlas pipeline phase failure that didn't fall into anything
            // narrower. Kept generic on purpose — gives the operator a
            // single class to grep when a new failure mode appears.
            pattern: r"error: phase \d+ \([^)]+\) failed",
        },
        BuiltinRule {
            bucket: "build_step_failed",
            // Broadest fallback for `enrich build` failures. Matches the
            // final summary line that every failed build step emits.
            pattern: r"error: step ['`][^'`]+['`] exited with code",
        },
    ]
}

fn regex_match(pattern: &str, hay: &str) -> bool {
    match regex::Regex::new(pattern) {
        Ok(re) => re.is_match(hay),
        Err(_) => false,
    }
}

/// Interns a custom bucket name into a process-wide set so the
/// classifier can return `&'static str` like the built-ins.
fn leak_bucket(name: &str) -> &'static str {
    use std::collections::HashSet;
    use std::sync::Mutex;
    static INTERNED: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);
    let mut guard = INTERNED.lock().unwrap();
    let set = guard.get_or_insert_with(HashSet::new);
    if let Some(&hit) = set.iter().find(|s| *s == &name) {
        return hit;
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    set.insert(leaked);
    leaked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_outcome_routes_to_timeout() {
        assert_eq!(classify(ExecOutcome::Timeout, &[]), "timeout");
    }

    #[test]
    fn exit_124_also_timeout() {
        let r = classify(
            ExecOutcome::Exit {
                code: Some(124),
                combined_output: "anything",
            },
            &[],
        );
        assert_eq!(r, "timeout");
    }

    #[test]
    fn vram_pattern_caught_by_builtin() {
        let r = classify(
            ExecOutcome::Exit {
                code: Some(1),
                combined_output: "ggml-cuda: cudaMalloc failed: out of memory",
            },
            &[],
        );
        assert_eq!(r, "vram_thrash");
    }

    #[test]
    fn vulkan_runtime_error_caught() {
        let r = classify(
            ExecOutcome::Exit {
                code: Some(1),
                combined_output: "ggml_vulkan: command buffer submit error\nbuild stopped",
            },
            &[],
        );
        assert_eq!(r, "gpu_vulkan");
    }

    #[test]
    fn vulkan_vk_error_code_caught() {
        let r = classify(
            ExecOutcome::Exit {
                code: Some(1),
                combined_output: "vk::Result::Err(VK_ERROR_DEVICE_LOST)",
            },
            &[],
        );
        assert_eq!(r, "gpu_vulkan");
    }

    #[test]
    fn rocm_runtime_error_caught() {
        let r = classify(
            ExecOutcome::Exit {
                code: Some(1),
                combined_output: "ggml-cuda: kernel launch error\nrocBLAS_status: success",
            },
            &[],
        );
        assert_eq!(r, "gpu_rocm");
    }

    #[test]
    fn rocm_hip_error_caught() {
        let r = classify(
            ExecOutcome::Exit {
                code: Some(1),
                combined_output: "HIP error: hipErrorIllegalAddress at queue.cpp:42",
            },
            &[],
        );
        assert_eq!(r, "gpu_rocm");
    }

    #[test]
    fn json_parse_failure_caught() {
        // The exact phrasing from sovereign-enrich when a grammar-
        // constrained completion produced unparseable output.
        let r = classify(
            ExecOutcome::Exit {
                code: Some(1),
                combined_output: "parse error: Serialization error: phase 1 (atlas) response contained no recognisable JSON object | response[head]: { \"section_id\": \"sec_0001\"...",
            },
            &[],
        );
        assert_eq!(r, "inference_json_parse");
    }

    #[test]
    fn inference_5xx_caught() {
        let r = classify(
            ExecOutcome::Exit {
                code: Some(1),
                combined_output: "/v1/chat/completions returned 502 server error",
            },
            &[],
        );
        assert_eq!(r, "inference_5xx");
    }

    #[test]
    fn daemon_down_caught() {
        // What enrich prints when its pre-flight finds no daemon.
        let r = classify(
            ExecOutcome::Exit {
                code: Some(1),
                combined_output: "note: daemon is not responding at http://localhost:9741.",
            },
            &[],
        );
        assert_eq!(r, "daemon_down");
    }

    #[test]
    fn stale_cache_caught() {
        // Real SEP failure when an old extract cache was produced by
        // a different atlas pipeline. Currently 22 of 53 SEP retries.
        let r = classify(
            ExecOutcome::Exit {
                code: Some(1),
                combined_output: "error: phase 2 (atlas) failed: Invalid input: phase 1 cache has no section_extraction payloads — re-init with an atlas pipeline (e.g. literary_atlas) and re-run extract before clustering",
            },
            &[],
        );
        // stale_cache (more specific) must beat phase_failed (generic).
        assert_eq!(r, "stale_cache");
    }

    #[test]
    fn phase_failed_catches_generic_atlas_failure() {
        // A phase-N failure whose cause text doesn't match any
        // narrower pattern — phase_failed is the fallback.
        let r = classify(
            ExecOutcome::Exit {
                code: Some(1),
                combined_output:
                    "error: phase 3 (cluster) failed: some new failure shape we haven't seen",
            },
            &[],
        );
        assert_eq!(r, "phase_failed");
    }

    #[test]
    fn build_step_failed_is_broadest_catch() {
        // No phase failure, no inference signal, just the final summary.
        let r = classify(
            ExecOutcome::Exit {
                code: Some(1),
                combined_output: "  ! auto-retry left 2 retriable failure(s) unresolved\nerror: step `extract` exited with code 1. Build stopped.",
            },
            &[],
        );
        assert_eq!(r, "build_step_failed");
    }

    #[test]
    fn json_parse_beats_phase_failed_when_both_present() {
        // Real-world output has both the inner parse error and the
        // outer "phase N failed". The narrower root-cause wins.
        let combined = "parse error: Serialization error: phase 1 (atlas) response contained no recognisable JSON object\nerror: phase 1 (atlas) failed: see above";
        let r = classify(
            ExecOutcome::Exit {
                code: Some(1),
                combined_output: combined,
            },
            &[],
        );
        assert_eq!(r, "inference_json_parse");
    }

    #[test]
    fn refused_pattern_caught_by_builtin() {
        let r = classify(
            ExecOutcome::Exit {
                code: Some(1),
                combined_output: "reqwest error: Connection refused (os error 111)",
            },
            &[],
        );
        assert_eq!(r, "refused");
    }

    #[test]
    fn unknown_falls_through() {
        let r = classify(
            ExecOutcome::Exit {
                code: Some(1),
                combined_output: "garbled nonsense",
            },
            &[],
        );
        assert_eq!(r, "unknown");
    }

    #[test]
    fn custom_rule_applies_after_builtins() {
        let custom = vec![ClassifierRule {
            bucket: "schema_drift".into(),
            stderr_pattern: r"schema version mismatch".into(),
        }];
        let r = classify(
            ExecOutcome::Exit {
                code: Some(2),
                combined_output: "schema version mismatch: expected 3 got 2",
            },
            &custom,
        );
        assert_eq!(r, "schema_drift");
    }

    #[test]
    fn builtin_wins_when_both_match() {
        // Recipe wants `peer_dropped` for "Connection refused", but
        // the built-in `refused` rule fires first. This is by design:
        // built-ins are the lingua franca, so dashboards across
        // recipes compare apples-to-apples.
        let custom = vec![ClassifierRule {
            bucket: "peer_dropped".into(),
            stderr_pattern: r"Connection refused".into(),
        }];
        let r = classify(
            ExecOutcome::Exit {
                code: Some(1),
                combined_output: "Connection refused",
            },
            &custom,
        );
        assert_eq!(r, "refused");
    }
}
