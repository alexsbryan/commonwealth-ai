// SPDX-License-Identifier: AGPL-3.0-or-later
//! Synthesis-layer role vocabulary — the data-driven counterpart to the
//! agent-loop role layer in `commonwealth-agent-tools/src/role/`.
//!
//! A [`Role`] is a closed enum of the cognitive operations the runtime performs
//! on a knowledge turn. Where the agent-loop roles (Planner/Implementer/
//! Evaluator) gate **tool primitives**, the synthesis roles gate **prompt +
//! tier + a verification contract**. The shape is lifted from the agent-tools
//! `RoleProfile`/`RoleModelMap` (ARCH §6: profiles are *data*), adapted to the
//! synthesis path.
//!
//! Three roles earn first-class status — each has ≥2 callers AND a distinct
//! verify predicate (the keystone discipline: *a `RoleProfile` ships with the
//! predicate that defines its correctness*):
//!
//! - **Router** — classify the turn and resolve its synthesis route/tier.
//!   Mechanism: [`crate::router_embed::EmbedRouter`] (intent) +
//!   [`crate::runtime::resolve_synthesis_route`] (route → tier). Today's wiring
//!   lives in [`crate::router_bootstrap::build_llm_router`].
//! - **Synthesizer** — assemble the grounded answer on the chosen slot.
//!   Mechanism: [`crate::runtime::build_synthesis_system_prompt`] (the prompt
//!   body SSOT) running on the route's slot.
//! - **Critic** — a SEPARATE forward pass that verifies the Synthesizer's
//!   output against the passages. Mechanism today: the bench grounding-verifier
//!   / abstain / caveat classifiers (`sovereign-cli-llm` live runner). Defined
//!   here so the bench and any future production critic share ONE definition;
//!   **not wired into the production synthesis flow yet** (opt-in, measured
//!   follow-on per the role-layer plan).
//!
//! This module is the formal vocabulary + the per-role default profiles. It is
//! intentionally behavior-free: the hot path still calls the mechanism
//! functions directly. The profiles are the documented SSOT for each role's
//! tier/sampling/verification policy, and the navigational index to where each
//! role is implemented.

use serde::{Deserialize, Serialize};

/// Closed enum of synthesis roles. Adding a variant requires touching
/// [`Role::all`], [`Role::id`]/[`Role::from_id`], [`default_profile_for`], and
/// [`RoleModelMap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Router,
    Synthesizer,
    Critic,
}

impl Role {
    pub const fn id(&self) -> &'static str {
        match self {
            Role::Router => "router",
            Role::Synthesizer => "synthesizer",
            Role::Critic => "critic",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "router" => Some(Role::Router),
            "synthesizer" => Some(Role::Synthesizer),
            "critic" => Some(Role::Critic),
            _ => None,
        }
    }

    pub const fn all() -> &'static [Role] {
        &[Role::Router, Role::Synthesizer, Role::Critic]
    }
}

/// The General-tier slot a role prefers. Maps to the runtime's
/// `SynthesisRoute` (Fast → `FastFocused`, Primary → `PrimarySynthesis`); kept
/// as its own enum so the role vocabulary stays decoupled from the per-turn
/// synthesis-routing internals. For the Synthesizer this is only the *default*
/// — the per-turn tier comes from [`crate::runtime::resolve_synthesis_route`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Fast slot (4B/9B): small budget, no thinking. Classification + bounded
    /// contrast.
    Fast,
    /// Primary slot (35B): full budget + thinking. Genuine synthesis +
    /// adversarial verification.
    Primary,
}

impl Tier {
    /// The daemon model-handle stem this tier resolves to (`/v1/models`
    /// aliases). The bridge from the role vocabulary to an actual slot — a
    /// caller that knows a role resolves its model via
    /// `default_profile_for(role).preferred_tier.model_stem()`.
    pub const fn model_stem(&self) -> &'static str {
        match self {
            Tier::Fast => "fast",
            Tier::Primary => "primary",
        }
    }
}

/// Per-role sampling overrides — lifted from the agent-tools shape. `None`
/// fields fall back to the caller's inference config.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SamplingOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

/// The data carrier for a synthesis role's invocation shape. Lifts the
/// agent-tools `RoleProfile` shape, swapping the agent-loop's
/// `allowed_primitives`/`forced_first_tool` for synthesis concerns: the
/// preferred tier and the verification predicate (the keystone — a profile
/// ships with the contract that defines its correctness).
#[derive(Debug, Clone)]
pub struct RoleProfile {
    pub role: Role,
    /// One-line description of the role's job.
    pub description: &'static str,
    /// Default General-tier slot for this role.
    pub preferred_tier: Tier,
    pub sampling: SamplingOverrides,
    /// The correctness contract this role is tuned to pass — the predicate the
    /// bench measures (test/train alignment by construction, the Goodhart
    /// defense). Prose today; a callable predicate when the Critic is wired.
    pub verify_predicate: &'static str,
    /// True iff this role MUST run as its own forward pass (cannot share the
    /// pass it verifies). The Critic's invariant — a model grading its own
    /// single-pass output is self-confirmation, not verification.
    pub separate_forward_pass: bool,
}

