// SPDX-License-Identifier: AGPL-3.0-or-later
//! A mesh verb run with a `config.toml` that exists and does not load refuses,
//! naming the path, and never dials the default ports (pc-cli-config-load-
//! silent-default). The incident: a sandbox whose one-line `[daemon]` config
//! failed `validate_class` had its `mesh join` land on the operator's node.
//! A missing file keeping the defaults is `SetupConfig::load_present`'s own
//! test in sovereign-contracts; this file drives the binary.
//!
//! Only read verbs run here: on a regression they would reach the developer's
//! node, and a read is the only thing that may.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-cli-mesh");

/// The verb under a sandbox whose every config root is `root`, so neither the
/// config read nor the legacy-path migration can touch the developer's.
fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join(".config"))
        .env("SVRNMESH_DATA_DIR", root.join("data"))
        .env_remove("SOVEREIGN_DATA_DIR")
        .env_remove("SVRNMESH_DAEMON_URL")
        .output()
        .expect("spawn sovereign-cli-mesh")
}

/// A loopback port nothing listens on.
fn dead_port() -> u16 {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    l.local_addr().expect("addr").port()
}

fn write_config(root: &Path, body: &str) -> std::path::PathBuf {
    let dir = root.join("data");
    std::fs::create_dir_all(&dir).expect("data dir");
    let path = dir.join("config.toml");
    std::fs::write(&path, body).expect("write config");
    path
}

#[test]
fn present_but_invalid_config_refuses_naming_the_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let rails = format!("http://127.0.0.1:{}", dead_port());
    // The incident's shape: it parses, and validate_class refuses it.
    let path = write_config(
        tmp.path(),
        &format!("[daemon]\nclient_port = 19751\nrails_base = \"{rails}\"\n"),
    );
    for verb in [
        &["mesh", "status"][..],
        &["mesh", "list"],
        &["mesh", "transport"],
    ] {
        let out = run(tmp.path(), verb);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{verb:?} exited 0: {stderr}");
        assert!(
            stderr.contains(&format!("{} does not load", path.display())),
            "{verb:?} did not name the config: {stderr}"
        );
        assert!(
            !stderr.contains("9747") && !stderr.contains("9741"),
            "{verb:?} fell back to a default port: {stderr}"
        );
    }
}

#[test]
fn present_but_unparsable_config_refuses() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = write_config(tmp.path(), "[daemon\nclient_port = ");
    let out = run(tmp.path(), &["mesh", "status"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "exited 0: {stderr}");
    assert!(
        stderr.contains(&format!("{} does not load", path.display())),
        "{stderr}"
    );
}

/// The other direction: a config that loads is dialled as written, so the
/// refusal above is about loading, not about having a config at all.
#[test]
fn loadable_config_dials_its_own_rails_base() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let rails = format!("http://127.0.0.1:{}", dead_port());
    write_config(
        tmp.path(),
        &format!(
            "[daemon]\nclient_port = 19751\nrails_base = \"{rails}\"\n\
             [models]\nprimary = \"/m/p.gguf\"\nfast = \"/m/f.gguf\"\nembed = \"/m/e.gguf\"\n"
        ),
    );
    let out = run(tmp.path(), &["mesh", "status"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "nothing listens there, yet exit 0: {stderr}"
    );
    assert!(
        stderr.contains(&rails),
        "did not dial the configured base: {stderr}"
    );
    assert!(!stderr.contains("does not load"), "{stderr}");
}
