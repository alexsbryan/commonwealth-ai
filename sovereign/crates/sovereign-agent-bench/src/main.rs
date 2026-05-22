//! Thin binary entrypoint. Production callers reach this surface via
//! `sovereign agent-bench …`; this binary exists so the crate can be
//! built and tested standalone without dragging the full sovereign-cli.

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let code = sovereign_agent_bench::run_agent_bench(&args[1..]).await;
    ExitCode::from(code)
}
