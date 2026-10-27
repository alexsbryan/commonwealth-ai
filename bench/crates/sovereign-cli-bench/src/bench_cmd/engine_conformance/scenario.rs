// SPDX-License-Identifier: AGPL-3.0-or-later
//! The scenarios the battery now drives itself (L I3), graded here so a
//! row's verdict does not depend on a hand-written `--scenario` file:
//!
//! - [`split_line`] (`projection.stream-split-line`): the daemon's typed
//!   stream cut across two byte chunks, read back through the client's own
//!   SSE reader. The line is delivered whole, or a delta was lost.
//! - [`admission`] (`cost.admission`): a call of an admitted turn parks where
//!   the same call without one is shed, under the one bound
//!   (`SOVEREIGN_MAX_QUEUE_WAIT_SECS`).
//! - [`lifecycle`] (`host.lifecycle`): each lifecycle call says what it did,
//!   and the router's state after it agrees — a `warmup` that answered `Ok`
//!   over a model that is not resident is the failure the row exists to
//!   catch.
//! - [`stream_usage`] (`projection.stream-usage`): the terminal `usage` frame
//!   gives the answer's length, not the prompt's. Two tokenizers count one
//!   generated turn differently, so the row accepts the count either the
//!   target's tokenizer or the stream's deltas names, or one within
//!   [`SLACK`] of the answer's own — never the prompt's.
//!
//! Every one is a pure function of what the run recorded, so the same input
//! grades the same way, and each is exercised in both directions by
//! `scenario_tests.rs`.

use serde_json::Value;

use super::check::{CellVerdict, FailCause};
use super::record::CaseRecord;

/// The one driver whose verdict is about its own target alone while a
/// reference record exists beside it (`cost.admission`, a named difference
/// between the engines rather than a fault in either).
pub const TARGET_ONLY_DRIVER: &str = "admission-shape";

/// The most a finish frame's completion count may exceed the answer's own
/// token count and still be read as the answer's. The measured case: one
/// templated turn read 55 on llama-server against 33 on this engine's own
/// count of the same generated text, and the stream's own delta count was 1
/// — so neither of those two is a token count either. The factor that
/// separates an answer from its prompt is far larger (273 prompt tokens
/// against 55 generated), and the unit test holds 120 against 33 as a
/// failure.
const SLACK: f64 = 2.0;

fn failed(detail: impl Into<String>) -> CellVerdict {
    CellVerdict::Failed {
        cause: FailCause::Differs,
        detail: detail.into(),
    }
}

fn unjudged(detail: impl Into<String>) -> CellVerdict {
    CellVerdict::CouldNotJudge(detail.into())
}

/// `projection.stream-split-line`: the split stream delivered the whole
/// answer, read through a reader that buffers to a line.
pub fn split_line(target: &CaseRecord) -> CellVerdict {
    let Some(split) = target.facets.split_line.as_ref() else {
        return match target.facets.unobserved.get("split_line") {
            Some(why) => unjudged(format!("split_line not observed: {why}")),
            None => unjudged("split_line not observed on this target"),
        };
    };
    let Some(output) = target.facets.output.as_ref() else {
        return unjudged("the faulted stream's answer was not recorded");
    };
    if split.text != output.text {
        return failed(format!(
            "the chunked stream delivered {:?} where the whole one delivered {:?} ({} deltas)",
            split.text, output.text, split.deltas
        ));
    }
    if split.deltas == 0 && !output.text.is_empty() {
        return failed(format!(
            "the chunked stream delivered no delta for {} chars",
            output.text.len()
        ));
    }
    CellVerdict::Passed
}

/// `projection.stream-error`: the corrupted stream ended in an error frame
/// (`Finish{Error}` on the client's reader), not a clean `Stop`.
pub fn stream_error(target: &CaseRecord) -> CellVerdict {
    let Some(output) = target.facets.output.as_ref() else {
        return unjudged("the corrupted stream's answer was not recorded");
    };
    let has_error =
        output.frames.iter().any(|f| f == "error") || output.finish.as_deref() == Some("error");
    if has_error {
        CellVerdict::Passed
    } else {
        failed(format!(
            "the stream ended {:?} with frames {:?}, not in an error",
            output.finish, output.frames
        ))
    }
}

/// `cost.admission`: the admitted call was SERVED and the fresh one behind it
/// was SHED (`shed:unadmitted`). The admitted call must not be the one shed.
pub fn admission(target: &CaseRecord) -> CellVerdict {
    if target.facets.admission.is_empty() {
        return match target.facets.unobserved.get("admission") {
            Some(why) => unjudged(format!("admission not observed: {why}")),
            None => unjudged("the admission sequence was not recorded"),
        };
    }
    let sequence = &target.facets.admission;
    let has = |what: &str| sequence.iter().any(|s| s == what);
    let admitted_shed = sequence.iter().any(|s| s == "shed:admitted");
    if has("served:admitted") && has("shed:unadmitted") && !admitted_shed {
        CellVerdict::Passed
    } else {
        failed(format!(
            "want the admitted call served and the fresh one behind it shed, got {sequence:?}"
        ))
    }
}

