// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for setup-config DEFAULTS and on-disk placement: tilde expansion, the
//! spec defaults, the config path + legacy migration, and the eager/extra slot
//! knobs — see `setup_config.rs`.
//!
//! Their own file because keeping them inline put `setup_config.rs` past
//! its arch-gate slack, and this file past the 1,200-line ceiling
//! (ARCH §3.1/§3.2). `#[path]`, so the names are unchanged.

use super::*;

#[test]
#[allow(clippy::disallowed_methods)] // test asserts tilde-expansion against the REAL home
fn expand_home_resolves_tilde() {
    // Reads the process-global HOME — must serialize against the tests
    // that swap it (see `crate::test_support::home_env_lock`).
    let _home_guard = crate::test_support::home_env_lock();
    let home = dirs::home_dir().unwrap();
    assert_eq!(expand_home(Path::new("~/foo/bar")), home.join("foo/bar"));
    assert_eq!(
        expand_home(Path::new("/abs/path")),
        PathBuf::from("/abs/path")
    );
}

#[test]
fn defaults_match_spec() {
    let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
"#;
    let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(cfg.daemon.client_port, 9741);
    assert_eq!(cfg.daemon.internal_port, 9742);
    assert!(cfg.daemon.autostart);
}

#[test]
fn yield_to_foreground_secs_defaults_to_60() {
    // A config that omits the field must come back with the
    // 60-second default so existing operators get yield enabled
    // without editing config.toml. Lock the default here so a
    // future bump (or zero-out) is intentional and reviewed.
    let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
"#;
    let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(cfg.daemon.yield_to_foreground_secs, 60);
}

#[test]
fn local_only_defaults_to_false_and_is_settable() {
    // A daemon is NETWORKED unless an operator says otherwise: the
    // local-only profile ships dark (sovereign/DEFAULTS_LEDGER.md), so an
    // existing config.toml must keep every mesh loop it had.
    let bare = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
"#;
    let cfg: SetupConfig = toml::from_str(bare).unwrap();
    assert!(!cfg.daemon.local_only, "the shipped default is networked");

    let opted_in = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"

[daemon]
local_only = true
"#;
    let cfg: SetupConfig = toml::from_str(opted_in).unwrap();
    assert!(cfg.daemon.local_only);
}

#[test]
fn max_peer_inflight_defaults_to_1() {
    // A config that omits the field must come back bounded (1), NOT
    // unbounded — a headless contributor with no ceiling is the
    // resource-exhaustion hole this default closes. Lock it so a future
    // change is intentional and reviewed.
    let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
"#;
    let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(cfg.daemon.max_peer_inflight, 1);
}

#[test]
fn freshness_watchers_enabled_defaults_to_true() {
    let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
"#;
    let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
    assert!(cfg.daemon.freshness_watchers_enabled);
}

#[test]
fn freshness_watchers_enabled_explicit_false() {
    let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"

[daemon]
freshness_watchers_enabled = false
"#;
    let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
    assert!(!cfg.daemon.freshness_watchers_enabled);
}

#[test]
fn yield_to_foreground_secs_explicit_override() {
    // Operators can set 0 to disable, or a higher value for a
    // longer pause window.
    let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"

[daemon]
yield_to_foreground_secs = 0
"#;
    let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(cfg.daemon.yield_to_foreground_secs, 0);
}

