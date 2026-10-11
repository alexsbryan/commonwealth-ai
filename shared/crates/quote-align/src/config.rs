// SPDX-License-Identifier: AGPL-3.0-or-later
//! `align.toml`: the aligner's knobs, as data.
//!
//! The file is flat `key = number` lines with `#` comments. The crate links
//! no TOML parser (its budget is std and unicode-normalization), so this
//! reads exactly that shape and refuses anything else by name: an unknown,
//! repeated or missing key is an error, never a default (ARCH 6).

use std::collections::BTreeMap;
use std::fmt;

const SHIPPED: &str = include_str!("../align.toml");

/// Every key the file must carry, once each.
const KEYS: [&str; 7] = [
    "coverage_floor",
    "candidate_k",
    "seeds",
    "max_anchors",
    "band",
    "elide_max_tokens",
    "bracket_max_tokens",
];

/// The aligner's knobs (`align.toml` documents each).
#[derive(Debug, Clone, PartialEq)]
pub struct AlignConfig {
    /// Least coverage an alignment needs to be kept, in `(0, 1]`.
    pub coverage_floor: f32,
    /// Candidate chunks the route takes from lexical search.
    pub candidate_k: u32,
    /// Rarest word n-grams (each gap-free run's own length, capped at 3) that
    /// seed windows.
    pub seeds: usize,
    /// Most anchors aligned per request.
    pub max_anchors: usize,
    /// Drift allowed off a seed's diagonal, in tokens.
    pub band: usize,
    /// Source tokens one ellipsis may stand for.
    pub elide_max_tokens: usize,
    /// Source tokens one bracketed insertion may stand for.
    pub bracket_max_tokens: usize,
}

/// Why `align.toml` did not load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlignConfigError {
    /// A line that is not `key = value`.
    Syntax {
        /// 1-based line number.
        line: usize,
    },
    /// A key this crate does not read.
    UnknownKey {
        /// The key.
        key: String,
    },
    /// A key given twice.
    Repeated {
        /// The key.
        key: String,
    },
    /// A key the file must carry and does not.
    Missing {
        /// The key.
        key: &'static str,
    },
    /// A value that does not parse, or is out of range.
    BadValue {
        /// The key.
        key: &'static str,
        /// The value as written.
        value: String,
    },
}

impl fmt::Display for AlignConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AlignConfigError::Syntax { line } => write!(f, "align.toml:{line}: not `key = value`"),
            AlignConfigError::UnknownKey { key } => write!(f, "align.toml: unknown key `{key}`"),
            AlignConfigError::Repeated { key } => write!(f, "align.toml: `{key}` given twice"),
            AlignConfigError::Missing { key } => write!(f, "align.toml: `{key}` missing"),
            AlignConfigError::BadValue { key, value } => {
                write!(f, "align.toml: `{key} = {value}` is not a valid value")
            }
        }
    }
}

impl std::error::Error for AlignConfigError {}

impl AlignConfig {
    /// The shipped `align.toml`, compiled in. A test pins that it loads;
    /// the caller still gets the refusal by name rather than a panic.
    pub fn shipped() -> Result<Self, AlignConfigError> {
        Self::parse(SHIPPED)
    }

    /// Read an `align.toml`.
    pub fn parse(src: &str) -> Result<Self, AlignConfigError> {
        let mut values: BTreeMap<&'static str, &str> = BTreeMap::new();
        for (i, raw) in src.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .ok_or(AlignConfigError::Syntax { line: i + 1 })?;
            let key = key.trim();
            let known = KEYS
                .iter()
                .copied()
                .find(|k| *k == key)
                .ok_or_else(|| AlignConfigError::UnknownKey { key: key.into() })?;
            if values.insert(known, value.trim()).is_some() {
                return Err(AlignConfigError::Repeated { key: key.into() });
            }
        }
        let get = |key: &'static str| {
            values
                .get(key)
                .copied()
                .ok_or(AlignConfigError::Missing { key })
        };
        let bad = |key: &'static str, value: &str| AlignConfigError::BadValue {
            key,
            value: value.into(),
        };
        let count = |key: &'static str| -> Result<usize, AlignConfigError> {
            let v = get(key)?;
            v.parse::<usize>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| bad(key, v))
        };
        let floor = get("coverage_floor")?;
        let coverage_floor = floor
            .parse::<f32>()
            .ok()
            .filter(|f| *f > 0.0 && *f <= 1.0)
            .ok_or_else(|| bad("coverage_floor", floor))?;
        let k = get("candidate_k")?;
        let candidate_k = k
            .parse::<u32>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| bad("candidate_k", k))?;
        Ok(AlignConfig {
            coverage_floor,
            candidate_k,
            seeds: count("seeds")?,
            max_anchors: count("max_anchors")?,
            band: count("band")?,
            elide_max_tokens: count("elide_max_tokens")?,
            bracket_max_tokens: count("bracket_max_tokens")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_file_loads() {
        let c = AlignConfig::shipped().expect("the shipped align.toml loads");
        assert!(c.coverage_floor > 0.0 && c.coverage_floor <= 1.0);
        assert!(c.candidate_k >= 1);
    }

    #[test]
    fn every_refusal_is_named() {
        let full: String = KEYS.iter().map(|k| format!("{k} = 1\n")).collect();
        assert!(AlignConfig::parse(&full).is_ok());
        assert_eq!(
            AlignConfig::parse(&format!("{full}bogus = 2\n")),
            Err(AlignConfigError::UnknownKey {
                key: "bogus".into()
            })
        );
        assert_eq!(
            AlignConfig::parse(&format!("{full}band = 2\n")),
            Err(AlignConfigError::Repeated { key: "band".into() })
        );
        assert_eq!(
            AlignConfig::parse(&full.replace("band = 1\n", "")),
            Err(AlignConfigError::Missing { key: "band" })
        );
        assert_eq!(
            AlignConfig::parse(&full.replace("coverage_floor = 1", "coverage_floor = 1.5")),
            Err(AlignConfigError::BadValue {
                key: "coverage_floor",
                value: "1.5".into()
            })
        );
        assert_eq!(
            AlignConfig::parse(&full.replace("seeds = 1", "seeds = 0")),
            Err(AlignConfigError::BadValue {
                key: "seeds",
                value: "0".into()
            })
        );
        assert_eq!(
            AlignConfig::parse("band 4\n"),
            Err(AlignConfigError::Syntax { line: 1 })
        );
    }
}
