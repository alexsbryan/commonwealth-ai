// SPDX-License-Identifier: AGPL-3.0-or-later
//! The wizard's join, end to end through the binaries (fp-cond2-c). Until
//! pb-distribution-svrn-lift-2 this was a sovereign-cli-daemon unit test that
//! drove `join` and `find_holders` in-process with this package's binary and
//! cw-rails as siblings, which a lifted svrn does not have.
//!
//! `svrn setup --terminal <link>` runs as an operator runs it: the
//! sovereign-cli-daemon binary execs `svrn mesh up` (sovereign-cli-mesh),
//! which brings cw-rails up on the DEFAULT base, then spawns this package's
//! binary as the admin-join child. The default base and ports are the host's
//! own, so the run happens in a private network namespace (`unshare -rn`, as
//! pod_worker_e2e does), and the boot unit `mesh up` installs goes to
//! stand-in `systemctl`/`loginctl` under a scratch HOME: the host's cw-rails,
//! daemon and user manager are never touched. Linux only.
//!
//! The entry is a model-less cw-rails mesh, so the wizard joins, sees the
//! founder through the child's venues and refuses with "the mesh has
//! members" (not "no peers appeared"); its exit stops the child and removes
//! the provisional config.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::Command;

/// `name` beside this package's binary, where a stock install puts it.
/// Absent is a FAILURE naming the build, never a skip (five-programs-62).
fn beside(name: &str, package: &str) -> PathBuf {
    let bin = Path::new(env!("CARGO_BIN_EXE_sovereign-stock")).with_file_name(name);
    assert!(
        bin.is_file(),
        "{} is missing: build it with `cargo build -p {package}`",
        bin.display()
    );
    bin
}

