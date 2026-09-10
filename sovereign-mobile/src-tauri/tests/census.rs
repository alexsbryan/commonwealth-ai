// SPDX-License-Identifier: AGPL-3.0-or-later
//! **The twin census for `sv-one-client`** — the bar says the client family
//! is the ONLY out-of-process consumer of the wire, and this is what makes
//! that structural rather than remembered (ARCH §7: encode the invariant so
//! it cannot be forgotten).
//!
//! # What it checks, and why source text
//!
//! It reads this crate's own `src/` and fails if any `enum` or `struct`
//! declared here re-declares a wire variant set — `Token` + `Complete`,
//! `Prompt` + `Notice`, an `Answer`-shaped set, and so on. Source text
//! rather than reflection because the failure being prevented is a NEW
//! declaration, and a type that does not exist yet cannot be reflected on.
//! It is the same reason the concept ratchet reads source.
//!
//! # The planted twin (§18.1 — a check with no failing input is not a check)
//!
//! `the_census_catches_a_planted_twin` runs the detector over a literal that
//! IS the mirror R6 deleted, and requires it to be caught. So the detector
//! is proven to fire before `no_wire_mirror_in_this_crate` is believed when
//! it passes. A census that has only ever been green is inventory.
//!
//! # What it deliberately does NOT flag
//!
//! Re-exports (`pub use sovereign_contracts::…`) and imports. Naming a
//! contract type is the whole point; DECLARING one beside it is the defect.

use std::path::{Path, PathBuf};

/// The wire's variant names, grouped by the enum that owns them. A local
/// type is a twin when it declares a QUORUM of one group — two or more —
/// which is what tells a mirror apart from a domain type that happens to
/// have a variant called `Complete`.
const WIRE_VARIANT_SETS: &[(&str, &[&str])] = &[
    (
        "TurnFrame",
        &[
            "Token",
            "Complete",
            "StreamError",
            "Narration",
            "QueuePosition",
            "Prompt",
            "Notice",
        ],
    ),
    ("TurnPrompt", &["Approval", "UserInput", "Information"]),
    ("TurnAnswer", &["Approved", "Text", "Information"]),
    (
        "TurnNotice",
        &[
            "TurnStarted",
            "TurnSettled",
            "StepDone",
            "MessageRefined",
            "LessonProposed",
            "ResolveAck",
            "InterpretationProposed",
            "ClarificationRequest",
        ],
    ),
    (
        "TurnRequest",
        &["Message", "Answer", "Resume", "Redirect", "Cancel"],
    ),
];

/// Minimum variants of one set a local declaration must carry to count as a
/// twin. Two, not one: `Message` alone is an ordinary word.
const QUORUM: usize = 2;

/// One offending declaration.
#[derive(Debug, PartialEq)]
struct Twin {
    /// The local type's name.
    type_name: String,
    /// Which wire enum it twins.
    mirrors: String,
    /// The variants it re-declared.
    variants: Vec<String>,
}

/// Find every type in `source` that re-declares a wire variant set.
///
/// Scans `enum`/`struct` blocks and matches their bodies against the sets
/// above. A `pub use` line declares nothing, so it is never a block and is
/// never scanned — the mechanism, not a special case.
fn twins(source: &str) -> Vec<Twin> {
    let mut found = Vec::new();
    let bytes: Vec<&str> = source.lines().collect();
    let mut i = 0;
    while i < bytes.len() {
        let line = bytes[i].trim_start();
        let Some(name) = declaration_name(line) else {
            i += 1;
            continue;
        };
        // Take the block body by brace depth from this line.
        let mut depth = 0usize;
        let mut body = String::new();
        let mut j = i;
        let mut opened = false;
        while j < bytes.len() {
            for c in bytes[j].chars() {
                if c == '{' {
                    depth += 1;
                    opened = true;
                } else if c == '}' {
                    depth = depth.saturating_sub(1);
                }
            }
            body.push_str(bytes[j]);
            body.push('\n');
            j += 1;
            if opened && depth == 0 {
                break;
            }
        }
        for (wire, variants) in WIRE_VARIANT_SETS {
            let hits: Vec<String> = variants
                .iter()
                .filter(|v| declares_variant(&body, v))
                .map(|v| v.to_string())
                .collect();
            if hits.len() >= QUORUM {
                found.push(Twin {
                    type_name: name.clone(),
                    mirrors: wire.to_string(),
                    variants: hits,
                });
            }
        }
        i = j.max(i + 1);
    }
    found
}

