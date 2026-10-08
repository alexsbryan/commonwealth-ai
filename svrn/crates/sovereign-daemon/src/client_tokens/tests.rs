// SPDX-License-Identifier: AGPL-3.0-or-later
//! The named-credential store: one record, both legacy forms, the posture
//! that gates minting, and same-lifetime revocation.

use super::*;

const OWNER: LoopbackPosture = LoopbackPosture {
    loopback: Loopback::Owner,
    declared: true,
};
const NONE: LoopbackPosture = LoopbackPosture {
    loopback: Loopback::None,
    declared: true,
};

fn store(dir: &Path, posture: LoopbackPosture) -> ClientTokenStore {
    ClientTokenStore::load(Some(dir.to_path_buf()), posture)
}

#[test]
fn the_posture_is_a_closed_set_and_an_unknown_spelling_refuses() {
    assert_eq!(ClientTokens::parse("shared"), Ok(ClientTokens::Shared));
    assert_eq!(
        ClientTokens::parse(" named-only "),
        Ok(ClientTokens::NamedOnly)
    );
    assert_eq!(ClientTokens::default(), ClientTokens::Shared);
    assert!(ClientTokens::Shared.admits_shared_token());
    assert!(!ClientTokens::NamedOnly.admits_shared_token());
    let err = ClientTokens::parse("named").unwrap_err();
    assert_eq!(err.value, "named");
    assert!(err.to_string().contains("named-only"));
}

#[test]
fn a_minted_credential_asserts_its_name_and_groups_and_a_wrong_one_does_not() {
    let tmp = tempfile::tempdir().unwrap();
    let s = store(&tmp.path().join("client-tokens"), OWNER);
    s.mint("claude-code", &[], "tok-cc".into()).unwrap();
    s.mint("it", &[KEY_ADMIN_GROUP.into()], "tok-it".into())
        .unwrap();
    assert_eq!(
        s.asserted_for("tok-cc"),
        Some(("claude-code".into(), vec![]))
    );
    assert_eq!(
        s.asserted_for("tok-it"),
        Some(("it".into(), vec![KEY_ADMIN_GROUP.to_string()]))
    );
    assert_eq!(s.asserted_for("tok-nobody"), None);
    assert_eq!(
        s.list().into_iter().map(|r| r.name).collect::<Vec<_>>(),
        vec!["claude-code".to_string(), "it".to_string()]
    );
}

/// The revoked name stops admitting IN THIS STORE, with no reload, and its
/// sibling is untouched. Failing input: delete the file and leave the map,
/// and `tok-laptop` still asserts `laptop`.
#[test]
fn revoking_one_name_leaves_the_other_admitting_with_no_reload() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("client-tokens");
    let s = store(&dir, OWNER);
    s.mint("laptop", &[], "tok-laptop".into()).unwrap();
    s.mint("tablet", &[], "tok-tablet".into()).unwrap();
    assert!(s.revoke("laptop"));
    assert_eq!(
        s.asserted_for("tok-laptop"),
        None,
        "a revoked name must stop admitting without a reload"
    );
    assert!(s.asserted_for("tok-tablet").is_some());
    assert!(!dir.join("laptop.key").exists());
    assert!(dir.join("tablet.key").exists());
    assert!(!s.revoke("laptop"), "nothing to revoke is not a success");
}

#[test]
fn the_set_survives_a_reload_and_a_revoked_one_does_not_come_back() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("client-tokens");
    let s = store(&dir, OWNER);
    s.mint("laptop", &[], "tok-laptop".into()).unwrap();
    s.mint("tablet", &["ops".into()], "tok-tablet".into())
        .unwrap();
    s.revoke("laptop");
    let reloaded = store(&dir, OWNER);
    assert_eq!(
        reloaded.asserted_for("tok-tablet"),
        Some(("tablet".into(), vec!["ops".into()]))
    );
    assert_eq!(reloaded.asserted_for("tok-laptop"), None);
}

#[test]
fn a_name_that_is_not_a_file_name_is_refused_and_no_mint_rotates() {
    let tmp = tempfile::tempdir().unwrap();
    let s = store(&tmp.path().join("client-tokens"), OWNER);
    for bad in ["", "  ", "../escape", "a/b", "sp ace", &"x".repeat(65)] {
        assert!(s.mint(bad, &[], "tok".into()).is_err(), "{bad:?}");
    }
    assert!(s.mint("ok", &["../g".into()], "tok".into()).is_err());
    s.mint("laptop", &[], "tok-laptop".into()).unwrap();
    assert!(s.mint("laptop", &[], "tok-other".into()).is_err());
    assert!(s.asserted_for("tok-laptop").is_some());
}

