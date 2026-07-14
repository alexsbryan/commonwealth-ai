// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared catalog + downloader for first-run model setup.
//!
//! Lifted out of `sovereign-cli/src/setup_cmd.rs` so both the CLI
//! (`sovereign setup`) and the desktop (`complete_setup_auto`) call
//! the same logic. Catalog construction, slot resolution, and the
//! resumable GGUF downloader live here; the CLI and desktop wrap
//! them with their own progress UIs (stderr renderer for CLI, Tauri
//! event stream for desktop).

use std::path::Path;
use std::time::Duration;

use futures::StreamExt as _;
use sovereign_core::models_manifest::{SlotConfig, DEFAULT_MANIFEST};

use crate::hardware::ProfileName;
use crate::{validate_gguf, GgufExpectation};

// ─── Catalog ──────────────────────────────────────────────────────

/// One row in the curated primary-model picker. Carries the slot
/// definition plus a `recommended` flag so callers know which entry
/// is the default for the detected hardware.
#[derive(Clone, Debug)]
pub struct PrimaryOption {
    /// Profile this slot was drawn from — `"high"`, `"default"` etc.
    pub profile: &'static str,
    pub slot: SlotConfig,
    pub recommended: bool,
}

impl std::ops::Deref for PrimaryOption {
    type Target = SlotConfig;
    fn deref(&self) -> &Self::Target {
        &self.slot
    }
}

#[derive(Clone, Copy, Debug)]
pub enum SlotKind {
    Fast,
    Embed,
}

/// Build the curated list of primary-model options for the user's
/// profile tier: the profile's own `thoughtful` slot (marked
/// recommended), plus each smaller profile's thoughtful slot so the
/// user can opt into a faster / smaller model if they prefer.
pub fn build_primary_catalog(profile: &ProfileName) -> Vec<PrimaryOption> {
    let order = [
        ("very_high", ProfileName::VeryHigh),
        ("high", ProfileName::High),
        ("default", ProfileName::Default),
        ("low_mem", ProfileName::LowMem),
        ("cpu_only", ProfileName::CpuOnly),
    ];
    let max_tier_rank = tier_rank(profile);

    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (name, p) in order {
        if tier_rank(&p) > max_tier_rank {
            continue;
        }
        let Some(prof_cfg) = DEFAULT_MANIFEST.profiles.get(name) else {
            continue;
        };
        let Some(slot) = prof_cfg.thoughtful.clone() else {
            continue;
        };
        let key = if slot.base_name.is_empty() {
            slot.file.clone()
        } else {
            slot.base_name.clone()
        };
        if !seen.insert(key) {
            continue;
        }
        out.push(PrimaryOption {
            profile: name,
            recommended: &p == profile,
            slot,
        });
    }

    // Curated opt-in alternatives (e.g. Gemma 4 12B) — NOT hardware tiers, so
    // hardware detection and `recommended_primary` never pick them. Surface
    // them as NON-recommended options for tiers that can actually run them
    // (Default and up); the auto-default stays the tier's recommended Qwen
    // pick pushed above. `profile: "default"` is only the display/sizing key
    // the picker reads — it does not make this a selectable hardware profile.
    if tier_rank(profile) >= tier_rank(&ProfileName::Default) {
        if let Some(slot) = DEFAULT_MANIFEST
            .profiles
            .get("alt_gemma_12b")
            .and_then(|p| p.thoughtful.clone())
        {
            out.push(PrimaryOption {
                profile: "default",
                recommended: false,
                slot,
            });
        }
    }

    out
}

pub fn tier_rank(p: &ProfileName) -> u8 {
    match p {
        ProfileName::CpuOnly => 0,
        ProfileName::LowMem => 1,
        ProfileName::Default => 2,
        ProfileName::High => 3,
        ProfileName::VeryHigh => 4,
    }
}

/// Resolve the fast or embed slot for a given profile, with
/// fallback to the `default` profile when the user's profile
/// doesn't define one (rare, e.g. embed on cpu_only).
pub fn resolve_slot(profile: &ProfileName, kind: SlotKind) -> Option<SlotConfig> {
    let profile_name = match *profile {
        ProfileName::CpuOnly => "cpu_only",
        ProfileName::LowMem => "low_mem",
        ProfileName::Default => "default",
        ProfileName::High => "high",
        ProfileName::VeryHigh => "very_high",
    };
    let prof_cfg = DEFAULT_MANIFEST.profiles.get(profile_name)?;
    let slot = match kind {
        SlotKind::Fast => prof_cfg.fast.clone(),
        SlotKind::Embed => prof_cfg.embed.clone(),
    };
    slot.or_else(|| {
        let default = DEFAULT_MANIFEST.profiles.get("default")?;
        match kind {
            SlotKind::Fast => default.fast.clone(),
            SlotKind::Embed => default.embed.clone(),
        }
    })
}

