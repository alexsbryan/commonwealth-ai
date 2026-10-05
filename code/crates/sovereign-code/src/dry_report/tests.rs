// SPDX-License-Identifier: AGPL-3.0-or-later
//! `dry_report`'s tests, a sibling file so the module stays inside its
//! arch-gate band (ARCH §3.1). The fixture tests build both stores for real —
//! a LanceDB chunk index and an on-disk SCIP graph — and drive
//! `build_dry_report` end to end, because each defect they pin (I1–I4, from
//! the svrngs audit of 2026-10-04) was visible only in the finished report.

use super::*;

use corpus_engine_scip::ScipSymbolRecord;
use corpus_index::index::{code_meta_from_json, InsertChunk};

fn body(lines: &[&str]) -> Vec<String> {
    normalize_body(lines)
}

/// The scope this report now applies, asserted on the exact paths that
/// made its headline unquotable. Before 2026-08-20 `dry_report` had no
/// source scope at all, so its 3,151 exact groups / ~96k redundant lines
/// counted one vendored llama.cpp helper SEVEN times across `target/`
/// build-hash directories. These are the failing inputs (§18.1).
#[test]
fn build_artifacts_and_vendored_code_are_out_of_scope() {
    let s = SourceScope::default();
    for excluded in [
        "target/debug/build/llama-cpp-sys-4-9f1/out/llama.cpp/convert.py",
        "vendor/llama-cpp-sys-4/llama.cpp/tools/server/tests/unit/test_chat.py",
        "research/deep-research/drb/vendor/pkg/mod.rs",
        ".claude/worktrees/agent-a99/sovereign/crates/sovereign-tools/src/code/dry_report.rs",
        "ingest/crates/corpus-engine/tests/extractor_smoke.rs",
        "svrn/crates/sovereign-core/benches/embed.rs",
    ] {
        assert!(!s.admits(excluded), "should be out of scope: {excluded}");
    }
    // …and first-party production code is still in. `deep_research` is the
    // one that regressed once already: `research` as a SUBSTRING swallowed
    // it, which is why the exclusions match whole path segments.
    for included in [
        "svrn/crates/sovereign-tools/src/code/dry_report.rs",
        "svrn/crates/sovereign-core/src/deep_research/icd.rs",
        "ingest/crates/corpus-engine/src/extractors/mod.rs",
    ] {
        assert!(s.admits(included), "should be in scope: {included}");
    }
}

#[test]
fn use_aliases_are_rejected() {
    // All forms of re-export alias rust-analyzer tags with a `fn().` descriptor.
    assert!(is_use_alias(&body(&[
        "use commonwealth_core::clock::unix_now_secs as now_secs;"
    ])));
    assert!(is_use_alias(&body(&[
        "pub use sovereign_time::unix_now as unix_now;"
    ])));
    assert!(is_use_alias(&body(&["pub(crate) use foo::bar as bar;"])));
    assert!(is_use_alias(&body(&["pub(super) use foo::bar as bar;"])));
}

#[test]
fn real_function_bodies_are_kept() {
    // A genuine body never opens with `use` — signatures and attributes do.
    assert!(!is_use_alias(&body(&[
        "fn now_secs() -> u64 {",
        "    SystemTime::now()",
        "}"
    ])));
    assert!(!is_use_alias(&body(&[
        "pub fn ctx() -> Ctx {",
        "    Ctx::default()",
        "}"
    ])));
    assert!(!is_use_alias(&body(&[
        "#[tokio::test]",
        "async fn run() {",
        "    let x = use_it();", // `use` mid-body must not trip the guard
        "}"
    ])));
    assert!(!is_use_alias(&[])); // empty body: not an alias
}

