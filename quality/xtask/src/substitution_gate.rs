// SPDX-License-Identifier: AGPL-3.0-or-later
//! substitution-gate — the identity/signature path defaults to NOTHING.
//!
//! Holds ROOT_CAUSE_FIXES C4 under ONE enforcement (principle 10): a silent
//! default on the path that mints ids, signs bytes or stamps time is the
//! §18.3 substitution with the worst blast radius — `""` signed is
//! catastrophe (`payload.rs` names it), `ts = 0` signed is a lie on the
//! wire. The code fixes made each site named (`expect` with the reason, a
//! `RailError::Clock` refusal); this gate is what keeps the next
//! `unwrap_or_default` from creeping back.
//!
//! Scanned: the three files whose defaults ARE identity (the id preimage,
//! the signed body, the signature timestamp). Not a workspace scan — this is
//! a rule about one path, and a path-only rule enforced everywhere is noise.

use crate::common;

/// The identity/signature path. A default in these files is a substitution
/// unless it is listed below.
const PATH: &[&str] = &[
    "shared/crates/commonwealth-rail-core/src/admit.rs",
    "oplog-types/src/lib.rs",
    "cmnwlth/crates/commonwealth-rail/src/journal.rs",
];

/// Documented semantic defaults — named here so the gate cannot be read as
/// forbidding them, with the reason each is NOT a substitution.
const SEMANTIC_DEFAULTS: &[&str] = &[
    // "an actor absent from it starts at zero" — the floors map's documented
    // bottom, not a swallowed failure.
    "floors.get(",
];

const TOKENS: &[&str] = &["unwrap_or_default", "unwrap_or(0)"];

fn hits(line: &str) -> usize {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") || trimmed.starts_with("///") {
        return 0;
    }
    if SEMANTIC_DEFAULTS.iter().any(|s| line.contains(s)) {
        return 0;
    }
    TOKENS.iter().map(|t| line.matches(t).count()).sum()
}

pub fn run(_args: &[String]) -> i32 {
    let root = common::repo_root();
    let mut found = 0;
    for rel in PATH {
        let path = root.join(rel);
        let Ok(text) = std::fs::read_to_string(&path) else {
            eprintln!("substitution-gate: COULD-NOT-JUDGE — cannot read {rel}");
            return 3;
        };
        for (n, line) in text.lines().enumerate() {
            let h = hits(line);
            if h > 0 {
                found += h;
                eprintln!(
                    "  ✗ {rel}:{} — silent default on the identity path: {}",
                    n + 1,
                    line.trim()
                );
            }
        }
    }
    if found == 0 {
        eprintln!("substitution-gate PASSED — the identity/signature path defaults to nothing");
        0
    } else {
        eprintln!(
            "substitution-gate FAILED ({found} site(s)) — name the failure, never default it"
        );
        1
    }
}