#[test]
fn default_path_is_hidden_brand_dir_with_config_toml() {
    // Reads the process-global HOME (`default_path()` ->
    // `default_data_dir()` -> `svrnmesh_root()`) — must serialize against
    // the tests that swap it (see `crate::test_support::home_env_lock`).
    // It passed under a swapped HOME only because the swapping test
    // populates BOTH brand dirs in its tempdir and the assertion accepts
    // either spelling; that is luck, not coverage.
    let _home_guard = crate::test_support::home_env_lock();
    // Config lives directly under home in a hidden, brand-named dir:
    // `~/.svrnmesh/config.toml` (preferred) or the legacy
    // `~/.svrnmesh/config.toml`. Post-rename, `default_path()` ->
    // `svrnmesh_root()` -> `rebrand::resolve_branded_dir` resolves to
    // whichever the machine actually has: a populated `~/.svrnmesh` wins,
    // else a populated legacy `~/.sovereign`, else `~/.svrnmesh` on a
    // fresh install. The brand component is therefore environment-
    // dependent, so the assertion must accept either spelling.
    //
    // The leading dot is load-bearing: `Path::ends_with` matches whole
    // components, so the dotted `.svrnmesh`/`.sovereign` distinguishes the
    // canonical hidden-dir layout from the legacy
    // `~/.config/sovereign/config.toml` (which ends with the *undotted*
    // `sovereign/config.toml`). Keep the dot literal so a regression back
    // to that legacy layout still fails this test.
    let p = SetupConfig::default_path();
    assert!(
        p.ends_with(".svrnmesh/config.toml") || p.ends_with(".sovereign/config.toml"),
        "unexpected path: {}",
        p.display()
    );
}

#[test]
fn migrate_moves_legacy_when_new_path_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let legacy = tmp.path().join("legacy/config.toml");
    let new_path = tmp.path().join("new/config.toml");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "primary=\"/m/p.gguf\"\n").unwrap();

    migrate_config_between(&legacy, &new_path);

    assert!(new_path.exists(), "new path should exist after migration");
    assert!(
        !legacy.exists(),
        "legacy path should be gone after migration"
    );
    assert_eq!(
        std::fs::read_to_string(&new_path).unwrap(),
        "primary=\"/m/p.gguf\"\n"
    );
}

#[test]
fn migrate_is_noop_when_new_path_already_exists() {
    // Operator has already migrated (or is on a fresh install): the
    // legacy file may still exist as a stale leftover, but we must
    // not clobber the canonical file.
    let tmp = tempfile::tempdir().unwrap();
    let legacy = tmp.path().join("legacy/config.toml");
    let new_path = tmp.path().join("new/config.toml");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::create_dir_all(new_path.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "stale\n").unwrap();
    std::fs::write(&new_path, "canonical\n").unwrap();

    migrate_config_between(&legacy, &new_path);

    assert_eq!(std::fs::read_to_string(&new_path).unwrap(), "canonical\n");
    assert!(legacy.exists(), "legacy untouched when new path present");
}

#[test]
fn migrate_is_noop_when_legacy_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let legacy = tmp.path().join("legacy/config.toml");
    let new_path = tmp.path().join("new/config.toml");
    // Neither file exists.
    migrate_config_between(&legacy, &new_path);
    assert!(!new_path.exists());
}

#[test]
fn eager_slots_unload_when_idle_by_default() {
    // The always-on posture, asserted rather than described: a node
    // that nobody has asked anything in fifteen minutes should not be
    // holding the fast model or the embedder. Before these knobs
    // existed both slots were pinned from boot to process exit, which
    // is why an idle daemon read as "tens of GB for nothing".
    let cfg = DaemonSection::default();
    assert_eq!(cfg.fast_idle_secs, 900);
    assert_eq!(cfg.embed_idle_secs, 900);
}

#[test]
fn eager_slot_idle_windows_outlast_a_slot_load() {
    // The failure mode these defaults are chosen against is NOT
    // "unloads too late" — it is an idle window SHORTER than the
    // slot's own load time, which makes every pause re-pay the load.
    // Field case: `primary_idle_secs = 60` produced seven reloads in
    // a day, one of them unloading and reloading a second apart
    // (note 419e273c). A cold load has been measured at 15-95s, so
    // any window at or under 95s is in that trap.
    let cfg = DaemonSection::default();
    let worst_observed_cold_load_secs = 95;
    assert!(
        cfg.fast_idle_secs > worst_observed_cold_load_secs,
        "fast_idle_secs {} is inside the thrash band",
        cfg.fast_idle_secs
    );
    assert!(
        cfg.embed_idle_secs > worst_observed_cold_load_secs,
        "embed_idle_secs {} is inside the thrash band",
        cfg.embed_idle_secs
    );
}

