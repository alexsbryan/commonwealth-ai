// SPDX-License-Identifier: AGPL-3.0-or-later
//! Intent-keyed policy for the situated agent.
//!
//! Replaces skill-keyed policy as the load-bearing decision layer.
//! Per the situated-agent principle, the model should arrive at every
//! turn already situated — which tools it may call, which voice
//! register governs synthesis, which prompt addenda apply. Skill
//! selection by the user is the wrong load-bearing axis: it forks the
//! production surface and gates the good path behind a menu choice.
//!
//! This module computes the policy from the system's own
//! classification of the turn:
//! - `Intent` from the router (KnowledgeQuery, ComparisonQuery, …)
//! - `SkillRegister` from the active mode (Factual default;
//!   Relational only for inner-work mode)
//! - `active_mode` for the two surviving named modes (inner-work,
//!   recipe-author) that take precedence over intent-derived tooling
//!
//! The shape mirrors how `narrow_tools_for_skill` worked in Phase 1
//! of the Tool-Mastery framework, but the dispatch keys on Intent
//! rather than skill — exactly the architectural inversion the
//! retire-the-skills-menu plan calls for.

use std::collections::HashSet;

use crate::skills::SkillRegister;
use crate::types::{Intent, ToolDescriptor};

// ─── Mode ids ──────────────────────────────────────────────────

/// The two surviving named modes after skill retirement. These match
/// the `id` field of the TOMLs at `sovereign/modes/<id>/skill.toml`.
pub const MODE_INNER_WORK: &str = "inner-work";
pub const MODE_RECIPE_AUTHOR: &str = "recipe-author";
/// Workflow-author workspace — the umbrella authoring mode. Same agent-loop
/// treatment as recipe-author (force the tool loop, Primary slot), different tools.
pub const MODE_WORKFLOW_AUTHOR: &str = "workflow-author";

// ─── Types ─────────────────────────────────────────────────────

/// Filter applied to the runtime's tool catalog at dispatch time.
/// Closed set per ARCH §2.1 — adding a new filter shape is one
/// variant + one render arm in `narrow_tools`.
#[derive(Debug, Clone)]
pub enum ToolFilter {
    /// Pass through the full catalog. Used by paths that don't have
    /// a concrete intent yet (test harnesses, headless CLI smokes).
    /// Default behaviour for `PolicySource::Unsituated`.
    Unrestricted,
    /// Include only tools whose `id` is in this set.
    Allowlist(HashSet<String>),
    /// Include the full catalog minus these.
    Denylist(HashSet<String>),
}

impl ToolFilter {
    pub fn allow<I, S>(ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::Allowlist(ids.into_iter().map(Into::into).collect())
    }

    pub fn deny<I, S>(ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::Denylist(ids.into_iter().map(Into::into).collect())
    }

    /// Empty allowlist — model gets no tools. Used by `ExpressiveQuery`,
    /// `ConationQuery`, inner-work mode.
    pub fn none() -> Self {
        Self::Allowlist(HashSet::new())
    }
}

/// Provenance for the policy decision. Surfaces in the routing
/// footer and tracing events (`ARCH §0.1` glassbox). Lets operators
/// answer "why did the model see this tool catalog?" without
/// re-running the turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicySource {
    /// Default-chat path: derived from the router's classified
    /// intent. The common case.
    IntentDerived,
    /// Inner-work surface: relational register, no tools, witness
    /// path. Mode takes precedence over intent.
    InnerWorkMode,
    /// Recipe-author workspace: bespoke tool set. Mode takes
    /// precedence over intent.
    RecipeAuthorMode,
    /// Workflow-author workspace: the workflow authoring tool set. Mode takes
    /// precedence over intent (same as recipe-author).
    WorkflowAuthorMode,
    /// No classification yet (test harness, headless boot). Falls
    /// through to `ToolFilter::Unrestricted`.
    Unsituated,
}

