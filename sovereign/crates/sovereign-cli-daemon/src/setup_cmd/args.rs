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
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--reset" => opts.reset = true,
            "--yes" | "-y" => opts.yes = true,
            "--repair" => opts.repair = true,
            "--wizard-only" => opts.wizard_only = true,
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
    Ok(opts)
}

const HELP: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn setup",
    summary: "First-run onboarding wizard: detect hardware, download models, write config. \
         Now an alias for `svrn daemon --setup-only`.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage(
            "svrn setup [--yes] [--reset] [--data-dir <path>]",
        ),
        sovereign_cli_shared::help::HelpSection::Flags(&[
            ("--yes, -y", "Non-interactive; accept recommended choices"),
            (
                "--reset",
                "Wipe config and re-run (uninstalls service first if present)",
            ),
            (
                "--data-dir <p>",
                "Override the default data root (~/.sovereign)",
            ),
            ("--help, -h", "Show this message"),
        ]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Writes config to ~/.sovereign/config.toml (alongside the rest of the user-scoped\n\
             sovereign state). Older installs that wrote to the XDG config dir are migrated\n\
             automatically on first load. Phase 4 split: this command no longer registers a\n\
             system service. To register the daemon with launchd/systemd so it survives logout,\n\
             run `svrn install-service` after the wizard completes. To start the daemon\n\
             once without registering it, run `svrn daemon`.",
        ),
    ],
};

pub(super) fn print_usage() {
    sovereign_cli_shared::help::print(&HELP);
}
