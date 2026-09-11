// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where is the daemon binary this build can use, and where should it live?
//!
//! Two questions, one answer each, and the only reason this module exists: a
//! client needs a path before it can ask `sovereign_turn_client`'s
//! `ServingHost` to bring a backend up (the `bundled-backend` capability). It
//! resolves a path and installs a copy — it does not start, register,
//! supervise or stop anything, which is the whole point of the campaign it
//! serves (`sv-surface`, `sv-no-daemon-management`).
//!
//! The service registration this was extracted from was CUT at svt-0: a
//! second story about who starts a daemon, resting on a Windows backend
//! nobody has run on Windows, with a branch that had the app calling
//! `stop_service`. Registration stays where it already lived, `svrn
//! install-service`.
//!
//! # Why a copy, and not the path inside the bundle
//!
//! svt-1 ships `sovereign-cli-daemon` as a Tauri sidecar, so on a packaged
//! app the resolver's second entry — beside this executable — hits
//! `…/Contents/MacOS/sovereign-cli-daemon`. That path is inside the `.app`,
//! and an `.app` moves: the user drags it to another disk, the updater
//! replaces the bundle under a still-running daemon, an uninstall takes the
//! directory with it. [`stable_daemon_binary`] therefore installs the sidecar
//! under the branded root once and hands back THAT path, which is the same
//! advice `BundledBackend::at`'s own doc gives.

use std::path::{Path, PathBuf};

/// The sidecar's base name, and the one place it is spelled in Rust.
///
/// Tauri's `externalBin` entry in `tauri.release.conf.json` is
/// `binaries/<this>`; it stages per-triple files (`<this>-<triple>`) and
/// installs the one matching file as `<this>` beside the app executable.
/// `packaging::the_sidecar_name_matches_the_release_config` reads that JSON
/// and asserts the two agree, because nothing else would notice a rename:
/// the app would simply resolve no binary and quietly lose its ability to
/// bring a backend up (ARCH principle 10 — make it structural).
pub(crate) const SIDECAR_BINARY: &str = "sovereign-cli-daemon";

/// Names that mean "a binary that understands `daemon run`", in preference
/// order within any one directory.
///
/// `svrn` is the symlink `landing/install.sh` creates and the name a user
/// will have; `sovereign-cli` is the dispatcher it points at, for an install
/// that placed one without the other. Both `exec` into the daemon sibling on
/// the `daemon` verb, so all three take the IDENTICAL argv — which is what
/// lets `serving_host` name one arg vector rather than one per name.
/// `SIDECAR_BINARY` is last because it is the only one present in a packaged
/// bundle, where the ordering cannot matter, and a dev tree that holds all
/// three should prefer the full dispatcher.
const DAEMON_BINARY_NAMES: [&str; 3] = ["svrn", "sovereign-cli", SIDECAR_BINARY];

/// The daemon binary this build can bring up, or `None`.
///
/// The order is the order a real install produces, and each entry is a place
/// a binary genuinely lands rather than a guess:
///
/// 1. `SVRNMESH_DAEMON_BINARY` / `SOVEREIGN_DAEMON_BINARY` — the operator's
///    override, read through `rebrand::svrnmesh_env` so both spellings work.
/// 2. Beside this executable — where Tauri's `externalBin` puts the sidecar
///    (`Contents/MacOS/` in an `.app`, the install dir on Windows,
///    `/usr/bin` in the deb).
/// 3. `~/.local/bin`, which is where `landing/install.sh` puts `svrn` by
///    default, then `/usr/local/bin`.
pub(crate) fn daemon_binary() -> Option<PathBuf> {
    // `svrnmesh_env` is the one reader of the two prefixes (ARCH principle
    // 8); spelling `var_os` here would honour one of them by accident.
    if let Some(raw) = sovereign_contracts::rebrand::svrnmesh_env("DAEMON_BINARY") {
        let p = PathBuf::from(raw);
        if p.is_file() {
            tracing::info!(path = %p.display(), "daemon-binary: using the DAEMON_BINARY override");
            return Some(p);
        }
        tracing::warn!(
            path = %p.display(),
            "daemon-binary: DAEMON_BINARY is set but is not a file — ignoring it"
        );
    }

    first_binary_in(&search_dirs())
}

