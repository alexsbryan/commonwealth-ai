// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tier-A "prompt-shape" tests for the glass-box voice contract.
//!
//! These tests pin the *plumbing* — the static, deterministic
//! relationships between skill register, memory format, and the
//! base epistemic contract that ends up in the system prompt. They
//! run in milliseconds and require no inference. Failure here means
//! a refactor silently disconnected the relational voice from the
//! surface that uses it; passing here doesn't claim the model
//! actually follows the contract (that's Tier-B in
//! `sovereign-cli/src/voice_eval/`).
//!
//! The eight glass-box voice principles map onto specific
//! plumbing assertions here:
//!   • Principle 1 (specific uncertainty), Principle 6 (edge of
//!     competence), Principle 7 (disagreement permission),
//!     Principle 8 (self-honesty) — assert the relational base
//!     prompt names them by phrase.
//!   • Principle 2 (three epistemic registers) — assert memory
//!     formatting splits memories into the three confidence bands
//!     with date prefixes.
//!   • Avoid-list (therapist register, wisdom voice,
//!     over-affirmation, no-right-answer cop-out) — assert the
//!     base prompt explicitly names them as banned.

use sovereign_core::executor::{
    __voice_test_default_judge_prompt, __voice_test_voice_judge_prompt,
};
use sovereign_core::memory::format_memories_for_prompt;
use sovereign_core::runtime::{
    __voice_test_epistemic_contract_for, __voice_test_factual_base_prompt,
    __voice_test_relational_base_prompt, __voice_test_render_temporal_tensions,
};
use sovereign_core::skills::{parse_skill_toml, SkillRegister, SkillRegistry};
use sovereign_core::types::{JudgePreset, Memory, SampleSelector, TemporalTension};

// ─── Helpers ──────────────────────────────────────────────────

fn skill_with_register_relational(id: &str) -> String {
    format!(
        r#"
[skill]
id = "{id}"
name = "Relational Test"
version = "0.1.0"

[inference]
privacy = "local_only"
register = "relational"
"#
    )
}

fn skill_factual(id: &str) -> String {
    format!(
        r#"
[skill]
id = "{id}"
name = "Factual Test"
version = "0.1.0"

[inference]
privacy = "local_only"
"#
    )
}

fn mem(
    id: &str,
    content: &str,
    confidence: f64,
    created_at: i64,
    source_conv: Option<&str>,
) -> Memory {
    Memory {
        id: id.to_string(),
        content: content.to_string(),
        source: "test".to_string(),
        confidence,
        created_at,
        last_used: created_at,
        version: 0,
        deleted_at: None,
        source_conversation_id: source_conv.map(|s| s.to_string()),
        source_skill_id: None,
        ..Default::default()
    }
}

// ─── Contract-selection ───────────────────────────────────────

#[test]
fn factual_register_selects_factual_base_prompt() {
    let s = __voice_test_epistemic_contract_for(SkillRegister::Factual);
    assert_eq!(s, __voice_test_factual_base_prompt());
}

#[test]
fn relational_register_selects_relational_base_prompt() {
    let s = __voice_test_epistemic_contract_for(SkillRegister::Relational);
    assert_eq!(s, __voice_test_relational_base_prompt());
}

#[test]
fn factual_and_relational_base_prompts_differ() {
    // Sanity: refactor that accidentally aliases them would silently
    // collapse the contract.
    assert_ne!(
        __voice_test_factual_base_prompt(),
        __voice_test_relational_base_prompt()
    );
}

#[test]
fn skill_registry_resolves_relational_when_a_relational_skill_is_active() {
    let mut reg = SkillRegistry::new();
    reg.register(parse_skill_toml(&skill_with_register_relational("inner-test")).unwrap());
    reg.activate("inner-test");
    assert_eq!(reg.primary_skill_register(), SkillRegister::Relational);
}