#[test]
fn union_find_components_are_transitive_and_isolated() {
    // 0-1-2 merged through pairwise unions; 3-4 separate; 5 singleton.
    let mut uf = UnionFind::new(6);
    uf.union(0, 1);
    uf.union(1, 2);
    uf.union(3, 4);
    assert_eq!(
        uf.find(0),
        uf.find(2),
        "transitivity through the middle node"
    );
    assert_eq!(uf.find(3), uf.find(4));
    assert_ne!(uf.find(0), uf.find(3), "disjoint pairs stay disjoint");
    assert_ne!(uf.find(5), uf.find(0));
    assert_eq!(uf.find(5), 5, "untouched element is its own root");
    // Re-unioning an existing component is a no-op, not a corruption.
    uf.union(2, 0);
    assert_eq!(uf.find(0), uf.find(2));
    assert_ne!(uf.find(0), uf.find(3));
}

#[test]
fn cosine_helpers_normalize_and_dot() {
    let n = normalize(&[3.0, 4.0]).expect("has a direction");
    assert!(
        (dot(&n, &n) - 1.0).abs() < 1e-6,
        "unit vector dots to 1 with itself"
    );
    // No direction, so no unit vector — `None`, not NaN and not a zero that
    // scores 0 against everything (I2).
    assert!(normalize(&[0.0, 0.0]).is_none());
    assert!(normalize(&[]).is_none());
}

// ── End-to-end fixtures: a real chunk index + a real SCIP graph ─────────────

const CORPUS: &str = "dry-fixture";
const DIM: usize = 4;
/// Orthogonal unit vectors: no two rows built from distinct ones are near.
const E: [[f32; DIM]; DIM] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];
/// What an `--fts-only` index stores in every row (svrn-ingest `index.rs`).
const ZERO: [f32; DIM] = [0.0; DIM];

/// A function definition, recorded in BOTH stores the way their writers
/// record it: SCIP's `line_start` is the 0-based line of the name and
/// `line_end` the 0-based line of the closing brace (scip_export.rs stores
/// `occ.range[0]` raw); the chunk index carries the same symbol with a vector.
struct Def {
    file: &'static str,
    name: &'static str,
    /// 0-based, as SCIP records it. `None` = this language has no SCIP rows.
    scip: Option<(i32, i32)>,
    vector: [f32; DIM],
}

struct Fixture {
    _tmp: tempfile::TempDir,
    index_path: PathBuf,
}

async fn fixture(files: &[(&str, &str)], defs: &[Def]) -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("repo");
    for (path, text) in files {
        let p = root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
    }
    let index_path = tmp.path().join("indexes").join(CORPUS);
    let index = CorpusIndex::create(&index_path, CORPUS, CORPUS, "test-mock", DIM, false, "MIT")
        .await
        .expect("create chunk index");
    index.set_source_path(&root).expect("stamp source_path");
    let chunks: Vec<(InsertChunk, Vec<f32>)> = defs
        .iter()
        .enumerate()
        .map(|(i, d)| {
            // A row with no SCIP record still needs its own span, or the
            // report's (file, start, end) dedup collapses it into a sibling.
            let (s, e) = d.scip.unwrap_or((4 * i as i32, 4 * i as i32 + 3));
            let meta = serde_json::json!({
                "symbol_name": d.name, "symbol_kind": "function", "file_path": d.file,
                "line_start": s, "line_end": e, "is_public": false,
            });
            let insert = InsertChunk {
                content: format!("{}::{}", d.file, d.name),
                title: None,
                url: None,
                metadata: Some(meta.to_string()),
                content_hash: None,
                source_doc_id: Some(d.file.to_string()),
                source_file: None,
                code: code_meta_from_json(Some(&meta)),
                unit_id: None,
            };
            (insert, d.vector.to_vec())
        })
        .collect();
    index.insert_batch(&chunks).await.expect("insert rows");
    index.mark_ingestion_complete().expect("mark complete");

    let graph = ScipGraph::open(&index_path.join("scip_graph.db"), CORPUS).expect("scip open");
    let symbols = defs
        .iter()
        .filter_map(|d| {
            d.scip.map(|(s, e)| ScipSymbolRecord {
                name: d.name.to_string(),
                qualified_name: String::new(),
                kind: "function".to_string(),
                file_path: d.file.to_string(),
                line_start: s,
                line_end: e,
                language: "rust".to_string(),
            })
        })
        .collect();
    graph
        .ingest_symbols_and_refs(symbols, Vec::new())
        .await
        .expect("ingest scip rows");
    Fixture {
        _tmp: tmp,
        index_path,
    }
}

