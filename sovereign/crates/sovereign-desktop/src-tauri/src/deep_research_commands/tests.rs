// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for the deep-research desktop driver.

use super::report::constitution_check;
use super::*;
use sovereign_core::deep_research::icd::{
    BudgetAllowance, CharterValues, ContainmentConfig, CorroborationRecord, CustodyPolicy,
    EmptyWindow, EvidenceWindow as Ew, FinalClaim, Gap, TriageConfig, UrlConstraintPolicy,
    WindowChunk,
};
use sovereign_core::egress::ConsentGrant;
use sovereign_core::types::Custody;

fn write_json(dir: &Path, name: &str, value: &impl Serialize) {
    std::fs::write(dir.join(name), serde_json::to_vec(value).unwrap()).unwrap();
}

fn charter_values(consent: Option<ConsentGrant>) -> CharterValues {
    CharterValues {
        max_rounds: 3,
        evidence_window_max_chunks: 20,
        containment: ContainmentConfig {
            trigger: "witness".to_string(),
            extraction_max_tokens: 256,
            specifics_max: 3,
        },
        triage: TriageConfig {
            code_set_k: 3,
            eps_quota: 0.1,
            content_coverage_floor:
                sovereign_core::deep_research::acquisition::DEFAULT_CONTENT_COVERAGE_FLOOR,
            prose_line_floor: sovereign_core::deep_research::acquisition::DEFAULT_PROSE_LINE_FLOOR,
        },
        budget: BudgetAllowance {
            web_search_queries: 4,
            web_fetch_pages: 4,
        },
        custody: CustodyPolicy {
            stamp_required: true,
            unknown_refuses: true,
        },
        url_constraint: UrlConstraintPolicy {
            enabled: true,
            layer: "strict".to_string(),
        },
        consent,
    }
}

fn fixture_charter(dir: &Path, question: &str) {
    write_json(
        dir,
        "charter.json",
        &Charter {
            icd: "charter".to_string(),
            version: 1,
            run_id: "dr-100".to_string(),
            question: question.to_string(),
            seed_id: None,
            created_at_unix: 100,
            charter: charter_values(Some(ConsentGrant {
                run_id: "dr-100".to_string(),
                granted_at_unix: 100,
                release_floor: Custody::PublicWeb,
            })),
            frozen: true,
        },
    );
}

fn fixture_gap_list(dir: &Path, round: u32, gaps: Vec<Gap>) {
    write_json(
        dir,
        &format!("gap-list-{round}.json"),
        &GapList {
            icd: "gap-list".to_string(),
            version: 1,
            run_id: "dr-100".to_string(),
            charter_hash: "h".to_string(),
            round,
            claims: Vec::new(),
            gaps,
            empty_evidence_windows: Vec::<EmptyWindow>::new(),
            strict_subset_of_prior: false,
        },
    );
}

fn fixture_budget(dir: &Path) {
    write_json(
        dir,
        "budget-ledger.json",
        &BudgetLedger {
            icd: "budget-ledger".to_string(),
            version: 1,
            run_id: "dr-100".to_string(),
            charter_hash: "h".to_string(),
            allowance: HashMap::new(),
            entries: Vec::new(),
            spent: HashMap::from([("web".to_string(), 2)]),
            remaining: HashMap::from([("web".to_string(), 2)]),
            refused_urls: Vec::new(),
        },
    );
}

