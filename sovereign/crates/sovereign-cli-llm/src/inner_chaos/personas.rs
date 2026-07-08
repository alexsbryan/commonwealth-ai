// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fixture loading for the inner-work chaos harness.
//!
//! Two fixture files, both under `sovereign/bench/inner_work/`:
//!
//! - `personas.toml` — the adversarial persona bank. Each persona is
//!   a distinct pressure on the witness (crisis disclosure, boundary
//!   testing, false premises, …) and sets the brain's system prompt.
//! - `memories.toml` — the resident memory fixtures every thread
//!   seeds before turn 1, so runs are comparable across iterations
//!   (the inner-work analogue of the knowledge harness's fixed
//!   resident-corpus set).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::voice_eval::scenarios::SeedMemory;

/// One adversarial persona from `personas.toml`. Mirrors the file's
/// documented shape: `system` becomes the brain's system prompt,
/// `turns` is the thread length, `probes` names the rubric lines the
/// persona pressures (report metadata, not behaviour), `escalate`
/// guides the arc, and `control = true` marks the non-adversarial
/// baseline.
#[derive(Debug, Clone, Deserialize)]
pub struct Persona {
    pub id: String,
    pub turns: usize,
    #[serde(default)]
    pub probes: Vec<String>,
    pub system: String,
    #[serde(default)]
    pub escalate: String,
    #[serde(default)]
    pub control: bool,
}

#[derive(Debug, Deserialize)]
struct PersonaBank {
    #[serde(rename = "persona", default)]
    personas: Vec<Persona>,
}

#[derive(Debug, Deserialize)]
struct MemoryFixture {
    #[serde(default)]
    memories: BTreeMap<String, SeedMemory>,
}

pub fn load_personas(path: &Path) -> Result<Vec<Persona>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("read personas file {}: {e}", path.display()))?;
    let bank: PersonaBank =
        toml::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))?;
    if bank.personas.is_empty() {
        return Err(format!("no [[persona]] entries in {}", path.display()));
    }
    for p in &bank.personas {
        if p.turns == 0 {
            return Err(format!("persona `{}` declares turns = 0", p.id));
        }
    }
    Ok(bank.personas)
}

/// Load the resident memory fixtures. Returned as `(key, seed)` pairs
/// in the file's BTreeMap order so seeding is deterministic.
pub fn load_memories(path: &Path) -> Result<BTreeMap<String, SeedMemory>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("read memories file {}: {e}", path.display()))?;
    let fixture: MemoryFixture =
        toml::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))?;
    Ok(fixture.memories)
}

/// Default fixture directory, resolved by walking up from CWD —
/// same discipline as voice_eval's scenarios-dir resolution.
pub fn resolve_bench_dir(explicit: Option<&PathBuf>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        return if p.is_dir() {
            Ok(p.clone())
        } else {
            Err(format!("--bench-dir `{}` is not a directory", p.display()))
        };
    }
    let mut here =
        std::env::current_dir().map_err(|e| format!("cannot resolve current dir: {e}"))?;
    loop {
        for prefix in ["sovereign/bench/inner_work", "bench/inner_work"] {
            let candidate = here.join(prefix);
            if candidate.is_dir() {
                return Ok(candidate);
            }
        }
        if !here.pop() {
            break;
        }
    }
    Err("could not find `bench/inner_work` walking up from CWD. Pass --bench-dir.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed persona bank must load — this pins the fixture
    /// file's shape to the loader for as long as both live in-tree.
    #[test]
    fn committed_personas_fixture_loads() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bench/inner_work");
        let personas = load_personas(&dir.join("personas.toml")).expect("personas.toml loads");
        assert!(personas.len() >= 6, "expected the full persona bank");
        let control: Vec<_> = personas.iter().filter(|p| p.control).collect();
        assert_eq!(control.len(), 1, "exactly one control persona");
        assert_eq!(control[0].id, "reflective_control");
        let crisis = personas
            .iter()
            .find(|p| p.id == "crisis_discloser")
            .expect("crisis_discloser present");
        assert!(crisis.probes.iter().any(|p| p == "crisis_mishandling"));
        assert!(crisis.turns >= 3);
    }

    #[test]
    fn committed_memories_fixture_loads() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bench/inner_work");
        let memories = load_memories(&dir.join("memories.toml")).expect("memories.toml loads");
        assert!(memories.len() >= 4);
        // The theme-only grief memory is load-bearing for the
        // privacy_prober persona — it must stay present.
        assert!(memories.contains_key("hard_night_march"));
        for (key, seed) in &memories {
            assert!(!seed.content.is_empty(), "memory `{key}` has empty content");
            assert!((0.0..=1.0).contains(&seed.confidence));
        }
    }

    #[test]
    fn empty_persona_bank_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("personas.toml");
        std::fs::write(&path, "# no personas\n").unwrap();
        let err = load_personas(&path).unwrap_err();
        assert!(err.contains("no [[persona]]"));
    }

    #[test]
    fn zero_turn_persona_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("personas.toml");
        std::fs::write(
            &path,
            "[[persona]]\nid = \"x\"\nturns = 0\nsystem = \"s\"\n",
        )
        .unwrap();
        let err = load_personas(&path).unwrap_err();
        assert!(err.contains("turns = 0"));
    }
}
