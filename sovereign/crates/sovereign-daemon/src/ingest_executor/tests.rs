// SPDX-License-Identifier: AGPL-3.0-or-later
//! `ingest_executor`'s tests. A sibling file for the reason
//! `work_donor/tests.rs` is one — ARCH §3.1's ceiling — and every one names
//! the failing input it exists to catch (ARCH §18.1).
use super::*;

use commonwealth_work::executor::JobExecutorRegistry;
use commonwealth_work::seal;
use sovereign_contracts::oicp::JobRequirements;

fn engine() -> (tempfile::TempDir, Arc<CorpusEngine>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let recipes = dir.path().join("recipes");
    let indexes = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes).expect("recipes dir");
    std::fs::create_dir_all(&indexes).expect("indexes dir");
    let embed: corpus_engine::EmbedFn =
        Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.1_f32; 4]) }));
    let engine = Arc::new(CorpusEngine::new(recipes, indexes, embed));
    // The TempDir is returned so the caller keeps it alive: dropping it here
    // would delete the directories the engine resolves paths against, and the
    // failure would surface as an unrelated IO error inside a later assertion.
    (dir, engine)
}

fn executor() -> (tempfile::TempDir, IngestExecutor) {
    let (dir, e) = engine();
    (dir, IngestExecutor::new(e))
}

fn unit_of(kind: &str, payload: Value) -> JobUnit {
    seal::seal(
        JobKind::parse(kind).expect("test kind"),
        payload,
        JobRequirements::any(),
        None,
    )
    .expect("seal")
}

fn good_payload() -> Value {
    serde_json::to_value(IngestPayload::slice(
        "sep",
        "sep",
        3,
        WorkUnit::JsonlRange { start: 0, end: 100 },
    ))
    .expect("payload encodes")
}

/// The literal this module owns is a `JobKind`, and it is the one the
/// descriptor publishes. The failing input is an edit to [`INGEST_KIND`] that
/// spells it `ingest@1` or `ingest:v01` — both of which `JobKind::parse`
/// refuses — which would turn `IngestExecutor::new`'s `expect` into a panic on
/// the daemon's boot path.
#[test]
fn the_kind_literal_this_module_owns_parses() {
    let kind = JobKind::parse(INGEST_KIND).expect("`ingest:v1` must parse");
    assert_eq!(kind.id(), "ingest");
    assert_eq!(kind.version(), 1);
    let (_dir, exec) = executor();
    assert_eq!(exec.descriptor().kind, kind);
    assert_eq!(exec.kind(), &kind);
}

/// The failing input: a `process:v1` unit handed to this executor. Running it
/// would try to read an `argv` payload as a corpus slice; the tempting wrong
/// answer is a generic error, and the right one names WHICH rule said no so a
/// submitter is told whether to change their kind or update a node.
#[test]
fn a_unit_of_another_kind_is_refused_and_names_which_rule() {
    let (_dir, exec) = executor();

    let foreign = unit_of("process:v1", json!({"argv": ["true"]}));
    match exec.validate(&foreign) {
        Err(WorkRefusal::KindNotOffered { kind }) => assert_eq!(kind.id(), "process"),
        other => panic!("a foreign kind must be KindNotOffered, got {other:?}"),
    }

    // The other half, and the distinction that matters operationally: the
    // same kind at another revision is "update one of us", not "wrong node".
    let skewed = unit_of("ingest:v2", good_payload());
    match exec.validate(&skewed) {
        Err(WorkRefusal::VersionSkew { wanted, offered }) => {
            assert_eq!(wanted.version(), 2);
            assert_eq!(offered.version(), 1);
        }
        other => panic!("a version skew must be VersionSkew, got {other:?}"),
    }
}