/// Pick the recommended primary slot for a profile, falling back to
/// the first row of the catalog if no entry is flagged recommended.
/// Returns `None` if the catalog is empty (manifest broken).
pub fn recommended_primary(profile: &ProfileName) -> Option<SlotConfig> {
    let catalog = build_primary_catalog(profile);
    catalog
        .iter()
        .find(|o| o.recommended)
        .or_else(|| catalog.first())
        .map(|o| o.slot.clone())
}

// ─── URL building ─────────────────────────────────────────────────

/// Build the direct GGUF download URL from a manifest slot. The
/// `hf_url` field in `models.toml` is the *repo* URL, not the file
/// URL, so we append `/resolve/main/<file>` to land on the raw LFS
/// blob. Matches the canonical `huggingface-cli download` path;
/// supports HTTP Range for resume.
pub fn hf_download_url(slot: &SlotConfig) -> String {
    let repo = slot
        .hf_url
        .trim_end_matches('/')
        .strip_prefix("https://huggingface.co/")
        .unwrap_or(&slot.hf_url);
    if slot.hf_url.contains("/resolve/") {
        slot.hf_url.clone()
    } else {
        format!("https://huggingface.co/{repo}/resolve/main/{}", slot.file)
    }
}

/// Read `HF_TOKEN` from the environment for HuggingFace bearer
/// auth. Authenticated requests bypass the anonymous rate-limit
/// and bot-detection paths that return HTML error pages.
pub fn hf_token() -> Option<String> {
    std::env::var("HF_TOKEN").ok().filter(|s| !s.is_empty())
}

// ─── Downloader ───────────────────────────────────────────────────

/// Progress callback signature. `(bytes_downloaded, total_bytes)`.
/// `total` is `None` while we haven't yet seen `Content-Length`
/// (rare with HuggingFace). The callback is invoked at most every
/// ~250ms during streaming, plus once at the start and once at the
/// end with the final values.
pub type DownloadProgressCb<'a> = &'a (dyn Fn(u64, Option<u64>) + Send + Sync);

/// Download `url` to `dest`, resuming from a `.part` sibling if
/// one exists. Validates the result against the slot's expected
/// GGUF magic + size floor (see `GgufExpectation::from_size_gb`).
///
/// If `dest` already exists and validates, returns `Ok(())`
/// immediately (no work). If `dest` exists but fails validation
/// (HTML error pages from prior runs, partial truncation, etc.),
/// it's deleted and re-downloaded.
///
/// `on_progress` is invoked during streaming for byte counts. If
/// you want to report ETA, compute it in the callback from
/// rolling samples — this function intentionally doesn't pre-bake
/// rate logic so the CLI's stderr renderer and the desktop's
/// `setup-progress` event stream can each format their own.
pub async fn download_gguf(
    url: &str,
    dest: &Path,
    expected: &GgufExpectation,
    on_progress: DownloadProgressCb<'_>,
) -> Result<(), String> {
    if has_content(dest) {
        match validate_gguf(dest, expected) {
            Ok(()) => {
                // Surface the final size so the caller can finish
                // their progress rendering at 100%.
                let len = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
                on_progress(len, Some(len));
                return Ok(());
            }
            Err(_) => {
                let _ = std::fs::remove_file(dest);
            }
        }
    }
    let part = dest.with_extension("part");
    let resume_from = part.metadata().map(|m| m.len()).unwrap_or(0);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60 * 60))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;
    let mut req = client.get(url);
    if resume_from > 0 {
        req = req.header("Range", format!("bytes={resume_from}-"));
    }
    if let Some(tok) = hf_token() {
        req = req.bearer_auth(tok);
    }
    let resp = req.send().await.map_err(|e| format!("GET {url}: {e}"))?;
    if !resp.status().is_success() && resp.status().as_u16() != 206 {
        return Err(format!("GET {url}: {}", resp.status()));
    }
    reject_non_binary_content_type(&resp, url)?;

    let total = resp.content_length().map(|c| c + resume_from);

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(resume_from > 0)
        .write(true)
        .truncate(resume_from == 0)
        .open(&part)
        .map_err(|e| format!("open {}: {e}", part.display()))?;

    on_progress(resume_from, total);

    let mut stream = resp.bytes_stream();
    let mut downloaded = resume_from;
    let mut last_emit = std::time::Instant::now();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| format!("stream: {e}"))?;
        std::io::Write::write_all(&mut file, &bytes)
            .map_err(|e| format!("write {}: {e}", part.display()))?;
        downloaded += bytes.len() as u64;
        if last_emit.elapsed() > Duration::from_millis(250) {
            on_progress(downloaded, total);
            last_emit = std::time::Instant::now();
        }
    }
    on_progress(downloaded, total);
    drop(file);

    if let Err(e) = validate_gguf(&part, expected) {
        let _ = std::fs::remove_file(&part);
        return Err(format!("download validation failed: {e}"));
    }

    std::fs::rename(&part, dest)
        .map_err(|e| format!("rename {} -> {}: {e}", part.display(), dest.display()))?;
    Ok(())
}

