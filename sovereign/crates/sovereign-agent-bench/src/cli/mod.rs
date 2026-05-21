//! `sovereign agent-bench` subcommand dispatch.

pub mod args;
pub mod run;

/// Public entrypoint. Dispatched from `sovereign-cli/src/main.rs` and
/// from the standalone `sovereign-agent-bench` binary.
pub async fn run_agent_bench(args: &[String]) -> u8 {
    init_tracing();
    let sub = args.first().map(String::as_str).unwrap_or("help");
    match sub {
        "run" => match run::run_command(&args[1..]).await {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("agent-bench run failed: {e}");
                1
            }
        },
        "list" => match run::list_problems(&args[1..]) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("agent-bench list failed: {e}");
                1
            }
        },
        "show" => match run::show_problem(&args[1..]) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("agent-bench show failed: {e}");
                1
            }
        },
        "help" | "-h" | "--help" => {
            eprintln!("{}", help_text());
            0
        }
        other => {
            eprintln!("agent-bench: unknown subcommand `{other}`\n");
            eprintln!("{}", help_text());
            2
        }
    }
}

fn init_tracing() {
    // Only attach if the parent process didn't already install a
    // subscriber. Failure is intentional and silent — the standalone
    // binary installs; the sovereign-cli dispatcher will already have.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("sovereign_agent_bench=info")),
        )
        .try_init();
}

fn help_text() -> String {
    r#"sovereign agent-bench <subcommand>

Subcommands:
  run         Run one or more problems against an agent. See `run --help`.
  list        List available problems (id, category, language).
  show <id>   Print the problem.toml + prompt.md for a problem.
  help        Show this help.

Common flags for `run`:
  --bench-root <path>          (default: ./sovereign/bench/agent-coding)
  --agent <id>                 (default: pi; from AgentRunnerRegistry)
  --model <handle>             (default: commonwealth/coder)
  --problems <ids>             comma-separated; default: all
  --judge-trials N             (default: 3)
  --judge-model <handle>       (default: same as --model)
  --judge-base-url <url>       (default: http://localhost:9741/v1)
  --token-cap-override N       (override budget.token_cap)
  --wall-seconds-override N    (override budget.wall_seconds_cap)
  --update-baseline            write baselines/agent-coding/<dated>.json + retarget latest.json
  --report <path>              (default: ./agent-bench-report.json)
"#
    .to_string()
}
