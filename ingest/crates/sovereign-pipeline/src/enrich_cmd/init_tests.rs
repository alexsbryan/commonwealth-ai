// SPDX-License-Identifier: AGPL-3.0-or-later
//! `enrich init` tests, moved out of init.rs under `#[path]` so the names are
//! unchanged and init.rs stays inside its arch-gate slack.

use super::*;

#[test]
fn parse_args_minimal_form() {
    let args = vec!["ak".to_string(), "--source".into(), "/tmp/ak.txt".into()];
    let p = parse_args(&args).unwrap();
    assert_eq!(p.corpus_id, "ak");
    assert_eq!(p.source_path, PathBuf::from("/tmp/ak.txt"));
    assert_eq!(p.pipeline_id, "literary");
    assert_eq!(
        p.min_section_body_words, 40,
        "default should match config default"
    );
    assert!(!p.dry_run);
    assert!(!p.force);
}

#[test]
fn parse_args_accepts_min_section_body_words_override() {
    let args: Vec<String> = [
        "ak",
        "--source",
        "/tmp/ak.txt",
        "--min-section-body-words",
        "0",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let p = parse_args(&args).unwrap();
    assert_eq!(p.min_section_body_words, 0);
}

#[test]
fn parse_args_rejects_non_numeric_min_section_body_words() {
    let args: Vec<String> = [
        "ak",
        "--source",
        "/tmp/ak.txt",
        "--min-section-body-words",
        "lots",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let err = parse_args(&args).unwrap_err();
    assert!(
        err.contains("non-negative integer"),
        "unexpected err: {err}"
    );
}

#[test]
fn parse_args_all_flags() {
    let args: Vec<String> = [
        "ak",
        "--source",
        "/abs/ak.txt",
        "--chapter-regex",
        "^BOOK",
        "--pipeline",
        "literary",
        "--chat-model",
        "chat-x",
        "--embed-model",
        "embed-y",
        "--dry-run",
        "--force",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let p = parse_args(&args).unwrap();
    assert_eq!(p.chapter_regex.as_deref(), Some("^BOOK"));
    assert_eq!(p.chat_model.as_deref(), Some("chat-x"));
    assert_eq!(p.embed_model.as_deref(), Some("embed-y"));
    assert!(p.dry_run);
    assert!(p.force);
}

#[test]
fn parse_args_rejects_unknown_flag() {
    let err = parse_args(&["ak".into(), "--gibberish".into()]).unwrap_err();
    assert!(err.contains("unknown flag"));
}

#[test]
fn parse_args_requires_corpus_id_and_source() {
    let err = parse_args(&[]).unwrap_err();
    assert!(err.contains("corpus-id"));
    let err = parse_args(&["ak".into()]).unwrap_err();
    assert!(err.contains("source"));
}

#[test]
fn parse_args_rejects_extra_positional() {
    let err = parse_args(&["a".into(), "--source".into(), "/x".into(), "b".into()]).unwrap_err();
    assert!(err.contains("unexpected positional"));
}

#[test]
fn parse_args_accepts_from_template_without_source() {
    let args: Vec<String> = ["fwd", "--from-template", "free-will-debate"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let p = parse_args(&args).unwrap();
    assert_eq!(p.corpus_id, "fwd");
    assert_eq!(p.from_template.as_deref(), Some("free-will-debate"));
    assert!(p.template_path.is_none());
    // source_path is filled in by cmd_init after materialisation.
    assert_eq!(p.source_path, std::path::PathBuf::new());
}

#[test]
fn parse_args_rejects_template_with_source() {
    let args: Vec<String> = [
        "x",
        "--from-template",
        "free-will-debate",
        "--source",
        "/tmp/x.txt",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let err = parse_args(&args).unwrap_err();
    assert!(err.contains("mutually exclusive"), "err: {err}");
}

#[test]
fn parse_args_rejects_both_template_flags() {
    let args: Vec<String> = [
        "x",
        "--from-template",
        "free-will-debate",
        "--template-path",
        "/tmp/x.toml",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let err = parse_args(&args).unwrap_err();
    assert!(err.contains("mutually exclusive"), "err: {err}");
}

#[test]
fn parse_args_tracks_explicit_pipeline_with_template() {
    // Default — pipeline_id is "literary" but unmodified by the
    // operator, so apply_template_to_parsed will overwrite with
    // the template's pipeline_id.
    let args: Vec<String> = ["x", "--from-template", "free-will-debate"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let p = parse_args(&args).unwrap();
    assert!(!p.pipeline_id_explicit);

    // With explicit override, the operator's choice wins.
    let args: Vec<String> = [
        "x",
        "--from-template",
        "free-will-debate",
        "--pipeline",
        "literary",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let p = parse_args(&args).unwrap();
    assert!(p.pipeline_id_explicit);
    assert_eq!(p.pipeline_id, "literary");
}

#[test]
fn a_recipe_that_does_not_load_is_an_error_never_a_registry_fallback() {
    let dir = tempfile::tempdir().unwrap();
    // No recipe: no custom ontology, and the caller may use a registry pipeline.
    assert_eq!(
        ontology_spec_at(&dir.path().join("absent/recipe.toml")),
        Ok(None)
    );
    // A recipe that does not parse is an error naming it, not a quiet `None`.
    let recipe = dir.path().join("recipe.toml");
    std::fs::write(&recipe, "[corpus\nid = ").unwrap();
    let err = ontology_spec_at(&recipe).unwrap_err();
    assert!(
        err.contains("does not load") && err.contains("recipe.toml"),
        "{err}"
    );
    std::fs::write(
        &recipe,
        r#"
[corpus]
id = "t"
name = "t"
[acquire]
type = "local_file"
path = "/tmp/t.jsonl"
[extract]
type = "markdown"
[chunk]
type = "passthrough"
[enrichment]
enabled = true
type = "atlas"
[enrichment.ontology]
version = 1
[[enrichment.ontology.types]]
name = "note"
kind = "entity"
"#,
    )
    .unwrap();
    assert!(ontology_spec_at(&recipe).unwrap().is_some());
}