async fn run(f: &Fixture, scope: Option<&str>) -> (DryReport, String, serde_json::Value) {
    let report = build_dry_report(DryInputs {
        index_path: &f.index_path,
        corpus_id: CORPUS,
        min_lines: 1,
        near_threshold: DEFAULT_NEAR_THRESHOLD,
        scope,
    })
    .await
    .expect("dry report");
    let text = render_dry_report(&report);
    let json = serde_json::to_value(&report).expect("report serializes");
    (report, text, json)
}

const BUMP: &str = "fn bump(x: u32) -> u32 {\n    let y = x + 1;\n    y\n}\n";
const CLEAN: &str = "_No duplication found above the configured thresholds._";

/// CONTROL, not a red test: passes before and after the I1–I4 fixes. Two
/// byte-identical files put the same line above each copy, so the exact
/// tier groups them under either line base. It proves the fixture reaches
/// both stores and the source root — without it, a red I1 could be a broken
/// fixture rather than the defect.
#[tokio::test]
async fn control_identical_files_form_one_exact_group() {
    let text = format!("/// Same comment.\n{BUMP}");
    let f = fixture(
        &[
            ("crates/a/src/lib.rs", &text),
            ("crates/b/src/lib.rs", &text),
        ],
        &[
            Def {
                file: "crates/a/src/lib.rs",
                name: "bump",
                scip: Some((1, 4)),
                vector: E[0],
            },
            Def {
                file: "crates/b/src/lib.rs",
                name: "bump",
                scip: Some((1, 4)),
                vector: E[1],
            },
        ],
    )
    .await;
    let (r, _, _) = run(&f, None).await;
    assert_eq!(r.exact_clones.len(), 1, "{:?}", r.exact_clones);
    assert_eq!(r.exact_clones[0].members.len(), 2);
}

/// CONTROL, not a red test: a fully judged clean report renders exactly as
/// before — the headline `scripts/nc-redundant.py` parses, and the clean
/// line. The could-not-judge fixes below change output only when a tier
/// could not judge.
#[tokio::test]
async fn control_a_judged_clean_report_renders_unchanged() {
    let two = "fn one(a: u8) -> u8 {\n    a\n        .wrapping_add(1)\n}\n\
               fn two(b: u16) -> u16 {\n    b\n        .wrapping_mul(3)\n}\n";
    let f = fixture(
        &[("crates/a/src/lib.rs", two)],
        &[
            Def {
                file: "crates/a/src/lib.rs",
                name: "one",
                scip: Some((0, 3)),
                vector: E[0],
            },
            Def {
                file: "crates/a/src/lib.rs",
                name: "two",
                scip: Some((4, 7)),
                vector: E[1],
            },
        ],
    )
    .await;
    let (_, text, _) = run(&f, None).await;
    assert!(
        text.contains(
            "**0** exact-clone groups · **0** near-clone clusters · \
             ~**0** redundant lines (lower bound).\n\n"
        ),
        "{text}"
    );
    assert!(text.contains(CLEAN), "{text}");
}

