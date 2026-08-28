// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn setup` argument parsing + usage — extracted from
//! `setup_cmd` (§3.2). Still a hand-rolled loop; adopting
//! `sovereign_cli_shared::args` is a separate, opportunistic change.

use std::path::PathBuf;

use super::Opts;

pub(super) fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut opts = Opts {
        reset: false,
        yes: false,
        data_dir: None,
        repair: false,
        help: false,
        wizard_only: false,
        fim: false,
        quant: None,
        skip_editor: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--reset" => opts.reset = true,
            "--yes" | "-y" => opts.yes = true,
            "--repair" => opts.repair = true,
            "--wizard-only" => opts.wizard_only = true,
            "--fim" => opts.fim = true,
            "--skip-editor" => opts.skip_editor = true,
            "--quant" => {
                i += 1;
                let raw = args
                    .get(i)
                    .ok_or_else(|| format!("--quant needs a rung ({})", rung_list()))?;
                // Accept the display spellings users will copy off the
                // banner ("Q6_K") as well as the canonical lowercase
                // rung name — a case mismatch is not worth a failed
                // onboarding run.
                let normalized = raw.to_ascii_lowercase();
                if sovereign_inference::setup_planner::fim_slot_for_rung(&normalized).is_none() {
                    return Err(format!(
                        "unknown --quant '{raw}' (expected {})",
                        rung_list()
                    ));
                }
                opts.quant = Some(normalized);
            }
            "--data-dir" => {
                i += 1;
                opts.data_dir = Some(PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "--data-dir needs a path".to_string())?,
                ));
            }
            "--help" | "-h" => opts.help = true,
            other => return Err(format!("unknown flag '{other}'")),
        }
        i += 1;
    }
    if opts.quant.is_some() && !opts.fim {
        return Err("--quant only applies to --fim".to_string());
    }
    if opts.skip_editor && !opts.fim {
        return Err("--skip-editor only applies to --fim".to_string());
    }
    Ok(opts)
}

/// Human-readable rung vocabulary for error text, read off the same
/// ladder the resolver uses so the two can't drift.
fn rung_list() -> String {
    sovereign_inference::setup_planner::FIM_RUNGS
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(" | ")
}

const HELP: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn setup",
    summary: "First-run onboarding wizard: detect hardware, download models, write config. \
         Now an alias for `svrn daemon --setup-only`. `--fim` runs the inline-completion \
         onboarding instead.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage(
            "svrn setup [--yes] [--reset] [--data-dir <path>]\n\
             svrn setup --fim [--quant <rung>] [--yes] [--skip-editor]",
        ),
        sovereign_cli_shared::help::HelpSection::Flags(&[
            ("--yes, -y", "Non-interactive; accept recommended choices"),
            (
                "--reset",
                "Wipe config and re-run (uninstalls service first if present)",
            ),
            (
                "--data-dir <p>",
                "Override the default data root (~/.svrnmesh)",
            ),
            (
                "--fim",
                "Set this machine up for inline completion end-to-end: download Mellum2, \
                 write [models.fim], restart the daemon, verify a real completion, install \
                 the editor extension",
            ),
            (
                "--quant <rung>",
                "FIM model rung: sweep_1_5b | mxfp4_moe | q4_k_m | q6_k | q8_0 (default: picked from \
                 detected hardware). Requires --fim",
            ),
            (
                "--skip-editor",
                "With --fim: stop after the daemon is verified; don't touch the editor",
            ),
            ("--help, -h", "Show this message"),
        ]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Writes config to ~/.svrnmesh/config.toml (alongside the rest of the user-scoped\n\
             sovereign state). Older installs that wrote to the XDG config dir are migrated\n\
             automatically on first load. Phase 4 split: this command no longer registers a\n\
             system service. To register the daemon with launchd/systemd so it survives logout,\n\
             run `svrn install-service` after the wizard completes. To start the daemon\n\
             once without registering it, run `svrn daemon`.\n\
             \n\
             --fim writes [models.edit] ONLY. Your [models].primary and [models].fast are\n\
             left alone, so the machine ends up with four slots: primary, fast, embed and a\n\
             pinned editing model that serves BOTH editing lanes — ghost text and the\n\
             next-edit Tab queue. The default rung is 1.54 GB, small enough to sit beside\n\
             any chat model on any tier.\n\
             \n\
             This replaces LEAN MODE, which aliased [models].primary to the editing model so\n\
             one copy served chat and completions. That existed because the only rung was\n\
             Mellum2, whose smallest artifact is 7 GB — too much to pin beside a 16-20 GB\n\
             primary — and it cost the user their chat model. The Mellum2 rungs are still\n\
             addressable with --quant. The previous config is backed up to\n\
             config.toml.pre-fim either way.\n\
             \n\
             Needs an existing config: run `svrn setup` first to choose a chat model. --fim\n\
             prints a plan and asks before it mutates anything; --yes skips the prompt.",
        ),
    ],
};

pub(super) fn print_usage() {
    sovereign_cli_shared::help::print(&HELP);
}
