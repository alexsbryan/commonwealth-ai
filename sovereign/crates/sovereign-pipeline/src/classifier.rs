//! Classify a failed exec into a coarse bucket.
//!
//! Failure buckets are how the operator decides what to do next:
//!
//! - `timeout`     — the command outlived `timeout_secs`. Usually
//!                   a model stall or a wedged peer. Bumping the
//!                   timeout once is reasonable; twice is a bug.
//! - `refused`     — peer is up but rejected the connection. Often
//!                   the mesh is briefly out of capacity; retry is
//!                   safe and usually wins.
//! - `vram_thrash` — model exhausted GPU VRAM. Capacity bug, not
//!                   transient; reduce slot count or quant.
//! - `mismatch`    — the command exited non-zero with output that
//!                   suggests bad input (404 on a slug, schema fail).
//!                   Retry won't help; needs a recipe fix.
//! - `unknown`     — anything else. Surfaces in the failure-bucket
//!                   histogram; if `unknown` grows, add a rule.
//!
//! Built-in rules are applied first, then user-supplied rules from
//! the recipe, then the default `unknown` bucket.

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
    &[
        BuiltinRule {
            bucket: "vram_thrash",
            pattern: r"(?i)out of memory|VRAM|cudaMalloc|HIP out of memory|hipMalloc",
        },
        BuiltinRule {
            bucket: "refused",
            pattern: r"(?i)Connection refused|connect failed|ECONNREFUSED",
        },
        BuiltinRule {
            bucket: "model_missing",
            pattern: r"(?i)no model named|model not found|model_missing",
        },
        BuiltinRule {
            bucket: "mismatch",
            pattern: r"(?i)404 Not Found|invalid slug|unknown category",
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
