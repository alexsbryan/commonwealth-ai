// SPDX-License-Identifier: AGPL-3.0-or-later
//! Installing the on-prem kit over main's leaves one daemon unit (ship gate
//! F7). Main shipped `firm-rag-daemon.service` and `firm-rag-server.service`
//! (18f783f44:sovereign/deploy/onprem/systemd/); the kit ships
//! `firm-rag.service`. This runs install.sh's own `retire-main-units` lines
//! in a sandbox unit directory holding both of main's units and fails on any
//! one left behind: left enabled, it binds the daemon's port beside the new
//! unit.

use std::process::Command;

#[path = "shared/repo_root.rs"]
mod repo_root;
use repo_root::repo_root;

const INSTALL: &str = "distributions/deploy/onprem/install.sh";
const MAIN_UNITS: [&str; 2] = ["firm-rag-daemon.service", "firm-rag-server.service"];

/// The lines between install.sh's `retire-main-units` markers.
fn retire_block(script: &str) -> String {
    let begin = "# retire-main-units: begin";
    let end = "# retire-main-units: end";
    let (_, rest) = script
        .split_once(begin)
        .unwrap_or_else(|| panic!("{INSTALL} has no `{begin}` marker"));
    let (block, _) = rest
        .split_once(end)
        .unwrap_or_else(|| panic!("{INSTALL} has no `{end}` marker"));
    block.to_string()
}

#[test]
fn install_retires_every_main_unit() {
    let script = std::fs::read_to_string(repo_root().join(INSTALL)).expect("read install.sh");
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("onprem_kit_upgrade_unit");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for unit in MAIN_UNITS {
        std::fs::write(dir.join(unit), "[Service]\n").unwrap();
    }
    std::fs::write(dir.join("firm-rag.service"), "[Service]\n").unwrap();

    let out = Command::new("bash")
        .arg("-euo")
        .arg("pipefail")
        .arg("-c")
        .arg(retire_block(&script))
        .env("NO_SYSTEMD", "1")
        .env("UNIT_DIR", &dir)
        .output()
        .expect("run bash");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "retire-main-units exited {}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    let left: Vec<&str> = MAIN_UNITS
        .iter()
        .copied()
        .filter(|u| dir.join(u).exists())
        .collect();
    assert!(
        left.is_empty(),
        "an upgrade over main leaves {left:?} beside firm-rag.service, both bound to the \
         daemon's port; {INSTALL}'s retire-main-units loop must name it. Output:\n{stdout}"
    );
    for unit in MAIN_UNITS {
        assert!(
            stdout.contains(&format!("retired {unit}")),
            "no `retired {unit}` line:\n{stdout}"
        );
    }
    assert!(
        dir.join("firm-rag.service").exists(),
        "the retirement removed the kit's own unit"
    );
}
