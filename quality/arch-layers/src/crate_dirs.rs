// SPDX-License-Identifier: AGPL-3.0-or-later
//! `[crate_dirs]` — which top-level directory each workspace member lives in.
//!
//! Since the top-level-programs move (2026-10-02) the top level is one
//! directory per program, so a crate's directory says which program owns it.
//! Nothing held the two together: a crate added under `svrn/` and declared in
//! `serve`'s row, or a new member in no row at all, built and passed every
//! gate. Here every member must be claimed by a row, and the row names the
//! top-level directory its crates live under:
//!
//! - a `[[package]]` row: its own `name`, since a program and its directory
//!   are one word (`svrn`, `serve`, `cmnwlth`, `ingest`, `code`, `bench`);
//! - a `[[package_leaf]]`: `package_leaf` (`shared`);
//! - a `[[distribution]]` crate: `distribution` (`distributions`);
//! - `client_crates`: `clients`; `tooling_crates`: `tooling` (`quality`).
//!
//! When two kinds claim one crate, the first of tooling, client,
//! distribution, package and leaf decides. `arch-layers` is a shared leaf by
//! its closure and the quality program's own parser by its home, so it lives
//! in `quality/`.
//!
//! The unit is the TOP-LEVEL directory. Inside it `<dir>/crates/<crate>` is
//! the convention, not the rule: a client sits at `clients/<name>/...`.

use serde::Deserialize;

use crate::{LayerMap, Violation};

/// The `[crate_dirs]` table: a top-level directory per row kind, and the two
/// crate lists that belong to no program, leaf set or distribution.
#[derive(Debug, Deserialize)]
pub struct CrateDirs {
    pub package_leaf: String,
    pub distribution: String,
    pub clients: String,
    pub tooling: String,
    /// Apps over the programs, in no package (desktop, mobile, studio).
    #[serde(default)]
    pub client_crates: Vec<String>,
    /// The quality program's own crates: the gates and the map's parser.
    #[serde(default)]
    pub tooling_crates: Vec<String>,
}

pub(crate) fn validate(map: &LayerMap) -> Result<(), String> {
    // The vacuity argument once more: v6 exists to carry this rule, and a map
    // without the table would place nothing and say so in the words of a
    // clean run.
    let Some(dirs) = &map.crate_dirs else {
        if map.schema_version >= 6 {
            return Err("ARCH_LAYERS.toml declares schema_version >= 6 but no \
                        [crate_dirs] table. v6 exists to hold every member to its \
                        program's directory, and without the table that rule would \
                        judge nothing. Declare it, or drop back to schema_version = 5."
                .to_string());
        }
        return Ok(());
    };
    for (key, dir) in [
        ("package_leaf", &dirs.package_leaf),
        ("distribution", &dirs.distribution),
        ("clients", &dirs.clients),
        ("tooling", &dirs.tooling),
    ] {
        if dir.is_empty() || dir.contains('/') {
            return Err(format!(
                "[crate_dirs] {key} = {dir:?}: a home is one top-level directory name"
            ));
        }
    }
    Ok(())
}

/// The top-level directory `name` must live under, and the row that says so.
/// `None` when no row claims it.
pub fn home_of<'a>(map: &'a LayerMap, name: &str) -> Option<(&'a str, &'static str)> {
    let dirs = map.crate_dirs.as_ref()?;
    let has = |list: &[String]| list.iter().any(|c| c == name);
    if has(&dirs.tooling_crates) {
        return Some((&dirs.tooling, "[crate_dirs] tooling_crates"));
    }
    if has(&dirs.client_crates) {
        return Some((&dirs.clients, "[crate_dirs] client_crates"));
    }
    if map.distributions.iter().any(|d| has(&d.crates)) {
        return Some((&dirs.distribution, "[[distribution]]"));
    }
    if let Some(p) = map.packages.iter().find(|p| has(&p.crates)) {
        return Some((&p.name, "[[package]]"));
    }
    if map.package_leaves.iter().any(|l| l.name == name) {
        return Some((&dirs.package_leaf, "[[package_leaf]]"));
    }
    None
}

