// SPDX-License-Identifier: AGPL-3.0-or-later
//! The wire differ — stage 6 of the refactor factory
//! (`quality/REFACTOR_FACTORY.md`), and the reason the factory can ever run
//! unattended:
//!
//! > For every type whose definition a refactor touches, serialise a fixture
//! > before and after and diff the bytes — across every surface the spec
//! > declares. A non-empty diff FAILS the refactor unless the spec declares
//! > the change intentional.
//!
//! **The compiler is exhaustive over TYPES and blind to ENCODING.** The
//! measured near-miss this gate exists for: `node_id: String -> NodeId`
//! compiles clean while turning `"node_id":"node-6c955b5f1361aaaa"` into
//! `"node_id":[108,149,…]` on live mesh endpoints — `define_id!` derives
//! serde on a `[u8; 16]` tuple struct, and derived serde on a byte tuple is
//! an integer array.
//!
//! # How a verdict is reached
//!
//! The byte computation is `kernel_types::wire::WireFixture` — the ONE
//! implementation (§10.6) that `ContentHash`'s and `CorpusId`'s wire tests
//! also assert through. This module owns what stage 6 adds on top:
//!
//! - the fixture REGISTRY ([`known_targets`]) — which types have fixtures,
//!   and every string rendering their production `String` sites carry today
//!   (an open set of types, so a registry, ARCH §4);
//! - the SURFACE set ([`WireSurface`]) — which persisted encodings the
//!   differ can serialise (a closed set, so an enum, ARCH §2);
//! - the VERDICT mapping — every check lands in a
//!   [`kernel_types::Judgement`], four verdicts, not two (§18.1): a surface
//!   the differ cannot serialise is *could-not-judge*, a surface the spec
//!   did not declare is *never-ran*, and neither is ever a pass. Declaring
//!   fewer surfaces cannot narrow the proof: known-but-undeclared surfaces
//!   fail the gate as never-ran, which is what closes the H2 escape hatch
//!   ("the differ's surface list is incomplete") from the inside.
//!
//! An item failing this gate is not scheduled — it is filed as a finding
//! (per-item entry gate, `quality/REFACTOR_FACTORY.md`). `node-id.toml` in
//! `quality/refactors/` is exactly that: the negative control, kept failing
//! on purpose, never to be applied.

use std::path::Path;

use kernel_types::wire::{WireFixture, WireFixtureError};
use kernel_types::{CorpusId, Judgement, NodeId, Reason, Verdict};
use serde::Deserialize;

// ── The spec, as stage 6 consumes it ────────────────────────────────────

/// The slice of a `quality/refactors/*.toml` spec the wire differ reads.
/// The full schema (discover/prepare/rules) belongs to the plan/classify
/// path; unknown tables and keys are deliberately ignored here so the two
/// readers cannot fight over fields only one of them consumes.
#[derive(Debug, Deserialize)]
pub struct RefactorSpec {
    pub id: String,
    /// Rust path of the adopted type, e.g. `kernel_types::CorpusId`. The
    /// registry key.
    pub target: String,
    #[serde(default)]
    pub safety: SafetySpec,
}

/// The `[safety]` table: what the spec CLAIMS about the wire, and which
/// persisted surfaces the claim covers. The differ's job is to prove or
/// refute the claim — never to assume it.
#[derive(Debug, Default, Deserialize)]
pub struct SafetySpec {
    /// `"transparent"` (bytes must be identical) or `"intentional"` (the
    /// diff is declared, migration tooling must exist elsewhere). Absent
    /// means unprovable, not transparent — absence is reported, never
    /// defaulted (§18.3).
    pub wire: Option<String>,
    #[serde(default)]
    pub surfaces: Vec<String>,
}

/// The wire claim a spec may make. Closed set (ARCH §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WireClaim {
    /// Adoption must not change a single byte on any declared surface.
    Transparent,
    /// The spec declares the encoding change on purpose; a diff is expected
    /// and byte-identity would mean the declaration is stale.
    Intentional,
}

impl WireClaim {
    fn parse(s: &str) -> Option<WireClaim> {
        match s {
            "transparent" => Some(WireClaim::Transparent),
            "intentional" => Some(WireClaim::Intentional),
            _ => None,
        }
    }
}

