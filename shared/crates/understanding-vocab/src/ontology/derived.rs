// SPDX-License-Identifier: AGPL-3.0-or-later
//! Derived attributes (`svrn/docs/specs/ONTOLOGY_PRIMITIVES.md` §8, agreed
//! 2026-10-06). Three flat declarations, each referenced by `id`: a **path**
//! walks the build's graph, a **set** names particulars by their attributes
//! and filters any step, a **fold** turns candidate values into an
//! attribute's value by one function from a closed registry. An attribute's
//! `derived` names the path or fold that fills it. Code knows the step kinds
//! and the functions; every other name is the recipe's.
//!
//! Path grammar (SPARQL 1.1 property paths, cut down):
//!
//! ```text
//! path    := seq                       -- `|` only inside parentheses
//! seq     := postfix ( "/" postfix )*
//! postfix := primary ( "[" "!"? NAME "]" )*
//! primary := "^"? NAME | "(" seq ( "|" seq )* ")"
//! ```
//!
//! A NAME is `subject`, `document`, a declared path or fold id, a declared
//! attribute, or (after `document`) a document field a metadata source reads.
//! `[s]` keeps what is in set `s`, `[!s]` drops it.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::decl::OntologyTypeDecl;

/// The step from a claim to the particular it is about, and back (`^subject`).
pub const SUBJECT: &str = "subject";
/// The step from a claim to the one document its evidence lands in. Also the
/// `type` of a set over documents.
pub const DOCUMENT: &str = "document";

/// `[[enrichment.ontology.paths]]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathDecl {
    pub id: String,
    pub path: String,
}

/// `[[enrichment.ontology.sets]]`: the particulars of one declared type (or
/// documents) whose attributes meet every condition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetDecl {
    pub id: String,
    #[serde(rename = "type")]
    pub of: String,
    #[serde(rename = "where", default)]
    pub conditions: BTreeMap<String, Condition>,
}

/// One condition on an attribute, compared after the identity fold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Condition {
    Is(String),
    In(Vec<String>),
    /// Equal, or ending in it at a word boundary (a subdomain of a domain).
    Suffix {
        suffix: String,
    },
}

/// `[[enrichment.ontology.folds]]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FoldDecl {
    pub id: String,
    pub by: FoldBy,
    /// Path expressions, a declared id among them, in priority order.
    pub from: Vec<String>,
    /// A recipe-defined state protocol, required only when `by = "protocol"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<ProtocolFoldDecl>,
}

/// Data for a qualified scalar fold over already-assigned, cited claims.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolFoldDecl {
    /// A source-supported, stable identity for one reported transition.
    pub identity: String,
    /// A source-supported transition effective time; never used as report order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_time: Option<String>,
    /// Explicit claim qualifications map to application state values.
    #[serde(default)]
    pub rules: Vec<ProtocolRuleDecl>,
}

/// One application-data mapping from a qualified claim to a scalar state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolRuleDecl {
    /// Stable recipe identity for this mapping, included in the audit.
    pub id: String,
    /// The declared claim type this mapping applies to.
    pub claim_kind: String,
    /// A value in the derived attribute's closed text set.
    pub state: String,
    /// Every named field must be source-supported at the exact declared value.
    #[serde(default)]
    pub when: BTreeMap<String, String>,
    /// An optional field whose supported identity must name a prior transition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrects: Option<String>,
}

/// The fold registry. Closed: a new function is a variant here and an arm in
/// the evaluator. Each deciding function yields one value or a counted
/// absence; `all` yields the set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FoldBy {
    /// The first input, in `from` order, that yields exactly one value; an
    /// input that yields several is ambiguous and the next is tried.
    First,
    /// Every value the inputs yield is the same one.
    Agree,
    /// The value the most distinct documents yield; a tie decides nothing.
    Most,
    /// Every value the inputs yield.
    All,
    /// The value of the earliest document by the declared clock.
    Earliest,
    /// The value of the latest document by the declared clock.
    Latest,
    /// A qualified state projection whose rules and states are recipe data.
    Protocol,
}

