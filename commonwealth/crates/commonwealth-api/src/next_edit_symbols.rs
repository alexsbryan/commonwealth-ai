// SPDX-License-Identifier: AGPL-3.0-or-later
//! The symbol lane — call-site NAVIGATION for a signature edit.
//! Sibling of [`crate::next_edit_syntax`], and the opposite kind of
//! seam: syntax NARROWS the rule lane's sites, this one PROPOSES sites
//! the rule lane cannot reach at all.
//!
//! WHAT IT OFFERS, AND WHAT IT DELIBERATELY DOES NOT. When the cursor
//! sits in a function's parameter list AND that list differs from the
//! one in the last save, this names the call sites of that function:
//! `path`, `line`,
//! `col`, and the line's text. **It proposes no edit.** Accepting a
//! navigation site moves the cursor; nothing is written. That is not a
//! staging decision, it is what the measurement supports — see below.
//!
//! WHY NAVIGATION AND NOT EDITS (`gym/next-edit/aligned/`, M1a,
//! 2026-08-28). On the shape this lane triggers on — an existing
//! function whose parameter list changed — the graph's site RECALL is
//! 95.8%, cluster-bootstrap 95% CI [87.0, 100.0] over 13 independent
//! commits, comfortably above the pre-registered 80% bar. Site
//! PRECISION on the same population is 69.7% [34.4, 91.5]: the 60% bar
//! lies inside the interval, so precision is a COULD-NOT-JUDGE, not a
//! pass (ARCH §18.1). A jump list is exactly the affordance whose bar
//! is recall — a wrong entry costs a keystroke — while proposing text
//! at each site would be spending a precision number nobody has.
//!
//! WHY A LAST-SAVE INDEX IS THE RIGHT INPUT HERE, when
//! `next_edit_syntax` declines it as stale. That objection holds for a
//! rename: the symbol the user is creating is by definition not in the
//! index. It does not hold for a signature edit — the function EXISTED
//! before its parameter list was touched, so the saved graph knows it
//! and knows its callers, and the call sites are themselves unedited.
//! This is the one shape where the last save is precisely right.
//!
//! WHY THE TRIGGER COMPARES AGAINST THE SAVED DECLARATION. The obvious
//! trigger — "the cursor is in a parameter list and the user just
//! edited" — is a gate that cannot fail (ARCH §18.1): next-edit fires
//! on edit-settle with the cursor AT the edit, so "the edit touched the
//! list the cursor is in" is true by construction. Comparing the
//! buffer's parameter list against the saved one is the same intent
//! made falsifiable, and it is also the shape M1a measured rather than
//! an approximation of it: an EXISTING function whose PARAMETER LIST
//! CHANGED. It rules out the two classes that dominated M0's population
//! and that this lane must never fire on — a function being typed for
//! the first time (not in the index: [`Decline::SymbolNotIndexed`]) and
//! a file that merely moved (signature identical:
//! [`Decline::SignatureUnchanged`]).
//!
//! THE FREE FILTER, measured. `refs` is an OCCURRENCE table, not a call
//! table (`ref_kind` is uniformly `direct` across 1.36M rows). Of the
//! over-offered cross-file sites in M1a, 105 were `use` imports and
//! `pub use` re-export lists — and NOT ONE was a site the author
//! edited. Dropping occurrences with no call paren after them removes
//! all 105 and loses zero true sites, so [`is_call_site`] runs
//! unconditionally rather than behind a flag.

use corpus_engine_scip::scip_graph::{Caller, ScipGraph};

/// Sites returned to the client. A jump list longer than this is not a
/// navigation affordance any more; the response says it was truncated
/// rather than silently shortening (ARCH §18.3).
const MAX_SITES: usize = 50;

/// Buffers above this are not parsed, matching
/// [`crate::next_edit_syntax::SyntaxOracle::parse`]'s cap: the trigger
/// runs on every coalesced edit unit.
const MAX_PARSE_BYTES: usize = 1024 * 1024;

/// Languages whose grammar this lane knows how to read a parameter list
/// from, by `LanguageConfig::id`.
///
/// SEPARATE from `next_edit_syntax::PROVEN_LANGUAGES` on purpose: that
/// list records where a FILTER was measured to help, this one records
/// where the trigger's node kinds are implemented. Rust is the only
/// language the graph indexes today (`languages_with_scip` on this
/// host), so adding an id here without an index behind it would build a
/// trigger that can only ever find zero sites.
const TRIGGER_LANGUAGES: &[&str] = &["rust"];

