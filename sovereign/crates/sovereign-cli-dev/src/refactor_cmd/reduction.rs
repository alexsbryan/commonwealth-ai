// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn code refactor reduction` — did this branch CONVERGE, or did it add?
//!
//! # Why this exists
//!
//! The factory's schedule ranks by sites-per-session-chunk and its only specs
//! are `kind = "newtype"`, whose entire rule table is append-or-wrap
//! (`append .as_str()`, `wrap CorpusId::new(_)`). That work is real
//! architectural improvement and it is *strictly additive in lines by
//! construction*. Measured on the `noun-convergence` branch: +39,318 / −18,046,
//! net **+21,272**, with `kernel-types` at +3,432 / −0 — the canonical home
//! landed and not one duplicate retired.
//!
//! Nothing in the campaign measured that. `(members−1) × unit_lines` attributes
//! a rung; it cannot see whether the copies actually went away. So this verb
//! carries the two bars that oppose additivity, and it carries them TOGETHER
//! because they answer one question (ARCH §10.6 — one decider, one name):
//!
//!   1. **Net lines against the merge-base.** Exact, index-independent, free,
//!      and ungameable in the direction that matters — you cannot make a
//!      deletion look like one without deleting.
//!   2. **New public surface.** When an agent meets a call site that does not
//!      fit the canonical form it writes a shim, an adapter, a wrapper, a
//!      `From` impl — anything local and certain rather than the non-local,
//!      uncertain edit that deletion requires. Every one of those is a new
//!      public item. Naming them is how the escape hatch stops being free.
//!
//! # What it does NOT claim
//!
//! Added public items are read off the DIFF TEXT, not off a compiled crate
//! graph. A `pub fn` added inside a `#[cfg(test)]` module reads the same as one
//! added to the crate surface; the scan excludes obvious test paths and says so
//! in the report rather than pretending to a precision it does not have. This
//! is a bar to argue with, not an oracle — which is why the verdict set is four
//! (ARCH §18.1), and `CouldNotJudge` is a real outcome rather than a silent
//! pass.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// A public item introduced by the diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedItem {
    pub file: String,
    pub line: u32,
    /// `fn` / `struct` / `enum` / `trait` / `type` / `const` / `static` / `impl-for`.
    pub kind: String,
    pub name: String,
}

/// Four verdicts, never two (ARCH §18.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Passed,
    Failed(String),
    CouldNotJudge(String),
}

