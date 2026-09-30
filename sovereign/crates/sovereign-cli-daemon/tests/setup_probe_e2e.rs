// SPDX-License-Identifier: AGPL-3.0-or-later
//! First-run setup plans and probes through the program that loads the model
//! (pb-distribution-setup; five-programs fp-25). This binary links no planner
//! and no hardware probe: `svrn setup` execs the loader, the stock binary
//! `daemon run` execs, on `--setup-probe`, on a fresh root with no config.
//!
//! Failing inputs: take the loader away and setup refuses naming it (the
//! third test); link the planner back into this crate and boundary-gate goes
//! red on `sovereign-cli-daemon -> sovereign-inference`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-cli-daemon");

/// `SOVEREIGN_DAEMON_BIN`, else `sovereign-stock` beside this binary. Absent
/// is a FAILURE naming the build, never a skip (five-programs-62).
fn loader() -> PathBuf {
    if let Some(p) = std::env::var_os("SOVEREIGN_DAEMON_BIN") {
        let p = PathBuf::from(p);
        assert!(
            p.is_file(),
            "SOVEREIGN_DAEMON_BIN names no file: {}",
            p.display()
        );
        return p;
    }
    let beside = Path::new(BIN).with_file_name("sovereign-stock");
    assert!(
        beside.is_file(),
        "{} is missing: build it with `cargo build -p sovereign-stock`",
        beside.display()
    );
    beside
}

/// A scratch dir beside the binaries, so a hard link of this binary works.
fn scratch() -> tempfile::TempDir {
    tempfile::tempdir_in(Path::new(BIN).parent().expect("binary dir")).expect("scratch dir")
}

/// `svrn setup <args>` from `bin`, on a fresh HOME with no config and no
/// operator root override, finding its loader through `loader`.
fn setup(bin: &Path, home: &Path, loader: Option<&Path>, args: &[&str]) -> Output {
    let mut cmd = Command::new(bin);
    cmd.arg("setup")
        .args(args)
        .current_dir(home)
        .env("HOME", home)
        .env("SOVEREIGN_QUIET_DEPRECATIONS", "1")
        .env("PATH", "/usr/bin:/bin")
        .env_remove("SVRNMESH_DATA_DIR")
        .env_remove("SOVEREIGN_DATA_DIR")
        .env_remove("SOVEREIGN_DAEMON_BIN");
    if let Some(l) = loader {
        cmd.env("SOVEREIGN_DAEMON_BIN", l);
    }
    cmd.output().expect("run sovereign-cli-daemon setup")
}

fn last_json_line(out: &Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| {
            panic!(
                "no JSON line on stdout:\n{stdout}\nstderr:\n{}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
    serde_json::from_str(line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"))
}

/// A file the loader's size-floor validation accepts: GGUF magic, then
/// sparse up to the slot's advertised size.
fn seed_gguf(path: &Path, size_gb: f64) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let f = std::fs::File::create(path).unwrap();
    std::io::Write::write_all(&mut &f, b"GGUF").unwrap();
    f.set_len((size_gb.max(0.01) * 1024.0 * 1024.0 * 1024.0) as u64)
        .unwrap();
}

/// `--plan --json` on a machine that was never set up: the loader detects
/// the hardware and answers the plan, which setup prints in the contracts
/// shape.
#[test]
fn a_fresh_root_plans_through_the_loader() {
    let home = scratch();
    let out = setup(
        Path::new(BIN),
        home.path(),
        Some(&loader()),
        &["--plan", "--json"],
    );
    assert!(
        out.status.success(),
        "setup --plan --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let plan = last_json_line(&out);
    assert!(plan["profile"].is_string(), "{plan}");
    assert!(
        !plan["catalog"].as_array().expect("catalog").is_empty(),
        "{plan}"
    );
    assert!(
        plan["hardware"]["system_ram_bytes"].as_u64().unwrap_or(0) > 0,
        "{plan}"
    );
    assert!(
        !home.path().join(".svrnmesh").join("config.toml").exists(),
        "a plan is a read: nothing is written"
    );
}

/// `--yes --json` on a fresh root: the loader plans, fetches (here: finds
/// the planned fast and embed files already valid, so nothing crosses the
/// network), and setup writes the config and ends on `done`.
#[test]
fn a_fresh_root_sets_up_through_the_loader() {
    let home = scratch();
    let loader = loader();
    let plan = last_json_line(&setup(
        Path::new(BIN),
        home.path(),
        Some(&loader),
        &["--plan", "--json"],
    ));
    let root = home.path().join("root");
    for kind in ["fast", "embed"] {
        let slot = &plan[kind];
        let file = slot["file"]
            .as_str()
            .unwrap_or_else(|| panic!("{kind}: {plan}"));
        seed_gguf(
            &root.join("models").join(file),
            slot["size_gb"].as_f64().unwrap_or(0.0),
        );
    }
    let primary = home.path().join("mine.gguf");
    seed_gguf(&primary, 0.01);

    let out = setup(
        Path::new(BIN),
        home.path(),
        Some(&loader),
        &[
            "--yes",
            "--json",
            "--primary",
            primary.to_str().unwrap(),
            "--data-dir",
            root.to_str().unwrap(),
        ],
    );
    let last = last_json_line(&out);
    assert!(
        out.status.success(),
        "setup --yes --json failed: {last}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(last["phase"], "done", "{last}");
    assert!(root.join("config.toml").is_file(), "the config was written");
}

/// With no loader anywhere setup can find, it refuses by name rather than
/// planning from a guess (principle 6). The binary runs from a directory of
/// its own, so nothing sits beside it and PATH holds no loader.
#[test]
fn without_the_loader_setup_refuses_naming_it() {
    let alone = scratch();
    let bin = alone.path().join("sovereign-cli-daemon");
    std::fs::hard_link(BIN, &bin)
        .or_else(|_| std::fs::copy(BIN, &bin).map(|_| ()))
        .expect("place the binary alone");
    let home = scratch();
    let out = setup(&bin, home.path(), None, &["--plan", "--json"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("sovereign-stock"), "{stderr}");
    assert!(stderr.contains("SOVEREIGN_DAEMON_BIN"), "{stderr}");
}