/// The run, inside the namespace. Each process this test starts is found by
/// the namespace it runs in, never by name, so nothing on the host is
/// signalled.
const SCRIPT: &str = r#"
ip link set lo up || exit 90
HOME="$F/home" CW_RAILS_DIR="$F/rails" "$RAILS" found Lab --name founder >/dev/null 2>"$F/found.err" || exit 91
HOME="$F/home" CW_RAILS_DIR="$F/rails" "$RAILS" run --listen 19747 --local-only >"$F/rails.log" 2>&1 &
link=""
for _ in $(seq 1 300); do
  link=$(curl -s --max-time 2 http://127.0.0.1:19747/v1/mesh/status | python3 -c "$JOIN_LINK" 2>/dev/null)
  [ -n "$link" ] && break
  sleep 0.2
done
curl -s --max-time 2 http://127.0.0.1:19747/v1/mesh/status >"$F/status.json"
if [ -n "$link" ]; then
  HOME="$J/home" CW_RAILS_DIR="$J/rails" SOVEREIGN_LOCAL_ONLY=1 PATH="$STUBS:/usr/bin:/bin" \
    SOVEREIGN_DAEMON_BIN="$STOCK" SOVEREIGN_CLI_MESH_BIN="$MESH" CW_RAILS_BIN="$RAILS" \
    timeout 180 "$CLI" setup --terminal "$link" --data-dir "$J/data" >"$J/stdout" 2>"$J/stderr" </dev/null
  echo "SETUP_EXIT=$?"
  curl -s --max-time 2 http://127.0.0.1:9747/v1/mesh/status >"$J/status.json"
fi
python3 -c "$NETNS_PROCS" >"$J/procs"
while read -r pid _; do [ "$pid" != "$$" ] && kill "$pid" 2>/dev/null; done <"$J/procs"
[ -n "$link" ] || exit 92
exit 0
"#;

/// The founder's invite from cw-rails' status.
const JOIN_LINK: &str = "import json,sys; print(json.load(sys.stdin).get('join_link') or '')";

/// `<pid> <cmdline>` for every process in this network namespace but itself.
const NETNS_PROCS: &str = r#"
import os
me, ns = os.getpid(), os.readlink('/proc/self/ns/net')
for p in os.listdir('/proc'):
    if not p.isdigit() or int(p) == me:
        continue
    try:
        if os.readlink(f'/proc/{p}/ns/net') != ns:
            continue
        cmd = open(f'/proc/{p}/cmdline', 'rb').read().replace(b'\0', b' ').decode()
    except OSError:
        continue
    print(p, cmd)
"#;

/// A recorded `systemctl` and a lingering `loginctl`, so `mesh up`'s boot
/// unit lands in the scratch HOME and reaches no user manager.
fn stand_ins(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let stubs = dir.join("stubs");
    std::fs::create_dir_all(&stubs).expect("stubs dir");
    let systemctl = format!(
        "#!/bin/sh\necho \"$@\" >> {}\nexit 0\n",
        dir.join("systemctl.log").display()
    );
    for (name, body) in [
        ("systemctl", systemctl.as_str()),
        ("loginctl", "#!/bin/sh\necho Linger=yes\n"),
    ] {
        let p = stubs.join(name);
        std::fs::write(&p, body).expect("stand-in");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    stubs
}

/// The `is_self` row's node id from a cw-rails status document.
fn self_id(status: &serde_json::Value) -> Option<serde_json::Value> {
    status["members"]
        .as_array()?
        .iter()
        .find(|r| r["is_self"] == true)
        .map(|r| r["node_id"].clone())
}

#[test]
fn the_wizard_joins_through_a_spawned_daemon_and_stops_it() {
    let cli = beside("sovereign-cli-daemon", "sovereign-cli-daemon");
    let mesh = beside("sovereign-cli-mesh", "sovereign-cli-mesh");
    let rails = beside("cw-rails", "commonwealth-rails");
    let root = tempfile::tempdir().expect("tempdir");
    let (fdir, jdir) = (root.path().join("founder"), root.path().join("joiner"));
    for d in [&fdir, &jdir] {
        std::fs::create_dir_all(d.join("home")).expect("home");
    }
    let stubs = stand_ins(root.path());

    let out = Command::new("unshare")
        .args(["-rn", "sh", "-c", SCRIPT])
        .env("F", &fdir)
        .env("J", &jdir)
        .env("CLI", &cli)
        .env("MESH", &mesh)
        .env("RAILS", &rails)
        .env("STOCK", env!("CARGO_BIN_EXE_sovereign-stock"))
        .env("STUBS", &stubs)
        .env("JOIN_LINK", JOIN_LINK)
        .env("NETNS_PROCS", NETNS_PROCS)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("SVRNMESH_DATA_DIR")
        .env_remove("SOVEREIGN_DATA_DIR")
        .output()
        .expect("unshare runs (util-linux)");
    let read = |p: PathBuf| std::fs::read_to_string(&p).unwrap_or_default();
    let (stdout, stderr) = (read(jdir.join("stdout")), read(jdir.join("stderr")));
    let ctx = format!(
        "script: {}\nwizard stdout:\n{stdout}\nwizard stderr:\n{stderr}\nfounder found.err: {}",
        String::from_utf8_lossy(&out.stdout),
        read(fdir.join("found.err"))
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "the namespace run failed\n{ctx}"
    );

    // The wizard joined the founder's mesh, saw it through the child's
    // venues, and refused a mesh with no model holder.
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("SETUP_EXIT=1"),
        "{ctx}"
    );
    assert!(
        stdout.contains("joined \"Lab\""),
        "the joined line names the founder's mesh\n{ctx}"
    );
    assert!(
        stderr.contains("the mesh has members"),
        "find_holders never saw the founder through /v1/mesh/venues\n{ctx}"
    );

    // The joiner's cw-rails holds the founder beside its own row.
    let status = |p: PathBuf| -> serde_json::Value {
        serde_json::from_str(&read(p)).unwrap_or(serde_json::Value::Null)
    };
    let founder = self_id(&status(fdir.join("status.json"))).expect("the founder's self row");
    let joiner = status(jdir.join("status.json"));
    assert!(
        joiner["members"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|r| r["node_id"] == founder && r["is_self"] != true)),
        "the founder is not on the joiner's roster: {joiner}"
    );

    // The wizard's exit stopped the child and removed the provisional config.
    let procs = std::fs::read_to_string(jdir.join("procs")).expect("the namespace scan ran");
    assert!(
        procs.contains("cw-rails"),
        "the namespace scan saw none of the processes the run started:\n{procs}"
    );
    assert!(
        !procs.contains("setup-join.toml"),
        "the join child outlived the wizard:\n{procs}"
    );
    assert!(
        !jdir.join("data").join("setup-join.toml").exists(),
        "provisional config left behind"
    );
}
