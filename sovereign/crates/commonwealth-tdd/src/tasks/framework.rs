// SPDX-License-Identifier: AGPL-3.0-or-later
//! Test-framework detection + per-framework defaults. Ported from
//! the old `red/framework.rs` because the auto-detected test
//! command is useful for every task type, not just Red.

use std::path::Path;

use crate::types::TrialConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framework {
    Pytest,
    Cargo,
    Vitest,
    Jest,
    GoTest,
    Playwright,
}

impl Framework {
    pub fn default_test_command(&self) -> &'static str {
        match self {
            Framework::Pytest => "pytest -q",
            Framework::Cargo => "cargo test --quiet",
            Framework::Vitest => "npx vitest run",
            Framework::Jest => "npx jest",
            Framework::GoTest => "go test -json ./...",
            // CI=1: standard scaffolds set `reuseExistingServer:
            // !process.env.CI` — without it, a dev server the USER
            // already has running would be silently reused and every
            // candidate would test the user's app instead of its own
            // scratch edit. --retries=0: retries mask flake and a
            // flaky pass would promote a junk edit. --workers=1: one
            // browser per run on a laptop already running inference.
            Framework::Playwright => {
                "CI=1 npx playwright test --reporter=line --retries=0 --workers=1"
            }
        }
    }
}

/// True when the workdir carries a Playwright config — used both by
/// detection and by the surface's "a unit framework is the default
/// but an e2e suite exists" note.
pub fn has_playwright_config(workdir: &Path) -> bool {
    workdir.join("playwright.config.ts").exists() || workdir.join("playwright.config.js").exists()
}

/// True when a test command runs Playwright — keys the browser-scale
/// trial profile off the command actually being run, so an explicit
/// `test_command` override gets the same profile as detection.
pub fn is_playwright_command(cmd: &str) -> bool {
    cmd.contains("playwright test")
}

/// Trial profile for a test command. Browser suites cost seconds to
/// minutes per run and each run may start a webServer on a fixed
/// port — so Playwright trials sample fewer candidates, allow 300s
/// per run, and run candidates serially (parallel candidates would
/// collide on the port, or worse, silently share one server and
/// test the wrong tree). Everything else keeps the validated
/// defaults.
pub fn trial_config_for_command(cmd: &str) -> TrialConfig {
    if is_playwright_command(cmd) {
        TrialConfig {
            candidates_per_round: 3,
            candidate_test_timeout: std::time::Duration::from_secs(300),
            serial_candidates: true,
            ..TrialConfig::default()
        }
    } else {
        TrialConfig::default()
    }
}

pub fn detect_framework(workdir: &Path) -> Framework {
    if workdir.join("pyproject.toml").exists()
        || workdir.join("pytest.ini").exists()
        || workdir.join("conftest.py").exists()
    {
        return Framework::Pytest;
    }
    if workdir.join("Cargo.toml").exists() {
        return Framework::Cargo;
    }
    if workdir.join("go.mod").exists() {
        return Framework::GoTest;
    }
    if let Ok(text) = std::fs::read_to_string(workdir.join("package.json")) {
        let lower = text.to_ascii_lowercase();
        if lower.contains("\"vitest\"") || workdir.join("vitest.config.ts").exists() {
            return Framework::Vitest;
        }
        if lower.contains("\"jest\"") || workdir.join("jest.config.js").exists() {
            return Framework::Jest;
        }
    }
    // After the unit frameworks on purpose: when a project has both,
    // unit stays the default and the caller steers to e2e explicitly
    // (spec SOLVE_PLAYWRIGHT — no guessing which suite a goal means).
    if has_playwright_config(workdir) {
        return Framework::Playwright;
    }
    let tests_dir = workdir.join("tests");
    if tests_dir.is_dir() {
        if let Ok(rd) = std::fs::read_dir(&tests_dir) {
            for entry in rd.flatten() {
                let n = entry.file_name();
                let s = n.to_string_lossy();
                if s.starts_with("test_") && s.ends_with(".py") {
                    return Framework::Pytest;
                }
            }
        }
    }
    Framework::Pytest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_cargo_from_cargo_toml() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        assert_eq!(detect_framework(tmp.path()), Framework::Cargo);
    }

    #[test]
    fn detect_pytest_from_conftest() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("conftest.py"), "").unwrap();
        assert_eq!(detect_framework(tmp.path()), Framework::Pytest);
    }

    #[test]
    fn detect_falls_back_to_pytest() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(detect_framework(tmp.path()), Framework::Pytest);
    }

    #[test]
    fn detect_playwright_from_config() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("playwright.config.ts"), "export default {}").unwrap();
        assert_eq!(detect_framework(tmp.path()), Framework::Playwright);
    }

    #[test]
    fn unit_framework_wins_over_playwright_when_both_present() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"devDependencies":{"vitest":"^2"}}"#,
        )
        .unwrap();
        std::fs::write(tmp.path().join("playwright.config.ts"), "export default {}").unwrap();
        assert_eq!(detect_framework(tmp.path()), Framework::Vitest);
        assert!(has_playwright_config(tmp.path()));
    }

    #[test]
    fn playwright_commands_get_the_browser_scale_profile() {
        let pw = trial_config_for_command("CI=1 npx playwright test --reporter=line");
        assert_eq!(pw.candidates_per_round, 3);
        assert!(pw.serial_candidates);
        assert_eq!(pw.candidate_test_timeout.as_secs(), 300);
        let unit = trial_config_for_command("pytest -q");
        assert!(!unit.serial_candidates);
        assert_eq!(unit.candidates_per_round, TrialConfig::default().candidates_per_round);
    }
}
