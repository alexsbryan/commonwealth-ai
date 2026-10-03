// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for the terminal-onboarding verb.

use super::*;

#[test]
fn entry_addresses_normalize_to_an_origin_and_a_v1_base() {
    for raw in [
        "halo:9741",
        "http://halo:9741",
        "http://halo:9741/",
        "http://halo:9741/v1",
        "http://halo:9741/v1/",
        "  http://halo:9741/v1  ",
    ] {
        let (origin, v1) = normalize_entry(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
        assert_eq!(origin, "http://halo:9741", "origin for {raw}");
        assert_eq!(v1, "http://halo:9741/v1", "v1 base for {raw}");
    }
}

#[test]
fn an_https_entry_keeps_its_scheme() {
    let (origin, v1) = normalize_entry("https://halo.example:8443/v1").unwrap();
    assert_eq!(origin, "https://halo.example:8443");
    assert_eq!(v1, "https://halo.example:8443/v1");
}

#[test]
fn an_empty_or_hostless_entry_is_refused() {
    assert!(normalize_entry("").is_err());
    assert!(normalize_entry("   ").is_err());
    assert!(normalize_entry("http://").is_err());
}

/// `/status` shapes: a node with an embed slot, a node without one, and a
/// body that says nothing about slots. The middle two are the same value
/// (`None`) and both are distinct from a probe error — which is why the
/// signature is `Result<Option<_>>` and not `Option<_>` (§18.2).
#[test]
fn the_embed_slot_is_read_by_role_not_by_position() {
    let body = serde_json::json!({
        "inference": {
            "resident": [
                {"role": "fast", "model_id": "Qwen3-1.7B-Q8_0"},
                {"role": "embed", "model_id": "qwen-embedding-0.6b"},
                {"role": "primary", "model_id": "Qwen3.5-35B-A3B"},
            ]
        }
    });
    let found = body
        .get("inference")
        .and_then(|i| i.get("resident"))
        .and_then(|r| r.as_array())
        .and_then(|slots| {
            slots
                .iter()
                .find(|s| s.get("role").and_then(|r| r.as_str()) == Some("embed"))
        })
        .and_then(|s| s.get("model_id"))
        .and_then(|m| m.as_str());
    assert_eq!(found, Some("qwen-embedding-0.6b"));
}

/// The wizard spawns the one daemon; this crate never constructs one
/// (fp-cond2-c, ARCH principle 10). The needle is split so this file does not
/// match itself. Watched to fail at landing: the construction re-added in
/// terminal.rs, red, reverted.
#[test]
fn no_daemon_is_constructed_in_this_crate() {
    const NEEDLE: &str = concat!("EmbeddedDaemon", "::new(");
    fn walk(dir: &std::path::Path, hits: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("read src dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, hits);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).expect("read source");
                for (n, line) in text.lines().enumerate() {
                    if line.contains(NEEDLE) {
                        hits.push(format!("{}:{}", path.display(), n + 1));
                    }
                }
            }
        }
    }
    let mut hits = Vec::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut hits,
    );
    assert!(
        hits.is_empty(),
        "`{NEEDLE}` in sovereign-cli-daemon — spawn the `sovereign-daemon` sibling \
         instead (setup_cmd/terminal/join_child.rs): {hits:?}"
    );
}
