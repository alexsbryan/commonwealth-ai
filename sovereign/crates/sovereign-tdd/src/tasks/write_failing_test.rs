// SPDX-License-Identifier: AGPL-3.0-or-later
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
    let framework = detect_framework(workdir_path);
    let test_command = args
        .test_command
        .unwrap_or_else(|| framework.default_test_command().to_string());
    // Resolve the test file path: caller hint wins, otherwise the
    // framework's convention. The prompt then MANDATES the model
    // include the resolved path in its write_file action — without
    // this the apply layer defaults to discover_source_file which
    // finds the production source (calc.py, src/lib.rs) and the
    // model ends up rewriting that instead of adding a new test
    // file. GenerateOneFailing then rejects every candidate
    // because total_tests didn't increase. Bug surfaced by the
    // 2026-05-24 real-model BDD probe (synthesis stalled in 7s
    // because every candidate clobbered calc.py with test code).
    let resolved_test_path = args.test_file_hint.clone().unwrap_or_else(|| {
        // Crude framework-default test path naming. The framework
        // detector's default_test_path lives in the (deleted) red
        // module; for now use simple conventions inline.
        match framework {
            crate::tasks::framework::Framework::Pytest => "tests/test_new_behavior.py".into(),
            crate::tasks::framework::Framework::Cargo => "tests/new_behavior.rs".into(),
            crate::tasks::framework::Framework::Vitest
            | crate::tasks::framework::Framework::Jest => "tests/new_behavior.test.ts".into(),
            crate::tasks::framework::Framework::GoTest => "new_behavior_test.go".into(),
            crate::tasks::framework::Framework::Playwright => "tests/e2e/pin.spec.ts".into(),
        }
    });
    let prompt = format!(
        "Write a failing test that captures this behavior:\n\n  {behavior}\n\n**CRITICAL: Use `write_file` with an explicit `path` field set to `{path}`.** Otherwise your test code will overwrite the production source instead of adding a new test file, which will fail the fitness check.\n\nThe shape your action MUST take:\n\n```json\n{{\"action\": \"write_file\", \"path\": \"{path}\"}}\n```\n\nfollowed by the test source in a fenced code block.\n\nThe test file runs against the unchanged code. Every test in it must FAIL — one passing test rejects the whole file. Failing at import (the function does not exist yet) counts as failing. Use idiomatic {framework_name} test conventions.{framework_extra}",
        behavior = args.behavior,
        path = resolved_test_path,
        framework_name = match framework {
            crate::tasks::framework::Framework::Pytest => "pytest",
            crate::tasks::framework::Framework::Cargo => "cargo test",
            crate::tasks::framework::Framework::Vitest => "vitest",
            crate::tasks::framework::Framework::Jest => "jest",
            crate::tasks::framework::Framework::GoTest => "go test",
            crate::tasks::framework::Framework::Playwright => "Playwright (@playwright/test)",
        },
        framework_extra = match framework {
            crate::tasks::framework::Framework::Playwright =>
                " Start with page.goto('/'). Locate by role or text (getByRole, getByText). One focused expectation.",
            _ => "",
        },
    );
    let config = args
        .config
        .unwrap_or_else(|| crate::tasks::framework::trial_config_for_command(&test_command));
    Trial {
        workdir: args.workdir,
        model: args.model,
        prompt,
        test_command,
        polarity: Polarity::GenerateOneFailing {
            test_name_hint: None,
        },
        config,
        syntax_validator: None,
    }
}