impl FoldBy {
    pub fn label(self) -> &'static str {
        match self {
            FoldBy::First => "first",
            FoldBy::Agree => "agree",
            FoldBy::Most => "most",
            FoldBy::All => "all",
            FoldBy::Earliest => "earliest",
            FoldBy::Latest => "latest",
            FoldBy::Protocol => "protocol",
        }
    }
}

/// The three declarations, as the policies carry them (Axis 5).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DerivedPolicy {
    #[serde(default)]
    pub paths: Vec<PathDecl>,
    #[serde(default)]
    pub sets: Vec<SetDecl>,
    #[serde(default)]
    pub folds: Vec<FoldDecl>,
}

/// What an id names.
#[derive(Debug, Clone, Copy)]
pub enum Named<'a> {
    PathDecl(&'a PathDecl),
    SetDecl(&'a SetDecl),
    FoldDecl(&'a FoldDecl),
}

impl DerivedPolicy {
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.sets.is_empty() && self.folds.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<Named<'_>> {
        self.paths
            .iter()
            .find(|p| p.id == id)
            .map(Named::PathDecl)
            .or_else(|| self.folds.iter().find(|f| f.id == id).map(Named::FoldDecl))
            .or_else(|| self.sets.iter().find(|s| s.id == id).map(Named::SetDecl))
    }

    /// The parsed expressions a path or fold id evaluates.
    pub fn exprs(&self, id: &str) -> Result<Vec<PathExpr>, String> {
        match self.get(id) {
            Some(Named::PathDecl(p)) => Ok(vec![
                PathExpr::parse(&p.path).map_err(|e| format!("path `{id}`: {e}"))?
            ]),
            Some(Named::FoldDecl(f)) => f
                .from
                .iter()
                .map(|s| PathExpr::parse(s).map_err(|e| format!("fold `{id}`: `{s}`: {e}")))
                .collect(),
            Some(Named::SetDecl(_)) => Err(format!("`{id}` is a set, not a path or fold")),
            None => Err(format!("`{id}` is no declared path or fold")),
        }
    }

    /// Every attribute a path or fold id reads, through the ids and sets it
    /// names: step names that are no keyword and no id, and the attributes the
    /// conditions of its sets read. Refuses an unknown id and an id that names
    /// itself.
    pub fn reads(&self, id: &str) -> Result<BTreeSet<String>, String> {
        let mut out = BTreeSet::new();
        self.collect_reads(id, &mut Vec::new(), &mut out)?;
        Ok(out)
    }

    fn collect_reads(
        &self,
        id: &str,
        stack: &mut Vec<String>,
        out: &mut BTreeSet<String>,
    ) -> Result<(), String> {
        if stack.iter().any(|s| s == id) {
            stack.push(id.to_string());
            return Err(format!(
                "ids name each other in a cycle: {}",
                stack.join(" → ")
            ));
        }
        stack.push(id.to_string());
        for e in self.exprs(id)? {
            for set in e.sets() {
                match self.get(set) {
                    Some(Named::SetDecl(s)) => out.extend(s.conditions.keys().cloned()),
                    _ => return Err(format!("`[{set}]` names no declared set")),
                }
            }
            for (name, _) in e.steps() {
                match self.get(name) {
                    Some(Named::PathDecl(_) | Named::FoldDecl(_)) => {
                        self.collect_reads(name, stack, out)?
                    }
                    Some(Named::SetDecl(_)) => {
                        return Err(format!(
                            "`{name}` is a set; a set filters a step as `[{name}]`"
                        ))
                    }
                    None if name == SUBJECT || name == DOCUMENT => {}
                    None => {
                        out.insert(name.to_string());
                    }
                }
            }
        }
        stack.pop();
        Ok(())
    }

    /// Every derived attribute as `(type, attribute, id)`, each after every
    /// derived attribute it reads. Refuses a cycle: the one order, for the
    /// recipe check and the build alike.
    pub fn order<'a>(
        &self,
        types: &'a [OntologyTypeDecl],
    ) -> Result<Vec<(&'a str, &'a str, &'a str)>, String> {
        let derived: Vec<(&str, &str, &str)> = types
            .iter()
            .flat_map(|t| {
                t.attributes.iter().filter_map(move |a| {
                    a.derived
                        .as_deref()
                        .map(|d| (t.name.as_str(), a.name.as_str(), d))
                })
            })
            .collect();
        let mut needs: Vec<BTreeSet<usize>> = Vec::with_capacity(derived.len());
        for (_, _, id) in &derived {
            let reads = self.reads(id)?;
            needs.push(
                derived
                    .iter()
                    .enumerate()
                    .filter(|(_, (_, a, _))| reads.contains(*a))
                    .map(|(j, _)| j)
                    .collect(),
            );
        }
        let (mut done, mut order) = (vec![false; derived.len()], Vec::new());
        while order.len() < derived.len() {
            let ready: Vec<usize> = (0..derived.len())
                .filter(|&i| !done[i] && needs[i].iter().all(|&j| done[j]))
                .collect();
            if ready.is_empty() {
                let stuck: Vec<String> = (0..derived.len())
                    .filter(|&i| !done[i])
                    .map(|i| format!("{}.{}", derived[i].0, derived[i].1))
                    .collect();
                return Err(format!(
                    "derived attributes read each other in a cycle: {}",
                    stuck.join(", ")
                ));
            }
            for i in ready {
                done[i] = true;
                order.push(derived[i]);
            }
        }
        Ok(order)
    }
}

