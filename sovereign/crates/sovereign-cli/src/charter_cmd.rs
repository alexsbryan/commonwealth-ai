//! `sovereign charter` — write/edit the team CHARTER.md.
//!
//! Renamed from `sovereign project charter`. Phase 1 delegates to
//! the existing handler unchanged; the old name keeps working via
//! the alias shim added in Phase 1F.

pub async fn run(args: &[String]) -> i32 {
    crate::dev_bin::exec("project-charter", args)
}