fn reject_non_binary_content_type(resp: &reqwest::Response, url: &str) -> Result<(), String> {
    let Some(ct) = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    else {
        return Ok(());
    };
    let lower = ct.to_ascii_lowercase();
    if lower.starts_with("text/") || lower.starts_with("application/json") {
        return Err(format!(
            "HuggingFace returned content-type={ct} for {url} — likely \
             bot-detection, rate limiting, or a gated-repo login page. \
             Try setting `HF_TOKEN` before setup to use authenticated downloads."
        ));
    }
    Ok(())
}

fn has_content(p: &Path) -> bool {
    p.metadata().map(|m| m.len() > 0).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offers_gemma_alt(profile: &ProfileName) -> bool {
        build_primary_catalog(profile)
            .iter()
            .any(|o| !o.recommended && o.slot.base_name.contains("gemma-4-12B"))
    }

    #[test]
    fn gemma_alternative_offered_to_capable_tiers_only() {
        // Surfaced as a NON-recommended option on Default and up...
        assert!(
            offers_gemma_alt(&ProfileName::Default),
            "default should offer Gemma"
        );
        assert!(
            offers_gemma_alt(&ProfileName::High),
            "high should offer Gemma"
        );
        assert!(
            offers_gemma_alt(&ProfileName::VeryHigh),
            "very_high should offer Gemma"
        );
        // ...and withheld from tiers that can't run a 7.4 GB model.
        assert!(
            !offers_gemma_alt(&ProfileName::LowMem),
            "low_mem must not offer Gemma"
        );
        assert!(
            !offers_gemma_alt(&ProfileName::CpuOnly),
            "cpu_only must not offer Gemma"
        );
    }

    #[test]
    fn moe_tiers_use_expected_models() {
        // very_high bumped to the 3.5-generation 35B-A3B MoE...
        let vh = recommended_primary(&ProfileName::VeryHigh).expect("very_high primary");
        assert!(
            vh.base_name.contains("Qwen3.5-35B-A3B"),
            "very_high should be the 35B-A3B bump, got {}",
            vh.base_name
        );
        // ...and high moves to the SAME 3.5-gen 35B-A3B at a smaller quant
        // (Q3_K_M) so it still fits a 20 GB card. It shares base_name with
        // very_high; the tier filter in build_primary_catalog keeps each
        // tier surfacing its own quant.
        let high = recommended_primary(&ProfileName::High).expect("high primary");
        assert!(
            high.base_name.contains("Qwen3.5-35B-A3B"),
            "high must be the 3.5-gen 35B-A3B (Q3_K_M, fits 20 GB), got {}",
            high.base_name
        );
    }

    #[test]
    fn gemma_alternative_is_never_the_auto_default() {
        for p in [
            ProfileName::Default,
            ProfileName::High,
            ProfileName::VeryHigh,
        ] {
            let rec = recommended_primary(&p).expect("a recommended primary exists");
            assert!(
                !rec.base_name.to_lowercase().contains("gemma"),
                "{p:?}: auto-default must stay Qwen, got {}",
                rec.base_name
            );
            let recommended_count = build_primary_catalog(&p)
                .iter()
                .filter(|o| o.recommended)
                .count();
            assert_eq!(recommended_count, 1, "{p:?}: exactly one recommended entry");
        }
    }
}
