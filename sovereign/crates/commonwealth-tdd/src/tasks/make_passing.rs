//! Default Green-equivalent task — drive currently-failing tests
//! to passing.

use std::path::Path;

use crate::tasks::framework::detect_framework;
use crate::types::{Polarity, Trial, TrialConfig};
use crate::workdir::Workdir;

pub struct MakePassingArgs {
    pub workdir: Workdir,
    pub model: String,
    /// Optional caller intent ("make failing tests pass" by default).
    /// Useful when the user has a specific goal in mind ("get the
    /// algorithm right", "split this file") and wants the model
    /// to know what we're aiming at beyond "pass tests."
    pub task: Option<String>,
    /// Override the test command. When None, auto-detect from the
    /// workdir's framework markers.
    pub test_command: Option<String>,
    pub config: Option<TrialConfig>,
}

pub fn make_failing_tests_pass(args: MakePassingArgs) -> Trial {
    let workdir_path: &Path = args.workdir.path();
    let test_command = args
        .test_command
        .unwrap_or_else(|| detect_framework(workdir_path).default_test_command().to_string());
    let prompt = args
        .task
        .unwrap_or_else(|| "Make all currently-failing tests pass without regressing any currently-passing test.".to_string());
    Trial {
        workdir: args.workdir,
        model: args.model,
        prompt,
        test_command,
        polarity: Polarity::MaximizePassing,
        config: args.config.unwrap_or_default(),
        syntax_validator: None,
    }
}
