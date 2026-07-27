// SPDX-License-Identifier: AGPL-3.0-or-later
//! Model download wrappers + progress rendering — extracted from
//! `setup_cmd` (§3.2). Thin CLI-stderr adapters over
//! `sovereign_inference::setup_planner::download_gguf`.

use std::io::{self, Write as _};
use std::path::Path;
use std::time::Duration;

use sovereign_inference::setup_planner::download_gguf;

// ─── Downloaders ───────────────────────────────────────────────────
//
// URL building, resume-aware streaming, GGUF validation, and the
// HF_TOKEN env helper all live in
// `sovereign_inference::setup_planner` so the desktop's
// `complete_setup_auto` flow can call the same code. The two
// thin wrappers below adapt that downloader to the CLI's
// stderr-renderer style — the CLI prints a ╲ progress bar with
// `print_progress`, the desktop emits Tauri events.

/// Download `url` to `dest`, streaming a percentage bar to stderr.
/// Resumes from a `.part` sibling if one exists; rejects HTML
/// error pages before they hit disk; validates the result against
/// the slot's advertised `size_gb`. If `dest` already exists and
/// validates, prints a "(already present)" line and returns Ok.
pub(crate) async fn download_with_progress(
    url: &str,
    dest: &Path,
    display: &str,
    size_gb: f64,
) -> Result<(), String> {
    let expected = sovereign_inference::GgufExpectation::from_size_gb(size_gb);

    // The shared downloader doesn't print "(already present)" on
    // its own — surface that here for parity with the prior CLI
    // behavior. (`download_gguf` *does* still skip the work; we
    // just want the line to print.)
    if dest.metadata().map(|m| m.len() > 0).unwrap_or(false)
        && sovereign_inference::validate_gguf(dest, &expected).is_ok()
    {
        println!("    \u{2713} {display} (already present)");
        return Ok(());
    }

    eprint!("    {display}  ");
    io::stderr().flush().ok();

    let display_owned = display.to_string();
    let last_print = std::sync::Mutex::new(std::time::Instant::now() - Duration::from_secs(1));
    let result = download_gguf(url, dest, &expected, &|done, total| {
        let mut lp = last_print.lock().unwrap();
        if lp.elapsed() > Duration::from_millis(250) || total.map(|t| done >= t).unwrap_or(false) {
            print_progress(&display_owned, done, total);
            *lp = std::time::Instant::now();
        }
    })
    .await;
    eprintln!();
    result
}

/// Same as `download_with_progress` but with no per-chunk
/// rendering. Used for fast + embed where the CLI shows only a
/// final ✓ line; the shared downloader does all the work.
pub(super) async fn download_silent(url: &str, dest: &Path, size_gb: f64) -> Result<(), String> {
    let expected = sovereign_inference::GgufExpectation::from_size_gb(size_gb);
    download_gguf(url, dest, &expected, &|_, _| {}).await
}
/// Given a model file path, look up the slot's advertised
/// `size_gb` from the bundled manifest by filename match. The
/// manifest indexes by profile + slot, so we scan every slot in
/// every profile for a filename match; first hit wins. Returns
/// `None` if the user has a custom / BYOM model whose filename
/// isn't in the manifest.
pub(super) fn lookup_slot_size_gb(
    manifest: &sovereign_core::models_manifest::ModelsManifest,
    path: &std::path::Path,
) -> Option<f64> {
    let file_name = path.file_name()?.to_str()?;
    for profile in manifest.profiles.values() {
        // `fim` is in the sweep so `svrn setup --repair` applies the
        // ladder's real size floor to a Mellum2 GGUF instead of the
        // 1 MB BYOM sentinel — a truncated 7 GB download would
        // otherwise pass validation and fail at load time.
        for s in [
            &profile.thoughtful,
            &profile.fast,
            &profile.embed,
            &profile.fim,
        ]
        .into_iter()
        .flatten()
        {
            if s.file == file_name {
                return Some(s.size_gb);
            }
        }
    }
    for user in &manifest.user_slots {
        if user.file == file_name {
            return Some(user.size_gb);
        }
    }
    None
}

// Used by the test module's `has_content_distinguishes_*` cases;
// production paths now go through `setup_planner::download_gguf`,
// which checks file size internally.
#[cfg(test)]
pub(super) fn has_content(p: &Path) -> bool {
    p.metadata().map(|m| m.len() > 0).unwrap_or(false)
}

fn print_progress(label: &str, done: u64, total: Option<u64>) {
    const BAR_WIDTH: usize = 20;
    let (pct_f, pct_s) = match total {
        Some(t) if t > 0 => {
            let p = (done as f64 / t as f64).clamp(0.0, 1.0);
            (p, format!("{:3.0}%", p * 100.0))
        }
        _ => (0.0, "--%".to_string()),
    };
    let filled = (pct_f * BAR_WIDTH as f64) as usize;
    let bar: String = (0..BAR_WIDTH)
        .map(|i| if i < filled { '\u{2588}' } else { '\u{2591}' })
        .collect();
    let done_mb = done as f64 / 1_048_576.0;
    let total_mb = total.map(|t| t as f64 / 1_048_576.0);
    let size = match total_mb {
        Some(t) => format!("{done_mb:>6.0}/{t:.0} MB"),
        None => format!("{done_mb:>6.0} MB"),
    };
    eprint!("\r    {label:<40}  [{bar}] {pct_s}  {size}");
    io::stderr().flush().ok();
}
