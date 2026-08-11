// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn path` — print the per-user directories the toolchain resolves,
//! straight from the path SSOT (`sovereign_contracts::rebrand`).
//!
//! **Why this verb exists.** Shell scripts and docs used to hard-code
//! `~/.sovereign`, and the rebrand sweep turned those into `~/.svrnmesh`.
//! That rewrite is correct for the steady state but drops the *legacy
//! fallback* that the Rust getters still honour: on a machine that has a
//! populated `~/.sovereign` and no `~/.svrnmesh` yet, a script doing
//! `mkdir -p ~/.svrnmesh` will POPULATE the rebranded dir, and
//! `resolve_branded_dir` then prefers it — silently orphaning the real
//! data root (models, indexes, notes.db) with no error at all.
//!
//! The fix is not a second copy of the preference order in shell. It is
//! this verb: scripts ask the binary that owns the decision.
//!
//! ```sh
//! root="$(svrn path root)"
//! mkdir -p "$root"
//! ```
//!
//! Output is a bare path plus a newline — no banner, no decoration — so it
//! substitutes directly into a shell expansion. `--explain` adds the reason
//! on stderr, keeping stdout clean for the same substitution.

use sovereign_contracts::rebrand;

pub async fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }

    let explain = args.iter().any(|a| a == "--explain");
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();

    // Default to `root`: it is the one every script wants, and a bare
    // `svrn path` printing nothing would be a silent failure.
    let which = positional.first().map(|s| s.as_str()).unwrap_or("root");

    let (path, reason) = match which {
        // The per-user root. NOT override-sensitive — `SVRNMESH_DATA_DIR`
        // does not move it, which is deliberate: the daemon run lock and
        // projects.json live here and reader/writer must agree.
        "root" => {
            let (p, choice) = rebrand::svrnmesh_root_explained();
            (p, choice.reason().to_string())
        }
        // The data root, honouring SVRNMESH_DATA_DIR / SOVEREIGN_DATA_DIR.
        "data" => {
            let overridden = rebrand::svrnmesh_env("DATA_DIR").is_some();
            let reason = if overridden {
                "SVRNMESH_DATA_DIR / SOVEREIGN_DATA_DIR override is set".to_string()
            } else {
                let (_, choice) = rebrand::svrnmesh_root_explained();
                format!("no DATA_DIR override; {}", choice.reason())
            };
            (rebrand::data_dir(), reason)
        }
        // Platform-native dirs for the embedded mesh / GUI settings. These
        // are distinct from `data` on Linux and Windows.
        "mesh-data" => (
            rebrand::mesh_data_dir(),
            "platform data dir + brand".to_string(),
        ),
        "config" => (
            rebrand::mesh_config_dir(),
            "platform config dir + brand".to_string(),
        ),
        other => {
            eprintln!(
                "svrn path: unknown path `{other}` \
                 (expected one of: root, data, mesh-data, config)"
            );
            return 2;
        }
    };

    if explain {
        eprintln!("svrn path {which}: {reason}");
    }
    println!("{}", path.display());
    0
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "svrn path",
    summary: "Print a per-user directory as the toolchain resolves it.",
    sections: &[
        crate::util::help::HelpSection::Usage("svrn path [root|data|mesh-data|config] [--explain]"),
        crate::util::help::HelpSection::Subcommands(&[
            (
                "root",
                "Per-user root (~/.svrnmesh, or a populated legacy ~/.sovereign). The default.",
            ),
            (
                "data",
                "Data root — as `root`, but honours SVRNMESH_DATA_DIR / SOVEREIGN_DATA_DIR",
            ),
            (
                "mesh-data",
                "Platform-native data dir for the embedded mesh's shared storage",
            ),
            (
                "config",
                "Platform-native config dir for GUI-owned settings (desktop.toml)",
            ),
        ]),
        crate::util::help::HelpSection::Notes(
            "Prints a bare path to stdout so it substitutes directly: \
             `root=\"$(svrn path root)\"`. Scripts MUST use this rather than \
             hard-coding ~/.svrnmesh — creating that directory by hand on a \
             not-yet-migrated machine orphans the real data root. \
             --explain writes the reason to stderr, leaving stdout clean.",
        ),
    ],
};
