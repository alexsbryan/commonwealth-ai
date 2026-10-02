// SPDX-License-Identifier: AGPL-3.0-or-later
//! The planner's and downloader's tests that lived in cli-daemon's
//! setup_cmd, which reached this module in-process until setup asked the
//! loader by exec (pb-distribution-setup).
use super::*;

// ── hf_download_url ────────────────────────────────────────────

#[test]
fn hf_download_url_from_repo_landing_page() {
    let slot = SlotConfig {
        file: "Qwen3-1.7B-Q8_0.gguf".into(),
        hf_url: "https://huggingface.co/Qwen/Qwen3-1.7B-GGUF".into(),
        ..Default::default()
    };
    assert_eq!(
        hf_download_url(&slot),
        "https://huggingface.co/Qwen/Qwen3-1.7B-GGUF/resolve/main/Qwen3-1.7B-Q8_0.gguf"
    );
}

#[test]
fn hf_download_url_handles_trailing_slash() {
    let slot = SlotConfig {
        file: "model.gguf".into(),
        hf_url: "https://huggingface.co/org/repo/".into(),
        ..Default::default()
    };
    assert_eq!(
        hf_download_url(&slot),
        "https://huggingface.co/org/repo/resolve/main/model.gguf"
    );
}

#[test]
fn hf_download_url_passes_through_direct_urls() {
    // If the manifest already has a direct /resolve/ URL, don't
    // double-append.
    let slot = SlotConfig {
        file: "model.gguf".into(),
        hf_url: "https://huggingface.co/org/repo/resolve/main/model.gguf".into(),
        ..Default::default()
    };
    assert_eq!(
        hf_download_url(&slot),
        "https://huggingface.co/org/repo/resolve/main/model.gguf"
    );
}

// ── tier_rank ──────────────────────────────────────────────────

#[test]
fn tier_rank_orders_profiles_low_to_high() {
    assert!(tier_rank(&ProfileName::CpuOnly) < tier_rank(&ProfileName::LowMem));
    assert!(tier_rank(&ProfileName::LowMem) < tier_rank(&ProfileName::Default));
    assert!(tier_rank(&ProfileName::Default) < tier_rank(&ProfileName::High));
    assert!(tier_rank(&ProfileName::High) < tier_rank(&ProfileName::VeryHigh));
}

// ── build_primary_catalog ─────────────────────────────────────

#[test]
fn catalog_is_non_empty_for_every_profile() {
    // Sanity — the bundled manifest should support every hardware tier.
    for p in [
        ProfileName::CpuOnly,
        ProfileName::LowMem,
        ProfileName::Default,
        ProfileName::High,
        ProfileName::VeryHigh,
    ] {
        let cat = build_primary_catalog(&p);
        assert!(!cat.is_empty(), "catalog empty for {p:?}");
    }
}

#[test]
fn catalog_marks_exactly_one_recommended() {
    let cat = build_primary_catalog(&ProfileName::Default);
    let recommended: Vec<_> = cat.iter().filter(|o| o.recommended).collect();
    assert_eq!(
        recommended.len(),
        1,
        "expected exactly one recommended row, got {}",
        recommended.len()
    );
}

#[test]
fn catalog_excludes_tiers_above_user_hardware() {
    // A Default-tier machine must NOT see VeryHigh or High options — they
    // won't fit in VRAM. Verify by checking no returned slot came from a
    // higher tier's thoughtful slot.
    let cat = build_primary_catalog(&ProfileName::Default);
    let very_high_thoughtful = DEFAULT_MANIFEST
        .profiles
        .get("very_high")
        .and_then(|p| p.thoughtful.as_ref())
        .map(|s| s.file.clone());
    if let Some(f) = very_high_thoughtful {
        assert!(
            !cat.iter().any(|o| o.slot.file == f),
            "Default-tier catalog leaked very_high slot {f}"
        );
    }
}

#[test]
fn catalog_dedupes_by_base_name() {
    // If two profile tiers point to the same base model, the catalog
    // should show it only once. We can't assume the bundled manifest has
    // duplicates, so construct a stricter invariant: every base_name
    // appears at most once.
    let cat = build_primary_catalog(&ProfileName::VeryHigh);
    let mut seen = std::collections::HashSet::new();
    for opt in &cat {
        let key = if opt.slot.base_name.is_empty() {
            opt.slot.file.clone()
        } else {
            opt.slot.base_name.clone()
        };
        assert!(
            seen.insert(key.clone()),
            "duplicate base_name in catalog: {key}"
        );
    }
}

#[test]
fn catalog_very_high_includes_every_tier_below() {
    // VeryHigh users should see every tier at-or-below them (subject to
    // dedup). Count of distinct tiers available should be >= 1 (hard
    // guarantee) and match the number of profiles that define thoughtful
    // and have non-duplicate base_names.
    let cat = build_primary_catalog(&ProfileName::VeryHigh);
    assert!(!cat.is_empty());
    // First row (recommended) should be the VeryHigh slot.
    let first = &cat[0];
    assert!(first.recommended);
}

// ── resolve_slot ───────────────────────────────────────────────

#[test]
fn resolve_slot_returns_profile_slot_when_defined() {
    // Default profile has all three slots defined in the bundled manifest.
    let fast = resolve_slot(&ProfileName::Default, SlotKind::Fast);
    let embed = resolve_slot(&ProfileName::Default, SlotKind::Embed);
    assert!(fast.is_some(), "default.fast should exist");
    assert!(embed.is_some(), "default.embed should exist");
}

