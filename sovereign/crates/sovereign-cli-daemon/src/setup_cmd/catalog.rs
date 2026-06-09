// SPDX-License-Identifier: AGPL-3.0-or-later
//! Primary-model catalog picker — extracted from `setup_cmd` (§3.2).
//! Renders the numbered picker (with the `[b]` BYOM branch) into a
//! `super::Pick`.

use std::io::{self, BufRead as _, Write as _};

use sovereign_core::models_manifest::SlotConfig;
use sovereign_inference::setup_planner::PrimaryOption;

use super::Pick;

/// Render the numbered picker, handle the `[b]` BYOM branch, and return
/// the chosen slot. In `--yes` mode, auto-picks the recommended row.
pub(super) fn pick_primary(catalog: &[PrimaryOption], yes: bool) -> Pick {
    println!("  Pick your main responder:");
    println!();
    println!("    #   Model                          Size     Notes");
    for (i, opt) in catalog.iter().enumerate() {
        let tag = if opt.recommended {
            "← recommended"
        } else {
            ""
        };
        println!(
            "    {}   {:30}  {:>5.1} GB {tag}",
            i + 1,
            display_name(&opt.slot),
            opt.size_gb,
        );
    }
    println!();
    println!("    [b] Bring my own GGUF files");
    println!();

    if yes {
        let rec = catalog
            .iter()
            .find(|o| o.recommended)
            .or_else(|| catalog.first());
        return match rec {
            Some(o) => Pick::Slot(o.slot.clone()),
            None => Pick::Abort,
        };
    }

    loop {
        eprint!("  \u{276f} ");
        io::stderr().flush().ok();
        let mut line = String::new();
        if io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
            return Pick::Abort;
        }
        let trimmed = line.trim().to_lowercase();

        if trimmed.is_empty() {
            // Enter = recommended
            if let Some(o) = catalog.iter().find(|o| o.recommended) {
                return Pick::Slot(o.slot.clone());
            }
            return Pick::Slot(catalog[0].slot.clone());
        }
        if trimmed == "b" {
            return Pick::Byom;
        }
        if let Ok(n) = trimmed.parse::<usize>() {
            if n >= 1 && n <= catalog.len() {
                return Pick::Slot(catalog[n - 1].slot.clone());
            }
        }
        eprintln!(
            "  (Enter a number 1..{}, 'b', or press enter for recommended.)",
            catalog.len()
        );
    }
}

pub(super) fn display_name(slot: &SlotConfig) -> String {
    if !slot.base_name.is_empty() {
        format!("{} {}", slot.base_name, slot.quant)
    } else {
        slot.file.trim_end_matches(".gguf").to_string()
    }
}
