//! Red-phase task — generate a single failing test capturing a new
//! behavior. Sets the `GenerateOneFailing` polarity.

use crate::tasks::framework::detect_framework;
use crate::types::{Polarity, Trial, TrialConfig};
use crate::workdir::Workdir;

pub struct WriteFailingTestArgs {
    pub workdir: Workdir,
    pub model: String,
    /// User-facing description of the new behavior the model should
    /// write a test for.
    pub behavior: String,
    /// Optional hint at the path where the new test should land.
    /// When None, the framework adapter picks a path from the
    /// detected convention.
    pub test_file_hint: Option<String>,
    pub test_command: Option<String>,
    pub config: Option<TrialConfig>,
}

pub fn write_failing_test(args: WriteFailingTestArgs) -> Trial {
    let workdir_path = args.workdir.path();
    let test_command = args
        .test_command
        .unwrap_or_else(|| detect_framework(workdir_path).default_test_command().to_string());
    let path_hint = args
        .test_file_hint
        .as_deref()
        .map(|p| format!(" Suggested file path: `{p}`."))
        .unwrap_or_default();
    let prompt = format!(
        "Write a SINGLE failing test that captures this behavior:\n\n  {}\n\nThe test will be run against the unchanged code. It MUST fail with an assertion-style error to count as discriminating — if it passes, the test isn't testing the new behavior and will be rejected.{}",
        args.behavior, path_hint,
    );
    Trial {
        workdir: args.workdir,
        model: args.model,
        prompt,
        test_command,
        polarity: Polarity::GenerateOneFailing { test_name_hint: None },
        config: args.config.unwrap_or_default(),
        syntax_validator: None,
    }
}