/// Every persisted encoding the differ knows how to serialise. Closed set
/// (ARCH §2); a spec declaring anything else gets *could-not-judge*, and
/// extending this enum is how the differ grows a surface — never by
/// special-casing one spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireSurface {
    /// serde_json bytes — HTTP responses, mesh records, JSON blobs in
    /// sqlite TEXT columns. Judged via `kernel_types::wire`.
    Json,
    /// The TEXT bytes rusqlite binds for a column parameter (e.g.
    /// `params![corpus_id, …]`, corpus-engine/src/facts_store.rs:202).
    Sqlite,
}

impl WireSurface {
    pub const ALL: &'static [WireSurface] = &[WireSurface::Json, WireSurface::Sqlite];

    pub fn parse(s: &str) -> Option<WireSurface> {
        WireSurface::ALL.iter().copied().find(|w| w.as_str() == s)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            WireSurface::Json => "json",
            WireSurface::Sqlite => "sqlite",
        }
    }
}

// ── The fixture registry ────────────────────────────────────────────────

/// One string rendering production `String` sites carry today, labelled
/// with where it was observed so a failure names real code, not a made-up
/// example.
struct JsonForm {
    label: &'static str,
    fixture: Result<WireFixture, WireFixtureError>,
}

/// A type the differ holds fixtures for. Adding a registry entry is the
/// whole cost of making a new type provable.
struct TargetFixture {
    /// Rust path, the registry key — matches a spec's `target`.
    target: &'static str,
    /// JSON surface: every observed string rendering, each diffed against
    /// the typed value's serde bytes.
    json: Vec<JsonForm>,
    /// sqlite surface: the TEXT bytes a `String` site binds today next to
    /// the TEXT rendering the typed value would bind — or the reason there
    /// is no single rendering to prove against.
    sqlite: Result<(String, String), &'static str>,
}

/// The registry (ARCH §4 — an open set of types). Every entry is a value
/// the production sites actually carry, with the source cited on the label.
fn known_targets() -> Vec<TargetFixture> {
    vec![
        corpus_id_fixture(),
        node_id_fixture(),
        content_hash_fixture(),
    ]
}

fn corpus_id_fixture() -> TargetFixture {
    // `#[serde(transparent)]` (kernel-types/src/ids.rs) — the positive
    // control: adoption must be a type change, not a data migration.
    let c = CorpusId::new("wikipedia").expect("non-empty literal");
    TargetFixture {
        target: "kernel_types::CorpusId",
        json: vec![JsonForm {
            label: "as_str (the one rendering; serde is transparent)",
            fixture: WireFixture::json(c.as_str(), &c),
        }],
        sqlite: Ok(("wikipedia".to_string(), c.as_str().to_string())),
    }
}

fn node_id_fixture() -> TargetFixture {
    // The negative control, pinned to the PRODUCTION fixture value in
    // commonwealth-api/src/routes_status.rs:662 ("node-6c955b5f1361aaaa").
    // `define_id!` derives serde over `[u8; 16]`: a 16-integer JSON array.
    let id = NodeId::from_u128(0x6c95_5b5f_1361_aaaa_0123_4567_89ab_cdef);
    TargetFixture {
        target: "kernel_types::NodeId",
        json: vec![
            JsonForm {
                label: "Display (StatusResponse.node_id, routes_status.rs:324)",
                fixture: WireFixture::json(&id.to_string(), &id),
            },
            JsonForm {
                label: "to_hex (PeerRequestStatus.node_id, routes_status.rs:428)",
                fixture: WireFixture::json(&id.to_hex(), &id),
            },
        ],
        sqlite: Err(
            "NodeId has no single canonical TEXT rendering to prove a String column \
             against: Display truncates to 8 bytes (`node-6c955b5f1361aaaa`), to_hex \
             is all 16 (`6c955b5f…abcdef`), and derived serde is a 16-int array. \
             kernel-types holds three incompatible id encodings (CorpusId transparent \
             string, ContentHash hex string, define_id! byte array) — no define_id! \
             id may be adopted at a String site until that is resolved",
        ),
    }
}