/// A parsed path expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathExpr {
    /// A named step, walked backwards when `inverse`.
    Step {
        name: String,
        inverse: bool,
    },
    Seq(Vec<PathExpr>),
    Alt(Vec<PathExpr>),
    /// What `inner` reaches, kept (`[set]`) or dropped (`[!set]`) by a set.
    Filter {
        inner: Box<PathExpr>,
        set: String,
        keep: bool,
    },
}

impl PathExpr {
    pub fn parse(src: &str) -> Result<PathExpr, String> {
        let tokens = tokenize(src)?;
        let mut p = Parser { tokens, at: 0 };
        let path = p.seq()?;
        match p.peek() {
            None => Ok(path),
            Some(Token::Bar) => Err(
                "`|` must sit inside parentheses: `document / (from | to)`, never `document / from | to`"
                    .into(),
            ),
            Some(t) => Err(format!("unexpected `{}`", t.text())),
        }
    }

    /// Every named step, with whether it is walked backwards.
    pub fn steps(&self) -> Vec<(&str, bool)> {
        match self {
            PathExpr::Step { name, inverse } => vec![(name.as_str(), *inverse)],
            PathExpr::Seq(ps) | PathExpr::Alt(ps) => ps.iter().flat_map(PathExpr::steps).collect(),
            PathExpr::Filter { inner, .. } => inner.steps(),
        }
    }

    /// Every set a filter names.
    pub fn sets(&self) -> Vec<&str> {
        match self {
            PathExpr::Step { .. } => Vec::new(),
            PathExpr::Seq(ps) | PathExpr::Alt(ps) => ps.iter().flat_map(PathExpr::sets).collect(),
            PathExpr::Filter { inner, set, .. } => {
                let mut v = inner.sets();
                v.push(set);
                v
            }
        }
    }

