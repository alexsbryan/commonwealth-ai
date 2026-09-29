// SPDX-License-Identifier: AGPL-3.0-or-later
//! `servable_model_files`' tests — see `model_transfer.rs` (moved from the
//! svrn daemon's tests with the function, pb-serve-distributes).

use super::*;

/// The regression this file's split-expansion comment describes: a slot
/// configured at shard 1 of a split GGUF must make ALL shards servable.
/// Advertising only shard 1 404s the rest, which strands any worker that
/// doesn't already hold the whole model — and because warm failure is
/// never-wedge safe, it surfaces as "the big model won't distribute"
/// rather than as an error.
#[test]
fn servable_files_expand_a_split_gguf_to_every_shard() {
    let dir = tempfile::tempdir().unwrap();
    let mk = |name: &str| {
        let p = dir.path().join(name);
        std::fs::write(&p, b"x").unwrap();
        p
    };
    let s1 = mk("big-00001-of-00003.gguf");
    let s2 = mk("big-00002-of-00003.gguf");
    let s3 = mk("big-00003-of-00003.gguf");
    let solo = mk("embed.gguf");

    // Config names shard 1 only; all three must become servable.
    let got = servable_model_files(&[s1.clone(), solo.clone()]);
    let canon = |p: &std::path::PathBuf| p.canonicalize().unwrap();
    assert_eq!(
        got,
        vec![canon(&s1), canon(&s2), canon(&s3), canon(&solo)],
        "split slot must advertise every shard, in order, then the solo slot"
    );

    // Dedup: primary_pool points several slots at the same GGUF.
    let got = servable_model_files(&[s1.clone(), s1.clone(), solo.clone()]);
    assert_eq!(
        got.len(),
        4,
        "same model twice must not be advertised twice"
    );
}

/// Never advertise what we cannot serve: with a sibling absent,
/// `shard_files` refuses to guess, so we fall back to the named file.
#[test]
fn servable_files_do_not_guess_missing_shards() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("t-00001-of-00002.gguf");
    std::fs::write(&p, b"x").unwrap();
    let got = servable_model_files(&[p.clone()]);
    assert_eq!(got, vec![p.canonicalize().unwrap()]);
}