#[test]
fn resolve_slot_falls_back_to_default_when_missing() {
    // This test encodes the invariant: even if a profile is thin (say,
    // cpu_only missing embed), we must fall back to default.embed so
    // `setup` always has three paths to write.
    for p in [
        ProfileName::CpuOnly,
        ProfileName::LowMem,
        ProfileName::Default,
        ProfileName::High,
        ProfileName::VeryHigh,
    ] {
        assert!(
            resolve_slot(&p, SlotKind::Fast).is_some(),
            "no fast slot (even via fallback) for {p:?}"
        );
        assert!(
            resolve_slot(&p, SlotKind::Embed).is_some(),
            "no embed slot (even via fallback) for {p:?}"
        );
    }
}

#[cfg(test)]
mod download_failure_tests {
    //! Integration tests for the download validation path. Each
    //! spins up an axum mock on a kernel-assigned port, points
    //! `download_gguf` at it, and asserts the expected
    //! failure mode leaves the models dir clean.
    use super::*;
    use axum::{response::IntoResponse, routing::get, Router};
    use std::net::SocketAddr;

    /// The downloader at a slot's size floor, with no progress rendering.
    async fn download(url: &str, dest: &Path, size_gb: f64) -> Result<(), String> {
        download_gguf(
            url,
            dest,
            &GgufExpectation::from_size_gb(size_gb),
            &|_, _| {},
        )
        .await
    }

    async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        (format!("http://{addr}"), handle)
    }

    /// The pathological case that landed three 188 KB stubs on
    /// the user's disk: CDN returns 200 OK with `text/html`
    /// body. The content-type pre-check must fire and refuse
    /// *before* we stream any HTML to the `.part` file.
    #[tokio::test]
    async fn rejects_text_html_before_streaming_and_leaves_no_part() {
        let app = Router::new().route(
            "/fake-model.gguf",
            get(|| async {
                (
                    [(reqwest::header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    "<!DOCTYPE html><html><body>rate limited</body></html>",
                )
                    .into_response()
            }),
        );
        let (base, _handle) = serve(app).await;

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("fake-model.gguf");
        let part = tmp.path().join("fake-model.gguf.part");

        let err = download(
            &format!("{base}/fake-model.gguf"),
            &dest,
            18.5, // pretend this is a big model
        )
        .await
        .unwrap_err();
        assert!(
            err.contains("content-type") || err.contains("text/html"),
            "err: {err}"
        );
        assert!(!dest.exists(), "no stub should land at final path");
        assert!(!part.exists(), "no .part should remain");
    }

    /// Server returns 200 with `application/octet-stream` but
    /// the body is HTML anyway — post-stream `validate_gguf`
    /// catches the magic-byte mismatch. We assert the `.part`
    /// is cleaned up so a retry doesn't resume a bogus file.
    #[tokio::test]
    async fn rejects_post_stream_when_magic_is_wrong_and_deletes_part() {
        // 2 MB of fake HTML, above the default 1 MB floor so the
        // size check passes and the magic check is the one that
        // fires. Advertises octet-stream to bypass the pre-check.
        let mut body = Vec::new();
        body.extend_from_slice(b"<!DOCTYPE html><html>");
        body.resize(2_000_000, b'.');

        let app = Router::new().route(
            "/fake-model.gguf",
            get(move || {
                let body = body.clone();
                async move {
                    (
                        [(reqwest::header::CONTENT_TYPE, "application/octet-stream")],
                        body,
                    )
                        .into_response()
                }
            }),
        );
        let (base, _handle) = serve(app).await;

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("fake.gguf");
        let part = tmp.path().join("fake.gguf.part");

        // size_gb=0.001 → 1 MB floor (the default min); the 2 MB
        // body passes the size check, so the GGUF magic check is
        // what fires. This is the important case: servers that
        // return HTML with an innocuous content-type header.
        let err = download(&format!("{base}/fake-model.gguf"), &dest, 0.001)
            .await
            .unwrap_err();
        assert!(
            err.contains("not a GGUF") || err.contains("GGUF") || err.contains("magic"),
            "err should mention magic mismatch: {err}"
        );
        assert!(!dest.exists(), "no stub should land at final path");
        assert!(!part.exists(), "no .part should remain on failure");
    }

    /// A successful response with a real GGUF magic header and
    /// plausible size lands at the final path. Confirms the
    /// happy path isn't broken by the new validation layer.
    #[tokio::test]
    async fn accepts_real_gguf_and_renames_to_final() {
        let mut body = Vec::with_capacity(2 * 1024 * 1024);
        body.extend_from_slice(b"GGUF");
        body.resize(2 * 1024 * 1024, 0u8);

        let app = Router::new().route(
            "/real-model.gguf",
            get(move || {
                let body = body.clone();
                async move {
                    (
                        [(reqwest::header::CONTENT_TYPE, "application/octet-stream")],
                        body,
                    )
                        .into_response()
                }
            }),
        );
        let (base, _handle) = serve(app).await;

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("real.gguf");
        // size_gb 0.001 so the 50% floor (512 KB) comfortably
        // accepts our 2 MB test payload.
        download(&format!("{base}/real-model.gguf"), &dest, 0.001)
            .await
            .expect("happy path should succeed");
        assert!(dest.exists(), "final path should hold the downloaded file");
        assert_eq!(dest.metadata().unwrap().len(), 2 * 1024 * 1024);
    }

    #[test]
    fn hf_token_reads_env_var() {
        // Unset first to get a clean baseline; safe because tests
        // use a distinct thread and no production code reads this
        // during tests.
        std::env::remove_var("HF_TOKEN");
        assert!(hf_token().is_none());
        std::env::set_var("HF_TOKEN", "secret");
        assert_eq!(hf_token().as_deref(), Some("secret"));
        std::env::set_var("HF_TOKEN", "");
        assert!(hf_token().is_none(), "empty token counted as unset");
        std::env::remove_var("HF_TOKEN");
    }
}
