// SPDX-License-Identifier: AGPL-3.0-or-later
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use super::*;

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The compiled pin's binaries in a fresh directory, with `manifest` beside
/// them when given.
fn installed(manifest: Option<&str>) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    for name in BuildPin::compiled().unwrap().binaries {
        std::fs::write(dir.path().join(name), b"").unwrap();
    }
    let binary = dir.path().join("llama-server");
    if let Some(text) = manifest {
        std::fs::write(manifest_path(&binary), text).unwrap();
    }
    (dir, binary)
}

/// What build.sh writes: the pin verbatim, then its own `[build]` table.
fn as_built(pin_text: &str) -> String {
    format!("{pin_text}\n[build]\ntarball_sha256 = \"00\"\nsource = \"/src\"\n")
}

#[test]
fn the_pin_lists_every_patch_on_disk_by_its_digest() {
    let mut on_disk: Vec<PinnedPatch> =
        std::fs::read_dir(Path::new(SERVER_SOURCE_DIR).join("patches"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "patch"))
            .map(|p| PinnedPatch {
                file: p.file_name().unwrap().to_string_lossy().into_owned(),
                sha256: hex(&std::fs::read(&p).unwrap()),
            })
            .collect();
    let mut listed = BuildPin::compiled().unwrap().patches;
    on_disk.sort_by(|a, b| a.file.cmp(&b.file));
    listed.sort_by(|a, b| a.file.cmp(&b.file));
    assert!(
        !on_disk.is_empty(),
        "no patches found under {SERVER_SOURCE_DIR}"
    );
    assert_eq!(listed, on_disk, "PIN.toml and patches/ disagree");
}

#[test]
fn a_build_of_the_compiled_pin_is_verified() {
    let (_dir, binary) = installed(Some(&as_built(COMPILED_PIN)));
    verify(&binary, &BuildPin::compiled().unwrap()).unwrap();
}

#[test]
fn a_binary_without_a_manifest_is_refused_with_the_build_command() {
    let (_dir, binary) = installed(None);
    let err = verify(&binary, &BuildPin::compiled().unwrap()).unwrap_err();
    assert!(err.contains(MANIFEST), "{err}");
    assert!(err.contains("build.sh"), "{err}");
}

#[test]
fn another_commit_is_refused_naming_both() {
    let pin = BuildPin::compiled().unwrap();
    let other = COMPILED_PIN.replace(&pin.llama_cpp_commit, &"f".repeat(40));
    let (_dir, binary) = installed(Some(&as_built(&other)));
    let err = verify(&binary, &pin).unwrap_err();
    assert!(err.contains("llama.cpp fffffffff"), "{err}");
    assert!(err.contains(&pin.llama_cpp_commit[..9]), "{err}");
}

#[test]
fn a_changed_patch_or_llguidance_is_refused() {
    let pin = BuildPin::compiled().unwrap();
    let digest = &pin.patches[0].sha256;
    for other in [
        COMPILED_PIN.replace(digest.as_str(), &"0".repeat(64)),
        COMPILED_PIN.replace("llguidance = true", "llguidance = false"),
    ] {
        assert_ne!(other, COMPILED_PIN, "the fixture changed nothing");
        let (_dir, binary) = installed(Some(&as_built(&other)));
        assert!(verify(&binary, &pin).is_err());
    }
}

#[test]
fn the_pin_ships_the_router_and_the_rpc_server() {
    let binaries = BuildPin::compiled().unwrap().binaries;
    assert_eq!(binaries, ["llama-server", "ggml-rpc-server"]);
}

#[test]
fn a_named_binary_missing_beside_the_manifest_is_refused() {
    let (dir, binary) = installed(Some(&as_built(COMPILED_PIN)));
    std::fs::remove_file(dir.path().join("ggml-rpc-server")).unwrap();
    let err = verify(&binary, &BuildPin::compiled().unwrap()).unwrap_err();
    assert!(err.contains("names ggml-rpc-server"), "{err}");
    assert!(err.contains("build.sh"), "{err}");
}

#[test]
fn an_unparseable_manifest_is_refused() {
    let (_dir, binary) = installed(Some("llama_cpp_commit = "));
    let err = verify(&binary, &BuildPin::compiled().unwrap()).unwrap_err();
    assert!(err.contains("does not parse"), "{err}");
}
