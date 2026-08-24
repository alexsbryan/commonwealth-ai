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
    /// Inline-completion model. Unlike Fast/Embed this does NOT read
    /// `[profiles.<hardware-tier>.fim]` — no such row exists. It reads
    /// the `fim_*` ladder pseudo-profiles via [`fim_rung_for_profile`].
    Fim,
}

// ─── FIM ladder ───────────────────────────────────────────────────
//
// One model (Mellum2-12B-A2.5B-Instruct), four quants. See the FIM
// LADDER block in `models.toml` for why it is quants-of-one rather
// than a model-per-tier: Mellum2 has no smaller sibling, and lean
// mode (primary and `[models.fim].path` are the same file) means the
// rung is bounded by total memory, not by headroom left over after a
// separate chat primary.

/// The rungs, smallest first. `(cli_name, manifest_profile)` — the
/// CLI name is what `svrn setup --fim --quant <q>` accepts and what
/// the setup banner prints; the manifest profile is the key in
/// `models.toml`. Ordering is load-bearing: [`next_fim_rung`] walks
/// it to suggest the next step up.
pub const FIM_RUNGS: &[(&str, &str)] = &[
    ("sweep_1_5b", "edit_sweep_1_5b"),
    ("mxfp4_moe", "fim_mxfp4_moe"),
    ("q4_k_m", "fim_q4_k_m"),
    ("q6_k", "fim_q6_k"),
    ("q8_0", "fim_q8_0"),
];

/// Which rung a hardware profile gets by default.
///
/// The floor is MXFP4_MOE (7.03 GB) because that is the smallest
/// Mellum2 artifact that exists — there is no 1–3 GB rung to fall
/// back to, so `cpu_only` and `low_mem` get the same one. The step to
/// Q4_K_M at `default` (8–19 GB) and Q6_K at `high`/`very_high` tracks
/// total memory, not free-VRAM-after-primary: in lean mode the FIM
/// model IS the resident model.
pub fn fim_rung_for_profile(profile: &ProfileName) -> &'static str {
    // Hardware no longer selects here, and that is the point. The
    // Mellum2 rungs below had to scale with total memory because lean
    // mode made the FIM model the RESIDENT model — the edit slot and
    // the chat primary were the same file. A 1.54 GB dual-lane model
    // fits as a DEDICATED slot beside any primary on any tier, so
    // there is nothing left for the profile to choose between, and the
    // chat model no longer has to be sacrificed to get completions.
    //
    // `--quant` still addresses the Mellum2 rungs by name for anyone
    // who wants that trade; see FIM_RUNGS.
    let _ = profile;
    "sweep_1_5b"
}

/// Resolve a rung by its CLI name (`"q6_k"`). `None` for an unknown
/// name — callers turn that into a usage error listing [`FIM_RUNGS`].
pub fn fim_slot_for_rung(rung: &str) -> Option<SlotConfig> {
    let (_, manifest_profile) = FIM_RUNGS.iter().find(|(name, _)| *name == rung)?;
    DEFAULT_MANIFEST
        .profiles
        .get(*manifest_profile)
        .and_then(|p| p.fim.clone())
}

