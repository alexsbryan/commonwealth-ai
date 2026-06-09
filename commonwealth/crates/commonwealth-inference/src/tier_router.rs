// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-request tier routing.
//!
//! Evaluates routing rules against an incoming request to decide whether
//! it goes to the Quality, Throughput, or FastSlot tier. Rules are
//! evaluated in priority order; the highest-priority matching rule wins.

use crate::plan::{RequestRouter, RoutingCondition, Tier};

/// Route a request to a tier based on the router configuration.
///
/// Evaluates rules in descending priority order. The first matching
/// rule determines the tier. If no rules match, returns `default_tier`.
pub fn route_request(router: &RequestRouter, request: &RequestContext) -> Tier {
    // Sort rules by priority (highest first).
    let mut rules = router.routing_rules.clone();
    rules.sort_by(|a, b| b.priority.cmp(&a.priority));

    for rule in &rules {
        if evaluate_condition(&rule.condition, request) {
            return rule.target;
        }
    }

    router.default_tier
}

/// Context about an incoming request, used to evaluate routing conditions.
pub struct RequestContext {
    /// Max tokens requested (0 if not specified).
    pub max_tokens: u32,
    /// Whether the request explicitly asked for the quality tier.
    pub quality_hint: bool,
    /// Current depth of the quality tier queue.
    pub quality_queue_depth: usize,
    /// Whether the requester's fairness ledger is in credit.
    pub requester_in_credit: bool,
}

fn evaluate_condition(condition: &RoutingCondition, ctx: &RequestContext) -> bool {
    match condition {
        RoutingCondition::MaxTokensAbove(threshold) => ctx.max_tokens > *threshold,
        RoutingCondition::QualityHint => ctx.quality_hint,
        RoutingCondition::QualityQueueDepthBelow(threshold) => ctx.quality_queue_depth < *threshold,
        RoutingCondition::RequesterInCredit => ctx.requester_in_credit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::RoutingRule;

    fn tiered_router() -> RequestRouter {
        RequestRouter {
            default_tier: Tier::Throughput,
            routing_rules: vec![
                RoutingRule {
                    condition: RoutingCondition::MaxTokensAbove(4096),
                    target: Tier::Quality,
                    priority: 10,
                },
                RoutingRule {
                    condition: RoutingCondition::QualityHint,
                    target: Tier::Quality,
                    priority: 20,
                },
            ],
        }
    }

    #[test]
    fn simple_request_goes_to_throughput() {
        let router = tiered_router();
        let ctx = RequestContext {
            max_tokens: 100,
            quality_hint: false,
            quality_queue_depth: 0,
            requester_in_credit: true,
        };
        assert_eq!(route_request(&router, &ctx), Tier::Throughput);
    }

    #[test]
    fn complex_request_goes_to_quality() {
        let router = tiered_router();
        let ctx = RequestContext {
            max_tokens: 8192,
            quality_hint: false,
            quality_queue_depth: 0,
            requester_in_credit: true,
        };
        assert_eq!(route_request(&router, &ctx), Tier::Quality);
    }

    #[test]
    fn quality_hint_overrides_token_count() {
        let router = tiered_router();
        let ctx = RequestContext {
            max_tokens: 50, // small request
            quality_hint: true,
            quality_queue_depth: 0,
            requester_in_credit: true,
        };
        assert_eq!(route_request(&router, &ctx), Tier::Quality);
    }

    #[test]
    fn no_rules_returns_default() {
        let router = RequestRouter {
            default_tier: Tier::FastSlot,
            routing_rules: vec![],
        };
        let ctx = RequestContext {
            max_tokens: 8192,
            quality_hint: true,
            quality_queue_depth: 0,
            requester_in_credit: true,
        };
        assert_eq!(route_request(&router, &ctx), Tier::FastSlot);
    }

    #[test]
    fn highest_priority_wins() {
        // Quality hint (priority 20) should win over max_tokens (priority 10)
        // even though both match. Quality hint targets Quality, max_tokens
        // also targets Quality — but if we added a conflicting rule:
        let router = RequestRouter {
            default_tier: Tier::Throughput,
            routing_rules: vec![
                RoutingRule {
                    condition: RoutingCondition::MaxTokensAbove(100),
                    target: Tier::Throughput, // tries to keep in throughput
                    priority: 5,
                },
                RoutingRule {
                    condition: RoutingCondition::QualityHint,
                    target: Tier::Quality,
                    priority: 20,
                },
            ],
        };
        let ctx = RequestContext {
            max_tokens: 200,
            quality_hint: true,
            quality_queue_depth: 0,
            requester_in_credit: true,
        };
        // Quality hint (priority 20) wins over max_tokens (priority 5)
        assert_eq!(route_request(&router, &ctx), Tier::Quality);
    }
}
