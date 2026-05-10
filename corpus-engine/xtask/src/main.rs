//! `cargo xtask` — corpus-engine maintenance commands.
//!
//! Usage:
//!   cargo xtask update-registry-snapshot   Fetch live registry and write snapshot

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");

    let exit_code = match cmd {
        "update-registry-snapshot" => cmd_update_registry_snapshot(),
        "help" | "--help" | "-h" => {
            print_usage();
            0
        }
        other => {
            eprintln!("Unknown xtask command: {other}");
            print_usage();
            1
        }
    };

    std::process::exit(exit_code);
}

// ── update-registry-snapshot ─────────────────────────────────────────────────

fn cmd_update_registry_snapshot() -> i32 {
    // Locate the snapshot file relative to CARGO_MANIFEST_DIR of xtask,
    // which is corpus-engine/xtask/. The snapshot is one level up.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let snapshot_path = std::path::Path::new(manifest_dir)
        .parent()
        .expect("xtask has no parent dir")
        .join("registry_snapshot.toml");

    eprintln!("Reading bundled snapshot: {}", snapshot_path.display());

    let current_text = match std::fs::read_to_string(&snapshot_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: failed to read snapshot: {e}");
            return 1;
        }
    };

    // Parse current snapshot to get registry_url.
    let current: toml::Value = match toml::from_str(&current_text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: failed to parse snapshot: {e}");
            return 1;
        }
    };

    let registry_url = current
        .get("registry_url")
        .and_then(|v| v.as_str())
        .unwrap_or(
            "https://raw.githubusercontent.com/alexsbryan/sovereign-recipes/main/registry.toml",
        );

    eprintln!("Fetching live registry from: {registry_url}");

    let live_text = match reqwest::blocking::get(registry_url) {
        Ok(resp) => {
            if !resp.status().is_success() {
                eprintln!("error: HTTP {} fetching registry", resp.status());
                return 1;
            }
            match resp.text() {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("error: failed to read response body: {e}");
                    return 1;
                }
            }
        }
        Err(e) => {
            eprintln!("error: failed to fetch registry: {e}");
            eprintln!();
            eprintln!(
                "If the public repo does not exist yet, you can skip this step.\n\
                 The bundled snapshot is the source of truth until the repo is live."
            );
            return 1;
        }
    };

    // Parse live registry.
    let live: toml::Value = match toml::from_str(&live_text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: failed to parse live registry: {e}");
            return 1;
        }
    };

    // Validate schema_version compatibility.
    let live_version = live
        .get("schema_version")
        .and_then(|v| v.as_integer())
        .unwrap_or(1);
    let current_version = current
        .get("schema_version")
        .and_then(|v| v.as_integer())
        .unwrap_or(1);

    if live_version > current_version {
        eprintln!(
            "warning: live registry schema_version ({live_version}) is newer than \
             bundled ({current_version}). Xtask update may need to be updated too."
        );
    }

    // Summarize changes.
    let live_entries = live
        .get("recipes")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let current_entries = current
        .get("recipes")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    if live_entries != current_entries {
        eprintln!(
            "  entries: {} → {} ({}{})",
            current_entries,
            live_entries,
            if live_entries > current_entries { "+" } else { "" },
            live_entries as i64 - current_entries as i64,
        );
    } else {
        eprintln!("  entries: {live_entries} (unchanged)");
    }

    // Write updated snapshot (preserving header comments is not possible via toml crate,
    // so prepend a standard header).
    let new_snapshot = format!(
        "# Registry snapshot — bundled at compile time.\n\
         # This file is the ONLY compile-time corpus catalog artifact.\n\
         # Keep up to date by running: cargo xtask update-registry-snapshot\n\
         #\n\
         {live_text}"
    );

    match std::fs::write(&snapshot_path, &new_snapshot) {
        Ok(()) => {
            eprintln!("Updated: {}", snapshot_path.display());
            0
        }
        Err(e) => {
            eprintln!("error: failed to write snapshot: {e}");
            1
        }
    }
}

fn print_usage() {
    eprintln!("Usage: cargo xtask <command>");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  update-registry-snapshot   Fetch live registry.toml and update the bundled snapshot");
}
