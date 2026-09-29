// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exec dispatch into `serve`'s binary, `sovereign-serve`, for the weight
//! verbs it owns under their `svrn mesh` spelling (phase-b-22). Same shape as
//! `mesh_bin::exec`.

use std::path::PathBuf;

const BIN_NAME: &str = "sovereign-serve";

/// The `svrn mesh` subcommands serve owns. Keep in step with
/// `sovereign_serve::WEIGHT_VERBS`, the list that binary routes.
pub const MESH_VERBS: &[&str] = &["warm-cache", "fetch-model", "fetch-ner", "plan", "bench"];

/// The serve verb `svrn <first> <rest…>` names, if it names one.
pub fn mesh_verb<'a>(first: &str, rest: &'a [String]) -> Option<&'a str> {
    if first != "mesh" {
        return None;
    }
    rest.first()
        .map(String::as_str)
        .filter(|v| MESH_VERBS.contains(v))
}

fn locate() -> Option<PathBuf> {
    sovereign_turn_client::reach::locate_sibling(BIN_NAME, "SOVEREIGN_SERVE_BIN")
}

pub fn exec(verb: &str, args: &[String]) -> i32 {
    let Some(bin) = locate() else {
        eprintln!(
            "svrn mesh {verb}: owned by serve, whose binary '{BIN_NAME}' was not \
             found. Build it with `cargo build -p sovereign-serve`, or set \
             SOVEREIGN_SERVE_BIN to its path."
        );
        return 127;
    };

    crate::sibling::warn_if_stale(&bin, BIN_NAME);

    crate::sibling::exec_into(&bin, verb, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn both_weight_spellings_route_to_serve() {
        assert_eq!(
            mesh_verb("mesh", &args(&["warm-cache", "m.gguf"])),
            Some("warm-cache")
        );
        assert_eq!(
            mesh_verb("mesh", &args(&["fetch-model", "m.gguf"])),
            Some("fetch-model")
        );
        assert_eq!(mesh_verb("mesh", &args(&["fetch-ner"])), Some("fetch-ner"));
        assert_eq!(mesh_verb("mesh", &args(&["plan", "m.gguf"])), Some("plan"));
        assert_eq!(mesh_verb("mesh", &args(&["bench"])), Some("bench"));
    }

    #[test]
    fn every_other_mesh_spelling_stays_with_cmnwlth() {
        for sub in ["status", "--help", "join", "check-invariants"] {
            assert_eq!(mesh_verb("mesh", &args(&[sub])), None, "{sub}");
        }
        assert_eq!(mesh_verb("mesh", &[]), None);
        assert_eq!(mesh_verb("ring", &args(&["warm-cache"])), None);
    }
}
