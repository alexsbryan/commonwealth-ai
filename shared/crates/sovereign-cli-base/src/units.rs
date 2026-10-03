// SPDX-License-Identifier: AGPL-3.0-or-later
//! Human-readable units for CLI output. `human_bytes` moved here from
//! sovereign-cli-llm `corpus_cmd/fmt.rs` because two programs' corpus verbs
//! print it: ingest's `corpus remove` plan and svrn's `corpus pull`
//! (pb-cli-llm-ingest-move).

/// Render a byte count as a human-readable size (KiB/MiB/GiB).
/// Used in the remove plan summary so operators see "5.2 GiB" instead
/// of `5582813696`.
pub fn human_bytes(bytes: u64) -> String {
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
