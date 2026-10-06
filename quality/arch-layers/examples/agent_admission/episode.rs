// SPDX-License-Identifier: AGPL-3.0-or-later
use super::{core_read, fixture};
use serde_json::Value;
use std::path::Path;

pub struct EpisodeFixture {
    pub task: &'static str,
    pub policy: &'static str,
    pub packages: &'static [&'static str],
    pub registry_deps: &'static [&'static str],
    pub behavior_args: &'static [&'static str],
    pub test: &'static str,
    pub source_contract: &'static str,
    pub extension: Option<fn(&Path, &str, &[String], &str) -> Result<(), String>>,
    pub properties: Option<fn() -> Value>,
    pub baseline: Option<fn(&Path) -> Result<(), String>>,
    pub baseline_args: &'static [&'static str],
}

pub static FORMATTER: EpisodeFixture = EpisodeFixture {
    task: fixture::TASK,
    policy: fixture::POLICY,
    packages: &fixture::PACKAGES,
    registry_deps: &[],
    test: "task_returns_expected_answer",
    source_contract: "",
    behavior_args: &[
        "test",
        "--offline",
        "--locked",
        "--package",
        "ask-app",
        "--lib",
        "--",
        "--exact",
        "task_returns_expected_answer",
    ],
    extension: None,
    properties: None,
    baseline: None,
    baseline_args: &[],
};

pub static CORE_READ: EpisodeFixture = EpisodeFixture {
    task: core_read::TASK,
    policy: core_read::POLICY,
    packages: core_read::PACKAGES,
    registry_deps: &["async-trait", "serde"],
    test: core_read::TEST,
    source_contract: core_read::SOURCE_CONTRACT,
    behavior_args: &[
        "test",
        "--offline",
        "--locked",
        "--package",
        "core-probe",
        "--lib",
        "--",
        "--exact",
        "core_read_surface_compiles",
    ],
    extension: Some(core_read::write),
    properties: Some(core_read::properties),
    baseline: Some(core_read::write_baseline_probe),
    baseline_args: &[
        "test",
        "--offline",
        "--locked",
        "--package",
        "core-probe",
        "--lib",
        "--message-format=json",
        "--",
        "--exact",
        "core_read_surface_compiles",
    ],
};