/// `enum Foo {` / `pub struct Bar {` → `Foo` / `Bar`. Anything else → None.
fn declaration_name(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix("pub ")
        .unwrap_or(line)
        .trim_start_matches("pub(crate) ");
    let rest = rest
        .strip_prefix("enum ")
        .or_else(|| rest.strip_prefix("struct "))?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// A variant is DECLARED when the identifier starts a line inside the block
/// (`    Token {`, `    Complete,`). A mention inside a doc comment or a
/// match arm on an imported type is not a declaration.
fn declares_variant(body: &str, variant: &str) -> bool {
    body.lines().skip(1).any(|l| {
        let t = l.trim_start();
        if t.starts_with("//") || t.starts_with("#[") {
            return false;
        }
        t.strip_prefix(variant).is_some_and(|rest| {
            rest.starts_with(['{', '(', ',', ':'])
                || rest.trim_start().starts_with('{')
                || rest.is_empty()
        })
    })
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src dir").flatten() {
        let p = entry.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

/// THE BAR. `sovereign-mobile` declares no type that mirrors a wire variant
/// set — every frame, prompt, notice, answer and request it handles is the
/// contract's own, reached through `sovereign-turn-client`.
#[test]
fn no_wire_mirror_in_this_crate() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(!files.is_empty(), "the census must actually read something");

    let mut offences = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).expect("read source");
        for t in twins(&text) {
            offences.push(format!(
                "{}: `{}` re-declares {} of `{}` ({})",
                f.strip_prefix(&src).unwrap_or(f).display(),
                t.type_name,
                t.variants.len(),
                t.mirrors,
                t.variants.join(", ")
            ));
        }
    }

    assert!(
        offences.is_empty(),
        "sv-one-client: the client family must be the only consumer of the \
         wire, but this crate re-declares part of it. Use the type from \
         `sovereign_contracts::types` instead of copying its variants.\n  {}",
        offences.join("\n  ")
    );
}

/// The census fires on the exact thing it exists to catch — `remote/dto.rs`'s
/// `ServerEvent` as it stood at c7e7d6a73, quoted verbatim.
#[test]
fn the_census_catches_a_planted_twin() {
    let planted = r#"
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ServerEvent {
    Token {
        message_id: String,
        chunk: String,
    },
    Complete {
        message_id: String,
        provenance: Option<ProvenanceDto>,
        citations: Vec<CitationDto>,
    },
    StreamError {
        message: String,
        retry_after_secs: Option<u64>,
    },
    Narration {
        message_id: String,
        text: String,
    },
    #[serde(other)]
    Ignored,
}
"#;
    let found = twins(planted);
    assert_eq!(
        found.len(),
        1,
        "the deleted mirror must be caught, got {found:?}"
    );
    assert_eq!(found[0].type_name, "ServerEvent");
    assert_eq!(found[0].mirrors, "TurnFrame");
    assert_eq!(
        found[0].variants,
        ["Token", "Complete", "StreamError", "Narration"],
        "all four of its variants are TurnFrame's"
    );
}

/// A second plant, on the half the frame enum does not cover: an inbound
/// request twin. Without this, the census could pass by only ever knowing
/// about outbound frames.
#[test]
fn the_census_catches_a_planted_request_twin() {
    let planted = r#"
pub enum ClientMessage {
    Message { content: String },
    Cancel {},
}
"#;
    let found = twins(planted);
    assert_eq!(found.len(), 1, "an inbound twin counts too, got {found:?}");
    assert_eq!(found[0].mirrors, "TurnRequest");
}

/// The detector must not fire on IMPORTING the contract, which is what
/// adoption looks like — otherwise the fix for a red census would be to
/// stop using the family, which is backwards.
#[test]
fn importing_the_contract_is_not_a_twin() {
    let adopted = r#"
use sovereign_contracts::types::{TurnAnswer, TurnFrame, TurnNotice, TurnPrompt};
pub use sovereign_contracts::types::projection::{Citation, Provenance};

fn handle(f: TurnFrame) {
    match f {
        TurnFrame::Token { .. } => {}
        TurnFrame::Complete { .. } => {}
        TurnFrame::StreamError { .. } => {}
        TurnFrame::Notice { .. } => {}
        _ => {}
    }
}
"#;
    assert_eq!(
        twins(adopted),
        vec![],
        "matching on the contract's own enum is the point, not the defect"
    );
}