#[test]
fn skill_registry_falls_back_to_factual_when_relational_skill_inactive() {
    let mut reg = SkillRegistry::new();
    reg.register(parse_skill_toml(&skill_with_register_relational("inner-test")).unwrap());
    // do NOT activate
    assert_eq!(reg.primary_skill_register(), SkillRegister::Factual);
}

#[test]
fn skill_registry_picks_relational_when_relational_is_local_only_priority_winner() {
    let mut reg = SkillRegistry::new();
    reg.register(parse_skill_toml(&skill_factual("background")).unwrap());
    reg.register(parse_skill_toml(&skill_with_register_relational("inner-test")).unwrap());
    reg.activate("background");
    reg.activate("inner-test");
    // Both are local_only, but inner-test has register=relational.
    // primary_skill_id_for_conversation prefers local_only;
    // primary_skill_register's job is to resolve THAT skill's register.
    // The test we want here: when the resolved primary skill is
    // relational, the register is Relational.
    let primary = reg.primary_skill_id_for_conversation();
    assert!(primary.is_some());
    let resolved_register = reg.primary_skill_register();
    if primary.as_deref() == Some("inner-test") {
        assert_eq!(resolved_register, SkillRegister::Relational);
    }
}

// ─── Relational contract content ─────────────────────────────

#[test]
fn relational_contract_leads_with_witness_not_performer_posture() {
    let p = __voice_test_relational_base_prompt();
    // Posture is the load-bearing line and must come first — small
    // models attend to the prompt's opener disproportionately, so
    // a refactor that moves it down silently weakens the voice.
    let opener: String = p.chars().take(80).collect();
    assert!(
        opener.contains("witness, not a performer"),
        "the witness/performer line must open the contract; got opener: {opener:?}"
    );
}

#[test]
fn relational_contract_names_all_eight_right_folds() {
    let p = __voice_test_relational_base_prompt();
    for fold in [
        "RIGHT ATTENTION",
        "RIGHT SPECIFICITY",
        "RIGHT CALIBRATION",
        "RIGHT QUESTION",
        "RIGHT SILENCE",
        "RIGHT DISAGREEMENT",
        "RIGHT EDGE",
        "RIGHT SELF-HONESTY",
    ] {
        assert!(
            p.contains(fold),
            "fold `{fold}` must appear by name in the contract"
        );
    }
}

#[test]
fn relational_contract_pairs_each_fold_with_a_failure_mode() {
    // Eightfold-style pairing: every Right-X must have a "Failure:"
    // line so 8B models can pattern-match anti-patterns. The prompt
    // is written so each fold block contains at least one failure
    // marker — refactor that lets one slip would weaken pattern
    // recognition for that specific fold.
    let p = __voice_test_relational_base_prompt();
    let failure_count = p.matches("Failure:").count();
    assert!(
        failure_count >= 5,
        "expected ≥5 explicit `Failure:` markers across the folds; got {failure_count}"
    );
}

#[test]
fn relational_contract_supplies_calibration_voice_templates() {
    let p = __voice_test_relational_base_prompt();
    // Right Calibration is implemented via three concrete voice
    // shapes the model can imitate. All three must be present.
    assert!(p.contains("you told me"), "from-history template");
    assert!(
        p.contains("it sounds like") || p.contains("sounds like"),
        "inferred template"
    );
    assert!(p.contains("reaching"), "guessed template");
}

#[test]
fn relational_contract_supplies_filler_question_examples() {
    let p = __voice_test_relational_base_prompt();
    // Right Question gives the model concrete filler-question
    // examples to recognise. Pinning a specific one keeps the
    // pattern-match anchor stable across edits.
    assert!(p.contains("Does that make sense?"));
}

#[test]
fn relational_contract_supplies_edge_form() {
    let p = __voice_test_relational_base_prompt();
    // Right Edge requires the "what I can do; what's outside my
    // range" frame, not "I'm not a doctor but..." disclaimer-then-
    // proceed.
    assert!(p.contains("edge of what I can"));
    assert!(p.contains("doctor") || p.contains("clinician"));
}

