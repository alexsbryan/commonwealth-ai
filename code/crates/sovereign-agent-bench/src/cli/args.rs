// SPDX-License-Identifier: AGPL-3.0-or-later
//! Argument parsing for the `run` subcommand. Hand-rolled (vs clap
//! derive) to keep the surface contained — the CLI is small enough
//! that adding a clap derive macro would be more boilerplate than it
//! removes, and we already pass `&[String]` from the dispatcher.

use std::path::PathBuf;

use sovereign_agent_tools::{Role, RoleModelMap};
use thiserror::Error;

use crate::problem::Tier;

#[derive(Debug, Clone)]
pub struct RunArgs {
    pub bench_root: PathBuf,
    pub agent: String,
    pub model: String,
    pub problems: Option<Vec<String>>,
    pub judge_trials: u8,
    pub judge_model: Option<String>,
    pub judge_base_url: String,
    /// The OpenAI-compatible server the agent itself talks to. `None`
    /// keeps the old coupling (the agent uses `judge_base_url`), so a run
    /// that names neither still reaches the local daemon. Only runners that
    /// take an endpoint (pi, opencode) accept it; any other refuses the run
    /// rather than silently using its own hardcoded URL.
    pub agent_base_url: Option<String>,
    /// The context window the agent is told the model has. pi and
    /// opencode size their compaction against it, so it should match what
    /// the server serves per request.
    pub agent_context_window: u32,
    /// Skip the rubric judge. Judge-scored dims are recorded as not judged
    /// (hybrid dims keep their auto floor), never as a judged 0.
    pub no_judge: bool,
    pub token_cap_override: Option<u64>,
    pub wall_seconds_override: Option<u64>,
    pub update_baseline: bool,
    pub report_path: PathBuf,
    pub pi_binary: Option<String>,
    /// Pin the `opencode` binary (default: `opencode` on PATH).
    pub opencode_binary: Option<String>,
    /// Where to persist per-run artifacts (agent workdir copy, pi
    /// stderr, judge prompts + raw responses). When `None`, defaults
    /// to `<bench_root>/.artifacts/<utc-date>-<agent>-<model-slug>/`
    /// at run time. The intent is the operator never has to think
    /// about where the run's evidence went — it's always next to the
    /// bench data.
    pub artifacts_dir: Option<PathBuf>,
    /// Number of independent agent runs per problem. Default 1
    /// preserves single-shot semantics; N>1 wraps the agent → witness
    /// → judge pipeline in a loop and surfaces mean ± stdev so the
    /// operator can tell a stable score from a lucky/unlucky single
    /// trial. Distinct from `--judge-trials`, which only varies the
    /// judge inside one agent run.
    pub trials: u8,
    /// When Some, overrides the per-problem `Tier` for this whole
    /// run. `FromScratch` skips both `install_scaffold` and the
    /// `prompt.md` workdir copy — measures the agent's project-
    /// scaffolding capability separately from its algorithmic
    /// capability. None preserves each problem's declared tier.
    pub tier_override: Option<Tier>,
    /// Per-role model overrides. Empty (default) → every role uses
    /// `--model`, which is PR-2 behavior. Populated from
    /// `--planner-model` / `--implementer-model` /
    /// `--evaluator-model`. Honored only by role-aware runners
    /// (`--agent native`); the monolithic and pi runners ignore it.
    pub role_model_map: RoleModelMap,
}

#[derive(Debug, Error)]
pub enum ArgsError {
    #[error("unknown flag `{0}` (try --help)")]
    UnknownFlag(String),
    #[error("flag `{0}` requires a value")]
    MissingValue(String),
    #[error("flag `{0}` value `{1}` is not a number")]
    BadNumber(String, String),
    #[error("flag `--tier` expects `scaffolded` or `from-scratch`, got `{0}`")]
    BadTier(String),
}