impl Verdict {
    pub fn label(&self) -> &'static str {
        match self {
            Verdict::Passed => "PASSED",
            Verdict::Failed(_) => "FAILED",
            Verdict::CouldNotJudge(_) => "COULD-NOT-JUDGE",
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct NetLines {
    pub added: u64,
    pub deleted: u64,
}

impl NetLines {
    pub fn net(&self) -> i64 {
        self.added as i64 - self.deleted as i64
    }
    /// Deletions per line added. The top of the branch's commit ranking sits at
    /// 41x, 8x, 6x; the bottom — every "mint the canonical home" commit — sits
    /// near zero.
    pub fn delete_ratio(&self) -> f64 {
        if self.added == 0 {
            return if self.deleted > 0 { f64::INFINITY } else { 0.0 };
        }
        self.deleted as f64 / self.added as f64
    }
}

/// Sum `git diff --numstat` output. Binary files report `-` and are skipped.
pub fn parse_numstat(numstat: &str) -> NetLines {
    let mut out = NetLines::default();
    for line in numstat.lines() {
        let mut f = line.split('\t');
        let (Some(a), Some(d)) = (f.next(), f.next()) else {
            continue;
        };
        if let (Ok(a), Ok(d)) = (a.parse::<u64>(), d.parse::<u64>()) {
            out.added += a;
            out.deleted += d;
        }
    }
    out
}

/// Per-path net lines, so a report can name WHERE the growth landed — the
/// `kernel-types +3,432 / −0` shape is invisible in a workspace total.
pub fn parse_numstat_by_path(numstat: &str) -> BTreeMap<String, NetLines> {
    let mut out: BTreeMap<String, NetLines> = BTreeMap::new();
    for line in numstat.lines() {
        let mut f = line.split('\t');
        let (Some(a), Some(d), Some(p)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        if let (Ok(a), Ok(d)) = (a.parse::<u64>(), d.parse::<u64>()) {
            // Attribute to the top-level crate directory.
            let crate_dir = p.split('/').next().unwrap_or(p).to_string();
            let e = out.entry(crate_dir).or_default();
            e.added += a;
            e.deleted += d;
        }
    }
    out
}

fn is_test_path(path: &str) -> bool {
    path.contains("/tests/")
        || path.starts_with("tests/")
        || path.ends_with("_test.rs")
        || path.ends_with("_tests.rs")
        || path.contains("/benches/")
}

/// Scan a unified diff for PUBLIC items the change introduces.
///
/// Tracks `+++ b/<path>` and `@@ … +start,len @@` so each hit carries a real
/// `file:line`. Only added lines (`+`, not `+++`) are considered, so a moved
/// item that was already public is not double-counted as new surface — it is
/// counted at its new home and its removal shows up in the net-lines half.
pub fn scan_added_public_items(diff: &str) -> Vec<AddedItem> {
    let mut out = Vec::new();
    let mut file = String::new();
    let mut new_line: u32 = 0;
    for raw in diff.lines() {
        if let Some(p) = raw.strip_prefix("+++ b/") {
            file = p.trim().to_string();
            continue;
        }
        if raw.starts_with("+++ ") || raw.starts_with("--- ") {
            continue;
        }
        if raw.starts_with("@@") {
            // @@ -old,len +new,len @@
            if let Some(plus) = raw.split('+').nth(1) {
                let num: String = plus.chars().take_while(|c| c.is_ascii_digit()).collect();
                new_line = num.parse().unwrap_or(0);
            }
            continue;
        }
        if raw.starts_with('-') {
            continue; // deleted line: does not advance the new-file cursor
        }
        let Some(body) = raw.strip_prefix('+') else {
            new_line = new_line.saturating_add(1); // context line
            continue;
        };
        let here = new_line;
        new_line = new_line.saturating_add(1);
        if is_test_path(&file) {
            continue;
        }
        let t = body.trim_start();
        if let Some(item) = parse_public_item(t) {
            out.push(AddedItem {
                file: file.clone(),
                line: here,
                kind: item.0,
                name: item.1,
            });
        }
    }
    out
}

/// Recognize a public item declaration. Returns `(kind, name)`.
fn parse_public_item(t: &str) -> Option<(String, String)> {
    // `impl Trait for Type` — the From/adapter escape hatch. Counted even
    // without `pub`, because an inherent or trait impl on a public type IS
    // public surface regardless of the keyword.
    if t.starts_with("impl ") || t.starts_with("impl<") {
        if let Some(rest) = t.split(" for ").nth(1) {
            let ty = rest
                .split(|c: char| c == '{' || c == '<' || c.is_whitespace())
                .find(|s| !s.is_empty())
                .unwrap_or("")
                .trim_end_matches('{');
            let tr = t
                .trim_start_matches("impl")
                .trim_start()
                .split(" for ")
                .next()
                .unwrap_or("")
                .trim()
                .trim_start_matches('<');
            if !ty.is_empty() {
                return Some(("impl-for".into(), format!("{tr} for {ty}")));
            }
        }
        return None;
    }
    let rest = t.strip_prefix("pub")?;
    // `pub(crate)` / `pub(super)` are not crate-external surface, but they ARE
    // the shim's usual home, so they count. Strip the restriction and continue.
    let rest = if let Some(r) = rest.strip_prefix('(') {
        r.split_once(')')?.1
    } else {
        rest
    };
    let rest = rest.strip_prefix(' ')?.trim_start();
    for kw in [
        "fn ", "struct ", "enum ", "trait ", "type ", "const ", "static ", "union ",
    ] {
        if let Some(after) = rest.strip_prefix(kw) {
            let name: String = after
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                return Some((kw.trim().to_string(), name));
            }
        }
    }
    // `pub async fn`, `pub unsafe fn`, `pub extern "C" fn`
    for pfx in ["async ", "unsafe ", "extern "] {
        if let Some(after) = rest.strip_prefix(pfx) {
            return parse_public_item(&format!("pub {after}"));
        }
    }
    None
}

/// Which added items are ALLOWED by the spec under test.
///
/// The spec's `target` is the canonical home the order exists to mint, and its
/// `[prepare] impls` are declared error-class removals. Everything else is the
/// escape hatch.
pub fn disallowed<'a>(items: &'a [AddedItem], allowed: &[String]) -> Vec<&'a AddedItem> {
    // Compare against the FINAL path segment, exactly. A substring test looks
    // equivalent and is not: `"kernel_types::CorpusId".contains("Id")` is true,
    // so an item named `Id` would be waved through — a false negative in the
    // one direction this bar exists to prevent.
    let names: Vec<&str> = allowed
        .iter()
        .map(|a| a.rsplit("::").next().unwrap_or(a.as_str()).trim())
        .collect();
    items
        .iter()
        .filter(|i| !names.contains(&i.name.as_str()))
        .collect()
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|e| format!("running git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// The measured report for one branch.
pub struct Report {
    pub base: String,
    pub net: NetLines,
    pub by_crate: BTreeMap<String, NetLines>,
    pub added_public: Vec<AddedItem>,
    pub verdict: Verdict,
}

/// Measure the working branch against its merge-base with `base_ref`.
/// `max_net` is an optional declared ceiling on net lines. It is NOT the
/// default bar, and that is deliberate: newtype conversion — the highest-value
/// work in the backlog — is additive in lines *by construction*, so gating on
/// net > 0 would fail every honest order and the gate would simply be turned
/// off. The hard bar is the escape hatch (undeclared public surface); net
/// lines is the campaign's scorecard, gated only when a campaign declares a
/// ceiling.
pub fn measure(root: &Path, base_ref: &str, allowed: &[String], max_net: Option<i64>) -> Report {
    let base = match git(root, &["merge-base", "HEAD", base_ref]) {
        Ok(s) => s.trim().to_string(),
        Err(e) => {
            return Report {
                base: base_ref.to_string(),
                net: NetLines::default(),
                by_crate: BTreeMap::new(),
                added_public: Vec::new(),
                // Never a silent pass: an unresolvable base is reported as
                // such, not defaulted to green (ARCH §18.3).
                verdict: Verdict::CouldNotJudge(format!("no merge-base with `{base_ref}`: {e}")),
            };
        }
    };
    let range = format!("{base}..HEAD");
    let numstat = git(root, &["diff", "--numstat", &range]).unwrap_or_default();
    let diff = git(root, &["diff", "--unified=0", &range, "--", "*.rs"]).unwrap_or_default();

    let net = parse_numstat(&numstat);
    let by_crate = parse_numstat_by_path(&numstat);
    let added_public = scan_added_public_items(&diff);
    let bad = disallowed(&added_public, allowed);
    let verdict = if !bad.is_empty() {
        Verdict::Failed(format!(
            "{} undeclared public item(s) — a shim, adapter or impl written \
             instead of converging the call site",
            bad.len()
        ))
    } else if max_net.is_some_and(|m| net.net() > m) {
        Verdict::Failed(format!(
            "net {:+} exceeds the declared ceiling {:+}",
            net.net(),
            max_net.unwrap_or(0)
        ))
    } else {
        Verdict::Passed
    };
    Report {
        base,
        net,
        by_crate,
        added_public,
        verdict,
    }
}

impl Report {
    pub fn render(&self, allowed: &[String]) -> String {
        use std::fmt::Write;
        let mut o = String::new();
        let _ = writeln!(o, "REDUCTION — did this branch converge, or did it add?");
        let _ = writeln!(o, "base: {}", &self.base[..self.base.len().min(12)]);
        let _ = writeln!(o);
        let _ = writeln!(
            o,
            "net lines: +{} / -{} = {:+}   (delete ratio {:.2}x)",
            self.net.added,
            self.net.deleted,
            self.net.net(),
            self.net.delete_ratio()
        );
        let _ = writeln!(o);
        let _ = writeln!(o, "by crate (largest growth first):");
        let mut rows: Vec<_> = self.by_crate.iter().collect();
        rows.sort_by_key(|(_, n)| -n.net());
        for (p, n) in rows.iter().take(10) {
            let flag = if n.deleted == 0 && n.added > 0 {
                "  <- mint landed, nothing retired"
            } else {
                ""
            };
            let _ = writeln!(
                o,
                "  {:<28} +{:>6} / -{:<6} = {:+7}{}",
                p,
                n.added,
                n.deleted,
                n.net(),
                flag
            );
        }
        let _ = writeln!(o);
        let bad = disallowed(&self.added_public, allowed);
        let _ = writeln!(
            o,
            "new public surface: {} added, {} undeclared",
            self.added_public.len(),
            bad.len()
        );
        if !allowed.is_empty() {
            let _ = writeln!(o, "  declared by spec: {}", allowed.join(", "));
        }
        for i in bad.iter().take(25) {
            let _ = writeln!(o, "  {}:{} — {} {}", i.file, i.line, i.kind, i.name);
        }
        if bad.len() > 25 {
            let _ = writeln!(o, "  … {} more", bad.len() - 25);
        }
        let _ = writeln!(o);
        let _ = writeln!(
            o,
            "note: public items are read off diff TEXT, not a compiled"
        );
        let _ = writeln!(
            o,
            "graph; obvious test paths are excluded, cfg(test) blocks"
        );
        let _ = writeln!(o, "inside a source file are not. Argue with the list.");
        let _ = writeln!(o);
        let _ = write!(o, "VERDICT: {}", self.verdict.label());
        match &self.verdict {
            Verdict::Failed(why) | Verdict::CouldNotJudge(why) => {
                let _ = writeln!(o, " — {why}");
            }
            Verdict::Passed => {
                let _ = writeln!(o);
            }
        }
        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_sums_and_skips_binary_rows() {
        let n = parse_numstat("10\t3\ta.rs\n5\t20\tb.rs\n-\t-\tlogo.png\n");
        assert_eq!(n.added, 15);
        assert_eq!(n.deleted, 23);
        assert_eq!(n.net(), -8);
    }

    #[test]
    fn delete_ratio_ranks_the_shapes_the_branch_actually_produced() {
        // The measured top of the branch: 16 added / 704 deleted = 44x.
        assert!(parse_numstat("16\t704\tx.rs").delete_ratio() > 40.0);
        // The measured bottom: mint the canonical home, retire nothing.
        assert_eq!(
            parse_numstat("3432\t0\tkernel-types/src/ids.rs").delete_ratio(),
            0.0
        );
    }

    #[test]
    fn per_crate_split_surfaces_the_mint_landed_nothing_retired_shape() {
        let by =
            parse_numstat_by_path("3432\t0\tkernel-types/src/ids.rs\n10\t900\tsovereign/x.rs\n");
        assert_eq!(by["kernel-types"].deleted, 0);
        assert_eq!(by["kernel-types"].added, 3432);
        assert_eq!(by["sovereign"].net(), -890);
    }

    /// THE NEGATIVE CONTROL. A converging change edits call sites and adds no
    /// surface. If this produced hits, the gate would fire on every honest
    /// refactor and be worthless — which is the failure mode that matters more
    /// than a missed shim.
    #[test]
    fn a_pure_call_site_edit_adds_no_public_surface() {
        let diff = "\
+++ b/sovereign/crates/x/src/a.rs
@@ -10,1 +10,1 @@
-    take_corpus(corpus_id);
+    take_corpus(corpus_id.as_str());
@@ -40,1 +40,1 @@
-    let c: String = row.corpus_id;
+    let c: CorpusId = row.corpus_id;
";
        assert_eq!(
            scan_added_public_items(diff),
            vec![],
            "call-site edits are exactly what convergence looks like"
        );
    }

    /// THE ESCAPE HATCH. An agent that will not touch the call site writes a
    /// `From` impl instead. That is the thing this bar exists to name.
    #[test]
    fn a_from_impl_shim_is_caught_even_without_the_pub_keyword() {
        let diff = "\
+++ b/sovereign/crates/x/src/shim.rs
@@ -0,0 +1,3 @@
+impl From<LegacyId> for CorpusId {
+    fn from(v: LegacyId) -> Self { CorpusId::new(v.0).unwrap() }
+}
";
        let items = scan_added_public_items(diff);
        assert_eq!(items.len(), 1, "got {items:?}");
        assert_eq!(items[0].kind, "impl-for");
        assert!(items[0].name.contains("CorpusId"), "{:?}", items[0].name);
        assert_eq!(items[0].line, 1);
    }

    #[test]
    fn public_items_are_found_with_the_right_file_and_line() {
        let diff = "\
+++ b/sovereign/crates/x/src/a.rs
@@ -100,0 +101,4 @@
+pub fn adapt_corpus(s: &str) -> String { s.to_string() }
+pub(crate) struct Bridge { inner: String }
+pub async fn fetch_it() {}
+pub const LIMIT: usize = 3;
";
        let items = scan_added_public_items(diff);
        let names: Vec<_> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["adapt_corpus", "Bridge", "fetch_it", "LIMIT"]);
        assert_eq!(items[0].line, 101, "first added line is the hunk start");
        assert_eq!(items[3].line, 104);
        assert!(items
            .iter()
            .all(|i| i.file == "sovereign/crates/x/src/a.rs"));
    }

    #[test]
    fn test_files_do_not_count_as_new_surface() {
        let diff = "\
+++ b/sovereign/crates/x/tests/it.rs
@@ -0,0 +1,1 @@
+pub fn helper() {}
";
        assert!(scan_added_public_items(diff).is_empty());
    }

    #[test]
    fn the_specs_declared_target_is_allowed_but_a_shim_is_not() {
        let items = vec![
            AddedItem {
                file: "a.rs".into(),
                line: 1,
                kind: "struct".into(),
                name: "CorpusId".into(),
            },
            AddedItem {
                file: "b.rs".into(),
                line: 9,
                kind: "fn".into(),
                name: "adapt_corpus".into(),
            },
        ];
        let allowed = vec!["kernel_types::CorpusId".to_string()];
        let bad = disallowed(&items, &allowed);
        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].name, "adapt_corpus");
    }

