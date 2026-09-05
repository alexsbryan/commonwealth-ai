// SPDX-License-Identifier: AGPL-3.0-or-later
//! The goals a frame may name, and which of them are PARTS of which.
//!
//! This is data the caller supplies, not a relation inferred from a string.
//! It was inferred once — `is_part_of` read pytest's `::` and `/` — and that
//! silently disabled `split` for every oracle whose goal ids do not look
//! like pytest node ids. Cargo's do not (`--tests`, `--test behaviour
//! area_works`), so `parts` came back empty, the split arm was dropped from
//! the grammar, and the model could not decompose anything (ring 4).
//!
//! A goal is whatever the oracle can run, so only the caller knows the
//! shape. `from_pytest` keeps the convenience where it is true; `from_tree`
//! states the relation outright; `flat` says there is none, which is
//! reported as "no split arm", never guessed at.

use super::frame::GoalId;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// Every goal a frame may name, plus the part relation over them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GoalCatalog {
    goals: Vec<GoalId>,
    /// child -> parent. A goal absent here is a root.
    parent: BTreeMap<GoalId, GoalId>,
}

impl GoalCatalog {
    /// No part relation: `push` and `edit` are available, `split` is not.
    pub fn flat(goals: impl IntoIterator<Item = GoalId>) -> Self {
        let mut goals: Vec<GoalId> = goals.into_iter().collect();
        goals.sort();
        goals.dedup();
        Self {
            goals,
            parent: BTreeMap::new(),
        }
    }

    /// `(child, parent)` pairs. Parents are added to the goal set even when
    /// they appear only as a parent, so a root goal is always nameable.
    pub fn from_tree(pairs: impl IntoIterator<Item = (GoalId, GoalId)>) -> Self {
        let mut parent = BTreeMap::new();
        let mut goals = Vec::new();
        for (child, p) in pairs {
            goals.push(child.clone());
            goals.push(p.clone());
            parent.insert(child, p);
        }
        goals.sort();
        goals.dedup();
        Self { goals, parent }
    }

    /// Every pytest node id plus its file, from `--collect-only`. The file
    /// is the node's parent; a directory prefix is a file's parent when the
    /// caller also names it.
    pub fn from_pytest(workdir: &Path) -> std::io::Result<Self> {
        let out = Command::new("python3")
            .args([
                "-m",
                "pytest",
                "--collect-only",
                "-q",
                "-p",
                "no:cacheprovider",
            ])
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .current_dir(workdir)
            .output()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut pairs = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if let Some((file, _)) = line.split_once("::") {
                pairs.push((GoalId::new(line), GoalId::new(file)));
            }
        }
        Ok(Self::from_tree(pairs))
    }

    /// Add `parent` above every goal that has none. Lets a caller name one
    /// root over a `from_pytest` catalog without restating the tree.
    pub fn under_root(mut self, root: GoalId) -> Self {
        let orphans: Vec<GoalId> = self
            .goals
            .iter()
            .filter(|g| **g != root && !self.parent.contains_key(*g))
            .cloned()
            .collect();
        for o in orphans {
            self.parent.insert(o, root.clone());
        }
        if !self.goals.contains(&root) {
            self.goals.push(root);
            self.goals.sort();
        }
        self
    }

    pub fn goals(&self) -> &[GoalId] {
        &self.goals
    }

    pub fn is_empty(&self) -> bool {
        self.goals.is_empty()
    }

    /// The goals directly under `goal`.
    pub fn parts_of(&self, goal: &GoalId) -> Vec<GoalId> {
        self.goals
            .iter()
            .filter(|g| self.parent.get(*g) == Some(goal))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(s: &str) -> GoalId {
        GoalId::new(s)
    }

    #[test]
    fn flat_has_goals_and_no_parts() {
        let c = GoalCatalog::flat([g("a"), g("b"), g("a")]);
        assert_eq!(c.goals(), &[g("a"), g("b")]);
        assert!(c.parts_of(&g("a")).is_empty());
    }

    #[test]
    fn a_tree_names_its_own_parts_whatever_the_ids_look_like() {
        // Cargo filter fragments: no `::`, no `/`, nothing to infer from.
        let root = g("--tests");
        let area = g("--test behaviour area_works");
        let text = g("--test behaviour text_works");
        let c =
            GoalCatalog::from_tree([(area.clone(), root.clone()), (text.clone(), root.clone())]);
        assert_eq!(c.parts_of(&root), vec![area.clone(), text]);
        assert!(c.parts_of(&area).is_empty());
        // The parent is nameable even though it was only ever a parent.
        assert!(c.goals().contains(&root));
    }

    #[test]
    fn under_root_adopts_only_the_orphans() {
        let root = g("tests");
        let file = g("tests/a.py");
        let node = g("tests/a.py::t");
        let c = GoalCatalog::from_tree([(node.clone(), file.clone())]).under_root(root.clone());
        assert_eq!(c.parts_of(&root), vec![file.clone()]);
        assert_eq!(c.parts_of(&file), vec![node]);
        assert!(
            c.parts_of(&root).len() == 1,
            "the node keeps its own parent"
        );
    }
}
