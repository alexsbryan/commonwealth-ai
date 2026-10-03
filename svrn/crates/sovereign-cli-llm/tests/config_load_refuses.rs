// SPDX-License-Identifier: AGPL-3.0-or-later
//! A daemon-dialling verb run with a `config.toml` that exists and does not
//! load refuses, naming the path, and never resolves the default port
//! (pc-cli-config-load-silent-default-base). `client_daemon_base` used to turn
//! that load error into :9741, the operator's node. A missing file keeps the
//! default. `atlas typed-extension` resolves its endpoint while parsing its
//! flags, before it opens anything, so nothing here dials.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-cli-llm");

/// The verb under a sandbox whose every config root is `root`.
fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join(".config"))
        .env("SVRNMESH_DATA_DIR", root.join("data"))
        .env_remove("SOVEREIGN_DATA_DIR")
        .env_remove("SOVEREIGN_DAEMON_URL")
        .env_remove("SVRNMESH_DAEMON_URL")
        .output()
        .expect("spawn sovereign-cli-llm")
}

fn write_config(root: &Path, body: &str) -> std::path::PathBuf {
    let dir = root.join("data");
    std::fs::create_dir_all(&dir).expect("data dir");
    let path = dir.join("config.toml");
    std::fs::write(&path, body).expect("write config");
    path
}

#[test]
fn present_but_bad_config_refuses_naming_the_path() {
    // The incident's shape (parses, fails validate_class), then an unparsable one.
    for body in ["[daemon]\nclient_port = 19751\n", "[daemon\nclient_port = "] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = write_config(tmp.path(), body);
        let out = run(tmp.path(), &["atlas", "typed-extension", "some-corpus"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{body:?} exited 0: {stderr}");
        assert!(
            stderr.contains(&format!("{} does not load", path.display())),
            "{body:?} did not name the config: {stderr}"
        );
        assert!(
            !stderr.contains("localhost:9741"),
            "{body:?} fell back to the default port: {stderr}"
        );
    }
}

/// The other direction: no config is a first run, and the endpoint is the
/// compiled default, the one `--help` states.
#[test]
fn missing_config_resolves_the_default() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = run(tmp.path(), &["atlas", "typed-extension", "--help"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("Default: http://localhost:9741/v1"),
        "{stdout}"
    );
    let out = run(tmp.path(), &["atlas", "typed-extension", "some-corpus"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("does not load"), "{stderr}");
}