fn content_hash_fixture() -> TargetFixture {
    // Hand-written serde as a plain hex string (kernel-types/src/hash.rs) —
    // the original the wire decider generalises.
    let h = kernel_types::ContentHash::of_str("wire");
    let hex = h.to_hex();
    TargetFixture {
        target: "kernel_types::ContentHash",
        json: vec![JsonForm {
            label: "to_hex (the one rendering; serde writes hex)",
            fixture: WireFixture::json(&hex, &h),
        }],
        sqlite: Ok((hex.clone(), hex)),
    }
}

// ── Judging ─────────────────────────────────────────────────────────────

/// The differ's output: one [`Judgement`] per check, plus the roll-up. The
/// rows carry the exact bytes in their reasons — the report IS the evidence
/// (principle 1).
pub struct WireReport {
    pub rows: Vec<Judgement>,
    pub overall: Judgement,
}

impl WireReport {
    /// The gate: did every declared surface get PROVEN? Anything else —
    /// failed, could-not-judge, never-ran — is not a pass (§18.2).
    pub fn passes(&self) -> bool {
        self.overall.verdict() == Verdict::Passed
    }
}

/// Run stage 6 for one spec. Pure over its inputs: the registry fixtures
/// are compiled in, so a verdict is reproducible from the spec text alone.
pub fn prove(spec: &RefactorSpec) -> WireReport {
    let subject_root = format!("{} wire", spec.id);
    let mut rows: Vec<Judgement> = Vec::new();

    let Some(claim) = spec.safety.wire.as_deref().and_then(WireClaim::parse) else {
        let why = match spec.safety.wire.as_deref() {
            None => "spec declares no [safety] wire claim — nothing to prove against; \
                     declare wire = \"transparent\" or wire = \"intentional\""
                .to_string(),
            Some(other) => format!(
                "[safety] wire = {other:?} is not a claim the differ knows \
                 (transparent | intentional)"
            ),
        };
        let row = Judgement::could_not_judge(
            format!("{subject_root} claim"),
            Reason::new(why).expect("non-placeholder literal"),
        );
        trace_row(&row);
        let overall = Judgement::roll_up(subject_root, [&row]);
        return WireReport {
            rows: vec![row],
            overall,
        };
    };

    let targets = known_targets();
    let fixture = targets.iter().find(|t| t.target == spec.target);

    if spec.safety.surfaces.is_empty() {
        rows.push(Judgement::could_not_judge(
            format!("{subject_root} surfaces"),
            Reason::new(format!(
                "spec declares no [safety] surfaces — a diff over no surfaces proves \
                 nothing; the differ knows: {}",
                known_surface_list()
            ))
            .expect("non-placeholder"),
        ));
    }

    for declared in &spec.safety.surfaces {
        let subject = format!("{subject_root} {declared}");
        let row = match WireSurface::parse(declared) {
            None => Judgement::could_not_judge(
                subject,
                Reason::new(format!(
                    "declared surface {declared:?} is not one the differ can serialise \
                     (knows: {}) — extend refactor_wire::WireSurface before scheduling",
                    known_surface_list()
                ))
                .expect("non-placeholder"),
            ),
            Some(surface) => match fixture {
                None => Judgement::could_not_judge(
                    subject,
                    Reason::new(format!(
                        "the differ has no fixture for {} — register one in \
                         refactor_wire::known_targets before this refactor can be \
                         scheduled",
                        spec.target
                    ))
                    .expect("non-placeholder"),
                ),
                Some(f) => {
                    // One row per fixture form, bytes on every row — the
                    // report IS the evidence, and a roll-up here would bury
                    // the exact before/after under a member list.
                    for row in judge_surface(&subject, surface, f, claim) {
                        trace_row(&row);
                        rows.push(row);
                    }
                    continue;
                }
            },
        };
        trace_row(&row);
        rows.push(row);
    }

    // A surface the differ KNOWS but the spec did not declare never ran —
    // and an unproven surface fails the gate. This is what stops a spec
    // from narrowing the proof by under-declaring (§18.1: a guard asserting
    // only on fields the subject supplies is not a guard).
    for known in WireSurface::ALL {
        if !spec
            .safety
            .surfaces
            .iter()
            .any(|s| WireSurface::parse(s) == Some(*known))
        {
            let row = Judgement::never_ran(
                format!("{subject_root} {}", known.as_str()),
                Reason::new(format!(
                    "surface {:?} is known to the differ but the spec does not declare \
                     it — declare it under [safety] surfaces so it is judged; an \
                     undeclared surface is an unproven one",
                    known.as_str()
                ))
                .expect("non-placeholder"),
            );
            trace_row(&row);
            rows.push(row);
        }
    }

    let overall = Judgement::roll_up(subject_root, rows.iter());
    WireReport { rows, overall }
}