/// Every member no row claims, and every member outside the top-level
/// directory its row names. `members` is `(package name, repo-relative dir)`.
/// A map from before v6 has no table, and the rule did not exist for it.
pub fn evaluate_crate_dirs(map: &LayerMap, members: &[(String, String)]) -> Vec<Violation> {
    if map.crate_dirs.is_none() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (name, dir) in members {
        let top = dir.split('/').next().unwrap_or_default();
        match home_of(map, name) {
            None => out.push(Violation::UnhomedCrate {
                name: name.clone(),
                dir: dir.clone(),
            }),
            Some((want, row)) if top != want => out.push(Violation::MisplacedCrate {
                name: name.clone(),
                dir: dir.clone(),
                want: want.to_string(),
                row: row.to_string(),
            }),
            Some(_) => {}
        }
    }
    out
}

#[cfg(test)]
mod crate_dirs_tests {
    use super::*;
    use crate::parse;

    const MAP: &str = r#"
schema_version = 6
backstage = ["xtask"]
[[layer]]
name = "all"
crates = ["*"]
[[package_leaf]]
name = "time"
allow = []
[[package_leaf]]
name = "arch-layers"
allow = []
[[package]]
name = "svrn"
doc = "d"
crates = ["daemon"]
[[package]]
name = "serve"
doc = "d"
crates = ["server"]
[thin_surfaces]
crates = ["desktop"]
may_not_reach = ["daemon"]
doc = "d"
bring_up_decider = { krate = "turn-client", file = "x/reach.rs", reason = "one decider" }
[[distribution]]
name = "stock"
crates = ["stock"]
doc = "d"
max_code_lines = 300
[crate_dirs]
package_leaf = "shared"
distribution = "distributions"
clients = "clients"
tooling = "quality"
client_crates = ["desktop"]
tooling_crates = ["xtask", "arch-layers"]
"#;

    fn members(rows: &[(&str, &str)]) -> Vec<(String, String)> {
        rows.iter()
            .map(|(n, d)| (n.to_string(), d.to_string()))
            .collect()
    }

    fn at_home() -> Vec<(String, String)> {
        members(&[
            ("daemon", "svrn/crates/daemon"),
            ("server", "serve/crates/server"),
            ("time", "shared/crates/time"),
            ("stock", "distributions/crates/stock"),
            ("desktop", "clients/desktop/src-tauri"),
            ("xtask", "quality/xtask"),
            ("arch-layers", "quality/arch-layers"),
        ])
    }

    #[test]
    fn every_member_at_its_rows_home_passes() {
        let map = parse(MAP).unwrap();
        assert_eq!(evaluate_crate_dirs(&map, &at_home()), vec![]);
    }

    #[test]
    fn a_package_crate_under_another_programs_dir_is_named() {
        let map = parse(MAP).unwrap();
        let mut m = at_home();
        m[1].1 = "svrn/crates/server".to_string();
        assert_eq!(
            evaluate_crate_dirs(&map, &m),
            vec![Violation::MisplacedCrate {
                name: "server".into(),
                dir: "svrn/crates/server".into(),
                want: "serve".into(),
                row: "[[package]]".into(),
            }]
        );
    }

    #[test]
    fn a_member_no_row_claims_is_named() {
        let map = parse(MAP).unwrap();
        let mut m = at_home();
        m.push(("stray".into(), "svrn/crates/stray".into()));
        assert_eq!(
            evaluate_crate_dirs(&map, &m),
            vec![Violation::UnhomedCrate {
                name: "stray".into(),
                dir: "svrn/crates/stray".into(),
            }]
        );
    }

    #[test]
    fn the_tooling_home_outranks_the_leaf_home() {
        let map = parse(MAP).unwrap();
        assert_eq!(home_of(&map, "arch-layers").map(|h| h.0), Some("quality"));
        let mut m = at_home();
        m[6].1 = "shared/crates/arch-layers".to_string();
        assert_eq!(evaluate_crate_dirs(&map, &m).len(), 1);
    }

    #[test]
    fn a_v6_map_without_the_table_is_refused() {
        let without = MAP.split("[crate_dirs]").next().unwrap();
        let err = parse(without).unwrap_err();
        assert!(err.contains("no [crate_dirs] table"), "{err}");
    }

    #[test]
    fn a_home_must_be_one_top_level_dir() {
        let bad = MAP.replace(
            r#"package_leaf = "shared""#,
            r#"package_leaf = "shared/crates""#,
        );
        let err = parse(&bad).unwrap_err();
        assert!(err.contains("one top-level directory"), "{err}");
    }
}