impl RunArgs {
    pub fn parse(argv: &[String]) -> Result<Self, ArgsError> {
        let mut bench_root = PathBuf::from("bench/lanes/agent-coding");
        let mut agent = "pi".to_string();
        let mut model = "commonwealth/coder".to_string();
        let mut problems: Option<Vec<String>> = None;
        let mut judge_trials: u8 = 3;
        let mut judge_model: Option<String> = None;
        let mut judge_base_url = "http://localhost:9741/v1".to_string();
        let mut agent_base_url: Option<String> = None;
        let mut agent_context_window: u32 = 32_768;
        let mut no_judge = false;
        let mut token_cap_override: Option<u64> = None;
        let mut wall_seconds_override: Option<u64> = None;
        let mut update_baseline = false;
        let mut report_path = PathBuf::from("agent-bench-report.json");
        let mut pi_binary: Option<String> = None;
        let mut opencode_binary: Option<String> = None;
        let mut artifacts_dir: Option<PathBuf> = None;
        let mut trials: u8 = 1;
        let mut tier_override: Option<Tier> = None;
        let mut role_model_map = RoleModelMap::new();

        let mut i = 0;
        while i < argv.len() {
            let a = argv[i].as_str();
            match a {
                "--bench-root" => {
                    bench_root = require_value("--bench-root", argv, &mut i)?.into();
                }
                "--agent" => {
                    agent = require_value("--agent", argv, &mut i)?;
                }
                "--model" => {
                    model = require_value("--model", argv, &mut i)?;
                }
                "--problems" => {
                    let v = require_value("--problems", argv, &mut i)?;
                    problems = Some(
                        v.split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect(),
                    );
                }
                "--judge-trials" => {
                    let v = require_value("--judge-trials", argv, &mut i)?;
                    judge_trials = v
                        .parse()
                        .map_err(|_| ArgsError::BadNumber("--judge-trials".into(), v.clone()))?;
                }
                "--judge-model" => {
                    judge_model = Some(require_value("--judge-model", argv, &mut i)?);
                }
                "--judge-base-url" => {
                    judge_base_url = require_value("--judge-base-url", argv, &mut i)?;
                }
                "--agent-base-url" => {
                    agent_base_url = Some(require_value("--agent-base-url", argv, &mut i)?);
                }
                "--agent-context-window" => {
                    let v = require_value("--agent-context-window", argv, &mut i)?;
                    agent_context_window = v.parse().map_err(|_| {
                        ArgsError::BadNumber("--agent-context-window".into(), v.clone())
                    })?;
                }
                "--no-judge" => {
                    no_judge = true;
                    i += 1;
                    continue;
                }
                "--token-cap-override" => {
                    let v = require_value("--token-cap-override", argv, &mut i)?;
                    token_cap_override = Some(v.parse().map_err(|_| {
                        ArgsError::BadNumber("--token-cap-override".into(), v.clone())
                    })?);
                }
                "--wall-seconds-override" => {
                    let v = require_value("--wall-seconds-override", argv, &mut i)?;
                    wall_seconds_override = Some(v.parse().map_err(|_| {
                        ArgsError::BadNumber("--wall-seconds-override".into(), v.clone())
                    })?);
                }
                "--update-baseline" => {
                    update_baseline = true;
                    i += 1;
                    continue;
                }
                "--report" => {
                    report_path = require_value("--report", argv, &mut i)?.into();
                }
                "--pi-binary" => {
                    pi_binary = Some(require_value("--pi-binary", argv, &mut i)?);
                }
                "--opencode-binary" => {
                    opencode_binary = Some(require_value("--opencode-binary", argv, &mut i)?);
                }
                "--artifacts-dir" => {
                    artifacts_dir = Some(require_value("--artifacts-dir", argv, &mut i)?.into());
                }
                "--trials" => {
                    let v = require_value("--trials", argv, &mut i)?;
                    trials = v
                        .parse()
                        .map_err(|_| ArgsError::BadNumber("--trials".into(), v.clone()))?;
                    if trials == 0 {
                        trials = 1;
                    }
                }
                "--tier" => {
                    let v = require_value("--tier", argv, &mut i)?;
                    tier_override = Some(parse_tier(&v)?);
                }
                "--planner-model" => {
                    let v = require_value("--planner-model", argv, &mut i)?;
                    role_model_map.set(Role::Planner, Some(v));
                }
                "--implementer-model" => {
                    let v = require_value("--implementer-model", argv, &mut i)?;
                    role_model_map.set(Role::Implementer, Some(v));
                }
                "--evaluator-model" => {
                    let v = require_value("--evaluator-model", argv, &mut i)?;
                    role_model_map.set(Role::Evaluator, Some(v));
                }
                "-h" | "--help" => {
                    return Err(ArgsError::UnknownFlag("--help".into()));
                }
                other => {
                    return Err(ArgsError::UnknownFlag(other.to_string()));
                }
            }
            i += 1;
        }
        Ok(Self {
            bench_root,
            agent,
            model,
            problems,
            judge_trials,
            judge_model,
            judge_base_url,
            agent_base_url,
            agent_context_window,
            no_judge,
            token_cap_override,
            wall_seconds_override,
            update_baseline,
            report_path,
            pi_binary,
            opencode_binary,
            artifacts_dir,
            trials,
            tier_override,
            role_model_map,
        })
    }
}

fn require_value(flag: &str, argv: &[String], i: &mut usize) -> Result<String, ArgsError> {
    if *i + 1 >= argv.len() {
        return Err(ArgsError::MissingValue(flag.to_string()));
    }
    *i += 1;
    Ok(argv[*i].clone())
}