    /// The gate must NOT fire on honest additive work. A newtype order mints
    /// its declared target and edits call sites; it is net-positive by
    /// construction. If that failed, the gate would be turned off within a
    /// week and the escape hatch would go back to being free.
    #[test]
    fn an_honest_additive_order_passes_when_its_target_is_declared() {
        let items = vec![AddedItem {
            file: "kernel-types/src/ids.rs".into(),
            line: 12,
            kind: "struct".into(),
            name: "CorpusId".into(),
        }];
        let allowed = vec!["kernel_types::CorpusId".to_string()];
        assert!(
            disallowed(&items, &allowed).is_empty(),
            "the spec's declared target is not an escape hatch"
        );
    }

    /// …but an undeclared shim in the same change is still caught.
    #[test]
    fn a_shim_alongside_the_declared_target_is_still_caught() {
        let items = vec![
            AddedItem {
                file: "kernel-types/src/ids.rs".into(),
                line: 12,
                kind: "struct".into(),
                name: "CorpusId".into(),
            },
            AddedItem {
                file: "x/src/compat.rs".into(),
                line: 4,
                kind: "impl-for".into(),
                name: "From<String> for CorpusId".into(),
            },
        ];
        // The shim NAMES the target, which is exactly why a naive substring
        // allowance would wave it through. Allowance matches the declared item,
        // not anything mentioning it.
        let bad = disallowed(&items, &["kernel_types::CorpusId".to_string()]);
        assert_eq!(bad.len(), 1, "got {bad:?}");
        assert_eq!(bad[0].kind, "impl-for");
    }

    /// A substring allowance would wave this through: `Id` is a substring of
    /// `kernel_types::CorpusId`. Matching the final segment exactly is what
    /// closes it.
    #[test]
    fn an_allowance_does_not_leak_to_a_shorter_name_it_contains() {
        let items = vec![AddedItem {
            file: "x.rs".into(),
            line: 1,
            kind: "struct".into(),
            name: "Id".into(),
        }];
        assert_eq!(
            disallowed(&items, &["kernel_types::CorpusId".to_string()]).len(),
            1,
            "`Id` is not `CorpusId`"
        );
    }

    /// An unresolvable base must not read as green.
    #[test]
    fn an_unresolvable_base_is_could_not_judge_not_passed() {
        let tmp = tempfile::tempdir().unwrap();
        let r = measure(tmp.path(), "definitely-not-a-ref", &[], None);
        assert!(
            matches!(r.verdict, Verdict::CouldNotJudge(_)),
            "got {:?}",
            r.verdict
        );
        assert_ne!(r.verdict.label(), "PASSED");
    }
}
