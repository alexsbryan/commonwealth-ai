// SPDX-License-Identifier: AGPL-3.0-or-later
//! `distribute`'s attribution reference: the rev a run pins and what a local run at it is attributed to. A sibling of
//! `distribute.rs` only so that file stays under ARCH §3.1's approach band
//! (pb-work-doors) — moved verbatim.

use super::*;

/// The rev of `repo`'s working checkout — what a LOCAL run of these lanes
/// would be a verdict about.
pub(in super::super) fn head_rev(repo: &Path) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .map_err(|e| format!("cannot run git in {}: {e}", repo.display()))?;
    if !out.status.success() {
        return Err(format!(
            "`git rev-parse HEAD` failed in {} — a distributed run pins its units to a revision, \
             and a checkout with no HEAD has none to pin",
            repo.display()
        ));
    }
    let rev = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if rev.is_empty() {
        return Err("`git rev-parse HEAD` printed nothing".to_string());
    }
    // A DIRTY TREE HAS NO REVISION THAT NAMES ITS BYTES, and until 2026-09-10
    // this function handed one out anyway. That was a FALSE ACCEPT in the field
    // built to prevent exactly it: the submitter pins HEAD, the donor checks
    // that same rev out into a clean worktree, both attributions carry the
    // identical sha, `ComputeAttribution::comparable_to` sees four fields agree
    // — and a verdict about DIFFERENT BYTES is adopted as evidence about this
    // checkout. Well-formed green about a tree nobody tested, which is the one
    // failure this whole comparability apparatus exists to stop
    // (WORK_PLANE.md's 5e bar iii, ARCH §18.1's "assert on something the
    // subject cannot author").
    //
    // Refused rather than marked. A `<sha>-dirty` rev would also fail to
    // match, but it would fail LATER and per-unit, as a donor refusing a rev it
    // cannot resolve — burning a whole run to say what is knowable before the
    // first act is signed. And it is refused HERE rather than in
    // `run_distributed` because both callers of this function are the
    // distributed path (the other is `--dry-run`, which prints the reason the
    // real run will refuse), so a future third caller inherits the rule instead
    // of having to remember it (§7).
    let dirt = uncommitted(repo)?;
    if !dirt.is_empty() {
        let shown: Vec<&str> = dirt.iter().take(3).map(String::as_str).collect();
        return Err(format!(
            "this checkout has {} uncommitted change(s) ({}{}) — a donor fetches COMMITTED bytes, \
             so every unit would be pinned to {} while the tree under test is something else, and \
             the donor's verdict would compare equal to a local run it is not about. Commit or \
             stash first; a local (non-distributed) run has no such requirement because it tests \
             the tree in front of it",
            dirt.len(),
            shown.join(", "),
            if dirt.len() > shown.len() {
                ", …"
            } else {
                ""
            },
            &rev[..rev.len().min(12)]
        ));
    }
    Ok(rev)
}

/// The paths `git` reports as not matching `HEAD` — modified, staged, renamed,
/// deleted or untracked-and-not-ignored.
///
/// Untracked files COUNT, and that is the whole reason this reads `status`
/// rather than `diff --quiet`: a new untracked `#[test]` changes the test count
/// a distributed run is about to compare, which is the exact number D2's bar
/// reads. Ignored files do not count, and `--porcelain` already excludes them.
fn uncommitted(repo: &Path) -> Result<Vec<String>, String> {
    let out = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo)
        .output()
        .map_err(|e| format!("cannot run git in {}: {e}", repo.display()))?;
    if !out.status.success() {
        // An unreadable status is not a clean tree. Absence is reported, never
        // defaulted into the permissive answer (§18.3).
        return Err(format!(
            "`git status --porcelain` failed in {} — a distributed run cannot confirm the tree it \
             would pin is the tree it is about",
            repo.display()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.get(3..).map(str::to_string))
        .filter(|p| !p.trim().is_empty())
        .collect())
}

/// What a LOCAL run at `repo_rev` would have been attributed to.
///
/// The reference every donor's `provenance` is checked against, so that "a
/// verdict that is not yours" is a typed question rather than a footnote.
///
/// **THE IMAGE IS THE CI ENVIRONMENT, so the reference is read from the same
/// boundary a donor uses** (operator, 2026-09-10, on what a distributed verdict
/// is comparable to — the ordinary answer anyone who has run CI would give).
/// This read `of_this_host` until then, and the consequence was not subtle: a
/// containerized donor reports the image's compiler (`rustc 1.97.1` here) while
/// the reference reported the laptop's (`1.95.0`), the toolchain field differed,
/// and `comparable_to` refused EVERY donor's verdict — including this node's own.
/// The distributed path could not have adopted a single row.
///
/// Read through cw-rails' attribution door (pb-work-doors), which runs
/// `Sandbox::probe` on the image this host declares and reads the compiler
/// itself, exactly as a donor does on its own machine — shared method,
/// separate values (§18.1). With no image declared the probe reports `Direct`
/// and the reference is this host, because a unit would have run natively too.
/// `[compute.work_offer] image` is this node's until pb-work-donor moves the
/// section to cw-rails.
pub(in super::super) async fn local_attribution(
    rails_base: &str,
    repo_rev: &str,
    image: Option<&str>,
) -> Result<ComputeAttribution, String> {
    let mut query = vec![("repo_rev", repo_rev)];
    if let Some(image) = image {
        query.push(("image", image));
    }
    let answer = rails_get(rails_base, WORK_ATTRIBUTION_PATH, &query)
        .await
        .map_err(|e| format!("cw-rails could not say what a local run is attributed to: {e}"))?;
    serde_json::from_value(answer)
        .map_err(|e| format!("cw-rails' attribution is a shape this build cannot read: {e}"))
}

/// Which fields of a donor's attribution do not match this checkout's.
///
/// `comparable_to` is the DECIDER — this only runs once it has already said no,
/// and only to name which halves differ, the same shape
/// `commonwealth_work::refusal`'s `UnmetRequirement` selection uses.
pub(super) fn incomparable_fields(
    mine: &ComputeAttribution,
    theirs: &ComputeAttribution,
) -> Vec<String> {
    let mut out = Vec::new();
    // A field is incomparable two ways, and only the first used to be
    // reported: the values DIFFER, or they agree on a named absence. The
    // second is not a corner case — it is what every host without `rustc` on
    // `PATH` produces, on both sides at once, and before this the row refused
    // correctly and then named nothing, rendering as "not about — ." with a
    // dangling dash. A refusal that cannot say which field it refused on is
    // the absence-shaped half of ARCH §18.3.
    let mut check = |field: &str, mine: &str, theirs: &str, shorten: bool| {
        let render = |v: &str| {
            if shorten {
                short(v)
            } else {
                v.to_string()
            }
        };
        if mine != theirs {
            out.push(format!(
                "{field} `{}` here against `{}` there",
                render(mine),
                render(theirs)
            ));
        } else if kernel_types::is_absent_marker(mine) {
            out.push(format!(
                "neither host could read its {field} (`{}`), so the two are not \
                 evidence about each other",
                render(mine)
            ));
        }
    };
    check("rev", &mine.repo_rev, &theirs.repo_rev, true);
    check("os", &mine.os, &theirs.os, false);
    check("arch", &mine.arch, &theirs.arch, false);
    check("toolchain", &mine.toolchain, &theirs.toolchain, false);
    out
}