fn known_surface_list() -> String {
    WireSurface::ALL
        .iter()
        .map(|w| w.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// One declared, serialisable surface against one registered fixture. One
/// row per fixture form — every observed string rendering must satisfy the
/// claim, and each row carries its own bytes.
fn judge_surface(
    subject: &str,
    surface: WireSurface,
    fixture: &TargetFixture,
    claim: WireClaim,
) -> Vec<Judgement> {
    match surface {
        WireSurface::Json => fixture
            .json
            .iter()
            .map(|form| {
                let form_subject = format!("{subject} [{}]", form.label);
                match &form.fixture {
                    Err(WireFixtureError::Serialize(e)) => Judgement::could_not_judge(
                        form_subject,
                        Reason::new(format!("fixture did not serialise: {e}"))
                            .expect("non-placeholder"),
                    ),
                    Err(WireFixtureError::RoundTrip { after, detail }) => Judgement::failed(
                        form_subject,
                        Reason::new(format!(
                            "typed value does not survive its own wire form {after}: {detail}"
                        ))
                        .expect("non-placeholder"),
                    ),
                    Ok(f) => judge_bytes(form_subject, &f.before, &f.after, claim),
                }
            })
            .collect(),
        WireSurface::Sqlite => vec![match &fixture.sqlite {
            Err(reason) => Judgement::could_not_judge(
                subject.to_string(),
                Reason::new(reason.to_string()).expect("non-placeholder"),
            ),
            Ok((before, after)) => judge_bytes(subject.to_string(), before, after, claim),
        }],
    }
}

/// The claim adjudication, stated once: identical bytes prove a transparent
/// claim and refute an intentional one; diverging bytes refute a
/// transparent claim and satisfy an intentional one. The bytes ride in the
/// reason either way.
fn judge_bytes(subject: String, before: &str, after: &str, claim: WireClaim) -> Judgement {
    let identical = before == after;
    match (claim, identical) {
        (WireClaim::Transparent, true) => Judgement::passed(
            subject,
            Reason::new(format!("byte-identical before and after: {before}"))
                .expect("non-placeholder"),
        ),
        (WireClaim::Transparent, false) => Judgement::failed(
            subject,
            Reason::new(format!(
                "adoption REWRITES the wire: before={before} after={after}"
            ))
            .expect("non-placeholder"),
        ),
        (WireClaim::Intentional, false) => Judgement::passed(
            subject,
            Reason::new(format!(
                "declared-intentional wire change observed: before={before} after={after}"
            ))
            .expect("non-placeholder"),
        ),
        (WireClaim::Intentional, true) => Judgement::failed(
            subject,
            Reason::new(format!(
                "spec declares an intentional wire change but the bytes are identical \
                 ({before}) — the declaration is stale or wrong"
            ))
            .expect("non-placeholder"),
        ),
    }
}

fn trace_row(row: &Judgement) {
    tracing::debug!(
        subject = row.subject(),
        verdict = row.verdict().as_str(),
        reason = row.reason().as_str(),
        "wire differ row"
    );
}

// ── CLI ─────────────────────────────────────────────────────────────────

pub fn load_spec(path: &Path) -> Result<RefactorSpec, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("cannot parse {}: {e}", path.display()))
}

