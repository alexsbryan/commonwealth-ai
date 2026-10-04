// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn ring roster add` writes the roster where cw-rails reads it: under
//! `$CW_RAILS_DIR`, never svrn's data dir. The journals moved to cw-rails'
//! root at the handover (rail_migration.rs), and the CLI kept opening
//! `sovereign_root()`, so every add since wrote a file cw-rails never read.
//! Seen 2026-10-04 on a throwaway node: a key outside the mesh was "added",
//! cw-rails' `/v1/rail/roster` still answered its derived roster, and the
//! add's own read-back exited 1.
//!
//! The daemon port is a dead loopback port, so the read-back reports the
//! daemon unreachable rather than reaching the developer's node.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-cli-mesh");

/// A loopback port nothing listens on.
fn dead_port() -> u16 {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    l.local_addr().expect("addr").port()
}

fn roster_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().is_some_and(|n| n == "roster.json") {
                out.push(p.display().to_string());
            }
        }
    }
    out
}

#[test]
fn roster_add_writes_under_cw_rails_root_not_svrns() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let rails = tmp.path().join("rails");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&rails).expect("rails dir");
    std::fs::write(
        data.join("config.toml"),
        format!(
            "[daemon]\nclient_port = {}\nrails_base = \"http://127.0.0.1:{}\"\n\
             [models]\nprimary = \"/m/p.gguf\"\nfast = \"/m/f.gguf\"\nembed = \"/m/e.gguf\"\n",
            dead_port(),
            dead_port()
        ),
    )
    .expect("write config");
    let key = "ab".repeat(32);

    let out = Command::new(BIN)
        .args([
            "ring", "roster", "add", "stranger", "--key", &key, "--ring", "t",
        ])
        .env("HOME", tmp.path())
        .env("XDG_CONFIG_HOME", tmp.path().join(".config"))
        .env("SVRNMESH_DATA_DIR", &data)
        .env("CW_RAILS_DIR", &rails)
        .env_remove("SOVEREIGN_DATA_DIR")
        .env_remove("SVRNMESH_DAEMON_URL")
        .output()
        .expect("spawn sovereign-cli-mesh");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout: {stdout}\nstderr: {stderr}");

    let written = rails.join("rings/t/roster.json");
    let body = std::fs::read_to_string(&written).unwrap_or_else(|e| {
        panic!(
            "no roster at cw-rails' root ({}): {e}; roster files under the sandbox: {:?}",
            written.display(),
            roster_files(tmp.path())
        )
    });
    assert!(
        body.contains(&key),
        "the key is not in {}: {body}",
        written.display()
    );
    assert!(
        roster_files(&data).is_empty(),
        "a roster landed in svrn's data dir: {:?}",
        roster_files(&data)
    );
}
