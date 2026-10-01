use super::*;
use host_kit::service::UnitState;

#[test]
fn the_unit_runs_the_bring_ups_binary_port_and_posture() {
    let text = unit_text(
        Path::new("/opt/cw rails/cw-rails"),
        &crate::rails_up::run_args(9747, true, false),
        "/usr/bin:/bin",
        Path::new("/home/a/.commonwealth-rails"),
    );
    assert!(
        text.contains(
            "ExecStart=\"/opt/cw rails/cw-rails\" \"run\" \"--listen\" \"9747\" \"--local-only\"\n"
        ),
        "one quoted word per argument, the bring-up's argv: {text}"
    );
    assert!(
        text.contains("Environment=\"PATH=/usr/bin:/bin\"\n"),
        "{text}"
    );
    assert!(
        text.contains("Environment=\"CW_RAILS_DIR=/home/a/.commonwealth-rails\"\n"),
        "the unit's cw-rails opens the root the bring-up resolved: {text}"
    );
    assert!(!text.contains('{'), "every placeholder filled: {text}");
}

#[test]
fn a_percent_or_quote_in_a_path_survives_the_unit_file() {
    let text = unit_text(
        Path::new("/opt/50%\"/cw-rails"),
        &crate::rails_up::run_args(9747, false, false),
        "/a%b",
        Path::new("/r"),
    );
    assert!(
        text.contains("ExecStart=\"/opt/50%%\\\"/cw-rails\" \"run\" \"--listen\" \"9747\"\n"),
        "{text}"
    );
    assert!(text.contains("PATH=/a%%b"), "{text}");
}

/// Stand-in `systemctl`/`loginctl`: `systemctl` records its argv and answers
/// `is-enabled` from whether an `enable` of that unit was recorded.
#[cfg(unix)]
fn stand_ins(dir: &Path) -> SystemdUser {
    use std::os::unix::fs::PermissionsExt;
    let log = dir.join("argv.log");
    let systemctl = dir.join("systemctl");
    std::fs::write(
        &systemctl,
        format!(
            "#!/bin/sh\necho \"$@\" >> {log}\n\
             if [ \"$2\" = is-enabled ]; then\n\
               if grep -q \"^--user enable.* $3\\$\" {log}; then echo enabled; else echo disabled; fi\n\
             fi\nexit 0\n",
            log = log.display()
        ),
    )
    .unwrap();
    let loginctl = dir.join("loginctl");
    std::fs::write(&loginctl, "#!/bin/sh\necho Linger=yes\n").unwrap();
    for p in [&systemctl, &loginctl] {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    SystemdUser::at(systemctl, loginctl)
}

/// The boot proof's unit half: after `mesh up`'s install the manager starts
/// cw-rails at boot, and the install started nothing (the bring-up did).
#[cfg(unix)]
#[test]
fn mesh_up_leaves_the_rails_unit_enabled_and_not_started_twice() {
    let dir = tempfile::tempdir().unwrap();
    let systemd = stand_ins(dir.path());
    let path = dir.path().join("user").join(RAILS_UNIT);
    assert_eq!(
        systemd.state(RAILS_UNIT),
        UnitState::Disabled("disabled".into())
    );
    install_with(
        &systemd,
        &path,
        Path::new("/usr/bin/cw-rails"),
        &crate::rails_up::run_args(9747, false, false),
        dir.path(),
    )
    .unwrap();
    assert_eq!(
        systemd.state(RAILS_UNIT),
        UnitState::Enabled,
        "a node that ran `svrn mesh up` is back on the mesh after a reboot"
    );
    let argv = std::fs::read_to_string(dir.path().join("argv.log")).unwrap();
    assert!(
        !argv.contains("--now"),
        "a second cw-rails on the bring-up's root would lose rails.lock: {argv}"
    );
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("ExecStart=\"/usr/bin/cw-rails\" \"run\" \"--listen\" \"9747\"\n"));
}

/// cw-rails browses mDNS when `[discovery] mdns` says so, and never on a
/// local-only node, whatever the config says: the daemon's
/// `mdns_enabled_effective` rule, now the bring-up's. Failing input: pass
/// the config value through unfiltered.
#[test]
fn mdns_follows_the_config_and_never_runs_local_only() {
    assert!(crate::rails_up::mdns_effective(false, true));
    assert!(!crate::rails_up::mdns_effective(false, false));
    assert!(!crate::rails_up::mdns_effective(true, true));
    assert_eq!(
        crate::rails_up::run_args(9747, false, true),
        ["run", "--listen", "9747", "--mdns"]
    );
}