/// The directories [`daemon_binary`] searches, in order.
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = exe_dir() {
        dirs.push(dir);
    }
    // `~/.local/bin` and `/usr/local/bin` are unix conventions, and
    // `landing/install.sh` — a bash script — is what puts `svrn` in the
    // first of them. `$HOME` rather than `dirs::home_dir`, which
    // `clippy.toml` denies so that per-user SOVEREIGN paths go through
    // `sovereign_contracts::rebrand`; this is not one of those, it is the
    // installer's own directory.
    #[cfg(unix)]
    {
        if let Some(home) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(home).join(".local").join("bin"));
        }
        dirs.push(PathBuf::from("/usr/local/bin"));
    }
    dirs
}

/// The directory holding this process's executable.
fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(Path::to_path_buf)
}

/// The daemon binary at a path that outlives the app bundle.
///
/// Resolution is [`daemon_binary`]'s; the only thing added is the install,
/// and only [`is_our_sidecar`] gets one. Everything else is returned
/// untouched.
///
/// An install failure is REPORTED and the in-bundle path is named as the
/// substitution (ARCH principle 6); it is not silently swallowed, and it is
/// not fatal — an in-bundle path still works for this run.
pub(crate) fn stable_daemon_binary() -> Option<PathBuf> {
    let resolved = daemon_binary()?;
    if !is_our_sidecar(&resolved) {
        return Some(resolved);
    }
    let dir = sovereign_contracts::rebrand::svrnmesh_root().join("bin");
    match install_to(&resolved, &dir) {
        Ok(stable) => {
            tracing::info!(
                from = %resolved.display(),
                to = %stable.display(),
                "daemon-binary: sidecar installed at a path that outlives the bundle"
            );
            Some(stable)
        }
        Err(reason) => {
            tracing::warn!(
                from = %resolved.display(),
                dir = %dir.display(),
                %reason,
                "daemon-binary: could not install the sidecar — SUBSTITUTING the in-bundle \
                 path, which breaks if the app is moved, updated or removed"
            );
            Some(resolved)
        }
    }
}

/// Is this the self-contained daemon this app SHIPS — the only thing that may
/// be moved to a stable path?
///
/// Two conditions, and the second one is not fussiness. `svrn` and
/// `sovereign-cli` are DISPATCHERS: they resolve `sovereign-cli-daemon` as a
/// sibling of their own `current_exe()` and only then fall back to `PATH`
/// (`sovereign-cli/src/daemon_bin.rs:7-8`). Relocate one away from its
/// siblings and `<copy> daemon run` dies with "cannot find the daemon binary"
/// — which is exactly what a dev tree would hit, where the desktop's
/// `current_exe()` is `target/debug/` and `sovereign-cli` is sitting right
/// there next to it. Only `SIDECAR_BINARY` is a whole daemon on its own, so
/// only `SIDECAR_BINARY` is portable.
///
/// The first condition — inside the bundle — is what stops the install
/// running for a `sovereign-cli-daemon` already in `~/.local/bin`, which is
/// stable already and belongs to the user's own CLI install.
fn is_our_sidecar(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let is_sidecar_name = name == SIDECAR_BINARY
        || name == format!("{SIDECAR_BINARY}{}", std::env::consts::EXE_SUFFIX);
    is_sidecar_name && path.parent() == exe_dir().as_deref()
}

/// Put `src` in `dir` under its own file name, and answer where it landed.
///
/// Idempotent by fingerprint: a destination whose length and mtime already
/// match the source is left alone, so only the first launch after an install
/// or an update pays anything. The write goes to a pid-stamped temporary and
/// is `rename`d into place, so a daemon currently executing the old file
/// keeps its inode instead of having the bytes changed underneath it.
///
/// A hard link is tried before a copy — same filesystem is the common case
/// and it is free — and a link is as durable as a copy for this purpose: the
/// inode survives the bundle being deleted. It does NOT track the source, so
/// the fingerprint check above is what notices an app update.
fn install_to(src: &Path, dir: &Path) -> Result<PathBuf, String> {
    let name = src
        .file_name()
        .ok_or_else(|| format!("{} has no file name", src.display()))?;
    let dest = dir.join(name);

    let src_meta = std::fs::metadata(src).map_err(|e| format!("stat {}: {e}", src.display()))?;
    if let Ok(dest_meta) = std::fs::metadata(&dest) {
        if same_bytes(&src_meta, &dest_meta) {
            return Ok(dest);
        }
    }

    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let tmp = dir.join(format!(
        "{}.incoming-{}",
        name.to_string_lossy(),
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp);
    if std::fs::hard_link(src, &tmp).is_err() {
        std::fs::copy(src, &tmp).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("copy {} -> {}: {e}", src.display(), tmp.display())
        })?;
    }
    std::fs::rename(&tmp, &dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename into {}: {e}", dest.display())
    })?;
    Ok(dest)
}