#[test]
fn eager_slot_idle_windows_parse_from_toml() {
    let toml_str = r#"
[daemon]
fast_idle_secs = 120
embed_idle_secs = 0
"#;
    let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(cfg.daemon.fast_idle_secs, 120);
    // `0` is the documented opt-out — pin the slot for the daemon's
    // lifetime — and must survive the round trip as 0 rather than
    // being back-filled by the default.
    assert_eq!(cfg.daemon.embed_idle_secs, 0);
}

#[test]
fn an_existing_config_without_the_knobs_still_loads() {
    // Operators upgrading the binary keep their config.toml. Absence
    // takes the default rather than failing the parse.
    let toml_str = r#"
[daemon]
primary_idle_secs = 300
"#;
    let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(cfg.daemon.primary_idle_secs, 300);
    assert_eq!(cfg.daemon.fast_idle_secs, 900);
    assert_eq!(cfg.daemon.embed_idle_secs, 900);
}

#[test]
fn extra_slots_default_empty_when_absent() {
    // Operators upgrading the binary keep their existing
    // config.toml. The `serde(default)` on `extra` means a config
    // without the `[models.extra]` table loads with an empty
    // map — preserving the legacy 3-slot lineup.
    let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
"#;
    let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
    assert!(cfg.models().unwrap().extra.is_empty());
}

#[test]
fn extra_slots_parse_from_toml_table() {
    // `[models.extra]` table → BTreeMap<String, PathBuf>.
    // BTreeMap iteration order is sorted, which makes startup
    // logging deterministic across reboots.
    let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"

[models.extra]
reasoning = "/m/big.gguf"
bulk = "/m/small.gguf"
"#;
    let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(cfg.models().unwrap().extra.len(), 2);
    assert_eq!(
        cfg.models().unwrap().extra.get("reasoning"),
        Some(&PathBuf::from("/m/big.gguf"))
    );
    assert_eq!(
        cfg.models().unwrap().extra.get("bulk"),
        Some(&PathBuf::from("/m/small.gguf"))
    );
}

#[test]
fn max_extras_memory_bytes_unset_returns_none() {
    let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
"#;
    let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
    assert!(cfg.models().unwrap().max_extras_memory_bytes().is_none());
}

#[test]
fn max_extras_memory_bytes_converts_gigabytes() {
    let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
max_extras_memory_gb = 12.0
"#;
    let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
    // 12 GiB = 12 * 2^30 bytes.
    assert_eq!(
        cfg.models().unwrap().max_extras_memory_bytes(),
        Some(12 * (1u64 << 30))
    );
}

#[test]
fn max_extras_memory_bytes_saturates_on_negative_input() {
    // Defensive: a negative or zero budget effectively forbids
    // any extras — return Some(0) rather than panicking or
    // overflowing.
    let toml_str = r#"
[models]
primary = "/m/p.gguf"
fast = "/m/f.gguf"
embed = "/m/e.gguf"
max_extras_memory_gb = 0.0
"#;
    let cfg: SetupConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(cfg.models().unwrap().max_extras_memory_bytes(), Some(0));
}

#[test]
#[allow(clippy::disallowed_methods)] // test asserts tilde-expansion against the REAL home
fn extra_slots_expand_home_at_load() {
    // `~/...` paths inside `[models.extra]` resolve like the
    // primary/fast/embed paths do — load-time expansion via
    // `expand_paths`.
    //
    // Reads the process-global HOME — must serialize against the tests
    // that swap it (see `crate::test_support::home_env_lock`).
    let _home_guard = crate::test_support::home_env_lock();
    let home = dirs::home_dir().unwrap();
    let toml_str = r#"
[models]
primary = "~/dev/primary.gguf"
fast = "/abs/fast.gguf"
embed = "~/dev/embed.gguf"

[models.extra]
reasoning = "~/dev/big.gguf"
"#;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(&path, toml_str).unwrap();
    let cfg = SetupConfig::load_from(&path).unwrap();
    assert_eq!(
        cfg.models().unwrap().extra.get("reasoning"),
        Some(&home.join("dev/big.gguf"))
    );
}

// ---- the daemon-endpoint decider (§10.6) -------------------------------
//