/// `svrn code wire-check <spec.toml>` — run stage 6 for one spec and print
/// the rows. Exit 0 only when every surface is PROVEN.
pub async fn run(args: &[String]) -> i32 {
    let Some(path) = args.first().filter(|a| !a.starts_with('-')) else {
        eprintln!("usage: svrn code wire-check <quality/refactors/SPEC.toml>");
        return 2;
    };
    let spec = match load_spec(Path::new(path)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("wire-check: {e}");
            return 2;
        }
    };
    let report = prove(&spec);
    println!(
        "wire differ — spec {} target {} claim {}",
        spec.id,
        spec.target,
        spec.safety.wire.as_deref().unwrap_or("(none)")
    );
    print!("{}", kernel_types::render_rows(&report.rows));
    println!(
        "overall: {} — {}",
        report.overall.label(),
        report.overall.reason()
    );
    if report.passes() {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_spec(rel: &str) -> RefactorSpec {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join(rel);
        load_spec(&path).expect("control spec must load")
    }

    fn row<'a>(report: &'a WireReport, subject_contains: &str) -> &'a Judgement {
        report
            .rows
            .iter()
            .find(|r| r.subject().contains(subject_contains))
            .unwrap_or_else(|| {
                panic!(
                    "no row with subject containing {subject_contains:?}; have: {:?}",
                    report.rows.iter().map(|r| r.subject()).collect::<Vec<_>>()
                )
            })
    }

    /// THE NEGATIVE CONTROL — the reason this order exists. The differ must
    /// FAIL `node_id: String -> NodeId`, with the production bytes in the
    /// verdict. A gate with no failing input you can name is not a gate
    /// (ARCH §18.1).
    #[test]
    fn negative_control_node_id_fails_with_the_production_bytes() {
        let spec = repo_spec("quality/refactors/node-id.toml");
        let report = prove(&spec);
        assert!(
            !report.passes(),
            "the negative control PASSED — the differ is a formatter"
        );
        assert_eq!(report.overall.verdict(), Verdict::Failed);

        // The Display form: exactly what StatusResponse.node_id serves today
        // (routes_status.rs:662 pins the same value).
        let display = row(&report, "Display");
        assert_eq!(display.verdict(), Verdict::Failed);
        assert!(
            display
                .reason()
                .as_str()
                .contains("before=\"node-6c955b5f1361aaaa\""),
            "{}",
            display.reason()
        );
        assert!(
            display
                .reason()
                .as_str()
                .contains("after=[108,149,91,95,19,97,170,170,1,35,69,103,137,171,205,239]"),
            "{}",
            display.reason()
        );

        // The to_hex form (PeerRequestStatus.node_id) diverges too.
        let hex = row(&report, "to_hex");
        assert_eq!(hex.verdict(), Verdict::Failed);
        assert!(
            hex.reason()
                .as_str()
                .contains("before=\"6c955b5f1361aaaa0123456789abcdef\""),
            "{}",
            hex.reason()
        );

        // And sqlite is unprovable — three incompatible id encodings in the
        // kernel; not a pass, not silently skipped.
        let sqlite = row(&report, "wire sqlite");
        assert_eq!(sqlite.verdict(), Verdict::CouldNotJudge);
        assert!(sqlite
            .reason()
            .as_str()
            .contains("three incompatible id encodings"));
    }

    /// THE POSITIVE CONTROL. `corpus_id: String -> CorpusId` is transparent
    /// on every declared surface, so the differ must PASS it. The spec text
    /// mirrors `quality/REFACTOR_FACTORY.md`'s corpus-id example; the
    /// on-disk `corpus-id.toml` belongs to the plan path (rf-1) and is
    /// deliberately not read here.
    #[test]
    fn positive_control_corpus_id_passes_every_declared_surface() {
        let spec: RefactorSpec = toml::from_str(
            r#"
                id     = "corpus-id"
                kind   = "newtype"
                target = "kernel_types::CorpusId"

                [safety]
                wire     = "transparent"
                surfaces = ["json", "sqlite"]
            "#,
        )
        .unwrap();
        let report = prove(&spec);
        assert!(
            report.passes(),
            "positive control failed: {}",
            kernel_types::render_rows(&report.rows)
        );
        assert!(row(&report, "wire json")
            .reason()
            .as_str()
            .contains("\"wikipedia\""));
    }

    /// A declared surface the differ cannot serialise is could-not-judge,
    /// and could-not-judge is not a pass (§18.2).
    #[test]
    fn an_unknown_surface_is_could_not_judge_never_a_pass() {
        let spec: RefactorSpec = toml::from_str(
            r#"
                id = "corpus-id"
                target = "kernel_types::CorpusId"
                [safety]
                wire = "transparent"
                surfaces = ["json", "sqlite", "parquet"]
            "#,
        )
        .unwrap();
        let report = prove(&spec);
        assert!(!report.passes());
        assert_eq!(report.overall.verdict(), Verdict::CouldNotJudge);
        assert_eq!(row(&report, "parquet").verdict(), Verdict::CouldNotJudge);
    }

    /// A target with no registered fixture cannot be proven — and an
    /// unproven refactor is not scheduled.
    #[test]
    fn an_unregistered_target_is_could_not_judge() {
        let spec: RefactorSpec = toml::from_str(
            r#"
                id = "origin"
                target = "kernel_types::Origin"
                [safety]
                wire = "transparent"
                surfaces = ["json", "sqlite"]
            "#,
        )
        .unwrap();
        let report = prove(&spec);
        assert!(!report.passes());
        assert!(row(&report, "wire json")
            .reason()
            .as_str()
            .contains("no fixture for kernel_types::Origin"));
    }

    /// Under-declaring surfaces must not narrow the proof: a surface the
    /// differ knows but the spec omits is NEVER-RAN and fails the gate.
    #[test]
    fn an_undeclared_known_surface_is_never_ran_and_fails_the_gate() {
        let spec: RefactorSpec = toml::from_str(
            r#"
                id = "corpus-id"
                target = "kernel_types::CorpusId"
                [safety]
                wire = "transparent"
                surfaces = ["json"]
            "#,
        )
        .unwrap();
        let report = prove(&spec);
        assert!(!report.passes());
        assert_eq!(row(&report, "wire sqlite").verdict(), Verdict::NeverRan);
    }

    /// No surfaces at all proves nothing.
    #[test]
    fn a_spec_with_no_surfaces_proves_nothing() {
        let spec: RefactorSpec = toml::from_str(
            r#"
                id = "corpus-id"
                target = "kernel_types::CorpusId"
                [safety]
                wire = "transparent"
            "#,
        )
        .unwrap();
        let report = prove(&spec);
        assert!(!report.passes());
        assert_eq!(row(&report, "surfaces").verdict(), Verdict::CouldNotJudge);
    }

    /// No wire claim: unprovable, not transparent-by-default (§18.3).
    #[test]
    fn a_missing_wire_claim_is_could_not_judge_not_transparent() {
        let spec: RefactorSpec = toml::from_str(
            r#"
                id = "corpus-id"
                target = "kernel_types::CorpusId"
                [safety]
                surfaces = ["json", "sqlite"]
            "#,
        )
        .unwrap();
        let report = prove(&spec);
        assert!(!report.passes());
        assert_eq!(report.overall.verdict(), Verdict::CouldNotJudge);
    }

    /// An intentional claim must match reality in BOTH directions: identical
    /// bytes refute it exactly as a diff refutes a transparent claim.
    #[test]
    fn an_intentional_claim_with_identical_bytes_is_stale_and_fails() {
        let spec: RefactorSpec = toml::from_str(
            r#"
                id = "corpus-id"
                target = "kernel_types::CorpusId"
                [safety]
                wire = "intentional"
                surfaces = ["json", "sqlite"]
            "#,
        )
        .unwrap();
        let report = prove(&spec);
        assert!(!report.passes());
        assert!(row(&report, "wire json")
            .reason()
            .as_str()
            .contains("declaration is stale"));
    }

    /// The declared-intentional path in the passing direction: node-id's
    /// json diff is allowed when declared — but the gate still refuses the
    /// whole spec because sqlite stays unprovable. Declaring intent buys
    /// nothing the fixtures cannot back.
    #[test]
    fn an_intentional_node_id_json_diff_is_allowed_but_sqlite_still_blocks() {
        let spec: RefactorSpec = toml::from_str(
            r#"
                id = "node-id"
                target = "kernel_types::NodeId"
                [safety]
                wire = "intentional"
                surfaces = ["json", "sqlite"]
            "#,
        )
        .unwrap();
        let report = prove(&spec);
        assert_eq!(row(&report, "wire json").verdict(), Verdict::Passed);
        assert_eq!(
            row(&report, "wire sqlite").verdict(),
            Verdict::CouldNotJudge
        );
        assert!(!report.passes());
    }
}