/// Node kinds that ARE a function declaration, per grammar.
fn is_function_node(kind: &str) -> bool {
    matches!(
        kind,
        "function_item" | "function_signature_item" | "function_declaration"
    )
}

/// Node kinds that ARE the parameter list of one.
fn is_parameter_list(kind: &str) -> bool {
    matches!(kind, "parameters" | "parameter_list")
}

/// Why the lane said nothing. Every arm is a distinct, nameable state —
/// there is no "silent" default, so a client asking `debug: true` is
/// told which gate held (ARCH §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decline {
    /// No `path` on the request, so no grammar and no corpus.
    NoPath,
    /// No grammar for the extension, or not a trigger language.
    UnsupportedLanguage,
    /// Buffer above [`MAX_PARSE_BYTES`], or the parse failed.
    NotParsed,
    /// The cursor is not inside a function's parameter list.
    CursorNotInParameterList,
    /// The declaration has no name node.
    NoSymbolName,
    /// The buffer's parameter list matches the last save. Nothing has
    /// changed yet, so no caller is obliged to.
    SignatureUnchanged,
    /// The graph does not know this function — it is being written for
    /// the first time. It has no callers to name, and M1a found this
    /// class dominating M0's population precisely because a naive
    /// trigger cannot tell it apart.
    SymbolNotIndexed,
    /// More than one symbol in this file carries the name. An overload
    /// set cannot be told apart by name, and the union of their call
    /// sites is a precision claim nothing supports.
    AmbiguousSymbol,
    /// The graph could not be opened or queried.
    GraphUnavailable,
    /// The lookup outran its budget. The lane sits on the interactive
    /// typing path, so it is bounded rather than trusted: a graph being
    /// rewritten by the reindexer can hold SQLite's lock, and a jump
    /// list is never worth a stalled keystroke.
    TimedOut,
}

impl Decline {
    pub fn as_str(self) -> &'static str {
        match self {
            Decline::NoPath => "no_path",
            Decline::UnsupportedLanguage => "unsupported_language",
            Decline::NotParsed => "not_parsed",
            Decline::CursorNotInParameterList => "cursor_not_in_parameter_list",
            Decline::NoSymbolName => "no_symbol_name",
            Decline::SignatureUnchanged => "signature_unchanged",
            Decline::SymbolNotIndexed => "symbol_not_indexed",
            Decline::AmbiguousSymbol => "ambiguous_symbol",
            Decline::GraphUnavailable => "graph_unavailable",
            Decline::TimedOut => "timed_out",
        }
    }

    /// Does this decline mean the developer's SETUP is incomplete —
    /// something they can act on — rather than "nothing to offer here"?
    ///
    /// Only `GraphUnavailable` qualifies, and the exclusions are the
    /// point. `SymbolNotIndexed` fires for a brand-new function, which
    /// is the healthy common case AND indistinguishable from a stale
    /// index; `NoPath` fires for any scratch file outside the
    /// workspace. Warning on either would fire on ordinary keystrokes
    /// and train the reader to ignore the log, which costs more than
    /// the nag ever buys (the same argument `doctor` makes for
    /// `WATCHERS_OFF_MSG`).
    pub fn is_actionable(self) -> bool {
        matches!(self, Decline::GraphUnavailable)
    }

    /// One line naming what to run. Present only where there IS
    /// something to run — absence is the honest answer everywhere else.
    pub fn remedy(self) -> Option<&'static str> {
        match self {
            Decline::GraphUnavailable => Some(
                "next-edit's call-site jump list is off: no SCIP graph for this \
                 workspace. Run `svrn init` in the repo, then `svrn doctor` \
                 and read the `scip_indexed` line. Every other next-edit lane is \
                 unaffected.",
            ),
            _ => None,
        }
    }
}

/// The function whose signature is being edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    /// Short name, as written in the declaration.
    pub name: String,
    /// Byte range of the whole declaration in the current buffer.
    pub decl_start: usize,
    pub decl_end: usize,
    /// The parameter list as it stands in the buffer, whitespace
    /// normalised so reformatting is not mistaken for a contract
    /// change.
    pub params: String,
}

