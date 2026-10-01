// SPDX-License-Identifier: AGPL-3.0-or-later
//! clone-gate — production lines that live in more than one file may only
//! shrink.
//!
//! boundary-gate steered the cut by the count of forbidden edges, and a copy
//! lowers that count: c23f3b4c0 forked panic_hook, memory_watch and
//! log_rotation "moved whole, unmodified" and took BOUNDARY 117 -> 102 with no
//! gate seeing it (phase-b-99). concept-gate counts duplicated NAMES,
//! lock-gate duplicated crates, scripts/twin-census.py the seven families
//! quality/twin-plants.toml names; nothing counted duplicated LINES.
//!
//! THE INSTRUMENT, fixed before any count was read:
//!
//! - scope: tracked `.rs` under each workspace member's `src/`; `tests/`,
//!   `*_tests.rs`, `tests.rs` and every `#[cfg(test)]` item are not production;
//! - normalized lines: trimmed; blanks, comments, `use` declarations and lines
//!   of only brackets and separators dropped;
//! - a clone is a window of [`WINDOW`] normalized lines found in two or more
//!   FILES (a repeat inside one file is not a twin of anything);
//! - the count is the number of normalized production lines covered by any
//!   clone window.
//!
//! The ratchet is on that one total. Families (the set of files sharing a
//! window) are recorded beside it so a red names what is new, but a family's
//! key is its file paths, so a moved file re-keys its family while the total
//! holds: a move is not a clone, and the gate stays green on one.
//!
//! `--tighten` banks a lower total by rewriting the whole snapshot; it never
//! raises the total. `--update-baseline` follows the house re-pin recipe.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;

use crate::{common, manifests, size_gate};

/// Lines in one clone window.
const WINDOW: usize = 8;

/// The baseline key that carries the ratcheted total. It sorts ahead of
/// every family key, which starts with a path.
const TOTAL_KEY: &str = "(total covered lines)";

/// One production file, reduced to the lines the gate compares.
struct NormFile {
    rel: String,
    /// `(1-based source line, normalized text)`.
    lines: Vec<(usize, String)>,
}

/// Files sharing a window, with the normalized line indices each contributes.
type Family = BTreeMap<usize, BTreeSet<usize>>;

pub fn run(args: &[String]) -> i32 {
    let root = common::repo_root();
    let flags = common::baseline_flags(args);
    let baseline_path = common::baselines_dir(&root).join("clones.tsv");
    let what = "production lines covered by an 8-line normalized window found in 2+ files \
                (total may only shrink; families recorded so a red names what is new)";

    let files = match production_files(&root) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    if files.is_empty() {
        eprintln!(
            "error: clone-gate read zero production files — the scope resolved to nothing, \
             which is a broken instrument, not a clean repo (ARCH §18.3)"
        );
        return 1;
    }
    let (total, families) = census(&files);
    let current = snapshot(total, &families);

    if flags.update {
        if let Err(e) = common::write_count_map(&baseline_path, "clone-gate", what, &current) {
            eprintln!("error: {e}");
            return 1;
        }
        eprintln!(
            "wrote {} ({total} covered lines in {} families)",
            baseline_path.display(),
            families.len()
        );
        return 0;
    }

    let baseline = common::load_count_map(&baseline_path);
    let Some(&cap) = baseline.get(TOTAL_KEY) else {
        eprintln!(
            "error: no baseline total at {}.\n  Run: cargo run -p xtask -- clone-gate --update-baseline",
            baseline_path.display()
        );
        return 1;
    };

    if flags.tighten {
        if total >= cap {
            eprintln!("clone-gate --tighten: nothing to bank ({total} now, ceiling {cap})");
            return 0;
        }
        if let Err(e) = common::write_count_map(&baseline_path, "clone-gate", what, &current) {
            eprintln!("error: {e}");
            return 1;
        }
        eprintln!(
            "clone-gate --tighten: {cap} → {total} ({} lines banked) → {}",
            cap - total,
            baseline_path.display()
        );
        return 0;
    }

    eprintln!(
        "clone-gate: {total} production lines in clones across {} families \
         ({} files read); baseline {cap}",
        families.len(),
        files.len()
    );
    if total <= cap {
        if total < cap {
            eprintln!("  ✓ no new clone — {} lines beatable, bank with --tighten", cap - total);
        } else {
            eprintln!("  ✓ no new clone");
        }
        return 0;
    }
    eprintln!("  ✗ {cap} → {total} (+{}). New or grown families:", total - cap);
    let mut shown = 0;
    for (key, family) in &families {
        let n = family_lines(family);
        if baseline.get(key).is_some_and(|&was| n <= was) {
            continue;
        }
        eprintln!("    {n} lines: {}", render(&files, family));
        shown += 1;
        if shown == 12 {
            break;
        }
    }
    eprintln!();
    eprintln!("clone-gate FAILED. Give the copied code one home and call it from both sites.");
    eprintln!("{}", common::fix_footer("clone-gate"));
    1
}

/// Every workspace member's tracked production `.rs`, normalized.
fn production_files(root: &Path) -> Result<Vec<NormFile>, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z", "--", "*.rs"])
        .output()
        .map_err(|e| format!("git ls-files: {e}"))?;
    if !out.status.success() {
        return Err(format!("git ls-files exited {}", out.status));
    }
    let src_dirs: Vec<String> = manifests::workspace_members(root)
        .into_iter()
        .map(|m| format!("{}/src/", m.dir))
        .collect();
    let mut files = Vec::new();
    for rel in String::from_utf8_lossy(&out.stdout).split('\0') {
        if !src_dirs.iter().any(|d| rel.starts_with(d.as_str())) || is_test_path(rel) {
            continue;
        }
        // Listed in the index but gone from the working tree: a staged move.
        let Ok(text) = std::fs::read_to_string(root.join(rel)) else {
            continue;
        };
        files.push(NormFile {
            rel: rel.to_string(),
            lines: normalize(&text),
        });
    }
    Ok(files)
}

