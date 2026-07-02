// SPDX-License-Identifier: AGPL-3.0-or-later
//! Call-graph trace builder — the *read* half of code-intelligence-in-chat.
//!
//! Given a corpus's SCIP graph and a symbol (matched by a retrieved summary
//! chunk), produce a compact, structured trace: who CALLS the symbol (the entry
//! / ancestry direction — surfaces the seam the question is really about) and
//! what the symbol CALLS (the implementation it delegates to). The chat runtime
//! appends a rendered trace to the synthesis evidence block, deterministically —
//! the chat answer path is single-shot, no tool-loop.
//!
//! Dynamic-dispatch boundaries (trait / dyn calls) are labelled, because that is
//! exactly where grep fails and the call-graph earns its keep.
//!
//! This lives in `corpus-engine-scip` (not `corpus-engine`) on purpose: it reads
//! the call graph via SQL over `scip_graph.db` and needs none of the tree-sitter
//! grammars. Keeping it here lets the chat runtime depend on this lean crate
//! directly — reading a graph must not drag the parser that built it into every
//! build. The grammars stay confined to the indexing path that writes the db.

use std::collections::HashSet;

use crate::scip_graph::{CallKind, ScipGraph};
use crate::Result;

/// Cap on callers/callees surfaced per symbol — keeps the evidence bounded.
const MAX_SITES: usize = 12;

/// One end of a call edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite {
    pub name: String,
    pub file_path: String,
    pub line: i32,
    /// Dispatch kind: "direct", "method", "trait", "dyn".
    pub kind: String,
}

/// A symbol's immediate (1-hop) call-graph neighborhood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolTrace {
    pub symbol: String,
    pub qualified_name: String,
    /// Who calls this symbol (the entry / ancestry direction).
    pub callers: Vec<CallSite>,
    /// What this symbol calls (the implementation).
    pub callees: Vec<CallSite>,
    pub callers_truncated: bool,
    pub callees_truncated: bool,
}

fn kind_label(k: &CallKind) -> &'static str {
    match k {
        CallKind::Direct => "direct",
        CallKind::Method => "method",
        CallKind::Trait => "trait",
        CallKind::Dynamic => "dyn",
    }
}

/// Build the 1-hop call-graph trace for a symbol. `qualified_name` may be empty
/// (callees are then skipped — `find_callees_qualified` needs the SCIP
/// descriptor; the code-intel store records it on every summary chunk, so it is
/// normally present).
pub async fn build_symbol_trace(
    scip: &ScipGraph,
    symbol: &str,
    qualified_name: &str,
) -> Result<SymbolTrace> {
    let (callers_raw, _staleness) = scip.find_callers(symbol, 1).await?;
    let mut callers: Vec<CallSite> = callers_raw
        .into_iter()
        .map(|c| CallSite {
            name: c.symbol_name,
            file_path: c.file_path,
            line: c.line,
            kind: kind_label(&c.call_kind).to_string(),
        })
        .collect();
    dedup_sites(&mut callers);
    let callers_truncated = callers.len() > MAX_SITES;
    callers.truncate(MAX_SITES);

    let mut callees: Vec<CallSite> = if qualified_name.is_empty() {
        Vec::new()
    } else {
        scip.find_callees_qualified(qualified_name)
            .await?
            .into_iter()
            .map(|c| CallSite {
                name: c.callee_name,
                file_path: c.file_path,
                line: c.line,
                kind: kind_label(&c.call_kind).to_string(),
            })
            .collect()
    };
    dedup_sites(&mut callees);
    let callees_truncated = callees.len() > MAX_SITES;
    callees.truncate(MAX_SITES);

    Ok(SymbolTrace {
        symbol: symbol.to_string(),
        qualified_name: qualified_name.to_string(),
        callers,
        callees,
        callers_truncated,
        callees_truncated,
    })
}