fn parse_tier(v: &str) -> Result<Tier, ArgsError> {
    // Accept the canonical TOML names (Scaffolded/FromScratch) plus
    // the kebab-case variants an operator types on the command line.
    match v.to_ascii_lowercase().as_str() {
        "scaffolded" => Ok(Tier::Scaffolded),
        "from-scratch" | "fromscratch" | "from_scratch" => Ok(Tier::FromScratch),
        _ => Err(ArgsError::BadTier(v.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parse_defaults() {
        let r = RunArgs::parse(&[]).unwrap();
        assert_eq!(r.agent, "pi");
        assert_eq!(r.model, "commonwealth/coder");
        assert_eq!(r.judge_trials, 3);
        assert!(!r.update_baseline);
        assert_eq!(r.trials, 1);
    }

    #[test]
    fn parse_agent_endpoint_flags() {
        let r = RunArgs::parse(&argv(&[
            "--agent-base-url",
            "http://127.0.0.1:18180/v1",
            "--agent-context-window",
            "65536",
            "--no-judge",
        ]))
        .unwrap();
        assert_eq!(
            r.agent_base_url.as_deref(),
            Some("http://127.0.0.1:18180/v1")
        );
        assert_eq!(r.agent_context_window, 65_536);
        assert!(r.no_judge);
        let d = RunArgs::parse(&[]).unwrap();
        assert_eq!(d.agent_base_url, None);
        assert!(!d.no_judge);
    }

    #[test]
    fn parse_trials_flag() {
        let r = RunArgs::parse(&argv(&["--trials", "5"])).unwrap();
        assert_eq!(r.trials, 5);
    }

    #[test]
    fn parse_trials_zero_clamps_to_one() {
        let r = RunArgs::parse(&argv(&["--trials", "0"])).unwrap();
        assert_eq!(r.trials, 1);
    }

    #[test]
    fn parse_tier_from_scratch_kebab() {
        let r = RunArgs::parse(&argv(&["--tier", "from-scratch"])).unwrap();
        assert_eq!(r.tier_override, Some(Tier::FromScratch));
    }

    #[test]
    fn parse_tier_scaffolded() {
        let r = RunArgs::parse(&argv(&["--tier", "scaffolded"])).unwrap();
        assert_eq!(r.tier_override, Some(Tier::Scaffolded));
    }

    #[test]
    fn parse_tier_default_is_none() {
        let r = RunArgs::parse(&[]).unwrap();
        assert!(r.tier_override.is_none());
    }

    #[test]
    fn parse_tier_bad_value_errors() {
        let err = RunArgs::parse(&argv(&["--tier", "nope"])).unwrap_err();
        assert!(matches!(err, ArgsError::BadTier(_)));
    }

    #[test]
    fn parse_problems_csv() {
        let r = RunArgs::parse(&argv(&["--problems", "1.1, 3.2 , 2.1"])).unwrap();
        assert_eq!(
            r.problems,
            Some(vec!["1.1".into(), "3.2".into(), "2.1".into()])
        );
    }

    #[test]
    fn parse_token_cap_override() {
        let r = RunArgs::parse(&argv(&["--token-cap-override", "32000"])).unwrap();
        assert_eq!(r.token_cap_override, Some(32_000));
    }

    #[test]
    fn parse_update_baseline_flag() {
        let r = RunArgs::parse(&argv(&["--update-baseline"])).unwrap();
        assert!(r.update_baseline);
    }

    #[test]
    fn parse_unknown_flag_errors() {
        let err = RunArgs::parse(&argv(&["--what"])).unwrap_err();
        assert!(matches!(err, ArgsError::UnknownFlag(_)));
    }

    #[test]
    fn parse_missing_value_errors() {
        let err = RunArgs::parse(&argv(&["--agent"])).unwrap_err();
        assert!(matches!(err, ArgsError::MissingValue(_)));
    }

    #[test]
    fn parse_bad_number_errors() {
        let err = RunArgs::parse(&argv(&["--judge-trials", "many"])).unwrap_err();
        assert!(matches!(err, ArgsError::BadNumber(_, _)));
    }

    #[test]
    fn parse_default_role_model_map_is_empty() {
        let r = RunArgs::parse(&[]).unwrap();
        assert!(r.role_model_map.is_empty());
    }

    #[test]
    fn parse_single_role_override() {
        let r = RunArgs::parse(&argv(&["--implementer-model", "commonwealth/primary"])).unwrap();
        assert!(!r.role_model_map.is_empty());
        assert_eq!(
            r.role_model_map.get(Role::Implementer),
            Some("commonwealth/primary")
        );
        assert_eq!(r.role_model_map.get(Role::Planner), None);
        assert_eq!(r.role_model_map.get(Role::Evaluator), None);
    }

    #[test]
    fn parse_three_role_overrides_heterogeneous() {
        let r = RunArgs::parse(&argv(&[
            "--planner-model",
            "commonwealth/coder",
            "--implementer-model",
            "commonwealth/primary",
            "--evaluator-model",
            "commonwealth/coder",
        ]))
        .unwrap();
        assert_eq!(
            r.role_model_map.get(Role::Planner),
            Some("commonwealth/coder")
        );
        assert_eq!(
            r.role_model_map.get(Role::Implementer),
            Some("commonwealth/primary")
        );
        assert_eq!(
            r.role_model_map.get(Role::Evaluator),
            Some("commonwealth/coder")
        );
    }

    #[test]
    fn parse_role_override_missing_value_errors() {
        let err = RunArgs::parse(&argv(&["--planner-model"])).unwrap_err();
        assert!(matches!(err, ArgsError::MissingValue(_)));
    }
}