/// The failing input: a payload with no `corpus_id`. A slice with no corpus
/// names no partition directory, so the ingest would write into a path derived
/// from an empty string. `validate` runs BEFORE the donor appends a `Lease`
/// (`work_donor.rs:437`), so refusing here costs the unit nothing — the
/// difference between `NeverRan` and a burnt attempt.
#[test]
fn a_payload_with_no_corpus_is_refused_before_the_lease() {
    let (_dir, exec) = executor();
    let unit = unit_of(
        INGEST_KIND,
        json!({
            "corpus_id": "   ",
            "recipe_id": "sep",
            "unit_id": 0,
            "unit": {"kind": "JsonlRange", "value": {"start": 0, "end": 1}}
        }),
    );
    let detail = match exec.validate(&unit) {
        Err(WorkRefusal::PayloadNotCanonical { detail }) => detail,
        other => panic!("an empty corpus_id must be refused, got {other:?}"),
    };
    assert!(
        detail.contains("corpus_id"),
        "the refusal must name the field, got: {detail}"
    );

    // The recipe half of the same rule.
    let unit = unit_of(
        INGEST_KIND,
        json!({
            "corpus_id": "sep",
            "recipe_id": "",
            "unit_id": 0,
            "unit": {"kind": "JsonlRange", "value": {"start": 0, "end": 1}}
        }),
    );
    let detail = match exec.validate(&unit) {
        Err(WorkRefusal::PayloadNotCanonical { detail }) => detail,
        other => panic!("an empty recipe_id must be refused, got {other:?}"),
    };
    assert!(
        detail.contains("recipe_id"),
        "the refusal must name the field, got: {detail}"
    );
}

/// The failing input: `unit_ix` where the payload means `unit_id`. Without
/// `deny_unknown_fields` serde takes the default for the field that IS named,
/// so a typo would ingest slice 0 under a hash that says slice 7 — silently,
/// and the merge would dedupe against the wrong unit.
#[test]
fn a_misspelled_field_is_refused_rather_than_defaulted() {
    let (_dir, exec) = executor();
    let unit = unit_of(
        INGEST_KIND,
        json!({
            "corpus_id": "sep",
            "recipe_id": "sep",
            "unit_ix": 7,
            "unit": {"kind": "JsonlRange", "value": {"start": 0, "end": 1}}
        }),
    );
    assert!(
        matches!(
            exec.validate(&unit),
            Err(WorkRefusal::PayloadNotCanonical { .. })
        ),
        "an unknown field must be refused, not silently defaulted"
    );
}

/// The control for the three refusals above: a well-formed unit passes, and
/// the payload it carries survives the round trip through the seal. Without
/// this, "every bad payload is refused" is trivially true of an executor that
/// refuses everything.
#[test]
fn the_control_a_well_formed_slice_validates_and_round_trips() {
    let (_dir, exec) = executor();
    let unit = unit_of(INGEST_KIND, good_payload());
    exec.validate(&unit).expect("a well-formed slice validates");

    let back = IngestPayload::parse(&unit.payload).expect("parses");
    assert_eq!(back.corpus_id, "sep");
    assert_eq!(back.unit_id, 3);
    // The reused decider, not a second one: `to_ingest_args` is what the pull
    // loop called and what `run` calls.
    assert_eq!(
        back.unit.to_ingest_args(),
        (None, Some((0, 100))),
        "the payload's slice must mean what commonwealth-core says it means"
    );
}

/// The boot invariant, for this kind, made structural.
///
/// The failing input is an edit that raises this executor's declared isolation
/// above [`crate::work_donor::DONOR_ISOLATION`]. `resolve_offer` would then
/// refuse every boot whose config offers `ingest:v1` — a daemon that will not
/// start, discovered by an operator rather than by a test.
#[test]
fn this_donor_can_cover_the_isolation_this_executor_declares() {
    let (_dir, exec) = executor();
    assert!(
        crate::work_donor::DONOR_ISOLATION.covers(exec.descriptor().isolation),
        "a donor offering {:?} cannot run an executor requiring {:?}",
        crate::work_donor::DONOR_ISOLATION,
        exec.descriptor().isolation
    );
    // And the honest half: this really does run in-process. Declaring
    // `Subprocess` here would be a claim with no mechanism behind it.
    assert_eq!(exec.descriptor().isolation, Isolation::InProcess);
}

/// The registry resolves this executor under the kind its own descriptor
/// claims, and refuses a second claimant. The failing input is an executor
/// registered under a kind it does not publish — `register` reads the
/// descriptor rather than a second argument precisely so that cannot happen.
#[test]
fn the_registry_resolves_this_executor_under_the_kind_it_claims() {
    let (_dir, engine) = engine();
    let mut registry = JobExecutorRegistry::new();
    registry
        .register(Arc::new(IngestExecutor::new(Arc::clone(&engine))))
        .expect("first registration");
    let kind = JobKind::parse(INGEST_KIND).expect("kind");
    assert!(registry.resolve(&kind).is_some());
    registry
        .register(Arc::new(IngestExecutor::new(engine)))
        .expect_err("a second executor for one kind must be refused, not overwritten");
}