fn dedup_sites(sites: &mut Vec<CallSite>) {
    let mut seen = HashSet::new();
    sites.retain(|s| seen.insert((s.name.clone(), s.line)));
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn render_site(c: &CallSite) -> String {
    let boundary = if c.kind == "trait" || c.kind == "dyn" {
        format!("  [dyn-dispatch: {}]", c.kind)
    } else {
        String::new()
    };
    format!(
        "    - `{}`{}  ({}:{})\n",
        c.name,
        boundary,
        basename(&c.file_path),
        c.line
    )
}

/// Render a trace as a compact evidence block for the synthesis prompt.
/// Dynamic-dispatch boundaries (trait / dyn) are flagged — that is where the
/// call-graph beats grep.
pub fn render_trace(t: &SymbolTrace) -> String {
    let mut out = format!("Call-graph trace for `{}`:\n", t.symbol);
    out.push_str("  called by (entry points):\n");
    if t.callers.is_empty() {
        out.push_str("    (none recorded)\n");
    } else {
        for c in &t.callers {
            out.push_str(&render_site(c));
        }
        if t.callers_truncated {
            out.push_str("    ... (more)\n");
        }
    }
    out.push_str("  calls:\n");
    if t.callees.is_empty() {
        out.push_str("    (none recorded)\n");
    } else {
        for c in &t.callees {
            out.push_str(&render_site(c));
        }
        if t.callees_truncated {
            out.push_str("    ... (more)\n");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scip_graph::{ScipRefRecord, ScipSymbolRecord};

    fn sym(name: &str, qn: &str) -> ScipSymbolRecord {
        ScipSymbolRecord {
            name: name.to_string(),
            qualified_name: qn.to_string(),
            kind: "function".to_string(),
            file_path: "x.rs".to_string(),
            line_start: 0,
            line_end: 1,
            language: "rust".to_string(),
        }
    }

    fn edge(
        caller: &str,
        caller_q: &str,
        callee: &str,
        callee_q: &str,
        line: i32,
        kind: &str,
    ) -> ScipRefRecord {
        ScipRefRecord {
            caller_symbol: caller.to_string(),
            callee_symbol: callee.to_string(),
            caller_qualified: caller_q.to_string(),
            callee_qualified: callee_q.to_string(),
            file_path: "x.rs".to_string(),
            line,
            ref_kind: kind.to_string(),
        }
    }

    #[tokio::test]
    async fn trace_surfaces_callers_and_callees_and_flags_dyn_dispatch() {
        let scip = ScipGraph::open_in_memory("c").unwrap();
        // a -> b (direct);  b -> c (trait). Trace of b: caller a, callee c.
        scip.ingest_symbols_and_refs(
            vec![
                sym("a", "crate::a"),
                sym("b", "crate::b"),
                sym("c", "crate::c"),
            ],
            vec![
                edge("a", "crate::a", "b", "crate::b", 10, "direct"),
                edge("b", "crate::b", "c", "crate::c", 20, "trait"),
            ],
        )
        .await
        .unwrap();

        let t = build_symbol_trace(&scip, "b", "crate::b").await.unwrap();
        assert_eq!(
            t.callers
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["a"],
            "b is called by a"
        );
        assert_eq!(
            t.callees
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["c"],
            "b calls c"
        );
        assert_eq!(t.callees[0].kind, "trait");

        let rendered = render_trace(&t);
        assert!(rendered.contains("called by"));
        assert!(rendered.contains("`a`"));
        assert!(rendered.contains("`c`"));
        assert!(
            rendered.contains("dyn-dispatch: trait"),
            "trait boundary flagged"
        );
    }

    #[tokio::test]
    async fn empty_qualified_name_skips_callees() {
        let scip = ScipGraph::open_in_memory("c").unwrap();
        scip.ingest_symbols_and_refs(vec![sym("b", "crate::b")], vec![])
            .await
            .unwrap();
        let t = build_symbol_trace(&scip, "b", "").await.unwrap();
        assert!(t.callees.is_empty());
        assert!(t.callers.is_empty());
    }
}
