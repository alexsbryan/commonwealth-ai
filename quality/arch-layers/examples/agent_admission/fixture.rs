// SPDX-License-Identifier: AGPL-3.0-or-later
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::Path};

pub const POLICY: &str = include_str!("../../tests/fixtures/agent-admission/policy.toml");
pub const TASK: &str = "Wire ask-app's missing formatter dependency so answer() returns answer: 42. svrn owns ask-app and ask-format; serve owns model-host; wire is shared. Preserve those boundaries.";
pub const PACKAGES: [&str; 4] = ["ask-app", "ask-format", "model-host", "wire"];

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn write(root: &Path, formatter: &str) -> Result<(), String> {
    fs::create_dir(root).map_err(|e| e.to_string())?;
    let app = include_str!("../../tests/fixtures/agent-admission/ask-app.rs");
    let render = include_str!("../../tests/fixtures/agent-admission/render.rs");
    let wire = include_str!("../../tests/fixtures/agent-admission/wire.rs");
    fs::write(
        root.join("Cargo.toml"),
        include_str!("../../tests/fixtures/agent-admission/workspace.toml.in"),
    )
    .map_err(|e| e.to_string())?;
    for name in PACKAGES {
        fs::create_dir_all(root.join(name).join("src")).map_err(|e| e.to_string())?;
        let mut manifest = format!("[package]\nname = {name:?}\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n");
        if name != "wire" {
            manifest.push_str("wire = { path = \"../wire\" }\n");
        }
        if name == "ask-app" && formatter != "none" {
            manifest.push_str(&format!(
                "formatter = {{ package = {formatter:?}, path = \"../{formatter}\" }}\n"
            ));
        }
        fs::write(root.join(name).join("Cargo.toml"), manifest).map_err(|e| e.to_string())?;
        fs::write(
            root.join(name).join("src/lib.rs"),
            match name {
                "ask-app" => app,
                "wire" => wire,
                _ => render,
            },
        )
        .map_err(|e| e.to_string())?;
    }
    // A controller-generated lockfile keeps Cargo from mutating checked inputs.
    let mut lock = String::from("version = 4\n");
    for name in PACKAGES {
        lock.push_str(&format!(
            "\n[[package]]\nname = {name:?}\nversion = \"0.0.0\"\n"
        ));
        let mut deps = Vec::new();
        if name != "wire" {
            deps.push("wire");
        }
        if name == "ask-app" && formatter != "none" && formatter != "wire" {
            deps.push(formatter);
        }
        deps.sort();
        if !deps.is_empty() {
            lock.push_str(&format!("dependencies = {deps:?}\n"));
        }
    }
    fs::write(root.join("Cargo.lock"), lock).map_err(|e| e.to_string())
}

pub fn digest_tree(root: &Path) -> Result<String, String> {
    if !fs::symlink_metadata(root)
        .map_err(|e| e.to_string())?
        .is_dir()
    {
        return Err("snapshot root is not a real directory".into());
    }
    fn collect(
        root: &Path,
        dir: &Path,
        files: &mut BTreeMap<String, Vec<u8>>,
    ) -> Result<(), String> {
        for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if metadata.is_dir() {
                collect(root, &path, files)?;
            } else if metadata.is_file() {
                files.insert(
                    path.strip_prefix(root)
                        .map_err(|e| e.to_string())?
                        .to_string_lossy()
                        .into_owned(),
                    fs::read(&path).map_err(|e| e.to_string())?,
                );
            } else {
                return Err("snapshot contains a symlink or special file".into());
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    collect(root, root, &mut files)?;
    let mut h = Sha256::new();
    for (name, bytes) in files {
        h.update((name.len() as u64).to_be_bytes());
        h.update(name.as_bytes());
        h.update((bytes.len() as u64).to_be_bytes());
        h.update(bytes);
    }
    Ok(format!("{:x}", h.finalize()))
}
