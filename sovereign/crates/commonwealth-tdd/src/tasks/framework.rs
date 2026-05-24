//! Test-framework detection + per-framework defaults. Ported from
//! the old `red/framework.rs` because the auto-detected test
//! command is useful for every task type, not just Red.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framework {
    Pytest,
    Cargo,
    Vitest,
    Jest,
    GoTest,
}

impl Framework {
    pub fn default_test_command(&self) -> &'static str {
        match self {
            Framework::Pytest => "pytest -q",
            Framework::Cargo => "cargo test --quiet",
            Framework::Vitest => "npx vitest run",
            Framework::Jest => "npx jest",
            Framework::GoTest => "go test -json ./...",
        }
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
}