#[cfg(unix)]
#[test]
fn the_record_is_0600_in_a_0700_directory() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("client-tokens");
    store(&dir, OWNER)
        .mint("laptop", &[], "tok-laptop".into())
        .unwrap();
    let file = std::fs::metadata(dir.join("laptop.key")).unwrap();
    assert_eq!(file.permissions().mode() & 0o777, 0o600);
    assert_eq!(
        std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[test]
fn a_store_with_no_directory_refuses_to_mint() {
    let s = ClientTokenStore::load(None, OWNER);
    assert!(s.mint("laptop", &[], "tok".into()).is_err());
    assert!(s.list().is_empty());
}

/// "Add a key" used to mean "enter keyed mode". On an undeclared posture the
/// intent is ambiguous, so minting refuses and names the declaration; a
/// revoke never refuses. Failing input: drop the `declared` check and the
/// undeclared owner-inferred store mints a `.key`, which would flip the next
/// boot's inference to `none`.
#[test]
fn an_undeclared_posture_refuses_to_mint_and_never_refuses_a_revoke() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("client-tokens");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("firm.key"), "tok-firm\n").unwrap();
    let posture = LoopbackPosture::resolve(None, Some(&dir)).unwrap();
    let s = store(&dir, posture);
    let err = s.mint("new", &[], "tok-new".into()).unwrap_err();
    assert!(err.0.contains("[daemon] loopback is not declared"), "{err}");
    assert!(!dir.join("new.key").exists());
    assert!(
        s.revoke("firm"),
        "a revoke is never held hostage to a declaration"
    );
}

/// Both forms on disk are one record kind. Undeclared, the legacy `.token`
/// stays where it is; declared, it is rewritten as `.key` and still admits.
#[test]
fn both_legacy_forms_load_as_one_record_and_tokens_migrate_only_when_declared() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("client-tokens");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("laptop.token"), "tok-laptop").unwrap();
    std::fs::write(dir.join("firm.key"), "tok-firm\ngroups = admin\n").unwrap();

    let undeclared = LoopbackPosture::resolve(None, Some(&dir)).unwrap();
    let s = store(&dir, undeclared);
    assert_eq!(
        s.asserted_for("tok-laptop"),
        Some(("laptop".into(), vec![]))
    );
    assert_eq!(
        s.asserted_for("tok-firm"),
        Some(("firm".into(), vec!["admin".into()]))
    );
    assert!(dir.join("laptop.token").exists(), "undeclared: no rewrite");

    let s = store(&dir, NONE);
    assert!(!dir.join("laptop.token").exists());
    assert!(dir.join("laptop.key").exists());
    assert_eq!(
        s.asserted_for("tok-laptop"),
        Some(("laptop".into(), vec![]))
    );
}

/// One name, one record: a `.key` beside a legacy `.token` of the same name is
/// the record, and the token does not admit.
#[test]
fn a_key_outranks_a_legacy_token_of_the_same_name() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("client-tokens");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("x.token"), "tok-old").unwrap();
    std::fs::write(dir.join("x.key"), "tok-new\n").unwrap();
    let s = store(&dir, OWNER);
    assert_eq!(s.asserted_for("tok-new"), Some(("x".into(), vec![])));
    assert_eq!(s.asserted_for("tok-old"), None);
    assert!(
        dir.join("x.token").exists(),
        "the shadowed token is left, named"
    );
}

/// The daemon's own credential is admitted, as an admin, only where loopback
/// grants nothing, and no listing shows it.
#[test]
fn the_self_credential_admits_only_under_none() {
    let own = self_credential().expect("entropy");
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("client-tokens");
    assert_eq!(store(&dir, OWNER).asserted_for(own), None);
    let keyed = store(&dir, NONE);
    assert_eq!(
        keyed.asserted_for(own),
        Some((SELF_SUB.to_string(), vec![KEY_ADMIN_GROUP.to_string()]))
    );
    assert!(keyed.list().is_empty());
    assert!(!keyed.revoke(SELF_SUB));
    assert!(check_label(SELF_SUB).is_err());
}