/// The computed policy for a single turn. Built once after
/// classification, consumed by the tool-narrowing site and (in a
/// follow-up) by synthesis system-message assembly.
#[derive(Debug, Clone)]
pub struct IntentPolicy {
    pub tool_filter: ToolFilter,
    /// Optional shape-level guidance appended to the synthesis
    /// system message for this intent. Forward-compatible: the
    /// initial migration leaves this `None` for every intent
    /// (skills' prompts.synthesis fields were never wired anyway —
    /// see runtime.rs comment block).
    pub synthesis_addendum: Option<&'static str>,
    pub register: SkillRegister,
    pub source: PolicySource,
    /// The router-classified intent after the relational-register
    /// override is applied. `Some(intent)` when the policy was
    /// built via [`policy_for`] (post-classification path);
    /// `None` when built via [`policy_for_mode_only`] (the
    /// pre-classification router-input path — intent isn't known
    /// yet, no override can apply).
    ///
    /// Consumers that dispatch on intent should read this rather
    /// than the router's raw `classification.primary.intent` so
    /// the inner-work mode's force-to-Expressive override fires
    /// uniformly — see the `apply_witness_intent_override` helper.
    pub effective_intent: Option<crate::types::Intent>,
}

// ─── Compute ───────────────────────────────────────────────────

/// Derive the policy for a turn from the classified intent, the
/// active mode (if any), and the mode's declared register.
///
/// Precedence:
/// 1. Inner-work mode forces relational register + no tools,
///    regardless of intent. The user explicitly entered the
///    reflective surface; tools would prime the wrong frame.
/// 2. Recipe-author mode allows the recipe tool family,
///    regardless of intent. The user is in a workspace; the tool
///    set is workspace-bound.
/// 3. Otherwise, the policy is intent-derived. This is the
///    default-chat path and accounts for the vast majority of
///    turns.
pub fn policy_for(
    intent: &Intent,
    register: SkillRegister,
    active_mode: Option<&str>,
) -> IntentPolicy {
    // Resolve effective register first — inner-work mode pins
    // Relational regardless of what the caller passed. (Today this
    // is also what `skill.inference.register` declares on the
    // inner-work TOML; the mode-arm here keeps the invariant
    // structural even if the TOML drifts.)
    let effective_register = match active_mode {
        Some(MODE_INNER_WORK) => SkillRegister::Relational,
        _ => register,
    };

    // Apply the relational-register intent override BEFORE the
    // tool-filter and source decisions. The override only fires for
    // Relational register, so default-chat + recipe-author flows
    // pass through unchanged. The override is structural (was
    // previously a separate `override_intent_for_relational_register`
    // call site that every dispatch site had to remember to invoke);
    // folding it into `policy_for` makes it impossible to forget.
    let effective_intent = apply_witness_intent_override(intent, effective_register);

    match active_mode {
        Some(MODE_INNER_WORK) => IntentPolicy {
            tool_filter: ToolFilter::none(),
            synthesis_addendum: None,
            register: SkillRegister::Relational,
            source: PolicySource::InnerWorkMode,
            effective_intent: Some(effective_intent),
        },
        Some(mode @ (MODE_RECIPE_AUTHOR | MODE_WORKFLOW_AUTHOR)) => {
            // Recipe-author workspace REQUIRES tool orchestration —
            // every meaningful turn calls `recipe_write_structured`,
            // `recipe_validate`, `recipe_test`, `decision_log`,
            // `probe_url`, etc. Without forcing the intent here, the
            // router classifies most messages ("fix the recipe",
            // "draft it", "test it") as `SimpleQuery` or
            // `KnowledgeQuery` and dispatches through plain-chat
            // handlers that never enter a tool loop. The agent's
            // response then becomes advisory text + a follow-up
            // question — exactly what the skill prompt forbids
            // ("Act, don't announce"). Forcing `ComplexTask` routes
            // every turn through `handle_complex_task`, which runs
            // the agent loop on the Primary slot with the recipe
            // tool catalog.
            //
            // Continuation is preserved as-is — it carries a task_id
            // for an in-flight tool loop, so re-classifying it would
            // break the resume contract.
            let recipe_intent = match &effective_intent {
                Intent::ComplexTask | Intent::Continuation { .. } => effective_intent.clone(),
                _ => Intent::ComplexTask,
            };
            let (tools, source) = if mode == MODE_WORKFLOW_AUTHOR {
                (workflow_author_tools(), PolicySource::WorkflowAuthorMode)
            } else {
                (recipe_author_tools(), PolicySource::RecipeAuthorMode)
            };
            IntentPolicy {
                tool_filter: ToolFilter::allow(tools),
                synthesis_addendum: None,
                register: effective_register,
                source,
                effective_intent: Some(recipe_intent),
            }
        }
        Some(other) => {
            // Unknown mode — warn and fall through. Forward-compat:
            // adding a third mode in the future shouldn't crash;
            // operators see the warn-once and migrate the table.
            tracing::warn!(
                mode = other,
                "intent_policy: unknown active mode — falling back to \
                 intent-derived policy"
            );
            intent_derived(&effective_intent, effective_register)
        }
        None => intent_derived(&effective_intent, effective_register),
    }
}

