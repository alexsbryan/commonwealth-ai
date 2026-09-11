// SPDX-License-Identifier: AGPL-3.0-or-later
//! `cw-rails` — the binary. Everything it does is in the library beside it,
//! so the tests and the instrument exercise the same code this runs.

use std::process::ExitCode;

use commonwealth_rails::cli;

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = match cli::Args::parse(std::env::args()) {
        Ok(a) => a,
        Err(why) => {
            eprintln!("cw-rails: {why}");
            eprintln!();
            eprintln!("{}", cli::USAGE);
            // Usage is a refusal, not an abstention: the command ran and said
            // no to what was typed.
            return ExitCode::from(1);
        }
    };

    // A host with no runtime is a precondition absent, not a refusal — the
    // daemon made no claim about the mesh at all.
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cw-rails: no tokio runtime on this host: {e}");
            return ExitCode::from(3);
        }
    };
    runtime.block_on(cli::main(args))
}