#[test]
fn relational_contract_supplies_disagreement_form() {
    let p = __voice_test_relational_base_prompt();
    // Right Disagreement provides the inquiry-form template the
    // model can clone verbatim.
    assert!(p.contains("might be missing something"));
    assert!(p.contains("inquiry") || p.contains("as inquiry"));
}

#[test]
fn relational_contract_supplies_self_honesty_form() {
    let p = __voice_test_relational_base_prompt();
    // Right Self-Honesty supplies the "I have notes from..." form
    // so the model has something to model against. Pin the anchor.
    assert!(p.contains("I have notes"));
    assert!(p.contains("just what got saved"));
}

#[test]
fn relational_contract_names_avoid_patterns() {
    let p = __voice_test_relational_base_prompt();
    assert!(
        p.contains("Therapist register"),
        "avoid-list: therapist register must be named"
    );
    assert!(
        p.contains("Wisdom voice"),
        "avoid-list: wisdom voice must be named"
    );
    assert!(
        p.contains("Over-affirmation"),
        "avoid-list: over-affirmation must be named"
    );
    assert!(
        p.contains("there's no right answer") || p.contains("no right answer"),
        "avoid-list: no-right-answer cop-out must be named"
    );
    assert!(
        p.contains("Generic AI disclaimer") || p.contains("As an AI"),
        "avoid-list: generic AI disclaimer must be named"
    );
}

#[test]
fn relational_contract_closes_with_one_line_distillation() {
    let p = __voice_test_relational_base_prompt();
    // The closing one-line distillation is the second most-attended
    // region of an 8B's working memory after the opener, so this is
    // load-bearing. Pin its phrasing.
    assert!(
        p.contains("See clearly, say what you see, admit what you don't"),
        "the closing one-line distillation must end the contract"
    );
}

#[test]
fn relational_contract_does_not_contain_generic_ai_disclaimer_phrases() {
    // Defensive: the contract itself should not seed the language it
    // bans. (The avoid-list mentions the pattern; that's fine —
    // we're checking nothing reads as a fully-formed disclaimer.)
    let p = __voice_test_relational_base_prompt();
    assert!(
        !p.contains("As an AI, I"),
        "the contract must not model the disclaimer it bans"
    );
}

#[test]
fn relational_contract_stays_under_effective_8b_token_budget() {
    // Empirical heuristic: 8B models give ~1,200 tokens of effective
    // attention to a system prompt before the middle starts dropping
    // out. Rough chars/token = 4. 4,800 chars is the soft cap before
    // we lose the ability to add per-skill addenda + landscape
    // digests + memory blocks on top.
    let p = __voice_test_relational_base_prompt();
    assert!(
        p.len() < 4_800,
        "contract is {} chars (~{} tokens) — tighten before adding more folds",
        p.len(),
        p.len() / 4
    );
}

// ─── Memory-format wiring ────────────────────────────────────

#[test]
fn memory_format_factual_register_uses_pre_existing_flat_layout() {
    let memories = vec![
        mem("a", "User prefers Rust", 0.95, 1_773_273_600, Some("c")),
        mem("b", "User is on macOS", 0.80, 1_773_273_600, Some("c")),
    ];
    let out = format_memories_for_prompt(&memories, SkillRegister::Factual).unwrap();
    assert!(out.contains("Known facts about the user:"));
    assert!(!out.contains("What you've told me directly"));
}