/// Compiled-in default profile for a role — the documented SSOT for each
/// role's tier/sampling/verification policy.
pub fn default_profile_for(role: Role) -> RoleProfile {
    match role {
        Role::Router => RoleProfile {
            role: Role::Router,
            description: "Classify the turn and resolve its synthesis route/tier.",
            // Classification + bounded contrast run on the fast slot; the
            // Router escalates to Primary via resolve_synthesis_route, it does
            // not itself need the big model.
            preferred_tier: Tier::Fast,
            sampling: SamplingOverrides {
                temperature: Some(0.0),
                ..Default::default()
            },
            verify_predicate:
                "Routes each turn to the tier the intent + evidence shape warrant; \
                 abstains (falls through to the LLM cascade) when the embed margin is low.",
            separate_forward_pass: false,
        },
        Role::Synthesizer => RoleProfile {
            role: Role::Synthesizer,
            description: "Assemble the grounded answer on the routed slot.",
            // Genuine synthesis defaults to Primary; bounded/concentrated turns
            // are demoted to Fast per resolve_synthesis_route.
            preferred_tier: Tier::Primary,
            // Uses the caller's inference config for sampling (temperature /
            // max_tokens vary by route + operator settings).
            sampling: SamplingOverrides::default(),
            verify_predicate:
                "Answers strictly from the retrieved passages (cites [Source: title]); \
                 abstains or notes the gap when evidence is thin; never fabricates.",
            separate_forward_pass: false,
        },
        Role::Critic => RoleProfile {
            role: Role::Critic,
            description: "Independently verify the Synthesizer's answer against the passages.",
            preferred_tier: Tier::Primary,
            sampling: SamplingOverrides {
                temperature: Some(0.0),
                ..Default::default()
            },
            verify_predicate:
                "Catches planted/ungrounded claims in the answer without flagging grounded \
                 ones (high recall on real errors, low phantom-error rate).",
            // A model grading its own single pass is self-confirmation bias.
            separate_forward_pass: true,
        },
    }
}

/// Optional per-role model handle override — lifted from the agent-tools
/// `RoleModelMap`. `None` for a role degrades to the caller's fallback model
/// (byte-identical to single-model operation). Pure data.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleModelMap {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthesizer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub critic: Option<String>,
}

impl RoleModelMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the per-role override. `None` clears any prior override.
    pub fn set(&mut self, role: Role, model: Option<String>) {
        match role {
            Role::Router => self.router = model,
            Role::Synthesizer => self.synthesizer = model,
            Role::Critic => self.critic = model,
        }
    }

    /// Override for `role`, if any was set.
    pub fn get(&self, role: Role) -> Option<&str> {
        match role {
            Role::Router => self.router.as_deref(),
            Role::Synthesizer => self.synthesizer.as_deref(),
            Role::Critic => self.critic.as_deref(),
        }
    }

    /// Resolve the model for `role`: per-role override if set, else `fallback`.
    pub fn model_for<'a>(&'a self, role: Role, fallback: &'a str) -> &'a str {
        self.get(role).unwrap_or(fallback)
    }

    /// True when no role has an override — single-model behavior.
    pub fn is_empty(&self) -> bool {
        self.router.is_none() && self.synthesizer.is_none() && self.critic.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_ids_round_trip() {
        for r in Role::all() {
            assert_eq!(Role::from_id(r.id()), Some(*r));
        }
        assert_eq!(Role::from_id("nope"), None);
    }

    #[test]
    fn every_role_has_a_profile_with_its_own_role_tag() {
        for r in Role::all() {
            let p = default_profile_for(*r);
            assert_eq!(p.role, *r, "profile.role must match the requested role");
            assert!(
                !p.verify_predicate.is_empty(),
                "every role ships a verify predicate (the keystone discipline)"
            );
        }
    }

    #[test]
    fn tiers_match_documented_policy() {
        // Router classifies on Fast; Synthesizer + Critic default to Primary.
        assert_eq!(default_profile_for(Role::Router).preferred_tier, Tier::Fast);
        assert_eq!(
            default_profile_for(Role::Synthesizer).preferred_tier,
            Tier::Primary
        );
        assert_eq!(
            default_profile_for(Role::Critic).preferred_tier,
            Tier::Primary
        );
    }

    #[test]
    fn only_the_critic_demands_a_separate_forward_pass() {
        assert!(
            default_profile_for(Role::Critic).separate_forward_pass,
            "the Critic must verify in its own pass (self-confirmation otherwise)"
        );
        assert!(!default_profile_for(Role::Router).separate_forward_pass);
        assert!(!default_profile_for(Role::Synthesizer).separate_forward_pass);
    }

    #[test]
    fn tier_model_stems_and_critic_resolves_primary() {
        assert_eq!(Tier::Fast.model_stem(), "fast");
        assert_eq!(Tier::Primary.model_stem(), "primary");
        // The bench sources the Critic's model from its profile — this is the
        // handle that puts verify_grounding on the 35B (the keystone's
        // "Critic(35B)"), instead of the bench's default fast judge.
        assert_eq!(
            default_profile_for(Role::Critic).preferred_tier.model_stem(),
            "primary"
        );
    }

    #[test]
    fn role_model_map_set_get_round_trip() {
        let mut m = RoleModelMap::new();
        assert!(m.is_empty());
        m.set(Role::Synthesizer, Some("commonwealth/primary".into()));
        assert_eq!(m.get(Role::Synthesizer), Some("commonwealth/primary"));
        assert_eq!(
            m.model_for(Role::Synthesizer, "fallback"),
            "commonwealth/primary"
        );
        assert_eq!(m.model_for(Role::Router, "fallback"), "fallback");
        assert!(!m.is_empty());
        m.set(Role::Synthesizer, None);
        assert!(m.is_empty());
    }

    #[test]
    fn role_model_map_serde_skips_none() {
        let mut m = RoleModelMap::new();
        m.set(Role::Critic, Some("commonwealth/primary".into()));
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("critic") && !json.contains("router"));
        let back: RoleModelMap = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
    }
}
