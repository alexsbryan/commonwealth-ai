// SPDX-License-Identifier: AGPL-3.0-or-later
//! `distribute`'s run: submit, poll, survey and merge, over cw-rails' doors. A sibling of
//! `distribute.rs` only so that file stays under ARCH §3.1's approach band
//! (pb-work-doors) — moved verbatim.

use super::*;

/// Submit this venue's selection to the `work` ring and merge what comes back.
///
/// Every door is cw-rails' (pb-work-doors), at the base `[daemon] rails_base`
/// names: seal, submit, the projection it polls, the refusal survey and the
/// attribution reference. `Err` is a refusal to run at all — no rev, no
/// cw-rails, a roster that will not take the act. It is never a green and
/// never an empty table.
pub(in super::super) async fn run_distributed(
    repo: &Path,
    lanes: &[&Instrument],
    trigger: &Trigger,
    budget_secs: u64,
) -> Result<DistributedRun, String> {
    let config = match sovereign_core::setup_config::SetupConfig::load() {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!(error = %e, "quality check: no setup config; the rails base is the default and no image is declared");
            None
        }
    };
    let rails_base = match &config {
        Some(c) => sovereign_turn_client::rails_kv::resolve_rails_base(&c.daemon),
        None => sovereign_turn_client::rails_kv::DEFAULT_RAILS_BASE.to_string(),
    };
    let image = config
        .as_ref()
        .and_then(|c| c.compute.work_offer.image.clone());

    let repo_rev = head_rev(repo)?;
    // The reference first: a run whose verdicts could not be judged against
    // this checkout is refused before anything is put on the ring.
    let mine = local_attribution(&rails_base, &repo_rev, image.as_deref()).await?;
    let units = units_for(&rails_base, lanes, trigger, budget_secs, &repo_rev).await?;
    let hashes: Vec<String> = units.iter().map(|u| u.unit_hash.clone()).collect();
    let kind = units
        .first()
        .map(|u| u.kind.clone())
        .ok_or("nothing selected — a submission with no units offers nothing to take")?;

    // TTL is how long the ring keeps OFFERING the work, which is the venue's
    // budget: past it this run has stopped reading, and work nobody is waiting
    // for should not go on being taken.
    let answer = rails_post(
        &rails_base,
        WORK_SUBMIT_PATH,
        &serde_json::json!({ "kind": kind, "units": units, "ttl_secs": budget_secs }),
    )
    .await
    .map_err(|e| format!("cw-rails would not take the submission: {e}"))?;
    let handoff: HandoffId = answer
        .get("handoff")
        .cloned()
        .ok_or_else(|| format!("cw-rails' `{WORK_SUBMIT_PATH}` answer carried no `handoff`"))
        .and_then(|v| {
            serde_json::from_value(v)
                .map_err(|e| format!("cw-rails answered a handoff this build cannot read: {e}"))
        })?;
    tracing::debug!(
        handoff = %handoff.to_hex(),
        units = hashes.len(),
        rev = %short(&repo_rev),
        seq = ?answer.get("seq"),
        rails = %rails_base,
        "quality check: submitted this venue to the work ring"
    );
    println!(
        "submitted {} unit(s) to the `work` ring at rev {} — handoff {}",
        hashes.len(),
        short(&repo_rev),
        handoff.to_hex()
    );
    println!(
        "  which node runs each is not this process's to choose: every node folds the same journal."
    );
    println!();

    let started = Instant::now();
    let budget = Duration::from_secs(budget_secs);
    let mut submitted_by: Option<String> = None;
    let mut announced: BTreeMap<String, ()> = BTreeMap::new();

    let proj = loop {
        let proj: WorkProjection =
            serde_json::from_value(rails_get(&rails_base, WORK_PROJECTION_PATH, &[]).await?)
                .map_err(|e| {
                    format!("cw-rails' projection is a shape this build cannot read: {e}")
                })?;
        let now_ms = sovereign_core::time::unix_millis();

        if let Some(h) = proj.handoffs.get(&handoff) {
            submitted_by = Some(h.submitter.as_str().to_string());
        }
        // Announce each unit as it settles, so a run that takes minutes is
        // legible while it happens rather than only in the table (ARCH §9.1).
        for (inst, hash) in lanes.iter().zip(&hashes) {
            if announced.contains_key(&inst.id) {
                continue;
            }
            let Some(p) = proj.handoffs.get(&handoff).and_then(|h| h.units.get(hash)) else {
                continue;
            };
            let status = p.status_at(now_ms);
            if status.is_terminal() {
                announced.insert(inst.id.clone(), ());
                let run = terminal_row(inst, &status, &mine);
                super::super::report::report_one(inst, &run);
            }
        }

        let terminal = lanes
            .iter()
            .zip(&hashes)
            .filter(|(_, hash)| {
                proj.handoffs
                    .get(&handoff)
                    .and_then(|h| h.units.get(*hash))
                    .is_some_and(|p| p.status_at(now_ms).is_terminal())
            })
            .count();
        tracing::debug!(
            terminal,
            of = hashes.len(),
            offers = proj.offers.len(),
            elapsed = started.elapsed().as_secs(),
            "quality check: distributed poll"
        );
        if terminal == hashes.len() || started.elapsed() >= budget {
            // The fold this loop broke on is the one the merge reads. Carried
            // out of the loop rather than re-fetched: a second read here would
            // merge a DIFFERENT journal from the one that decided the run was
            // over, and the two would disagree about exactly the unit that
            // settled in between.
            break proj;
        }
        tokio::time::sleep(POLL).await;
    };

    let now_ms = sovereign_core::time::unix_millis();
    let queued: Vec<&String> = hashes
        .iter()
        .filter(|hash| {
            proj.handoffs
                .get(&handoff)
                .and_then(|h| h.units.get(*hash))
                .is_some_and(|p| matches!(p.status_at(now_ms), WorkUnitStatus::Queued { .. }))
        })
        .collect();
    let surveys: Surveys = if queued.is_empty() {
        Surveys::new()
    } else {
        // SURVEYED AT THE LAST INSTANT THE WORK WAS ON OFFER, not at the
        // merge's own clock. The handoff's TTL is the venue's budget, so by
        // the time this runs it has just lapsed and `WorkHandoff::admits`
        // refuses every actor — every unplaced row would then read
        // `not-allowed`, which is true about the closed window and says
        // nothing about why nobody took the unit while it was open. Watched:
        // the first live 5e run reported exactly that for both unplaced rows.
        let body = serde_json::json!({
            "handoff": handoff,
            "units": queued,
            "at_ms": survey_ms(&proj, &handoff, now_ms),
        });
        match rails_post(&rails_base, WORK_REFUSALS_PATH, &body).await {
            Ok(v) => serde_json::from_value(v).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "quality check: the refusal survey is a shape this build cannot read");
                Surveys::new()
            }),
            // The rows for these units say the survey did not answer; the
            // verdicts that DID come back are not thrown away for it.
            Err(e) => {
                tracing::warn!(error = %e, "quality check: cw-rails' refusal survey did not answer");
                Surveys::new()
            }
        }
    };
    Ok(DistributedRun {
        results: merge(lanes, &hashes, &proj, &handoff, now_ms, &mine, &surveys),
        submitted_by,
        handoff,
        repo_rev,
    })
}