/// I1. SCIP lines are 0-based; the exact tier read them as 1-based, so each
/// hashed body began one line ABOVE the fn and stopped before its closing
/// brace. Identical bodies under different doc comments then hashed apart,
/// and a fn on a file's first line (`line_start` 0) was skipped outright.
#[tokio::test]
async fn i1_identical_bodies_under_different_lines_above_are_one_exact_group() {
    let a = format!("// SPDX-License-Identifier: MIT\n/// Adds one, the first way.\n{BUMP}");
    let b = format!("/// A different comment above the same body.\n{BUMP}");
    let f = fixture(
        &[
            ("crates/a/src/lib.rs", &a),
            ("crates/b/src/lib.rs", &b),
            ("crates/c/src/lib.rs", BUMP),
        ],
        &[
            // fn on 1-based line 3, `}` on 6 → SCIP 2..5.
            Def {
                file: "crates/a/src/lib.rs",
                name: "bump",
                scip: Some((2, 5)),
                vector: E[0],
            },
            Def {
                file: "crates/b/src/lib.rs",
                name: "bump",
                scip: Some((1, 4)),
                vector: E[1],
            },
            // The very first line of its file.
            Def {
                file: "crates/c/src/lib.rs",
                name: "bump",
                scip: Some((0, 3)),
                vector: E[2],
            },
        ],
    )
    .await;
    let (r, _, _) = run(&f, None).await;
    assert_eq!(
        r.exact_clones.len(),
        1,
        "three identical bodies must be one group: {:?}",
        r.exact_clones
    );
    let files: Vec<&str> = r.exact_clones[0]
        .members
        .iter()
        .map(|m| m.file.as_str())
        .collect();
    assert_eq!(
        files,
        [
            "crates/a/src/lib.rs",
            "crates/b/src/lib.rs",
            "crates/c/src/lib.rs"
        ]
    );
}

/// I2. An `--fts-only` index stores zero vectors, which the empty-embedding
/// check let through: the near pass scored 0 against everything and the
/// report printed "No duplication found". It could not judge.
#[tokio::test]
async fn i2_a_vectorless_index_is_could_not_judge_for_the_near_tier() {
    let two = "fn one(a: u8) -> u8 {\n    a\n        .wrapping_add(1)\n}\n\
               fn two(b: u16) -> u16 {\n    b\n        .wrapping_mul(3)\n}\n";
    let f = fixture(
        &[("crates/a/src/lib.rs", two)],
        &[
            Def {
                file: "crates/a/src/lib.rs",
                name: "one",
                scip: Some((0, 3)),
                vector: ZERO,
            },
            Def {
                file: "crates/a/src/lib.rs",
                name: "two",
                scip: Some((4, 7)),
                vector: ZERO,
            },
        ],
    )
    .await;
    let (r, text, json) = run(&f, None).await;
    assert_eq!(r.skipped_no_embedding, 2, "a zero vector is no embedding");
    assert_eq!(json["near_tier"]["verdict"], "could-not-judge", "{json}");
    assert_eq!(json["exact_tier"]["verdict"], "passed", "{json}");
    assert!(
        !text.contains(CLEAN),
        "a tier that could not judge is not clean:\n{text}"
    );
    assert!(!text.contains("**0** near-clone clusters"), "{text}");
    assert!(
        text.contains("near-clone clusters **could-not-judge**"),
        "{text}"
    );
}

/// I3. `--scope crates/svrngs` matched a raw string prefix, so it also took
/// in `crates/svrngs-core`, and nothing in the output said so. It matches
/// whole path components now, and the output names what it took in and the
/// prefix siblings it left out — with or without a trailing slash.
#[tokio::test]
async fn i3_scope_matches_whole_path_components_and_names_what_it_took_in() {
    let text = format!("/// Same comment.\n{BUMP}");
    let f = fixture(
        &[
            ("crates/svrngs/src/lib.rs", &text),
            ("crates/svrngs-core/src/lib.rs", &text),
        ],
        &[
            Def {
                file: "crates/svrngs/src/lib.rs",
                name: "bump",
                scip: Some((1, 4)),
                vector: E[0],
            },
            Def {
                file: "crates/svrngs-core/src/lib.rs",
                name: "bump",
                scip: Some((1, 4)),
                vector: E[1],
            },
        ],
    )
    .await;
    for scope in ["crates/svrngs", "crates/svrngs/"] {
        let (r, out, json) = run(&f, Some(scope)).await;
        let taken: Vec<&str> = r
            .exact_clones
            .iter()
            .flat_map(|g| g.members.iter().map(|m| m.file.as_str()))
            .collect();
        assert!(
            !taken.iter().any(|f| f.starts_with("crates/svrngs-core/")),
            "scope `{scope}` took in a sibling crate: {taken:?}"
        );
        assert_eq!(
            r.considered, 1,
            "scope `{scope}`: one symbol under crates/svrngs/"
        );
        assert_eq!(json["scope_files"], 1, "{json}");
        assert_eq!(
            json["scope_excluded"],
            serde_json::json!(["crates/svrngs-core"]),
            "{json}"
        );
        assert!(out.contains("1 file(s) under `crates/svrngs/`"), "{out}");
        assert!(out.contains("`crates/svrngs-core`"), "{out}");
    }
}