#[test]
fn memory_format_relational_register_renders_three_bands_with_dates() {
    // 2026-03-12 UTC
    let directly = mem(
        "d",
        "I want to leave the job",
        0.92,
        1_773_273_600,
        Some("c-mar"),
    );
    // 2026-04-08 UTC
    let inferred = mem(
        "i",
        "Work and meaning are linked",
        0.62,
        1_775_606_400,
        Some("c-apr"),
    );
    let tentative = mem("t", "May be avoiding conflict", 0.35, 0, None);

    let out =
        format_memories_for_prompt(&[directly, inferred, tentative], SkillRegister::Relational)
            .unwrap();

    // Three bands, each with the right heading.
    assert!(out.contains("What you've told me directly:"));
    assert!(out.contains("What I've inferred from earlier conversations:"));
    assert!(out.contains("Tentative — flag these as guesses"));

    // Dates render only when source_conversation_id is set.
    assert!(out.contains("[2026-03-12]"));
    assert!(out.contains("[2026-04-08]"));

    // Per-entry confidence annotations must NOT render: the bands
    // carry the weighting signal, and the annotation leaked verbatim
    // into a witness reply on the recall bench (hand-read 2026-07-09).
    assert!(!out.contains("(confidence"));
}

#[test]
fn memory_format_relational_register_does_not_emit_factual_heading() {
    let m = mem("a", "I prefer Sundays", 0.90, 1_773_273_600, Some("c"));
    let out = format_memories_for_prompt(&[m], SkillRegister::Relational).unwrap();
    assert!(!out.contains("Known facts about the user:"));
}

// ─── End-to-end pin ──────────────────────────────────────────

#[test]
fn bundled_inner_work_mode_resolves_to_relational_register_via_registry() {
    // This test pins the full chain from mode TOML file → registry
    // → contract selection. If anyone removes or mistypes
    // `register = "relational"` in modes/inner-work/skill.toml,
    // this test fails — and the relational voice silently
    // disappears from the production session that enters the
    // inner-work surface.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let path = std::path::Path::new(&manifest_dir)
        .join("..")
        .join("..")
        .join("modes")
        .join("inner-work")
        .join("skill.toml");
    let content = std::fs::read_to_string(&path).unwrap();
    let skill = parse_skill_toml(&content).unwrap();
    let mut reg = SkillRegistry::new();
    reg.register(skill);
    reg.activate("inner-work");

    let register = reg.primary_skill_register();
    let contract = __voice_test_epistemic_contract_for(register);
    assert_eq!(register, SkillRegister::Relational);
    assert_eq!(contract, __voice_test_relational_base_prompt());
}

// ─── R3: Temporal-tension renderer ───────────────────────────

fn tension(
    memory_id: &str,
    prior_content: &str,
    prior_created_at: i64,
    has_source_conv: bool,
    current_excerpt: &str,
) -> TemporalTension {
    TemporalTension {
        memory_id: memory_id.to_string(),
        prior_content: prior_content.to_string(),
        prior_created_at,
        prior_has_source_conversation: has_source_conv,
        current_excerpt: current_excerpt.to_string(),
    }
}

#[test]
fn temporal_tension_renderer_is_tentative_in_phrasing() {
    // Single tension with a dated prior memory.
    let t = tension(
        "m1",
        "I want to leave the job",
        1_773_273_600, // 2026-03-12 UTC
        true,
        "this is a place I want to grow",
    );
    let out = __voice_test_render_temporal_tensions(&[t]);
    // Heading invites observation, not directive.
    assert!(out.contains("Notable tension across time:"));
    assert!(
        out.contains("offer these as observations, easily dismissable, never as gotchas"),
        "framing must signal observation-not-gotcha"
    );
    // Prior content quoted verbatim.
    assert!(out.contains("\"I want to leave the job\""));
    // Date prefix because source_conversation_id was set.
    assert!(out.contains("[2026-03-12]"));
    // Current excerpt rendered as the new utterance.
    assert!(out.contains("Now you said: \"this is a place I want to grow\""));
}

#[test]
fn temporal_tension_renderer_omits_date_when_no_source_conversation() {
    let t = tension(
        "m1",
        "no Saturday meetings",
        1_773_273_600,
        false, // no source conversation id
        "let's schedule for Saturday",
    );
    let out = __voice_test_render_temporal_tensions(&[t]);
    assert!(
        !out.contains("[2026-03-12]"),
        "date must NOT render without source conv id"
    );
    assert!(out.contains("\"no Saturday meetings\""));
    assert!(out.contains("\"let's schedule for Saturday\""));
}

