// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the shipped llama-server must have been built from, and the check
//! that it was.
//!
//! `llama-server/PIN.toml` in this crate names the llama.cpp commit, whether
//! llguidance is on, and each patch by its sha256. This crate compiles it in;
//! `llama-server/build.sh` copies it into `llama-server.manifest.toml`
//! beside the binary it installs. [`verify`] compares the two, because the
//! binary cannot say what it is: its `--version` names whatever git repo the
//! build ran in.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The source of the shipped server: `PIN.toml`, `build.sh` and `patches/`.
/// Inside this crate, so the package carries the recipe for the binary it runs.
pub const SERVER_SOURCE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/llama-server");

/// `PIN.toml` as this binary was compiled with it.
const COMPILED_PIN: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/llama-server/PIN.toml"
));

/// The manifest's name, beside the binary.
pub const MANIFEST: &str = "llama-server.manifest.toml";

/// A llama-server build, as the pin declares it and the manifest records it.
/// The manifest's `[build]` table is the build's own record and is not
/// compared.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BuildPin {
    pub llama_cpp_commit: String,
    pub llguidance: bool,
    #[serde(default)]
    pub patches: Vec<PinnedPatch>,
}

/// One patch applied on top of the commit, in order.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PinnedPatch {
    pub file: String,
    pub sha256: String,
}

impl BuildPin {
    /// The pin this binary was compiled with.
    pub fn compiled() -> Result<Self, String> {
        Self::parse(COMPILED_PIN, "the compiled llama-server/PIN.toml")
    }

    fn parse(text: &str, what: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| format!("{what} does not parse: {e}"))
    }
}

impl fmt::Display for BuildPin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let commit = self
            .llama_cpp_commit
            .get(..9)
            .unwrap_or(&self.llama_cpp_commit);
        let llg = if self.llguidance { "on" } else { "off" };
        write!(f, "llama.cpp {commit}, llguidance {llg}, patches [")?;
        for (i, p) in self.patches.iter().enumerate() {
            let digest = p.sha256.get(..8).unwrap_or(&p.sha256);
            let sep = if i == 0 { "" } else { ", " };
            write!(f, "{sep}{} {digest}", p.file)?;
        }
        write!(f, "]")
    }
}

/// The manifest beside `binary`.
pub fn manifest_path(binary: &Path) -> PathBuf {
    binary.with_file_name(MANIFEST)
}

/// Refuse a binary whose manifest is absent, unreadable, or names another
/// build than `pin`. The refusal says what each side holds and how to build
/// the right one.
pub fn verify(binary: &Path, pin: &BuildPin) -> Result<(), String> {
    let path = manifest_path(binary);
    let hint = format!("hint: build it with {SERVER_SOURCE_DIR}/build.sh");
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "{} has no readable manifest at {} ({e}), so what it was built from is unknown\n{hint}",
            binary.display(),
            path.display()
        )
    })?;
    let built = BuildPin::parse(&text, &path.display().to_string())?;
    if &built != pin {
        return Err(format!(
            "{} was built from {built}; this daemon was compiled against {pin}\n{hint}",
            binary.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "build_pin_tests.rs"]
mod tests;