/// Apply the relational-register intent override. Used internally
/// by `policy_for`; exposed for the legacy test mod in `runtime.rs`
/// that pins the override-behavior invariants. New code should
/// consume `policy.effective_intent` instead of calling this
/// directly.
///
/// Behavior pinned by the routing benches:
/// - Non-Relational register: returns intent unchanged.
/// - Relational + ExpressiveQuery / DeepQuery / Continuation:
///   returns intent unchanged (already on the witness path).
/// - Relational + anything else (KnowledgeQuery, MetalingualQuery,
///   ComparisonQuery, …): forces ExpressiveQuery so the witness
///   handler takes over.
pub fn apply_witness_intent_override(intent: &Intent, register: SkillRegister) -> Intent {
    if register != SkillRegister::Relational {
        return intent.clone();
    }
    match intent {
        // GenerativeQuery is its own no-retrieval creative path — don't force it
        // onto the emotive witness path just because a relational skill is active.
        Intent::ExpressiveQuery | Intent::DeepQuery | Intent::GenerativeQuery => intent.clone(),
        Intent::Continuation { .. } => intent.clone(),
        other => {
            tracing::info!(
                original_intent = ?other,
                "intent_policy: forcing ExpressiveQuery — relational register active"
            );
            Intent::ExpressiveQuery
        }
    }
}

fn intent_derived(intent: &Intent, register: SkillRegister) -> IntentPolicy {
    IntentPolicy {
        tool_filter: tool_filter_for_intent(intent),
        synthesis_addendum: None,
        register,
        source: PolicySource::IntentDerived,
        effective_intent: Some(intent.clone()),
    }
}

/// Mode-only policy, used at PRE-CLASSIFICATION call sites where the
/// router hasn't produced an intent yet. The router benefits from
/// seeing the broadest catalog the surface admits, so default-chat
/// returns `Unrestricted` here; inner-work and recipe-author still
/// narrow (their mode wins regardless of upcoming intent).
///
/// Post-classification call sites should use [`policy_for`] instead
/// to pick up the intent-derived narrowing.
pub fn policy_for_mode_only(register: SkillRegister, active_mode: Option<&str>) -> IntentPolicy {
    match active_mode {
        Some(MODE_INNER_WORK) => IntentPolicy {
            tool_filter: ToolFilter::none(),
            synthesis_addendum: None,
            register: SkillRegister::Relational,
            source: PolicySource::InnerWorkMode,
            effective_intent: None,
        },
        Some(mode @ (MODE_RECIPE_AUTHOR | MODE_WORKFLOW_AUTHOR)) => {
            let (tools, source) = if mode == MODE_WORKFLOW_AUTHOR {
                (workflow_author_tools(), PolicySource::WorkflowAuthorMode)
            } else {
                (recipe_author_tools(), PolicySource::RecipeAuthorMode)
            };
            IntentPolicy {
                tool_filter: ToolFilter::allow(tools),
                synthesis_addendum: None,
                register,
                source,
                effective_intent: None,
            }
        }
        _ => IntentPolicy {
            tool_filter: ToolFilter::Unrestricted,
            synthesis_addendum: None,
            register,
            source: PolicySource::Unsituated,
            effective_intent: None,
        },
    }
}