fn is_test_path(rel: &str) -> bool {
    rel.contains("/tests/") || rel.ends_with("_tests.rs") || rel.ends_with("/tests.rs")
}

/// The lines the gate compares, each with its source line number.
fn normalize(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut in_block = false;
    let mut in_use = false;
    let mut test_item: Option<ItemSkip> = None;
    for (i, raw) in text.lines().enumerate() {
        let mut t = raw.trim();
        if size_gate::is_noise(t, &mut in_block) {
            continue;
        }
        if let Some(rest) = t.strip_prefix("#[cfg(test)]") {
            test_item = Some(ItemSkip::default());
            t = rest.trim();
            if t.is_empty() {
                continue;
            }
        }
        if let Some(skip) = test_item.as_mut() {
            if skip.consume(t) {
                test_item = None;
            }
            continue;
        }
        if in_use || is_use(t) {
            in_use = !t.ends_with(';');
            continue;
        }
        if t.chars().all(|c| "{}()[];,".contains(c)) {
            continue;
        }
        out.push((i + 1, t.to_string()));
    }
    out
}

fn is_use(t: &str) -> bool {
    let t = t.strip_prefix("pub ").unwrap_or(t);
    let t = ["pub(crate) ", "pub(super) "]
        .iter()
        .find_map(|p| t.strip_prefix(p))
        .unwrap_or(t);
    t.starts_with("use ")
}

/// Skips one item after `#[cfg(test)]`: its attributes, then either a
/// `;`-terminated line or a braced body to its closing brace.
#[derive(Default)]
struct ItemSkip {
    depth: i64,
    opened: bool,
}

impl ItemSkip {
    /// Feed one line; true when the item has ended on it.
    fn consume(&mut self, t: &str) -> bool {
        if !self.opened && t.starts_with("#[") {
            return false;
        }
        for delta in braces(t) {
            self.depth += delta;
            self.opened = true;
        }
        if self.opened {
            self.depth <= 0
        } else {
            t.ends_with(';')
        }
    }
}

/// `+1` per `{`, `-1` per `}`, outside string and char literals and line
/// comments. An approximation: a raw string holding a brace miscounts.
fn braces(t: &str) -> Vec<i64> {
    let mut out = Vec::new();
    let mut chars = t.chars().peekable();
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            match c {
                '\\' => {
                    chars.next();
                }
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '/' if chars.peek() == Some(&'/') => break,
            '\'' => {
                // `'{'` is a char literal; `'a` is a lifetime.
                let mut ahead = chars.clone();
                if let (Some(_), Some('\'')) = (ahead.next(), ahead.next()) {
                    chars.next();
                    chars.next();
                }
            }
            '{' => out.push(1),
            '}' => out.push(-1),
            _ => {}
        }
    }
    out
}

/// The covered-line total and every clone family, keyed by its files' paths.
fn census(files: &[NormFile]) -> (usize, BTreeMap<String, Family>) {
    let mut windows: HashMap<u64, Vec<(usize, usize)>> = HashMap::new();
    for (f, file) in files.iter().enumerate() {
        for start in 0..file.lines.len().saturating_sub(WINDOW - 1) {
            let mut h = DefaultHasher::new();
            for (_, line) in &file.lines[start..start + WINDOW] {
                line.hash(&mut h);
            }
            windows.entry(h.finish()).or_default().push((f, start));
        }
    }
    let mut covered: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut families: BTreeMap<String, Family> = BTreeMap::new();
    for hits in windows.values() {
        let members: BTreeSet<usize> = hits.iter().map(|&(f, _)| f).collect();
        if members.len() < 2 {
            continue;
        }
        let key = members
            .iter()
            .map(|&f| files[f].rel.as_str())
            .collect::<Vec<_>>()
            .join(" <-> ");
        let family = families.entry(key).or_default();
        for &(f, start) in hits {
            for i in start..start + WINDOW {
                covered.insert((f, i));
                family.entry(f).or_default().insert(i);
            }
        }
    }
    (covered.len(), families)
}

fn family_lines(family: &Family) -> usize {
    family.values().map(BTreeSet::len).sum()
}

/// The baseline map: the total, then each family's covered lines.
fn snapshot(total: usize, families: &BTreeMap<String, Family>) -> BTreeMap<String, usize> {
    let mut map: BTreeMap<String, usize> = families
        .iter()
        .map(|(k, fam)| (k.clone(), family_lines(fam)))
        .collect();
    map.insert(TOTAL_KEY.to_string(), total);
    map
}

/// `a.rs:10-40, b.rs:100-130` — each file's covered runs as source ranges.
fn render(files: &[NormFile], family: &Family) -> String {
    let mut parts = Vec::new();
    for (&f, idxs) in family {
        let file = &files[f];
        let mut ranges: Vec<String> = Vec::new();
        let mut run: Option<(usize, usize)> = None;
        for &i in idxs {
            run = match run {
                Some((a, b)) if i == b + 1 => Some((a, i)),
                Some((a, b)) => {
                    ranges.push(format!("{}-{}", file.lines[a].0, file.lines[b].0));
                    Some((i, i))
                }
                None => Some((i, i)),
            };
        }
        if let Some((a, b)) = run {
            ranges.push(format!("{}-{}", file.lines[a].0, file.lines[b].0));
        }
        parts.push(format!("{}:{}", file.rel, ranges.join(",")));
    }
    parts.join("  <->  ")
}

#[cfg(test)]
#[path = "clone_gate_tests.rs"]
mod tests;