/// `cost.admission`: whether this engine sheds a fresh call behind an
/// admitted one, judged on the target alone. The engine's `turn_admission`
/// parks the admitted caller past the queue bound and sheds the fresh one;
/// a router has no admission concept, so it serves both and sheds neither.
/// The row is a difference between the engines and is predicted
/// `fail:differs`, so the scenario names which shape it saw rather than
/// calling one of them wrong: either is a verdict, and a sequence of neither
/// shape (nothing served, or the admitted call shed) is the failure.
///
/// This is the one scenario that reads its own target's record while a
/// reference exists, so it is listed in `Check::target_only` — the engine's
/// record says what the engine does, and the comparison is the seat's table,
/// not this check's.
pub fn admission_shape(target: &CaseRecord) -> CellVerdict {
    let sequence = &target.facets.admission;
    if sequence.is_empty() {
        return match target.facets.unobserved.get("admission") {
            Some(why) => unjudged(format!("admission not observed: {why}")),
            None => unjudged("the admission sequence was not recorded"),
        };
    }
    let has = |want: &str| sequence.iter().any(|s| s == want);
    if !has("served:admitted") {
        return failed(format!(
            "the admitted call was not served, so the bound was never tested: {sequence:?}"
        ));
    }
    CellVerdict::Passed
}

/// `host.lifecycle`: each call's own answer agrees with the router's state
/// after it. `Ok{model}` names a model that is resident; an `Err` is an
/// honest refusal (the row may be predicted `fail:refused`); `NotAsked`
/// where the row asked is a gap in the instrument, not a verdict.
pub fn lifecycle(target: &CaseRecord) -> CellVerdict {
    let l = &target.facets.lifecycle;
    if l.is_empty() {
        return match target.facets.unobserved.get("lifecycle") {
            Some(why) => unjudged(format!("lifecycle not observed: {why}")),
            None => unjudged("the lifecycle calls were not recorded"),
        };
    }
    let resident: Vec<String> = target.facets.host_observed["resident_models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    let mut diffs = Vec::new();
    let mut gaps = Vec::new();
    for key in ["warmup_loaded", "extra_loaded", "extra_unloaded"] {
        let Some(call) = l.get(key) else {
            gaps.push(format!("{key} not reported"));
            continue;
        };
        match call["kind"].as_str() {
            Some("ok") => {
                // A warmup names the primary; its model must be resident
                // after the call. An extra load names the id it returned,
                // which the router must list once loaded.
                let model = call["model"].as_str().map(str::to_string);
                let Some(model) = model.filter(|m| !m.is_empty()) else {
                    continue;
                };
                if !resident.iter().any(|r| *r == model) {
                    diffs.push(format!(
                        "{key}: answered Ok{{model: {model}}} but the router lists {resident:?}"
                    ));
                }
            }
            // An honest refusal is a verdict, not a gap: the row compares
            // it to its own prediction.
            Some("err") => {}
            Some("not-asked") => {
                // The extra half is only asked when the run named a slot; a
                // `not-asked` there is the instrument's own report and the
                // row stays could-not-judge rather than passing on absence.
                // `warmup_loaded` not-asked is the same gap, named so.
                gaps.push(format!("{key} was not asked"));
            }
            other => gaps.push(format!("{key}: unreadable answer {other:?}")),
        }
    }
    if !diffs.is_empty() {
        failed(diffs.join("; "))
    } else if !gaps.is_empty() {
        unjudged(gaps.join("; "))
    } else {
        CellVerdict::Passed
    }
}

/// A scenario the driver runs and grades itself, by the name a row's
/// `driver` gives it. `None` for a name this build does not drive, so the
/// caller reports it rather than silently passing.
pub fn grade(driver: &str, target: &CaseRecord) -> Option<CellVerdict> {
    Some(match driver {
        "split-line" => split_line(target),
        "stream-error" => stream_error(target),
        "stream-usage" => stream_usage(target),
        "admission" => admission(target),
        "admission-shape" => admission_shape(target),
        "lifecycle" => lifecycle(target),
        _ => return None,
    })
}

/// `projection.stream-usage`: the usage the stream's finish frame carried
/// counts what the answer generated. The engine counts the tokens it emitted;
/// llama-server counts what it evaluated, and a draft head gives it more than
/// the answer has. So the frame passes when its completion count is either the
/// answer's token count or the number of `token` frames the same stream
/// delivered, and it fails when the count is the prompt's, a different answer's,
/// or absent.
pub fn stream_usage(target: &CaseRecord) -> CellVerdict {
    let Some(carried) = target.facets.stream_usage.as_ref() else {
        return match target.facets.unobserved.get("stream_usage") {
            Some(why) => unjudged(format!("stream_usage not observed: {why}")),
            None => unjudged("the stream's usage frame was not recorded"),
        };
    };
    // A count of zero ties nothing: a stream that delivered nothing cannot
    // vouch for a usage frame.
    let mut accepted: Vec<u64> = Vec::new();
    if carried.deltas > 0 {
        accepted.push(carried.deltas);
    }
    accepted.extend(carried.answer_tokens);
    accepted.sort_unstable();
    accepted.dedup();
    if accepted.is_empty() {
        return unjudged(format!(
            "the finish frame reported {carried:?} and the stream no deltas to tie it to"
        ));
    }
    if accepted.contains(&carried.completion) {
        return CellVerdict::Passed;
    }
    // A count a little above the answer's own is the generator's, not the
    // prompt's: this engine's template tokens cost a few more than the
    // server's tokenizer makes of the same generated turn, and the two counts
    // are taken over different tokenizers. The bar separates "the answer's
    // length" from "the prompt's" without reading one tokenizer's arithmetic
    // as a loss; past it the count belongs to something else.
    if let Some(tokens) = carried.answer_tokens.filter(|t| *t > 0) {
        let ratio = carried.completion as f64 / tokens as f64;
        if (1.0..=SLACK).contains(&ratio) {
            return CellVerdict::Passed;
        }
    }
    failed(format!(
        "the finish frame's completion count is {} where the stream delivered {accepted:?}",
        carried.completion
    ))
}