/// Parse `text` with the grammar registered for `path`'s extension,
/// if this lane covers that language. One parse helper, so the trigger
/// and the saved-declaration comparison cannot disagree about what a
/// parameter list is.
fn parse(path: &str, text: &str) -> Option<tree_sitter::Tree> {
    if text.len() > MAX_PARSE_BYTES {
        return None;
    }
    let ext = path.rsplit('.').next()?;
    let cfg = corpus_engine::extractors::code::language_for_extension(ext)?;
    if !TRIGGER_LANGUAGES.contains(&cfg.id) {
        return None;
    }
    let language: tree_sitter::Language = cfg.lang.into();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).ok()?;
    parser.parse(text, None)
}

/// A parameter list reduced to its CONTRACT, so that reformatting is
/// not read as a change. Comparing raw text — or even
/// whitespace-collapsed text — fires the lane on every rustfmt rewrap:
/// `(\n    a: usize,\n)` collapses to `( a: usize, )` and compares
/// unequal to `(a: usize)` on two counts, the padding and the trailing
/// comma. Both are dropped here.
///
/// Whitespace-insensitivity cannot make two DIFFERENT contracts
/// compare equal: Rust has no parameter list whose meaning depends on
/// spacing.
fn normalise(s: &str) -> String {
    let dense: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    dense.replace(",)", ")")
}

/// Is the cursor inside a function's parameter list, and what is that
/// list?
///
/// PURE — no graph, no filesystem, no clock. Anchoring on the parameter
/// LIST rather than on the declaration is what keeps a body edit, which
/// obliges no caller to change, from reaching the graph at all.
pub fn trigger_at(path: Option<&str>, text: &str, cursor: usize) -> Result<Trigger, Decline> {
    let path = path.ok_or(Decline::NoPath)?;
    if text.len() > MAX_PARSE_BYTES {
        return Err(Decline::NotParsed);
    }
    let ext = path.rsplit('.').next().ok_or(Decline::UnsupportedLanguage)?;
    let cfg = corpus_engine::extractors::code::language_for_extension(ext)
        .ok_or(Decline::UnsupportedLanguage)?;
    if !TRIGGER_LANGUAGES.contains(&cfg.id) {
        return Err(Decline::UnsupportedLanguage);
    }
    let tree = parse(path, text).ok_or(Decline::NotParsed)?;

    let mut node = tree
        .root_node()
        .descendant_for_byte_range(cursor, cursor)
        .ok_or(Decline::CursorNotInParameterList)?;
    let mut params = None;
    loop {
        if is_parameter_list(node.kind()) {
            params = Some(node);
            break;
        }
        match node.parent() {
            Some(p) => node = p,
            None => break,
        }
    }
    let params = params.ok_or(Decline::CursorNotInParameterList)?;
    let decl = params
        .parent()
        .filter(|p| is_function_node(p.kind()))
        .ok_or(Decline::CursorNotInParameterList)?;

    let name = decl
        .child_by_field_name("name")
        .and_then(|n| text.get(n.start_byte()..n.end_byte()))
        .ok_or(Decline::NoSymbolName)?
        .to_string();

    Ok(Trigger {
        name,
        decl_start: decl.start_byte(),
        decl_end: decl.end_byte(),
        params: normalise(text.get(params.start_byte()..params.end_byte()).unwrap_or_default()),
    })
}

/// The parameter list of `name` as declared in `snippet` — the saved
/// text the graph's line span points at. `None` when the snippet does
/// not parse or does not declare that function, which the caller must
/// treat as "cannot judge" rather than as "unchanged": firing on an
/// unreadable save would be guessing, and declining costs one missed
/// offer (ARCH §18.3).
pub fn params_of(path: &str, snippet: &str, name: &str) -> Option<String> {
    let tree = parse(path, snippet)?;
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if is_function_node(n.kind())
            && n.child_by_field_name("name")
                .and_then(|x| snippet.get(x.start_byte()..x.end_byte()))
                == Some(name)
        {
            if let Some(p) = (0..n.child_count())
                .filter_map(|i| n.child(i as u32))
                .find(|c| is_parameter_list(c.kind()))
            {
                return snippet.get(p.start_byte()..p.end_byte()).map(normalise);
            }
        }
        for i in 0..n.child_count() {
            if let Some(c) = n.child(i as u32) {
                stack.push(c);
            }
        }
    }
    None
}

/// One place to jump to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub path: String,
    /// 0-based, as the graph stores it.
    pub line: i32,
    pub col: i32,
    /// The line's text, trimmed — a jump list without context is a list
    /// of numbers.
    pub preview: String,
}

