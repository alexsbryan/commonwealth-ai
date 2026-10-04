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

// `human_bytes` is the CLI leaf's; svrn's `corpus pull` prints it too.
pub(super) use sovereign_cli_base::units::human_bytes;

pub(super) fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