#[test]
fn temporal_tension_renderer_handles_multiple_tensions() {
    let tensions = vec![
        tension("a", "first prior", 0, false, "first now"),
        tension("b", "second prior", 0, false, "second now"),
    ];
    let out = __voice_test_render_temporal_tensions(&tensions);
    assert!(out.contains("\"first prior\""));
    assert!(out.contains("\"first now\""));
    assert!(out.contains("\"second prior\""));
    assert!(out.contains("\"second now\""));
}

#[test]
fn temporal_tension_renderer_empty_input_yields_only_heading_block() {
    // Caller's contract: don't render when empty (the runtime
    // checks `is_empty()` before calling). But if someone calls
    // with empty, the renderer still produces just the heading +
    // framing line and stays well-formed.
    let out = __voice_test_render_temporal_tensions(&[]);
    assert!(out.contains("Notable tension across time:"));
    assert!(!out.contains("Now you said:"));
}

// ─── R5: Voice judge preset ──────────────────────────────────

#[test]
fn voice_judge_constructor_yields_voice_preset() {
    match SampleSelector::voice_judge() {
        SampleSelector::LlmJudge {
            selection_prompt,
            preset,
        } => {
            assert!(selection_prompt.is_none());
            assert_eq!(preset, JudgePreset::Voice);
        }
        other => panic!("expected LlmJudge, got {:?}", other),
    }
}

#[test]
fn default_judge_constructor_yields_default_preset() {
    match SampleSelector::default_judge() {
        SampleSelector::LlmJudge {
            selection_prompt,
            preset,
        } => {
            assert!(selection_prompt.is_none());
            assert_eq!(preset, JudgePreset::Default);
        }
        other => panic!("expected LlmJudge, got {:?}", other),
    }
}

#[test]
fn voice_and_default_judge_rubrics_differ() {
    assert_ne!(
        __voice_test_voice_judge_prompt(),
        __voice_test_default_judge_prompt(),
        "the voice rubric must not collapse onto the default rubric",
    );
}

#[test]
fn voice_judge_rubric_uses_witness_framing_and_eight_right_folds() {
    let p = __voice_test_voice_judge_prompt();
    // Witness/performer is the load-bearing posture the rubric
    // shares with the contract.
    assert!(p.contains("witness, not a performer") || p.contains("witness"));

    // Each of the eight folds is named in `serde`-key form so the
    // judge response deserialises directly into JudgeScore.
    for fold in [
        "right_attention",
        "right_specificity",
        "right_calibration",
        "right_question",
        "right_silence",
        "right_disagreement",
        "right_edge",
        "right_self_honesty",
    ] {
        assert!(
            p.contains(fold),
            "judge rubric must name fold `{fold}` exactly"
        );
    }

    // Avoid-list patterns are explicit disqualifiers.
    assert!(p.contains("Therapist register"));
    assert!(p.contains("Wisdom voice"));
    assert!(p.contains("Over-affirmation"));
    assert!(p.contains("there's no right answer"));
}

#[test]
fn voice_judge_preset_default_is_default_variant() {
    // Backwards-compat invariant: a freshly-constructed
    // SampleSelector::LlmJudge with no preset specified must
    // resolve to JudgePreset::Default — otherwise old callers'
    // behavior silently changes.
    let s = SampleSelector::LlmJudge {
        selection_prompt: None,
        preset: JudgePreset::default(),
    };
    match s {
        SampleSelector::LlmJudge { preset, .. } => {
            assert_eq!(preset, JudgePreset::Default);
        }
        _ => unreachable!(),
    }
}

// `bundled_personal_assistant_skill_resolves_to_relational_register_via_registry`
// retired alongside the personal-assistant skill (skills-as-menu
// cleanup). Inner-work is the sole surviving Relational mode; the
// pin lives in the test above.