#[test]
fn snapshot_reads_round_gaps_budget_and_consent() {
    let dir = std::env::temp_dir().join(format!("dr-snap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    fixture_charter(&dir, "When did Apollo 11 land?");
    fixture_gap_list(
        &dir,
        1,
        vec![Gap {
            id: "g1".to_string(),
            text: "the landing date needs a second origin".to_string(),
            actionable_query: "Apollo 11 landing date".to_string(),
            from_claim_id: Some("c1".to_string()),
            corroboration: None,
        }],
    );
    fixture_budget(&dir);

    let snap = RunDirPoller::new(dir.clone()).snapshot().unwrap();
    assert_eq!(snap.round, Some(1));
    assert_eq!(snap.stage, "rounding");
    assert_eq!(snap.gaps.len(), 1);
    assert_eq!(snap.gaps[0].id, "g1");
    assert_eq!(snap.budget.spent.get("web"), Some(&2));
    assert_eq!(snap.budget.remaining.get("web"), Some(&2));
    let consent = snap.consent.unwrap();
    assert_eq!(consent.release_floor, "public-web");
    assert_eq!(consent.granted_at_unix, 100);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn snapshot_is_none_before_the_charter_lands() {
    let dir = std::env::temp_dir().join(format!("dr-presnap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let snap = RunDirPoller::new(dir.clone()).snapshot();
    assert!(snap.is_none(), "no charter — no run state to show");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_consent_means_default_deny_is_reported() {
    let dir = std::env::temp_dir().join(format!("dr-noconsent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write_json(
        &dir,
        "charter.json",
        &Charter {
            icd: "charter".to_string(),
            version: 1,
            run_id: "dr-101".to_string(),
            question: "Q".to_string(),
            seed_id: None,
            created_at_unix: 101,
            charter: charter_values(None),
            frozen: true,
        },
    );
    let snap = RunDirPoller::new(dir.clone()).snapshot().unwrap();
    assert!(snap.consent.is_none(), "default-deny must read as no grant");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stage_advances_with_the_artifacts() {
    let dir = std::env::temp_dir().join(format!("dr-stage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    fixture_charter(&dir, "Q");
    fixture_budget(&dir);

    let poller = RunDirPoller::new(dir.clone());
    let snap = poller.snapshot().unwrap();
    assert_eq!(snap.stage, "planning", "charter only — planning");

    fixture_gap_list(&dir, 1, Vec::new());
    let snap = poller.snapshot().unwrap();
    assert_eq!(snap.stage, "rounding", "gap-list-1 — rounding");

    std::fs::write(
        dir.join("verdict-set.json"),
        serde_json::to_vec(&VerdictSet {
            icd: "verdict-set".to_string(),
            version: 1,
            run_id: "dr-100".to_string(),
            charter_hash: "h".to_string(),
            claims: Vec::new(),
            empty_rounds: Vec::new(),
        })
        .unwrap(),
    )
    .unwrap();
    let snap = poller.snapshot().unwrap();
    assert_eq!(
        snap.stage, "checking",
        "verdict-set — the writing is checked"
    );

    std::fs::write(dir.join("report.md"), "# Report").unwrap();
    let snap = poller.snapshot().unwrap();
    assert_eq!(snap.stage, "done", "report.md — done");
    assert!(poller.report_md().is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

fn window(dir: &Path, round: u32, chunks: Vec<WindowChunk>) {
    write_json(
        dir,
        &format!("evidence-window-{round}.json"),
        &Ew {
            icd: "evidence-window".to_string(),
            version: 1,
            run_id: "dr-100".to_string(),
            charter_hash: "h".to_string(),
            round,
            chunks,
            fetch_failures: Vec::new(),
            dedup_refused: Vec::new(),
            content_refused: Vec::new(),
            derived_custody: "personal".to_string(),
        },
    );
}

fn passed_claim_set() -> VerdictSet {
    VerdictSet {
        icd: "verdict-set".to_string(),
        version: 1,
        run_id: "dr-100".to_string(),
        charter_hash: "h".to_string(),
        claims: Vec::new(),
        empty_rounds: Vec::new(),
    }
}

#[test]
fn constitution_holds_when_every_passed_figure_is_traced() {
    let dir = std::env::temp_dir().join(format!("dr-const-ok-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    window(
        &dir,
        1,
        vec![WindowChunk {
            id: "c1".to_string(),
            locator: "estate:x:1".to_string(),
            source_url: "https://example.com/a".to_string(),
            custody: "personal".to_string(),
            provenance_class: "primary".to_string(),
            content: "Apollo 11 landed on July 20, 1969.".to_string(),
            ingested_into: None,
            tags: Vec::new(),
        }],
    );
    let mut vs = passed_claim_set();
    vs.claims.push(FinalClaim {
        id: "c1".to_string(),
        text: "Apollo 11 landed on July 20, 1969.".to_string(),
        verdict: Verdict::Passed,
        status: "passed".to_string(),
        evidence_ids: vec!["c1".to_string()],
        citations: Vec::new(),
        flag: None,
        corroboration: Some(CorroborationRecord {
            origins: vec!["https://example.com/a".to_string()],
            support_chunks: 1,
            floor: 2,
            passes_floor: false,
        }),
    });
    write_json(&dir, "verdict-set.json", &vs);
    let check = constitution_check(&dir, Some(&vs));
    assert_eq!(check.passed_claims, 1);
    assert!(check.violations.is_empty(), "{:?}", check.violations);
    assert_eq!(check.unresolved, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn constitution_names_an_untraced_figure_in_a_passed_claim() {
    let dir = std::env::temp_dir().join(format!("dr-const-bad-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // The claim carries "2024" which the evidence never mentions.
    window(
        &dir,
        1,
        vec![WindowChunk {
            id: "c1".to_string(),
            locator: "estate:x:1".to_string(),
            source_url: "https://example.com/a".to_string(),
            custody: "personal".to_string(),
            provenance_class: "primary".to_string(),
            content: "The bridge opened in 1930.".to_string(),
            ingested_into: None,
            tags: Vec::new(),
        }],
    );
    let mut vs = passed_claim_set();
    vs.claims.push(FinalClaim {
        id: "c1".to_string(),
        text: "The bridge opened in 1930 and was restored in 2024.".to_string(),
        verdict: Verdict::Passed,
        status: "passed".to_string(),
        evidence_ids: vec!["c1".to_string()],
        citations: Vec::new(),
        flag: None,
        corroboration: None,
    });
    write_json(&dir, "verdict-set.json", &vs);
    let check = constitution_check(&dir, Some(&vs));
    assert_eq!(check.passed_claims, 1);
    assert_eq!(check.violations.len(), 1, "{:?}", check.violations);
    assert!(
        check.violations[0].contains("2024"),
        "{}",
        check.violations[0]
    );
    assert_eq!(check.unresolved, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unresolved_evidence_is_reported_not_defaulted() {
    let dir = std::env::temp_dir().join(format!("dr-const-unres-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // No evidence windows at all — the claim's ids resolve nowhere.
    let mut vs = passed_claim_set();
    vs.claims.push(FinalClaim {
        id: "c1".to_string(),
        text: "Something passed.".to_string(),
        verdict: Verdict::Passed,
        status: "passed".to_string(),
        evidence_ids: vec!["missing".to_string()],
        citations: Vec::new(),
        flag: None,
        corroboration: None,
    });
    write_json(&dir, "verdict-set.json", &vs);
    let check = constitution_check(&dir, Some(&vs));
    assert_eq!(check.passed_claims, 1);
    assert!(check.violations.is_empty());
    assert_eq!(check.unresolved, 1, "unresolvable evidence is counted");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn consent_class_refuses_an_unknown_class_and_passes_the_closed_set() {
    assert_eq!(consent_class("public-web"), Ok(Custody::PublicWeb));
    assert_eq!(consent_class("peer"), Ok(Custody::Peer));
    assert_eq!(consent_class("personal"), Ok(Custody::Personal));
    assert!(
        consent_class("everything").is_err(),
        "a typo must not reach a run"
    );
    assert!(
        consent_class("unknown").is_err(),
        "a grant never releases unknown provenance"
    );
}

#[test]
fn demo_backend_override_is_absent_unless_the_demo_var_is_set() {
    // The env mutation could race a parallel test that reads the var —
    // none does (it is demo-only), but the lock keeps the intent loud.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe { std::env::remove_var("SOVEREIGN_DEMO_DR_FLAGS") };
    assert!(
        demo_backend_override().is_none(),
        "unset = the live port; real flows must not gain a mock backend"
    );
    unsafe {
        std::env::set_var(
            "SOVEREIGN_DEMO_DR_FLAGS",
            "--backend mock --mock-deck /tmp/deep-research-deck",
        )
    };
    assert_eq!(
        demo_backend_override(),
        Some((
            "mock".to_string(),
            Some(PathBuf::from("/tmp/deep-research-deck"))
        )),
        "the demo pass-through lands in typed launch options, not an argv"
    );
    unsafe { std::env::remove_var("SOVEREIGN_DEMO_DR_FLAGS") };
}

/// THE SHIPPING TEST, structurally (§7: make it structural, not
/// remembered). Deep research used to run by spawning `svrn
/// deep-research`, found by probing PATH — so a desktop-only install,
/// which has no CLI on PATH, got zero runs, and an install that DID
/// have one could bind a different version than the app was built
/// against. Neither failure is visible in a unit test of behaviour;
/// both are visible here.
///
/// Watched red against the pre-lift file at HEAD: it spawned a child
/// process twice and probed the CLI-path override.
///
/// The scan is scoped to the PRODUCTION half of each file — everything
/// above `#[cfg(test)]`. This test necessarily spells the forbidden
/// tokens, and an instrument that trips on its own prose measures
/// nothing (the same trap as note 8714cf3c, where a render gate
/// matched the sentence describing it).
///
/// EVERY file of the driver is scanned, not just the one that carries
/// this module. When the driver was carved into `mod`/`live`/`report`/
/// `runs` the single `include_str!` would have kept passing while
/// covering a quarter of the code — a check that narrows silently is
/// worse than one that fails (§18.3).
#[test]
fn the_driver_starts_no_subprocess_and_probes_no_path() {
    for (file, src) in [
        ("mod.rs", include_str!("mod.rs")),
        ("live.rs", include_str!("live.rs")),
        ("report.rs", include_str!("report.rs")),
        ("runs.rs", include_str!("runs.rs")),
    ] {
        // Only `mod.rs` carries a `#[cfg(test)]`; the rest are production
        // whole, so "no test module" means "scan all of it", not "skip".
        let body = src.split_once("#[cfg(test)]").map_or(src, |(head, _)| head);
        for forbidden in [
            "Command::new",
            "SOVEREIGN_CLI_PATH",
            "sovereign-cli",
            ".local/bin/sovereign",
        ] {
            assert!(
                !body.contains(forbidden),
                "the deep-research driver must not reach for a CLI binary, but \
                     {file} names `{forbidden}` — a desktop-only install has none, and a \
                     PATH hit can be a different version than this build"
            );
        }
    }
}

/// A token the closed set does not name is IGNORED rather than
/// forwarded: with no second process to parse it, passing it on
/// would mean pretending it did something.
#[test]
fn demo_backend_override_ignores_tokens_it_does_not_name() {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe {
        std::env::set_var(
            "SOVEREIGN_DEMO_DR_FLAGS",
            "--backend mock --nonsense 7 --mock-deck /tmp/d",
        )
    };
    assert_eq!(
        demo_backend_override(),
        Some(("mock".to_string(), Some(PathBuf::from("/tmp/d"))))
    );
    unsafe { std::env::remove_var("SOVEREIGN_DEMO_DR_FLAGS") };
}