/// The next rung up from `rung`, or `None` at the top. Drives the
/// "try this next" line in the setup banner — a concrete upgrade the
/// operator can act on, rather than a vague "you can change models".
pub fn next_fim_rung(rung: &str) -> Option<(&'static str, SlotConfig)> {
    let idx = FIM_RUNGS.iter().position(|(name, _)| *name == rung)?;
    let (next_name, _) = FIM_RUNGS.get(idx + 1)?;
    fim_slot_for_rung(next_name).map(|slot| (*next_name, slot))
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
    // FIM resolves off the ladder, not off the hardware profile's own
    // table — `[profiles.high.fim]` intentionally does not exist. Doing
    // this before the profile lookup keeps the fallback below (which
    // reaches for `default.fast` / `default.embed`) from silently
    // returning a chat model for a FIM request.
    if let SlotKind::Fim = kind {
        return fim_slot_for_rung(fim_rung_for_profile(profile));
    }
    let prof_cfg = DEFAULT_MANIFEST.profiles.get(profile_name)?;
    let slot = match kind {
        SlotKind::Fast => prof_cfg.fast.clone(),
        SlotKind::Embed => prof_cfg.embed.clone(),
        SlotKind::Fim => unreachable!("handled above"),
    };
    slot.or_else(|| {
        let default = DEFAULT_MANIFEST.profiles.get("default")?;
        match kind {
            SlotKind::Fast => default.fast.clone(),
            SlotKind::Embed => default.embed.clone(),
            SlotKind::Fim => unreachable!("handled above"),
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
mod fim_ladder_tests {
    use super::*;

    const ALL_PROFILES: [ProfileName; 5] = [
        ProfileName::CpuOnly,
        ProfileName::LowMem,
        ProfileName::Default,
        ProfileName::High,
        ProfileName::VeryHigh,
    ];

    /// Every rung named in `FIM_RUNGS` must actually resolve against
    /// the bundled manifest. This is the test that fails loudly if
    /// someone renames a `[profiles.fim_*]` table without updating the
    /// ladder — the alternative is a runtime "no FIM model available"
    /// on a user's machine during onboarding.
    #[test]
    fn every_rung_resolves_in_the_bundled_manifest() {
        for (cli_name, manifest_profile) in FIM_RUNGS {
            let slot = fim_slot_for_rung(cli_name)
                .unwrap_or_else(|| panic!("rung {cli_name} ({manifest_profile}) did not resolve"));
            assert!(!slot.file.is_empty(), "rung {cli_name} has no file");
            assert!(!slot.hf_url.is_empty(), "rung {cli_name} has no hf_url");
            assert!(slot.size_gb > 0.0, "rung {cli_name} has no size");
        }
    }

    /// Every rung's vocab must carry atomic FIM markers — THAT is the
    /// constraint, and it is why this test used to read
    /// `every_rung_is_mellum2`.
    ///
    /// The reasoning behind the old name still stands: the daemon's
    /// marker probe withholds the FIM lane from a model whose vocab
    /// lacks them, so a well-meaning swap to some other coder GGUF
    /// yields a 503 at completion time rather than a build error. But
    /// "is Mellum2" was a PROXY for the real property, and it excluded
    /// models that satisfy it — Sweep-Next-Edit-1.5B is a
    /// Qwen2.5-Coder derivative whose vocab carries the atomic
    /// `<|fim_prefix|>` family, measured on the artifact rather than
    /// assumed (2026-08-24: `/status.inference.edit` reported
    /// `fim_style: "qwen_coder"` on a dedicated slot and
    /// `/v1/completions` returned a correct infill).
    ///
    /// A vocab cannot be probed without the weights, so this is an
    /// allowlist, not a check. Adding a family here is a claim that
    /// someone ran the probe against that GGUF and saw a style come
    /// back. Do not add one on a model card's say-so.
    #[test]
    fn every_rung_has_verified_fim_markers() {
        const MARKER_VERIFIED: &[&str] = &["Mellum2-12B-A2.5B", "sweep-next-edit-1.5b"];
        for (cli_name, _) in FIM_RUNGS {
            let slot = fim_slot_for_rung(cli_name).expect("rung resolves");
            assert!(
                MARKER_VERIFIED.contains(&slot.base_name.as_str()),
                "rung {cli_name} ({}) is not in the marker-verified allowlist — \
                 run the vocab probe against the GGUF and add it, or drop the rung",
                slot.base_name
            );
        }
    }

    /// The default rung must fit beside a chat primary on the SMALLEST
    /// tier, because that is the whole claim of the four-slot story:
    /// primary + fast + embed + edit, nothing overwritten.
    #[test]
    fn default_rung_is_small_enough_for_a_dedicated_slot() {
        let slot = fim_slot_for_rung(fim_rung_for_profile(&ProfileName::CpuOnly))
            .expect("default rung resolves");
        assert!(
            slot.size_gb <= 3.0,
            "default FIM rung is {} GB — too big to pin beside a primary; \
             lean mode (overwriting [models].primary) would be back",
            slot.size_gb
        );
    }

    /// The ladder must be ordered smallest-first — `next_fim_rung`
    /// walks it as a monotonic upgrade path, so an out-of-order entry
    /// would recommend a *downgrade* as the next step to try.
    #[test]
    fn rungs_are_ordered_smallest_first() {
        let sizes: Vec<f64> = FIM_RUNGS
            .iter()
            .map(|(n, _)| fim_slot_for_rung(n).expect("rung resolves").size_gb)
            .collect();
        for pair in sizes.windows(2) {
            assert!(
                pair[0] < pair[1],
                "ladder is not ascending: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn every_hardware_profile_maps_to_a_resolvable_rung() {
        for p in ALL_PROFILES {
            let rung = fim_rung_for_profile(&p);
            assert!(
                fim_slot_for_rung(rung).is_some(),
                "{p:?} maps to unknown rung {rung}"
            );
            assert!(
                resolve_slot(&p, SlotKind::Fim).is_some(),
                "resolve_slot(Fim) returned None for {p:?}"
            );
        }
    }

    /// `resolve_slot(Fim)` must never fall through to the `default`
    /// profile's fast/embed slot the way Fast/Embed do — that fallback
    /// would hand back a Qwen chat model for a FIM request, and the
    /// failure would surface as garbage ghost text rather than a clear
    /// refusal.
    #[test]
    fn fim_never_falls_back_to_a_chat_slot() {
        // The guard is "came from the FIM ladder", not "is Mellum2" —
        // the old spelling pinned the family as a stand-in for that,
        // and a second FIM-capable rung made the stand-in wrong while
        // the concern stayed exactly the same. Comparing against the
        // ladder's own base_names cannot be satisfied by a chat slot,
        // which is what this test is actually here to prevent.
        let ladder: Vec<String> = FIM_RUNGS
            .iter()
            .map(|(n, _)| fim_slot_for_rung(n).expect("rung resolves").base_name)
            .collect();
        for p in ALL_PROFILES {
            let slot = resolve_slot(&p, SlotKind::Fim).expect("resolves");
            assert!(
                ladder.contains(&slot.base_name),
                "{p:?} resolved to {:?}, which is not a FIM ladder rung — \
                 Fim fell through to a fast/embed/thoughtful slot",
                slot.base_name
            );
        }
    }

    #[test]
    fn next_rung_walks_up_and_stops_at_the_top() {
        let (first, _) = FIM_RUNGS[0];
        let (second, next_slot) = next_fim_rung(first).expect("a rung above the floor exists");
        assert_eq!(second, FIM_RUNGS[1].0);
        assert!(next_slot.size_gb > fim_slot_for_rung(first).unwrap().size_gb);

        let (top, _) = FIM_RUNGS[FIM_RUNGS.len() - 1];
        assert!(
            next_fim_rung(top).is_none(),
            "top rung {top} should have nothing above it"
        );
    }

    #[test]
    fn unknown_rung_names_resolve_to_none() {
        assert!(fim_slot_for_rung("q3_k_s").is_none());
        assert!(next_fim_rung("not-a-rung").is_none());
    }
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
