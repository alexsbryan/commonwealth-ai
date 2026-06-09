// SPDX-License-Identifier: AGPL-3.0-or-later
//! Corpus display formatters — extracted from `corpus_cmd` (§3.2).
//! Byte/count humanisation + recursive directory sizing, shared by the
//! inventory + partition commands.

use std::path::Path;

/// Recursive directory size in bytes. Returns 0 on any I/O error so a
/// failed stat doesn't abort the remove plan summary — we'd rather
/// show "0 B" than refuse to render the plan.
pub(super) fn dir_size_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total = total.saturating_add(dir_size_bytes(&p));
        } else {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

/// Render a byte count as a human-readable size (KiB/MiB/GiB).
/// Used in the remove plan summary so operators see "5.2 GiB" instead
/// of `5582813696`.
pub(super) fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{:.2} {}", size, UNITS[unit])
    }
}
pub(super) fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