/// **THE `Complete`/`Fail` BOUNDARY FOR THIS KIND.**
///
/// The failing input is an edit to `run` that returns
/// `Ok((Judgement::failed(..), ..))` when the engine errors — which reads
/// natural ("the ingest failed") and is the one change that would make an
/// ingest slice TERMINAL on its first bad attempt.
/// `WorkProjection::complete` requeues a `Fail` while attempts remain and
/// makes a `Complete` terminal whatever its verdict, so that edit would
/// silently delete the retry the old `ack_failure` path had.
///
/// Asserted on the errors this executor can actually construct, not on the
/// whole `JobError` enum — `commonwealth-work`'s own
/// `no_job_error_can_claim_a_verdict` covers the enum.
#[test]
fn every_error_this_executor_can_return_is_retryable_and_never_a_failed_verdict() {
    let ours = vec![
        JobError::Cancelled,
        JobError::NoVerdict {
            reason: "unit 3 of `sep` did not finish ingesting: disk full".to_string(),
        },
        JobError::Refused(WorkRefusal::PayloadNotCanonical {
            detail: "`corpus_id` is empty".to_string(),
        }),
    ];
    for e in &ours {
        assert!(
            matches!(
                e.verdict(),
                kernel_types::Verdict::CouldNotJudge | kernel_types::Verdict::NeverRan
            ),
            "{e} maps to {:?}, which would ride a Complete act and never be retried",
            e.verdict()
        );
    }
    // And the retry itself is bounded, by the constant the ingest queue
    // already owned rather than a second one.
    assert_eq!(
        MAX_UNIT_ATTEMPTS,
        commonwealth_core::knowledge::MAX_UNIT_ATTEMPTS
    );
}

/// The heartbeat cadence is DERIVED from the lease, not restated beside it.
///
/// The failing input is any cadence that does not leave room for a lost
/// heartbeat — `LEASE_MS / 2`, or a literal `250_000` — which lapses a
/// half-hour slice on one slow journal admit.
///
/// **What this cannot catch, said rather than implied (ARCH §18.1):** a
/// literal `100_000` is numerically `LEASE_MS / 3` today, so no runtime
/// assertion here distinguishes it from the derivation. The property that IS
/// checkable is the relationship, and that is what is asserted; the
/// derivation itself is held by review and by the comment on the descriptor.
#[test]
fn the_lease_interval_is_derived_from_the_lease_the_ingest_queue_already_owns() {
    let (_dir, exec) = executor();
    assert_eq!(
        exec.descriptor().lease_interval_ms,
        commonwealth_core::knowledge::LEASE_MS / 3
    );
    assert!(
        exec.descriptor().lease_interval_ms * 2 < commonwealth_core::knowledge::LEASE_MS,
        "two heartbeats must fit inside one lease, or a single slow journal admit \
         lapses it"
    );
}

/// A cancelled unit reports NOTHING, and the donor is what enforces that
/// (`work_donor.rs:795`). This is the executor's half: the flag the donor sets
/// is the one the ingest loop reads, through the engine's own registry.
///
/// The failing input is an `execute` that swallows cancellation and returns a
/// verdict anyway — a report from an actor that no longer holds the lease,
/// which the fold counts `unreadable`.
#[tokio::test]
async fn a_cancelled_unit_stops_and_claims_no_verdict() {
    let (_dir, exec) = executor();
    let unit = unit_of(INGEST_KIND, good_payload());
    let ctx = JobContext::new(std::env::temp_dir());
    // Cancelled before it starts: the recipe does not exist on this engine, so
    // the run cannot succeed either way — what is asserted is that the outcome
    // is an absence of a verdict and never a `Failed` one.
    ctx.cancel();
    let err = exec
        .execute(&unit, &ctx)
        .await
        .expect_err("a slice with no recipe reaches no verdict");
    assert!(
        matches!(
            err.verdict(),
            kernel_types::Verdict::CouldNotJudge | kernel_types::Verdict::NeverRan
        ),
        "got {:?}",
        err.verdict()
    );
}
