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

/// serve's bundle answers the list it was handed, over loopback, and the
/// list is every shard of each advertised slot (pb-serve-distributes).
/// Failing input: publish `servable_for`'s input paths unexpanded, and the
/// split's second shard is missing from the listing.
#[tokio::test]
async fn the_bundle_lists_every_shard_of_the_advertised_slots() {
    let dir = tempfile::tempdir().unwrap();
    for name in [
        "big-00001-of-00002.gguf",
        "big-00002-of-00002.gguf",
        "embed.gguf",
    ] {
        std::fs::write(dir.path().join(name), b"x").unwrap();
    }
    let models = sovereign_contracts::setup_config::ModelsSection {
        primary: dir.path().join("big-00001-of-00002.gguf"),
        embed: dir.path().join("embed.gguf"),
        ..Default::default()
    };
    assert!(servable_for(None).is_empty());
    let servable = ServableModelFilesReader::default();
    servable.publish(servable_for(Some(&models)));
    let app = host_kit::shell::mount(vec![bundle(servable)]);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .ok()
    });
    let listing: ModelFileListing = reqwest::get(format!(
        "{base}{}",
        oicp_types::model_transfer::MODELS_LIST_PATH
    ))
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let mut names: Vec<String> = listing.files.into_iter().map(|f| f.name).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "big-00001-of-00002.gguf",
            "big-00002-of-00002.gguf",
            "embed.gguf"
        ]
    );
}