/// Intent → allowed tool ids. Generous on first pass per the plan's
/// risk note ("start with generous allowlists, tighten iteratively").
/// The lists are the union of the retired-skill `required ∪ optional`
/// declarations mapped to the intent that best matches each skill's
/// work shape.
fn tool_filter_for_intent(intent: &Intent) -> ToolFilter {
    match intent {
        // Synthesis-oriented intents — the unified front door plus
        // the legacy SearchTool (kept for web supplementation) and
        // the epistemic-graph tools. Document is included because
        // research and analysis often pulls user documents in.
        Intent::KnowledgeQuery | Intent::ComparisonQuery | Intent::DeepQuery => {
            ToolFilter::allow([
                "knowledge_lookup",
                "search",
                "knowledge",
                "claim_search",
                "epistemic_landscape",
                "document",
                "wikipedia_fetch",
                "web_fetch",
            ])
        }
        // Codebase / project-internal vocabulary questions (Metalingual) and
        // first-class "how does this code work" questions (CodeQuery). Code
        // intelligence tools + the unified front door for cross-cutting
        // questions that live in notes / project docs.
        Intent::MetalingualQuery | Intent::CodeQuery => ToolFilter::allow([
            "knowledge_lookup",
            "symbol_lookup",
            "code_search",
            "recent_changes",
            "find_callers",
            "find_callees",
            "blast_radius",
        ]),
        // Multi-step planning. Full read catalog plus write tools
        // (the executor's approval gates govern write safety, not
        // the catalog filter).
        Intent::ComplexTask => ToolFilter::allow([
            "knowledge_lookup",
            "search",
            "knowledge",
            "claim_search",
            "epistemic_landscape",
            "document",
            "document_operation",
            "wikipedia_fetch",
            "web_fetch",
            "symbol_lookup",
            "code_search",
            "recent_changes",
            "find_callers",
            "find_callees",
            "blast_radius",
            "shell",
            "file",
            "file_write",
            "note",
            "run_tests",
        ]),
        // Single-tool dispatch — only that tool is allowed. The
        // router already picked the tool; the catalog filter just
        // enforces that the planner doesn't substitute another.
        Intent::SimpleAction { tool } => ToolFilter::allow(std::iter::once(tool.clone())),
        // Emotive / commitment / imperative / creative — these shouldn't reach
        // for tools at all. Empty allowlist makes that structural.
        Intent::ExpressiveQuery
        | Intent::ConationQuery
        | Intent::CommissiveQuery
        | Intent::GenerativeQuery => ToolFilter::none(),
        // Continuation resumes a prior task — its policy comes from
        // the prior task's plan, not from this dispatch. Unrestricted
        // here lets the continuation handler decide.
        Intent::Continuation { .. } => ToolFilter::Unrestricted,
        // Simple chit-chat / smalltalk — model uses pretrained
        // knowledge, no tool needed.
        Intent::SimpleQuery => ToolFilter::none(),
    }
}