/// Are these two the same bytes? Length plus mtime — the fingerprint an
/// installer can afford on every launch. A content hash of a ~200 MB binary
/// is not that, and a version string would need the daemon to be RUN to
/// answer, which this module must never do.
fn same_bytes(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    a.len() == b.len() && a.modified().ok() == b.modified().ok()
}

/// The search itself, over directories a caller supplies — so the order can
/// be tested without moving this host's real binaries around.
fn first_binary_in(dirs: &[PathBuf]) -> Option<PathBuf> {
    for dir in dirs {
        for name in DAEMON_BINARY_NAMES {
            // `EXE_SUFFIX` rather than a `#[cfg(windows)]` arm: it is `""` on
            // unix, so this is one code path both platforms take and both
            // platforms' behaviour is reachable from a test on either. Tauri
            // installs the Windows sidecar as `sovereign-cli-daemon.exe`, and
            // a resolver that only tried the bare name would return `None`
            // there — no bring-up, no error, on the one platform nobody has
            // run this on.
            for candidate in [
                dir.join(name),
                dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)),
            ] {
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &std::path::Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, b"#!/bin/sh\n").unwrap();
        p
    }

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "svc-adopt-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn nothing_installed_anywhere_is_no_binary_not_a_guess() {
        let empty = tmp("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(first_binary_in(&[empty]), None);
    }

    #[test]
    fn the_first_directory_wins_so_a_sidecar_beats_an_installed_cli() {
        let beside = tmp("beside");
        let installed = tmp("installed");
        let sidecar = touch(&beside, "svrn");
        touch(&installed, "svrn");
        assert_eq!(
            first_binary_in(&[beside, installed]),
            Some(sidecar),
            "a binary shipped with the app must win over whatever else is on the box"
        );
    }

    #[test]
    fn svrn_wins_over_the_dispatcher_within_one_directory() {
        let dir = tmp("both");
        let svrn = touch(&dir, "svrn");
        touch(&dir, "sovereign-cli");
        assert_eq!(first_binary_in(&[dir]), Some(svrn));
    }

    #[test]
    fn the_dispatcher_is_found_when_the_symlink_is_absent() {
        let dir = tmp("dispatcher-only");
        let cli = touch(&dir, "sovereign-cli");
        assert_eq!(first_binary_in(&[dir.clone()]), Some(cli));
    }

    #[test]
    fn a_directory_is_not_a_binary() {
        let dir = tmp("dir-named-svrn");
        std::fs::create_dir_all(dir.join("svrn")).unwrap();
        assert_eq!(
            first_binary_in(&[dir]),
            None,
            "is_file() is the test — a directory called svrn resolves to a path \
             nothing can execute, and the caller finds out at spawn time"
        );
    }

    #[test]
    fn the_sidecar_alone_is_a_daemon_binary() {
        // The packaged case: Tauri stages ONE file beside the executable and
        // it is named after the crate, not after the dispatcher symlink.
        // Before svt-1 this directory resolved to `None`.
        let dir = tmp("sidecar-only");
        let sidecar = touch(&dir, SIDECAR_BINARY);
        assert_eq!(first_binary_in(&[dir]), Some(sidecar));
    }

    #[test]
    fn the_windows_spelling_is_found_wherever_it_is_the_only_one() {
        // `EXE_SUFFIX` is `""` on unix, so on this host the two candidate
        // spellings collapse and the `.exe` file is NOT found — which is the
        // honest result and what this asserts. On Windows the same call finds
        // it. Pinning both halves here is what keeps the unix-only CI from
        // reading as proof about Windows.
        let dir = tmp("windows-spelling");
        touch(&dir, &format!("{SIDECAR_BINARY}.exe"));
        let found = first_binary_in(&[dir]);
        if std::env::consts::EXE_SUFFIX == ".exe" {
            assert!(
                found.is_some(),
                "the Windows sidecar must resolve on Windows"
            );
        } else {
            assert_eq!(
                found, None,
                "a `.exe` is not executable here; EXE_SUFFIX is what decides"
            );
        }
    }

    #[test]
    fn a_dispatcher_is_never_relocated() {
        // The dev-tree case, and a real bug before this predicate existed:
        // `cargo tauri dev` puts the desktop exe in `target/debug/`, where
        // `sovereign-cli` is sitting beside it and wins the name race. Hard
        // link that dispatcher into `<root>/bin` and `daemon run` dies —
        // the dispatcher looks for `sovereign-cli-daemon` NEXT TO ITSELF
        // (`sovereign-cli/src/daemon_bin.rs:7`), and at the new location
        // there is no sibling.
        let here = exe_dir().expect("this test has an executable");
        for name in ["svrn", "sovereign-cli"] {
            assert!(
                !is_our_sidecar(&here.join(name)),
                "{name} is a dispatcher; moving it away from its siblings breaks it"
            );
        }
        assert!(
            is_our_sidecar(&here.join(SIDECAR_BINARY)),
            "the sidecar is a whole daemon and is the one thing that may move"
        );
    }

    #[test]
    fn a_sidecar_outside_the_bundle_is_left_alone() {
        // A `sovereign-cli-daemon` in `~/.local/bin` came from the user's own
        // CLI install. It is already at a stable path, and copying it would
        // shadow their next `svrn` upgrade with our frozen copy.
        let elsewhere = tmp("outside-bundle").join(SIDECAR_BINARY);
        assert!(!is_our_sidecar(&elsewhere));
    }

    #[test]
    fn installing_puts_the_binary_where_the_bundle_cannot_take_it() {
        let bundle = tmp("install-src");
        let stable = tmp("install-dest");
        let src = touch(&bundle, SIDECAR_BINARY);
        let dest = install_to(&src, &stable).expect("install");
        assert_eq!(dest, stable.join(SIDECAR_BINARY));
        assert!(dest.is_file());
        // The bundle is gone; the installed binary is not.
        std::fs::remove_dir_all(&bundle).unwrap();
        assert!(
            dest.is_file(),
            "an install that dies with the bundle has bought nothing"
        );
    }

    #[test]
    fn installing_twice_does_not_rewrite_an_identical_binary() {
        let bundle = tmp("idem-src");
        let stable = tmp("idem-dest");
        let src = touch(&bundle, SIDECAR_BINARY);
        let first = install_to(&src, &stable).expect("first install");
        let stamp = std::fs::metadata(&first).unwrap().modified().unwrap();
        let second = install_to(&src, &stable).expect("second install");
        assert_eq!(first, second);
        assert_eq!(
            std::fs::metadata(&second).unwrap().modified().unwrap(),
            stamp,
            "a matching fingerprint must skip the write — every launch pays this"
        );
    }

    #[test]
    fn an_updated_app_replaces_the_installed_binary() {
        let bundle = tmp("update-src");
        let stable = tmp("update-dest");
        let src = touch(&bundle, SIDECAR_BINARY);
        let dest = install_to(&src, &stable).expect("install v1");
        assert_eq!(std::fs::read(&dest).unwrap(), b"#!/bin/sh\n");

        // What an updater does: a NEW file at the same path. A hard link to
        // the old inode must not be mistaken for the new version.
        std::fs::remove_file(&src).unwrap();
        std::fs::write(&src, b"#!/bin/sh\n# v2\n").unwrap();
        let again = install_to(&src, &stable).expect("install v2");
        assert_eq!(again, dest);
        assert_eq!(
            std::fs::read(&again).unwrap(),
            b"#!/bin/sh\n# v2\n",
            "the length differs, so the fingerprint must have caught the update"
        );
    }

    /// The Rust name and the packaging name are one fact spelled in two
    /// files. Nothing at runtime would report their disagreement — the app
    /// would resolve no binary and silently lose the ability to bring a
    /// backend up — so it is asserted here (ARCH principle 10).
    mod packaging {
        use super::*;

        fn release_config() -> serde_json::Value {
            let path =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.release.conf.json");
            let raw = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
        }

        #[test]
        fn the_sidecar_name_matches_the_release_config() {
            let cfg = release_config();
            let bins = cfg["bundle"]["externalBin"]
                .as_array()
                .expect("tauri.release.conf.json must declare bundle.externalBin");
            let want = format!("binaries/{SIDECAR_BINARY}");
            assert!(
                bins.iter().any(|b| b.as_str() == Some(want.as_str())),
                "externalBin {bins:?} does not stage {want} — the resolver would \
                 find nothing beside the app and the bring-up would be silently lost"
            );
        }

        #[test]
        fn the_base_config_stages_no_sidecar() {
            // RELEASING.md "Tauri config split": tauri-build ERRORS when
            // `externalBin` names files that do not exist, and in plain dev
            // nobody has staged them. An entry landing in the base config
            // breaks `cargo check` and `cargo tauri dev` for everyone.
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json");
            let raw = std::fs::read_to_string(&path).unwrap();
            let cfg: serde_json::Value = serde_json::from_str(&raw).unwrap();
            assert!(
                cfg["bundle"]["externalBin"].is_null(),
                "tauri.conf.json must NOT declare externalBin — it is the dev config"
            );
        }
    }
}