/// Is this occurrence a CALL, or merely a mention?
///
/// `end_col` is the column just past the name. A call has `(` next,
/// allowing whitespace and a turbofish; `use foo::bar;` and a
/// `pub use` re-export list do not. See the module header for the
/// measurement that makes this unconditional.
pub fn is_call_site(line: &str, end_col: i32) -> bool {
    if end_col < 0 {
        // No span recorded (legacy row): cannot judge, so keep it
        // rather than drop a site on a guess.
        return true;
    }
    let Some(rest) = char_slice_from(line, end_col as usize) else {
        return true;
    };
    let rest = rest.trim_start();
    let rest = rest.strip_prefix("::").map(str::trim_start).unwrap_or(rest);
    if let Some(after) = rest.strip_prefix('<') {
        // Turbofish: skip to its close before looking for the paren.
        let mut depth = 1usize;
        for (i, c) in after.char_indices() {
            match c {
                '<' => depth += 1,
                '>' => {
                    depth -= 1;
                    if depth == 0 {
                        return after[i + 1..].trim_start().starts_with('(');
                    }
                }
                _ => {}
            }
        }
        return false;
    }
    rest.starts_with('(')
}

/// `line`'s tail starting at CHARACTER index `col`. The graph's columns
/// are character offsets, so byte slicing would split a multi-byte
/// character and panic on the first non-ASCII line.
fn char_slice_from(line: &str, col: usize) -> Option<&str> {
    if col == 0 {
        return Some(line);
    }
    line.char_indices().nth(col).map(|(b, _)| &line[b..])
}

/// Turn the graph's callers into a jump list.
///
/// `read_line` supplies the text of `(path, line)` — the route passes
/// the LIVE BUFFER for the file being edited and the file on disk for
/// every other, because the index describes the last save. Returning
/// `None` drops that site: an occurrence whose line cannot be read
/// cannot be shown to a developer, and a jump into a line that moved is
/// worse than one entry fewer.
pub fn sites_from_callers<F>(
    callers: &[Caller],
    decl_path: &str,
    decl_line_span: Option<(i32, i32)>,
    name: &str,
    mut read_line: F,
) -> (Vec<Site>, bool, usize)
where
    F: FnMut(&str, i32) -> Option<String>,
{
    let mut out = Vec::new();
    let mut dropped = 0usize;
    for c in callers {
        // The declaration is the trigger, never a destination.
        if c.file_path == decl_path {
            if let Some((s, e)) = decl_line_span {
                if c.line >= s && c.line <= e {
                    continue;
                }
            }
        }
        let Some(text) = read_line(&c.file_path, c.line) else {
            dropped += 1;
            continue;
        };
        // Line drift: the graph describes the last save. If the name is
        // not on the line any more, the row does not describe this text
        // and the site is dropped rather than pointed at.
        if !text.contains(name) {
            dropped += 1;
            continue;
        }
        if !is_call_site(&text, c.end_col) {
            dropped += 1;
            continue;
        }
        out.push(Site {
            path: c.file_path.clone(),
            line: c.line,
            col: (c.end_col - name.chars().count() as i32).max(0),
            preview: text.trim().to_string(),
        });
    }
    let truncated = out.len() > MAX_SITES;
    out.truncate(MAX_SITES);
    (out, truncated, dropped)
}

/// The lane's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Navigation {
    pub symbol: String,
    pub sites: Vec<Site>,
    /// More call sites than [`MAX_SITES`]; the list was cut.
    pub truncated: bool,
    /// Occurrences the graph named that were NOT offered — a bare
    /// mention, or a line that no longer names the symbol. Reported so
    /// a shrinking list is visible rather than silent (ARCH §18.3).
    pub dropped: usize,
}

/// Resolve the trigger's short name to ONE qualified symbol declared in
/// this file, then ask the graph for its call sites.
///
/// The qualified descriptor is the whole point: `find_callers` matches
/// a short name, and on this repo's graph `new` maps to 631 distinct
/// symbols. Resolution is scoped to the file being edited, so the
/// answer is the function under the cursor rather than every function
/// that happens to share its name.
async fn resolve(
    graph: &ScipGraph,
    decl_path: &str,
    name: &str,
) -> Result<(Vec<Caller>, (i32, i32)), Decline> {
    let rows = graph
        .symbols_in_file(decl_path)
        .await
        .map_err(|_| Decline::GraphUnavailable)?;
    let mut matching = rows
        .iter()
        .filter(|r| r.name == name && !r.qualified_name.is_empty());
    let sym = matching.next().ok_or(Decline::SymbolNotIndexed)?;
    if matching.next().is_some() {
        return Err(Decline::AmbiguousSymbol);
    }
    let (callers, _caution) = graph
        .find_callers_qualified(&sym.qualified_name, name)
        .await
        .map_err(|_| Decline::GraphUnavailable)?;
    Ok((callers, (sym.line_start, sym.line_end)))
}

