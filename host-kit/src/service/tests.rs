use super::*;

#[test]
fn the_probe_asks_logind_for_the_linger_property() {
    assert_eq!(
        linger_probe_argv("alex"),
        ["show-user", "alex", "--property=Linger"],
        "show-user answers for a user with no active session; `list-users` \
         does not list one, which is the state a remote install runs in"
    );
}

#[test]
fn enabling_is_per_user_and_asks_for_no_elevation() {
    assert_eq!(linger_enable_argv("alex"), ["enable-linger", "alex"]);
}

#[test]
fn linger_is_read_from_the_whole_line_not_a_substring() {
    assert!(linger_is_enabled("Linger=yes"));
    assert!(linger_is_enabled("IdleHint=no\nLinger=yes\nState=active\n"));
    assert!(!linger_is_enabled("Linger=no"));
    assert!(!linger_is_enabled(""));
    // The trap this guards: a `contains("Linger")` check reads "no" as "yes"
    // and the install then silently skips the only step that matters.
    assert!(!linger_is_enabled("Linger=no\nLingerHint=yes-ish"));
}

/// A refusal must arrive as its consequence plus its repair, never as
/// logind's stderr alone (ARCH principle 6).
#[test]
fn the_warning_names_the_consequence_and_the_exact_fix() {
    let w = linger_refusal_warning("svrnmesh", "daemon", "alex", "Access denied\n");
    assert!(
        w.contains("loginctl enable-linger alex"),
        "the fix must be a command the user can paste: {w}"
    );
    assert!(
        w.contains("The daemon will STOP AT LOGOUT"),
        "the consequence must be in plain words: {w}"
    );
    assert!(
        w.contains("ABSENT"),
        "and in mesh terms — the node leaves, it does not idle: {w}"
    );
    assert!(
        w.contains("Access denied"),
        "logind's own words survive, they are just not the whole message: {w}"
    );
    assert!(
        w.lines().all(|l| l.starts_with("svrnmesh:")),
        "every line speaks as the caller's program: {w}"
    );
}

/// An empty detail must not leave a dangling `()` in the sentence.
#[test]
fn the_warning_reads_cleanly_with_no_detail() {
    let w = linger_refusal_warning("svrnmesh", "daemon", "alex", "   ");
    assert!(w.contains("for alex."), "{w}");
    assert!(!w.contains("()"), "{w}");
}

#[test]
fn enable_starts_now_only_when_asked() {
    assert_eq!(
        enable_argv("cw-rails.service", false),
        ["--user", "enable", "cw-rails.service"]
    );
    assert_eq!(
        enable_argv("svrnmesh.service", true),
        ["--user", "enable", "--now", "svrnmesh.service"]
    );
}

#[test]
fn is_enabled_reads_each_state_it_can_answer() {
    assert_eq!(parse_is_enabled("enabled\n", ""), UnitState::Enabled);
    assert_eq!(parse_is_enabled("linked\n", ""), UnitState::Enabled);
    assert_eq!(
        parse_is_enabled("disabled\n", ""),
        UnitState::Disabled("disabled".into())
    );
    assert_eq!(
        parse_is_enabled("masked\n", ""),
        UnitState::Disabled("masked".into())
    );
    assert_eq!(parse_is_enabled("not-found\n", ""), UnitState::NotInstalled);
    assert_eq!(
        parse_is_enabled(
            "",
            "Failed to get unit file state for x.service: No such file or directory"
        ),
        UnitState::NotInstalled
    );
    assert!(matches!(
        parse_is_enabled("", "Failed to connect to bus"),
        UnitState::Unknown(_)
    ));
}

#[test]
fn a_percent_survives_the_unit_file() {
    assert_eq!(escape_specifiers("/opt/50%/bin"), "/opt/50%%/bin");
}

/// A stand-in `systemctl`: it records every argv, and answers `is-enabled`
/// from what `enable` recorded — the shape of the manager the install drives.
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

#[cfg(unix)]
#[test]
fn an_installed_unit_is_written_reloaded_and_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let systemd = stand_ins(dir.path());
    let path = dir.path().join("user").join("x.service");
    assert_eq!(
        systemd.state("x.service"),
        UnitState::Disabled("disabled".into())
    );
    systemd
        .install(
            &UserUnit {
                name: "x.service",
                path: &path,
                content: "[Service]\nExecStart=/bin/true\n",
                prefix: "test",
                what: "program",
            },
            false,
        )
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[Service]\nExecStart=/bin/true\n"
    );
    let argv = std::fs::read_to_string(dir.path().join("argv.log")).unwrap();
    let lines: Vec<&str> = argv.lines().collect();
    assert_eq!(
        &lines[1..],
        ["--user daemon-reload", "--user enable x.service"],
        "reload before enable, and no --now when not asked: {argv}"
    );
    assert_eq!(systemd.state("x.service"), UnitState::Enabled);
    systemd.uninstall("x.service", &path).unwrap();
    assert!(!path.exists());
}
