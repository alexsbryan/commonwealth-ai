// SPDX-License-Identifier: AGPL-3.0-or-later
//! The value ruler, loaded from `quality/backlog-ruler.toml`.
//!
//! There is exactly one ruler and it is a file. `scripts/co-backlog.py`
//! reads it to render and rank the backlog; this module reads the same
//! file to build the scorer's system prompt. Neither carries a built-in
//! copy — if the file is missing, both refuse and say so, because a
//! silent default is how the second copy grows back (ARCH §10.6, §18.3).

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The env override, spelled exactly as `scripts/co-backlog.py` spells
/// it (`ruler_path()`): one name for one path, across two languages.
pub const RULER_ENV: &str = "CO_BACKLOG_RULER";

#[derive(Debug, Deserialize)]
pub struct Ruler {
    pub version: String,
    #[serde(default)]
    pub minted: String,
    #[serde(rename = "axis")]
    pub axes: Vec<Axis>,
    pub scoring: Scoring,
    pub value: ValueRange,
    pub cost: Cost,
    pub format: Format,
    /// Where it was actually read from. Printed by the verb so an
    /// operator can see WHICH ruler scored an item, not just that one
    /// did (ARCH §9, glassbox).
    #[serde(skip)]
    pub path: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct Axis {
    pub letter: String,
    pub name: String,
    pub yardstick: String,
}

#[derive(Debug, Deserialize)]
pub struct Scoring {
    pub scale: Vec<String>,
    pub blocks_rule: String,
    #[serde(default)]
    pub roi: String,
}

#[derive(Debug, Deserialize)]
pub struct ValueRange {
    pub min: i64,
    pub max: i64,
}

#[derive(Debug, Deserialize)]
pub struct Cost {
    pub chunks: std::collections::BTreeMap<String, i64>,
}

#[derive(Debug, Deserialize)]
pub struct Format {
    pub header_keys: Vec<String>,
}

/// Collapse the TOML's hand-wrapped strings to one line. The ruler is
/// wrapped for human editing; the prompt wants sentences.
fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Ruler {
    /// Load the ruler, or say why not. The `Err` is a finished sentence
    /// for the operator — every caller prints it and refuses.
    pub fn load(explicit: Option<&Path>) -> Result<Self, String> {
        let path = resolve_path(explicit)?;
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read the value ruler at {}: {e}", path.display()))?;
        let mut ruler: Ruler = toml::from_str(&text).map_err(|e| {
            format!(
                "the value ruler at {} is malformed: {e}. Fix the file; there \
                 is no built-in ruler to fall back to.",
                path.display()
            )
        })?;
        if ruler.axes.is_empty() {
            return Err(format!(
                "the value ruler at {} declares no axes",
                path.display()
            ));
        }
        ruler.path = path;
        Ok(ruler)
    }

    pub fn axis_letters(&self) -> Vec<String> {
        self.axes.iter().map(|a| a.letter.clone()).collect()
    }

    pub fn cost_letters(&self) -> Vec<String> {
        let mut v: Vec<_> = self.cost.chunks.iter().collect();
        v.sort_by_key(|(_, chunks)| **chunks);
        v.into_iter().map(|(k, _)| k.clone()).collect()
    }

    /// The scorer's system prompt, rendered from the ruler.
    ///
    /// Written short and non-contradictory on purpose: the operator's
    /// standing convention for small open-weight models, and this runs
    /// against whatever is resident, not against a frontier model. Every
    /// normative sentence here is the ruler's own text or a direct
    /// reading of it — the prompt does not add scoring rules the file
    /// does not state, or the file stops being the one decider.
    ///
    /// Validated on 10 real migrated items before it was written here
    /// (order backlog-insert-system, D2): 10/10 parsed, 9/10 within one
    /// point of the seat's own Value. The `measurement` field exists
    /// because the model would not hold its own ceiling — see
    /// [`super::score::Score::apply_measurement_cap`].
    pub fn system_prompt(&self) -> String {
        let axes = self
            .axes
            .iter()
            .map(|a| format!("{} {} — {}", a.letter, a.name, flat(&a.yardstick)))
            .collect::<Vec<_>>()
            .join("\n");
        let scale = self.scale_lines().join("\n");
        let costs = {
            let mut v: Vec<_> = self.cost.chunks.iter().collect();
            v.sort_by_key(|(_, c)| **c);
            v.iter()
                .map(|(k, c)| format!("{k}={c}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!(
            "You score one backlog item for a software project. Output JSON only.\n\n\
             VALUE AXES — pick the one axis the item most moves:\n{axes}\n\n\
             VALUE SCALE:\n{scale}\n\n\
             {blocks}\n\n\
             HOW TO APPLY THE SCALE: start at {min} and go up only if the item \
             clears the next bar. Most items are 2 or 3. Give {max} only if the \
             item text itself states a measurement; with no measurement stated, \
             {near_max} is the highest available. If the item moves no axis at \
             all, the value is {min} — name the least-bad axis and say in the \
             rationale that it barely fits.\n\n\
             APPROACH: state how the item gets solved, in 1-3 sentences, using \
             ONLY what the item text says. Name the existing component it builds \
             on. If the text does not say how, answer exactly: unknown — needs a \
             design pass\n\n\
             COST: session-chunks that the approach you just wrote would take \
             ({costs}). Size the approach, not the ambition.\n\n\
             MEASUREMENT: quote the number or benchmark result the item text \
             states as evidence, word for word. If the item text states no \
             number, answer with an empty string.\n\n\
             TITLE: name the item the way a colleague would say it out loud \
             — under 60 characters, no note ids, no leading 'Backlog item'. \
             Say the thing that is wrong or the thing to build, not the \
             category it belongs to.\n\n\
             Answer with these fields: title, value, axis, rationale, \
             approach, cost, measurement.\n\
             rationale is ONE falsifiable line and names the axis letter.",
            blocks = flat(&self.scoring.blocks_rule),
            min = self.value.min,
            max = self.value.max,
            near_max = self.value.max - 1,
        )
    }

    pub fn scale_lines(&self) -> Vec<String> {
        self.scoring.scale.iter().map(|s| flat(s)).collect()
    }
}

/// Where the ruler is, in one place.
///
/// `--ruler` beats the env var beats the repo it is committed in. The
/// repo walk starts at the cwd because the ruler is a REPO artifact, not
/// per-user state — unlike the notes store, which is never discovered
/// from cwd (invariant 0f8abed1). Not found is a refusal, never a
/// built-in default.
fn resolve_path(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        return if p.exists() {
            Ok(p.to_path_buf())
        } else {
            Err(format!("--ruler {} does not exist", p.display()))
        };
    }
    if let Ok(env) = std::env::var(RULER_ENV) {
        if !env.is_empty() {
            let p = PathBuf::from(env);
            return if p.exists() {
                Ok(p)
            } else {
                Err(format!(
                    "{RULER_ENV} points at {}, which does not exist",
                    p.display()
                ))
            };
        }
    }
    let rel = Path::new("quality").join("backlog-ruler.toml");
    let start =
        std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))?;
    for dir in start.ancestors() {
        let candidate = dir.join(&rel);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "no value ruler found: looked for {} from {} upwards. Run this from \
         the repo, or set {RULER_ENV} to the file. Scoring cannot proceed \
         without the ruler — there is no built-in copy.",
        rel.display(),
        start.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_ruler() -> Ruler {
        // The committed ruler, found the same way the verb finds it.
        Ruler::load(None).expect("the repo's own ruler must load")
    }

    #[test]
    fn the_committed_ruler_loads_and_is_v2_shaped() {
        let r = repo_ruler();
        assert_eq!(r.axis_letters(), ["A", "B", "C", "D", "E", "F"]);
        assert_eq!(r.value.min, 1);
        assert_eq!(r.value.max, 5);
        assert_eq!(r.cost_letters(), ["S", "M", "L"]);
        assert!(r.format.header_keys.iter().any(|k| k == "Scored-by"));
    }

    #[test]
    fn the_prompt_carries_every_axis_and_every_scale_level() {
        let r = repo_ruler();
        let p = r.system_prompt();
        for a in &r.axes {
            assert!(p.contains(&a.name), "prompt drops axis {}", a.letter);
            // the yardstick is the argument; an axis name alone is a label
            assert!(
                p.contains(&flat(&a.yardstick)),
                "prompt drops the yardstick for axis {}",
                a.letter
            );
        }
        for line in r.scale_lines() {
            assert!(p.contains(&line), "prompt drops scale level {line:?}");
        }
    }

    #[test]
    fn a_missing_ruler_refuses_and_names_the_path() {
        let err = Ruler::load(Some(Path::new("/nonexistent/backlog-ruler.toml")))
            .expect_err("a missing ruler must not load");
        assert!(err.contains("/nonexistent/backlog-ruler.toml"), "{err}");
    }

    #[test]
    fn a_malformed_ruler_refuses_rather_than_defaulting() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("backlog-ruler.toml");
        std::fs::write(&p, "version = \"2\"\n# and nothing else\n").unwrap();
        let err = Ruler::load(Some(&p)).expect_err("a ruler with no axes must not load");
        assert!(
            err.contains("malformed") || err.contains("no axes"),
            "{err}"
        );
    }
}