    /// The path in words, for `recipe validate`.
    pub fn describe(&self) -> String {
        match self {
            PathExpr::Step { name, inverse } => match (name.as_str(), inverse) {
                (SUBJECT, true) => "the claims about it".into(),
                (SUBJECT, false) => "its subject".into(),
                (DOCUMENT, _) => "its document".into(),
                (n, true) => format!("what names it as `{n}`"),
                (n, false) => format!("`{n}`"),
            },
            PathExpr::Seq(ps) => ps
                .iter()
                .map(PathExpr::describe)
                .collect::<Vec<_>>()
                .join(" → "),
            PathExpr::Alt(ps) => {
                let parts: Vec<String> = ps.iter().map(PathExpr::describe).collect();
                match parts.split_last() {
                    Some((last, rest)) if !rest.is_empty() => {
                        format!("{} or {last}", rest.join(", "))
                    }
                    _ => parts.join(""),
                }
            }
            PathExpr::Filter { inner, set, keep } => format!(
                "{} {} `{set}`",
                inner.describe(),
                if *keep { "in" } else { "not in" }
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Name(String),
    Caret,
    Slash,
    Bar,
    Open,
    Close,
    LBracket,
    RBracket,
    Bang,
}

impl Token {
    fn text(&self) -> String {
        match self {
            Token::Name(n) => n.clone(),
            Token::Caret => "^".into(),
            Token::Slash => "/".into(),
            Token::Bar => "|".into(),
            Token::Open => "(".into(),
            Token::Close => ")".into(),
            Token::LBracket => "[".into(),
            Token::RBracket => "]".into(),
            Token::Bang => "!".into(),
        }
    }
}

fn tokenize(src: &str) -> Result<Vec<Token>, String> {
    let mut out = Vec::new();
    let mut chars = src.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let t = match c {
            c if c.is_whitespace() => continue,
            '^' => Token::Caret,
            '/' => Token::Slash,
            '|' => Token::Bar,
            '(' => Token::Open,
            ')' => Token::Close,
            '[' => Token::LBracket,
            ']' => Token::RBracket,
            '!' => Token::Bang,
            c if c.is_alphanumeric() || c == '_' || c == '-' => {
                let mut end = i + c.len_utf8();
                while let Some(&(j, d)) = chars.peek() {
                    if !(d.is_alphanumeric() || d == '_' || d == '-') {
                        break;
                    }
                    end = j + d.len_utf8();
                    chars.next();
                }
                Token::Name(src[i..end].to_string())
            }
            c => return Err(format!("`{c}` is not part of a path")),
        };
        out.push(t);
    }
    if out.is_empty() {
        return Err("the path is empty".into());
    }
    Ok(out)
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.at).cloned();
        self.at += 1;
        t
    }

    fn name(&mut self, after: &str) -> Result<String, String> {
        match self.next() {
            Some(Token::Name(n)) => Ok(n),
            Some(t) => Err(format!(
                "expected a name after `{after}`, found `{}`",
                t.text()
            )),
            None => Err(format!("expected a name after `{after}`")),
        }
    }

    fn seq(&mut self) -> Result<PathExpr, String> {
        let mut parts = vec![self.postfix()?];
        while self.peek() == Some(&Token::Slash) {
            self.at += 1;
            parts.push(self.postfix()?);
        }
        Ok(if parts.len() == 1 {
            parts.remove(0)
        } else {
            PathExpr::Seq(parts)
        })
    }

    fn postfix(&mut self) -> Result<PathExpr, String> {
        let mut p = self.primary()?;
        while self.peek() == Some(&Token::LBracket) {
            self.at += 1;
            let keep = if self.peek() == Some(&Token::Bang) {
                self.at += 1;
                false
            } else {
                true
            };
            let set = self.name("[")?;
            if self.next() != Some(Token::RBracket) {
                return Err(format!("`[{set}` is not closed with `]`"));
            }
            p = PathExpr::Filter {
                inner: Box::new(p),
                set,
                keep,
            };
        }
        Ok(p)
    }

    fn primary(&mut self) -> Result<PathExpr, String> {
        match self.next() {
            Some(Token::Caret) => Ok(PathExpr::Step {
                name: self.name("^")?,
                inverse: true,
            }),
            Some(Token::Name(name)) => Ok(PathExpr::Step {
                name,
                inverse: false,
            }),
            Some(Token::Open) => {
                let mut alts = vec![self.seq()?];
                while self.peek() == Some(&Token::Bar) {
                    self.at += 1;
                    alts.push(self.seq()?);
                }
                if self.next() != Some(Token::Close) {
                    return Err("`(` is not closed with `)`".into());
                }
                Ok(if alts.len() == 1 {
                    alts.remove(0)
                } else {
                    PathExpr::Alt(alts)
                })
            }
            Some(t) => Err(format!("a step cannot start with `{}`", t.text())),
            None => Err("the path ends where a step should be".into()),
        }
    }
}

#[cfg(test)]
#[path = "derived_tests.rs"]
mod tests;