/// The whole lane, from buffer to jump list.
///
/// `read_saved` returns a file's contents AS OF THE LAST SAVE — which
/// is what the graph describes. The buffer is used for the file being
/// edited (its previews are live and its declaration is the new one);
/// every other file's previews come from `read_saved`. `None` from it
/// is "cannot read", never "empty".
pub async fn navigate<R>(
    graph: &ScipGraph,
    path: Option<&str>,
    text: &str,
    cursor: usize,
    read_saved: R,
) -> Result<Navigation, Decline>
where
    R: Fn(&str) -> Option<String>,
{
    let trigger = trigger_at(path, text, cursor)?;
    let decl_path = path.ok_or(Decline::NoPath)?;
    let (callers, decl_span) = resolve(graph, decl_path, &trigger.name).await?;

    // The falsifiable half of the trigger: the buffer's parameter list
    // must DIFFER from the saved one. An unreadable or unparsable save
    // declines rather than assuming a change.
    let saved = read_saved(decl_path).ok_or(Decline::SignatureUnchanged)?;
    let saved_params =
        params_of(decl_path, &saved, &trigger.name).ok_or(Decline::SignatureUnchanged)?;
    if saved_params == trigger.params {
        return Err(Decline::SignatureUnchanged);
    }

    let buffer_lines: Vec<&str> = text.split('\n').collect();
    let (sites, truncated, dropped) = sites_from_callers(
        &callers,
        decl_path,
        Some(decl_span),
        &trigger.name,
        |p, line| {
            if p == decl_path {
                buffer_lines.get(line as usize).map(|l| l.to_string())
            } else {
                read_saved(p)
                    .and_then(|src| src.split('\n').nth(line as usize).map(str::to_string))
            }
        },
    );
    Ok(Navigation {
        symbol: trigger.name,
        sites,
        truncated,
        dropped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"
fn helper(a: usize, b: usize) -> usize {
    a + b
}

fn caller() {
    let _ = helper(1, 2);
}
"#;

    fn at(needle: &str) -> usize {
        SRC.find(needle).expect("needle in SRC")
    }

    #[test]
    fn cursor_in_a_parameter_list_names_the_function_and_its_params() {
        let cursor = at("b: usize");
        let t = trigger_at(Some("x.rs"), SRC, cursor).unwrap();
        assert_eq!(t.name, "helper");
        assert_eq!(t.params, "(a:usize,b:usize)");
    }

    #[test]
    fn cursor_in_the_body_does_not_trigger() {
        assert_eq!(
            trigger_at(Some("x.rs"), SRC, at("a + b")),
            Err(Decline::CursorNotInParameterList)
        );
    }

    #[test]
    fn a_language_the_graph_does_not_index_declines_rather_than_finding_nothing() {
        assert_eq!(
            trigger_at(Some("x.ts"), "function f(a) {}", 11),
            Err(Decline::UnsupportedLanguage)
        );
        assert_eq!(trigger_at(None, SRC, 5), Err(Decline::NoPath));
    }

    #[test]
    fn the_saved_declaration_is_read_back_as_the_same_parameter_list() {
        // The comparison the trigger turns on: identical source must
        // normalise to an identical parameter list, or the lane would
        // fire on every keystroke in an untouched signature.
        let t = trigger_at(Some("x.rs"), SRC, at("b: usize")).unwrap();
        assert_eq!(params_of("x.rs", SRC, "helper").as_deref(), Some(t.params.as_str()));
    }

    #[test]
    fn reformatting_a_signature_is_not_a_contract_change() {
        // rustfmt rewrapping a long signature must not read as an edit
        // callers have to follow.
        let wrapped = "fn helper(\n    a: usize,\n    b: usize,\n) -> usize { a }";
        let flat = params_of("x.rs", SRC, "helper").unwrap();
        assert_eq!(params_of("x.rs", wrapped, "helper").unwrap(), flat);
    }

    #[test]
    fn a_changed_parameter_list_is_visible_as_a_difference() {
        let before = params_of("x.rs", SRC, "helper").unwrap();
        let after = params_of("x.rs", "fn helper(a: usize, b: usize, c: u8) -> usize { a }", "helper")
            .unwrap();
        assert_ne!(before, after);
    }

    #[test]
    fn params_of_a_function_that_is_not_there_is_none_not_empty() {
        // "cannot judge" and "no parameters" are different answers and
        // the caller branches on them differently (ARCH §18.3).
        assert_eq!(params_of("x.rs", SRC, "absent"), None);
        assert_eq!(params_of("x.rs", "fn f() {}", "f").as_deref(), Some("()"));
    }

    #[test]
    fn only_a_missing_graph_is_worth_warning_about() {
        // A warn that fires on ordinary typing is a warn nobody reads.
        assert!(Decline::GraphUnavailable.is_actionable());
        assert!(Decline::GraphUnavailable.remedy().is_some());
        for ordinary in [
            Decline::SymbolNotIndexed,      // a function being typed for the first time
            Decline::SignatureUnchanged,    // every keystroke in an untouched signature
            Decline::CursorNotInParameterList,
            Decline::UnsupportedLanguage,
            Decline::NoPath,
            Decline::NotParsed,
            Decline::NoSymbolName,
            Decline::AmbiguousSymbol,
            Decline::TimedOut,
        ] {
            assert!(!ordinary.is_actionable(), "{ordinary:?} must not warn");
            assert!(ordinary.remedy().is_none(), "{ordinary:?} has nothing to run");
        }
    }

    #[test]
    fn call_sites_are_kept_and_bare_mentions_are_not() {
        // The measured free filter: `use` imports and re-export lists
        // were 105 of M1a's over-offers and none was a real site.
        assert!(is_call_site("    let _ = helper(1, 2);", 18));
        assert!(is_call_site("    helper::<u8>(1);", 10));
        assert!(is_call_site("    helper (1);", 10));
        assert!(!is_call_site("use crate::thing::helper;", 24));
        assert!(!is_call_site("pub use helper, other;", 14));
    }

    #[test]
    fn a_column_past_a_multibyte_character_does_not_panic() {
        // The graph's columns are character offsets; byte slicing here
        // would split the `é` and panic.
        assert!(!is_call_site("// café helper;", 14));
    }

    #[test]
    fn a_site_whose_line_no_longer_names_the_symbol_is_dropped_not_pointed_at() {
        let callers = vec![
            Caller {
                symbol_name: "caller".into(),
                file_path: "a.rs".into(),
                line: 6,
                call_kind: corpus_engine_scip::scip_graph::CallKind::Direct,
                end_col: 18,
            },
            Caller {
                symbol_name: "drifted".into(),
                file_path: "b.rs".into(),
                line: 99,
                call_kind: corpus_engine_scip::scip_graph::CallKind::Direct,
                end_col: 18,
            },
        ];
        let (sites, truncated, dropped) =
            sites_from_callers(&callers, "decl.rs", None, "helper", |p, _| {
                match p {
                    "a.rs" => Some("    let _ = helper(1, 2);".to_string()),
                    _ => Some("    something_else();".to_string()),
                }
            });
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].path, "a.rs");
        assert_eq!(dropped, 1);
        assert!(!truncated);
    }

    #[test]
    fn the_declaration_itself_is_never_a_destination() {
        let callers = vec![Caller {
            symbol_name: "helper".into(),
            file_path: "decl.rs".into(),
            line: 1,
            call_kind: corpus_engine_scip::scip_graph::CallKind::Direct,
            end_col: 9,
        }];
        let (sites, _, _) =
            sites_from_callers(&callers, "decl.rs", Some((1, 3)), "helper", |_, _| {
                Some("fn helper(a: usize) -> usize {".to_string())
            });
        assert!(sites.is_empty());
    }

    #[test]
    fn a_jump_list_past_the_cap_reports_truncation_rather_than_shortening_silently() {
        let callers: Vec<Caller> = (0..MAX_SITES as i32 + 5)
            .map(|i| Caller {
                symbol_name: "c".into(),
                file_path: format!("f{i}.rs"),
                line: i,
                call_kind: corpus_engine_scip::scip_graph::CallKind::Direct,
                end_col: 10,
            })
            .collect();
        let (sites, truncated, _) =
            sites_from_callers(&callers, "decl.rs", None, "helper", |_, _| {
                Some("    helper();".to_string())
            });
        assert_eq!(sites.len(), MAX_SITES);
        assert!(truncated);
    }
}
