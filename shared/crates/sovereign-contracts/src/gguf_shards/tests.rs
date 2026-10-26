// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

/// The one shard-name parser. `shard_files`, `total_model_bytes`, and the
/// mesh's `split_sibling_names` all route through this, so a disagreement
/// about what "the whole model" means is no longer expressible.
#[test]
fn split_shard_names_is_the_single_parser() {
    // A split expands to every sibling, in order, preserving zero-pad width.
    assert_eq!(
        split_shard_names("m-00001-of-00003.gguf").unwrap(),
        vec![
            "m-00001-of-00003.gguf",
            "m-00002-of-00003.gguf",
            "m-00003-of-00003.gguf"
        ]
    );
    // Any shard of the set names the same set — the worker is handed shard
    // 2's name by the manifest and must still resolve the whole model.
    assert_eq!(
        split_shard_names("m-00002-of-00003.gguf").unwrap(),
        split_shard_names("m-00001-of-00003.gguf").unwrap()
    );
    // Real unsloth shape: dashes in the stem must not confuse the parser.
    let ds4 = split_shard_names("DeepSeek-V4-Flash-0731-UD-Q4_K_XL-00001-of-00005.gguf")
        .expect("5-way split");
    assert_eq!(ds4.len(), 5);
    assert_eq!(
        ds4[4],
        "DeepSeek-V4-Flash-0731-UD-Q4_K_XL-00005-of-00005.gguf"
    );
    // Not a split: plain name, a 1-of-1 "split", and non-numeric junk.
    assert!(split_shard_names("model.gguf").is_none());
    assert!(split_shard_names("m-00001-of-00001.gguf").is_none());
    assert!(split_shard_names("m-abc-of-00003.gguf").is_none());
    assert!(split_shard_names("m-00001-of-xyz.gguf").is_none());
}

#[test]
fn shard_files_enumerates_siblings_and_never_guesses() {
    let dir = tempfile::tempdir().unwrap();
    let mk = |name: &str| {
        let p = dir.path().join(name);
        std::fs::write(&p, b"x").unwrap();
        p
    };
    // Non-split: itself.
    let single = mk("model.gguf");
    assert_eq!(shard_files(&single), vec![single.clone()]);
    // Split with all siblings present: full ordered list.
    let s1 = mk("m-00001-of-00003.gguf");
    let s2 = mk("m-00002-of-00003.gguf");
    let s3 = mk("m-00003-of-00003.gguf");
    assert_eq!(shard_files(&s1), vec![s1.clone(), s2, s3]);
    // A missing sibling → fall back to the named file alone (never guess);
    // downstream the empty-warm guard turns that into a loud refusal.
    let t1 = mk("t-00001-of-00002.gguf");
    assert_eq!(shard_files(&t1), vec![t1.clone()]);
}