/// Recipe-author workspace tools. Mirrors the surviving
/// `recipe-author/skill.toml` required list — these are the only
/// tools the workspace surface should expose.
fn recipe_author_tools() -> Vec<&'static str> {
    vec![
        "registry_browse",
        "recipe_read",
        "recipe_write",
        "recipe_write_structured",
        "recipe_validate",
        "recipe_test",
        "web_search",
        "web_fetch",
        "checkpoint",
        "decision_log",
        "capability_request",
        "research_finding",
        "probe_url",
    ]
}

/// Tool allowlist for the workflow-author workspace. The umbrella authoring tools;
/// Inc1 will add the recipe sub-flow tools here so a workflow can author its ingest
/// stage via the proven recipe loop.
fn workflow_author_tools() -> Vec<&'static str> {
    vec![
        "workflow_write",
        "workflow_write_structured",
        "workflow_validate",
        "workflow_test",
        // The recipe sub-flow — author the ingest/enrich stage a workflow's
        // `recipe:<id>` step references, with the same validate/dry-run rigor.
        "registry_browse",
        "recipe_read",
        "recipe_write_structured",
        "recipe_validate",
        "recipe_test",
        "note",
        "notes",
    ]
}

// ─── Apply ─────────────────────────────────────────────────────

/// Apply the policy's `tool_filter` to a catalog. Preserves catalog
/// ordering (the planner's prompt-cache may depend on stable order)
/// and is a no-op when the filter is `Unrestricted`.
pub fn narrow_tools(catalog: &[ToolDescriptor], policy: &IntentPolicy) -> Vec<ToolDescriptor> {
    let narrowed: Vec<ToolDescriptor> = match &policy.tool_filter {
        ToolFilter::Unrestricted => catalog.to_vec(),
        ToolFilter::Allowlist(ids) => catalog
            .iter()
            .filter(|d| ids.contains(d.id.as_str()))
            .cloned()
            .collect(),
        ToolFilter::Denylist(ids) => catalog
            .iter()
            .filter(|d| !ids.contains(d.id.as_str()))
            .cloned()
            .collect(),
    };

    tracing::debug!(
        source = ?policy.source,
        register = ?policy.register,
        catalog_size = catalog.len(),
        narrowed_size = narrowed.len(),
        filter_kind = match &policy.tool_filter {
            ToolFilter::Unrestricted => "unrestricted",
            ToolFilter::Allowlist(_) => "allowlist",
            ToolFilter::Denylist(_) => "denylist",
        },
        "intent_policy:narrow_tools"
    );

    narrowed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Effect, Idempotency, Latency, Scope};

    fn fake_descriptor(id: &str) -> ToolDescriptor {
        ToolDescriptor {
            id: id.to_string(),
            name: id.to_string(),
            description: format!("fake {id}"),
            parameters: serde_json::json!({}),
            examples: Vec::new(),
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Session,
            output_schema: None,
        }
    }

    fn full_catalog() -> Vec<ToolDescriptor> {
        [
            "knowledge_lookup",
            "search",
            "knowledge",
            "shell",
            "symbol_lookup",
            "code_search",
            "recent_changes",
            "find_callers",
            "find_callees",
            "blast_radius",
            "file",
            "file_write",
            "document",
            "document_operation",
            "claim_search",
            "epistemic_landscape",
            "web_search",
            "web_fetch",
            "wikipedia_fetch",
            "registry_browse",
            "recipe_read",
            "recipe_write",
            "recipe_write_structured",
            "recipe_validate",
            "recipe_test",
            "checkpoint",
            "decision_log",
            "capability_request",
            "research_finding",
            "probe_url",
            "note",
            "run_tests",
            "workflow_write",
            "workflow_validate",
            "workflow_test",
        ]
        .iter()
        .map(|id| fake_descriptor(id))
        .collect()
    }

    #[test]
    fn policy_for_knowledge_query_allowlists_retrieval_tools() {
        let policy = policy_for(&Intent::KnowledgeQuery, SkillRegister::Factual, None);
        assert_eq!(policy.source, PolicySource::IntentDerived);
        assert_eq!(policy.register, SkillRegister::Factual);
        let narrowed = narrow_tools(&full_catalog(), &policy);
        let ids: Vec<&str> = narrowed.iter().map(|d| d.id.as_str()).collect();
        assert!(ids.contains(&"knowledge_lookup"));
        assert!(ids.contains(&"search"));
        assert!(ids.contains(&"epistemic_landscape"));
        assert!(!ids.contains(&"shell"));
        assert!(!ids.contains(&"file_write"));
    }

    #[test]
    fn policy_for_metalingual_query_allowlists_code_intel() {
        let policy = policy_for(&Intent::MetalingualQuery, SkillRegister::Factual, None);
        let narrowed = narrow_tools(&full_catalog(), &policy);
        let ids: Vec<&str> = narrowed.iter().map(|d| d.id.as_str()).collect();
        assert!(ids.contains(&"symbol_lookup"));
        assert!(ids.contains(&"code_search"));
        assert!(ids.contains(&"find_callers"));
        assert!(ids.contains(&"knowledge_lookup"));
        assert!(!ids.contains(&"search"));
        assert!(!ids.contains(&"document"));
    }

    #[test]
    fn policy_for_complex_task_allows_write_tools() {
        let policy = policy_for(&Intent::ComplexTask, SkillRegister::Factual, None);
        let narrowed = narrow_tools(&full_catalog(), &policy);
        let ids: Vec<&str> = narrowed.iter().map(|d| d.id.as_str()).collect();
        assert!(ids.contains(&"shell"));
        assert!(ids.contains(&"file_write"));
        assert!(ids.contains(&"note"));
        assert!(ids.contains(&"run_tests"));
    }

    #[test]
    fn policy_for_simple_action_allowlists_just_that_tool() {
        let policy = policy_for(
            &Intent::SimpleAction {
                tool: "knowledge_lookup".into(),
            },
            SkillRegister::Factual,
            None,
        );
        let narrowed = narrow_tools(&full_catalog(), &policy);
        assert_eq!(narrowed.len(), 1);
        assert_eq!(narrowed[0].id, "knowledge_lookup");
    }

    #[test]
    fn policy_for_expressive_query_has_no_tools() {
        for intent in [
            Intent::ExpressiveQuery,
            Intent::ConationQuery,
            Intent::CommissiveQuery,
            Intent::SimpleQuery,
        ] {
            let policy = policy_for(&intent, SkillRegister::Factual, None);
            let narrowed = narrow_tools(&full_catalog(), &policy);
            assert!(
                narrowed.is_empty(),
                "{intent:?} should produce an empty tool catalog"
            );
        }
    }

    #[test]
    fn policy_for_inner_work_mode_has_empty_tools_and_relational_register() {
        // Even on a KnowledgeQuery intent, inner-work mode wins —
        // no tools, relational register.
        let policy = policy_for(
            &Intent::KnowledgeQuery,
            SkillRegister::Factual, // intent-side register; mode overrides
            Some(MODE_INNER_WORK),
        );
        assert_eq!(policy.source, PolicySource::InnerWorkMode);
        assert_eq!(policy.register, SkillRegister::Relational);
        let narrowed = narrow_tools(&full_catalog(), &policy);
        assert!(narrowed.is_empty());
    }

    #[test]
    fn policy_for_recipe_author_mode_has_recipe_tools() {
        let policy = policy_for(
            &Intent::ComplexTask,
            SkillRegister::Factual,
            Some(MODE_RECIPE_AUTHOR),
        );
        assert_eq!(policy.source, PolicySource::RecipeAuthorMode);
        let narrowed = narrow_tools(&full_catalog(), &policy);
        let ids: Vec<&str> = narrowed.iter().map(|d| d.id.as_str()).collect();
        assert!(ids.contains(&"recipe_read"));
        assert!(ids.contains(&"recipe_validate"));
        assert!(ids.contains(&"registry_browse"));
        // Should NOT include unrelated tools even though intent is
        // ComplexTask.
        assert!(!ids.contains(&"shell"));
        assert!(!ids.contains(&"symbol_lookup"));
    }

    #[test]
    fn policy_for_workflow_author_mode_has_workflow_tools() {
        // Even on a plain query, workflow-author mode forces the tool loop
        // (ComplexTask) + its own tool set + the WorkflowAuthorMode source.
        let policy = policy_for(
            &Intent::SimpleQuery,
            SkillRegister::Factual,
            Some(MODE_WORKFLOW_AUTHOR),
        );
        assert_eq!(policy.source, PolicySource::WorkflowAuthorMode);
        assert_eq!(policy.effective_intent, Some(Intent::ComplexTask));
        let narrowed = narrow_tools(&full_catalog(), &policy);
        let ids: Vec<&str> = narrowed.iter().map(|d| d.id.as_str()).collect();
        assert!(ids.contains(&"workflow_write"));
        assert!(ids.contains(&"workflow_validate"));
        assert!(ids.contains(&"workflow_test"));
        // The recipe sub-flow is available too — the workflow-author authors a
        // workflow's ingest/enrich stage via the proven recipe loop.
        assert!(ids.contains(&"recipe_write_structured"));
        // But not unrelated tools.
        assert!(!ids.contains(&"shell"));
    }

    #[test]
    fn policy_for_unknown_mode_falls_back_to_intent_derived() {
        let policy = policy_for(
            &Intent::KnowledgeQuery,
            SkillRegister::Factual,
            Some("future-mode-not-yet-defined"),
        );
        assert_eq!(policy.source, PolicySource::IntentDerived);
        let narrowed = narrow_tools(&full_catalog(), &policy);
        let ids: Vec<&str> = narrowed.iter().map(|d| d.id.as_str()).collect();
        assert!(ids.contains(&"knowledge_lookup"));
    }

    #[test]
    fn narrow_tools_preserves_catalog_ordering() {
        // Catalog-order preservation matters for prompt-cache hits;
        // the planner's prompt construction is order-sensitive.
        let policy = IntentPolicy {
            tool_filter: ToolFilter::allow([
                "find_callees",
                "symbol_lookup",
                "code_search",
                "find_callers",
            ]),
            synthesis_addendum: None,
            register: SkillRegister::Factual,
            source: PolicySource::IntentDerived,
            effective_intent: None,
        };
        let narrowed = narrow_tools(&full_catalog(), &policy);
        // Catalog order for these four: symbol_lookup, code_search,
        // find_callers, find_callees (see full_catalog above).
        let ids: Vec<&str> = narrowed.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "symbol_lookup",
                "code_search",
                "find_callers",
                "find_callees"
            ]
        );
    }

    #[test]
    fn unrestricted_filter_is_identity() {
        let catalog = full_catalog();
        let policy = IntentPolicy {
            tool_filter: ToolFilter::Unrestricted,
            synthesis_addendum: None,
            register: SkillRegister::Factual,
            source: PolicySource::Unsituated,
            effective_intent: None,
        };
        let narrowed = narrow_tools(&catalog, &policy);
        assert_eq!(narrowed.len(), catalog.len());
    }

    #[test]
    fn denylist_excludes_named_ids() {
        let policy = IntentPolicy {
            tool_filter: ToolFilter::deny(["shell", "file_write"]),
            synthesis_addendum: None,
            register: SkillRegister::Factual,
            source: PolicySource::IntentDerived,
            effective_intent: None,
        };
        let narrowed = narrow_tools(&full_catalog(), &policy);
        let ids: Vec<&str> = narrowed.iter().map(|d| d.id.as_str()).collect();
        assert!(!ids.contains(&"shell"));
        assert!(!ids.contains(&"file_write"));
        assert!(ids.contains(&"knowledge_lookup"));
    }
}