/// I4. When a language's SCIP export failed, the exact tier had no symbols
/// for a directory of that language and still printed "0 exact-clone
/// groups" — a clean result it had not judged.
#[tokio::test]
async fn i4_a_scope_with_no_scip_symbols_is_could_not_judge_for_the_exact_tier() {
    let ts = "export function one(a: number) {\n  return a + 1;\n}\n\
              export function two(b: number) {\n  return b * 3;\n}\n";
    let f = fixture(
        &[("crates/a/src/lib.rs", BUMP), ("bridge/src/app.ts", ts)],
        &[
            Def {
                file: "crates/a/src/lib.rs",
                name: "bump",
                scip: Some((0, 3)),
                vector: E[0],
            },
            // TypeScript: chunked by tree-sitter, absent from SCIP.
            Def {
                file: "bridge/src/app.ts",
                name: "one",
                scip: None,
                vector: E[1],
            },
            Def {
                file: "bridge/src/app.ts",
                name: "two",
                scip: None,
                vector: E[2],
            },
        ],
    )
    .await;
    let (_, text, json) = run(&f, Some("bridge")).await;
    assert!(
        !text.contains("**0** exact-clone groups"),
        "no SCIP symbol under `bridge/`, so 0 groups is not a result:\n{text}"
    );
    assert!(!text.contains(CLEAN), "{text}");
    assert!(
        text.contains("exact-clone groups **could-not-judge**"),
        "{text}"
    );
    assert_eq!(json["exact_tier"]["verdict"], "could-not-judge", "{json}");
    assert_eq!(json["near_tier"]["verdict"], "passed", "{json}");
}

/// A scope that matches nothing (a typo, a renamed crate) judged nothing in
/// either tier; before the verdicts it read "No duplication found".
#[tokio::test]
async fn a_scope_matching_nothing_is_could_not_judge_in_both_tiers() {
    let f = fixture(
        &[("crates/a/src/lib.rs", BUMP)],
        &[Def {
            file: "crates/a/src/lib.rs",
            name: "bump",
            scip: Some((0, 3)),
            vector: E[0],
        }],
    )
    .await;
    let (_, text, json) = run(&f, Some("crates/nope")).await;
    assert!(!text.contains(CLEAN), "{text}");
    assert!(text.contains("0 file(s) under `crates/nope/`"), "{text}");
    assert_eq!(json["near_tier"]["verdict"], "could-not-judge", "{json}");
    assert_eq!(json["exact_tier"]["verdict"], "could-not-judge", "{json}");
}

/// An unreadable SCIP graph used to fall through to an empty exact tier that
/// rendered "**0** exact-clone groups" — an `Err` read as a clean result.
#[tokio::test]
async fn an_unreadable_scip_graph_is_could_not_judge_for_the_exact_tier() {
    let f = fixture(
        &[("crates/a/src/lib.rs", BUMP)],
        &[Def {
            file: "crates/a/src/lib.rs",
            name: "bump",
            scip: Some((0, 3)),
            vector: E[0],
        }],
    )
    .await;
    std::fs::write(
        f.index_path.join("scip_graph.db"),
        b"not a sqlite database at all",
    )
    .expect("corrupt the graph");
    let (_, text, json) = run(&f, None).await;
    assert!(!text.contains("**0** exact-clone groups"), "{text}");
    assert_eq!(json["exact_tier"]["verdict"], "could-not-judge", "{json}");
    let reason = json["exact_tier"]["reason"].as_str().unwrap_or_default();
    assert!(reason.starts_with("SCIP graph unreadable"), "{json}");
}
