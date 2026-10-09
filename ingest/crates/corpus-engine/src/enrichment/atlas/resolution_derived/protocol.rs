// SPDX-License-Identifier: AGPL-3.0-or-later
//! Qualified scalar state folds over claims already assigned by RESOLVE.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use tracing::debug;

use super::{DerivedOutcome, DerivedProtocolAudit, Effects, Graph, Node};
use crate::enrichment::ontology::derived::{FoldDecl, ProtocolRuleDecl};
use crate::enrichment::pipeline::document_read::{
    field_evidence_exists, DocumentReadField, CLAIM_FIELDS_ATTRIBUTE,
};

#[derive(Debug)]
struct ProtocolCandidate {
    history_index: usize,
    claim_id: String,
    identity: String,
    rule_id: String,
    state: String,
    report_time: Option<String>,
    effective_time: Option<String>,
    corrects: Option<String>,
}

pub(super) fn derive(
    graph: &Graph<'_>,
    fold: &FoldDecl,
    target: &Node,
    target_document: Option<String>,
    fx: &mut Effects,
) -> Result<(DerivedOutcome, DerivedProtocolAudit), String> {
    let protocol = fold
        .protocol
        .as_ref()
        .ok_or_else(|| format!("protocol fold `{}` has no protocol declaration", fold.id))?;
    let Node::Entity(target_index) = target else {
        return Err(format!(
            "protocol fold `{}` must start at its assigned record entity",
            fold.id
        ));
    };
    let target_id = graph.entities[*target_index].id.as_str().to_string();
    let expressions = graph
        .parsed
        .get(fold.id.as_str())
        .ok_or_else(|| format!("protocol fold `{}` has no parsed input path", fold.id))?;
    let start: super::Reached = [(target.clone(), target_document)].into();
    let mut reached = BTreeMap::<usize, (usize, Option<String>)>::new();
    for (input, expression) in expressions.iter().enumerate() {
        for (node, document) in graph.walk(expression, start.clone(), fx)? {
            if let Node::Claim(index) = node {
                reached.entry(index).or_insert((input, document));
            }
        }
    }

    let wire = serde_json::to_vec(fold).map_err(|error| {
        format!(
            "serializing protocol fold `{}` for its audit: {error}",
            fold.id
        )
    })?;
    let rule_fingerprint = blake3::hash(&wire).to_hex().to_string();
    let (mut history, mut candidates) = (Vec::new(), Vec::new());
    let (mut basis, mut assignments, mut rule_ids, mut pending, mut alternatives) = (
        BTreeSet::new(),
        BTreeMap::new(),
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
    );

    for (claim_index, (input, _walk_document)) in reached {
        let claim = &graph.claims[claim_index];
        let claim_id = claim.id.as_str().to_string();
        basis.insert(claim_id.clone());
        if let Some(subject) = &claim.subject {
            assignments.insert(claim_id.clone(), subject.as_str().to_string());
        }
        let document_id = graph.claim_doc[claim_index].map(str::to_string);
        let report_time = document_id
            .as_deref()
            .and_then(|document| graph.clock.get(document).cloned());
        let source_document = document_id
            .as_deref()
            .and_then(|document| graph.docs.get(document).copied());
        let claim_kind = claim.claim_kind.as_deref();
        let rules: Vec<&ProtocolRuleDecl> = protocol
            .rules
            .iter()
            .filter(|rule| claim_kind.is_some_and(|kind| graph.index.is_a(kind, &rule.claim_kind)))
            .collect();
        let field_evidence = claim
            .attributes
            .get(CLAIM_FIELDS_ATTRIBUTE)
            .cloned()
            .unwrap_or(Value::Null);

        if rules.is_empty() {
            history.push(history_entry(
                input,
                claim,
                document_id.as_deref(),
                report_time.as_deref(),
                None,
                None,
                None,
                None,
                "unmatched_kind",
                None,
                field_evidence,
            ));
            continue;
        }
        if claim.subject.as_ref().map(|subject| subject.as_str()) != Some(target_id.as_str()) {
            let reason = "claim is not assigned to this record".to_string();
            for rule in &rules {
                alternatives.insert(rule.state.clone());
            }
            pending.insert(reason.clone());
            history.push(history_entry(
                input,
                claim,
                document_id.as_deref(),
                report_time.as_deref(),
                None,
                None,
                None,
                None,
                "pending",
                Some(&reason),
                field_evidence,
            ));
            continue;
        }
        if claim.evidence.is_empty() || source_document.is_none() {
            let reason = "claim has no unique cited source document".to_string();
            for rule in &rules {
                alternatives.insert(rule.state.clone());
            }
            pending.insert(reason.clone());
            history.push(history_entry(
                input,
                claim,
                document_id.as_deref(),
                report_time.as_deref(),
                None,
                None,
                None,
                None,
                "pending",
                Some(&reason),
                field_evidence,
            ));
            continue;
        }

        let mut matching = Vec::new();
        let mut uncertain = Vec::<(&ProtocolRuleDecl, Vec<String>)>::new();
        for rule in &rules {
            match matches_rule(claim, source_document, rule) {
                Ok(true) => matching.push(*rule),
                Ok(false) => {}
                Err(reasons) => uncertain.push((*rule, reasons)),
            }
        }
        if matching.len() != 1 || !uncertain.is_empty() {
            let mut reasons = BTreeSet::new();
            rule_ids.extend(matching.iter().map(|rule| rule.id.clone()));
            for (_, missing) in &uncertain {
                reasons.extend(missing.iter().cloned());
            }
            rule_ids.extend(uncertain.iter().map(|(rule, _)| rule.id.clone()));
            if matching.len() > 1 {
                reasons.insert("more than one protocol rule matches this claim".into());
            }
            if !reasons.is_empty() {
                for rule in &matching {
                    alternatives.insert(rule.state.clone());
                }
                for (rule, _) in &uncertain {
                    alternatives.insert(rule.state.clone());
                }
                pending.extend(reasons.iter().cloned());
                let reason = reasons.iter().cloned().collect::<Vec<_>>().join("; ");
                let possible_rule = if matching.len() == 1 {
                    Some(matching[0])
                } else {
                    None
                };
                history.push(history_entry(
                    input,
                    claim,
                    document_id.as_deref(),
                    report_time.as_deref(),
                    possible_rule.map(|rule| rule.id.as_str()),
                    possible_rule.map(|rule| rule.state.as_str()),
                    None,
                    None,
                    "pending",
                    Some(&reason),
                    field_evidence,
                ));
            } else {
                history.push(history_entry(
                    input,
                    claim,
                    document_id.as_deref(),
                    report_time.as_deref(),
                    None,
                    None,
                    None,
                    None,
                    "unmatched_qualification",
                    None,
                    field_evidence,
                ));
            }
            continue;
        }

        let rule = matching[0];
        rule_ids.insert(rule.id.clone());
        let identity = match source_field_value(claim, source_document, &protocol.identity) {
            Ok(value) if !value.is_empty() => value,
            Ok(_) => {
                let reason = format!("identity field `{}` is empty", protocol.identity);
                pending.insert(reason.clone());
                alternatives.insert(rule.state.clone());
                history.push(history_entry(
                    input,
                    claim,
                    document_id.as_deref(),
                    report_time.as_deref(),
                    Some(&rule.id),
                    Some(&rule.state),
                    None,
                    None,
                    "pending",
                    Some(&reason),
                    field_evidence,
                ));
                continue;
            }
            Err(reason) => {
                let reason = format!("identity field `{}`: {reason}", protocol.identity);
                pending.insert(reason.clone());
                alternatives.insert(rule.state.clone());
                history.push(history_entry(
                    input,
                    claim,
                    document_id.as_deref(),
                    report_time.as_deref(),
                    Some(&rule.id),
                    Some(&rule.state),
                    None,
                    None,
                    "pending",
                    Some(&reason),
                    field_evidence,
                ));
                continue;
            }
        };
        let effective_time = match protocol.effective_time.as_deref() {
            Some(field) => match source_field_value(claim, source_document, field) {
                Ok(value) if !value.is_empty() => Some(value),
                Ok(_) => {
                    let reason = format!("effective-time field `{field}` is empty");
                    pending.insert(reason.clone());
                    alternatives.insert(rule.state.clone());
                    history.push(history_entry(
                        input,
                        claim,
                        document_id.as_deref(),
                        report_time.as_deref(),
                        Some(&rule.id),
                        Some(&rule.state),
                        Some(&identity),
                        None,
                        "pending",
                        Some(&reason),
                        field_evidence,
                    ));
                    continue;
                }
                Err(error) => {
                    let reason = format!("effective-time field `{field}`: {error}");
                    pending.insert(reason.clone());
                    alternatives.insert(rule.state.clone());
                    history.push(history_entry(
                        input,
                        claim,
                        document_id.as_deref(),
                        report_time.as_deref(),
                        Some(&rule.id),
                        Some(&rule.state),
                        Some(&identity),
                        None,
                        "pending",
                        Some(&reason),
                        field_evidence,
                    ));
                    continue;
                }
            },
            None => None,
        };
        let corrects = match rule.corrects.as_deref() {
            Some(field) => match source_field_value(claim, source_document, field) {
                Ok(value) if !value.is_empty() => Some(value),
                Ok(_) => {
                    let reason = format!("correction target field `{field}` is empty");
                    pending.insert(reason.clone());
                    alternatives.insert(rule.state.clone());
                    history.push(history_entry(
                        input,
                        claim,
                        document_id.as_deref(),
                        report_time.as_deref(),
                        Some(&rule.id),
                        Some(&rule.state),
                        Some(&identity),
                        effective_time.as_deref(),
                        "pending",
                        Some(&reason),
                        field_evidence,
                    ));
                    continue;
                }
                Err(error) => {
                    let reason = format!("correction target field `{field}`: {error}");
                    pending.insert(reason.clone());
                    alternatives.insert(rule.state.clone());
                    history.push(history_entry(
                        input,
                        claim,
                        document_id.as_deref(),
                        report_time.as_deref(),
                        Some(&rule.id),
                        Some(&rule.state),
                        Some(&identity),
                        effective_time.as_deref(),
                        "pending",
                        Some(&reason),
                        field_evidence,
                    ));
                    continue;
                }
            },
            None => None,
        };
        let history_index = history.len();
        history.push(history_entry(
            input,
            claim,
            document_id.as_deref(),
            report_time.as_deref(),
            Some(&rule.id),
            Some(&rule.state),
            Some(&identity),
            effective_time.as_deref(),
            "qualified",
            None,
            field_evidence,
        ));
        alternatives.insert(rule.state.clone());
        candidates.push(ProtocolCandidate {
            history_index,
            claim_id,
            identity,
            rule_id: rule.id.clone(),
            state: rule.state.clone(),
            report_time,
            effective_time,
            corrects,
        });
    }

    candidates.sort_by(|left, right| {
        (
            &left.identity,
            &left.rule_id,
            &left.state,
            &left.report_time,
            &left.effective_time,
            &left.corrects,
            &left.claim_id,
        )
            .cmp(&(
                &right.identity,
                &right.rule_id,
                &right.state,
                &right.report_time,
                &right.effective_time,
                &right.corrects,
                &right.claim_id,
            ))
    });

    let mut seen = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let duplicate_key = (
            candidate.identity.clone(),
            candidate.rule_id.clone(),
            candidate.state.clone(),
            candidate.report_time.clone(),
            candidate.effective_time.clone(),
            candidate.corrects.clone(),
        );
        if !seen.insert(duplicate_key) {
            duplicates.insert(index);
            set_disposition(&mut history, candidate.history_index, "duplicate");
        }
    }

    let mut corrected = BTreeSet::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let Some(target_identity) = candidate.corrects.as_deref() else {
            continue;
        };
        if target_identity == candidate.identity {
            pending.insert(format!(
                "correction `{}` names its own transition identity",
                candidate.claim_id
            ));
            continue;
        }
        let targets: Vec<&ProtocolCandidate> = candidates
            .iter()
            .filter(|prior| {
                prior.identity == target_identity && prior.identity != candidate.identity
            })
            .collect();
        if targets.is_empty() {
            pending.insert(format!(
                "correction `{}` names unavailable transition identity `{target_identity}`",
                candidate.claim_id
            ));
            set_disposition(&mut history, candidate.history_index, "pending");
            continue;
        }
        let Some(correction_time) = candidate.report_time.as_deref() else {
            pending.insert(format!(
                "correction `{}` has no report time to order against its target",
                candidate.claim_id
            ));
            set_disposition(&mut history, candidate.history_index, "pending");
            continue;
        };
        let Some(correction_class) = report_time_class(correction_time) else {
            pending.insert(format!(
                "correction `{}` has an unsupported report-time precision",
                candidate.claim_id
            ));
            set_disposition(&mut history, candidate.history_index, "pending");
            continue;
        };
        let mut target_latest: Option<&str> = None;
        let mut target_unordered = false;
        for target in &targets {
            let Some(target_time) = target.report_time.as_deref() else {
                target_unordered = true;
                break;
            };
            if report_time_class(target_time) != Some(correction_class) {
                target_unordered = true;
                break;
            }
            target_latest =
                Some(target_latest.map_or(target_time, |current| current.max(target_time)));
        }
        if target_unordered {
            pending.insert(format!(
                "correction `{}` and target `{target_identity}` do not have comparable report times",
                candidate.claim_id
            ));
            set_disposition(&mut history, candidate.history_index, "pending");
            continue;
        }
        if target_latest.is_some_and(|target_time| correction_time < target_time) {
            pending.insert(format!(
                "correction `{}` is reported before target `{target_identity}`",
                candidate.claim_id
            ));
            set_disposition(&mut history, candidate.history_index, "pending");
            continue;
        }
        corrected.insert(target_identity.to_string());
        if !duplicates.contains(&index) {
            set_disposition(&mut history, candidate.history_index, "correction");
        }
    }
    for (index, candidate) in candidates.iter().enumerate() {
        if corrected.contains(&candidate.identity) && !duplicates.contains(&index) {
            set_disposition(&mut history, candidate.history_index, "corrected");
        }
    }

    let active: Vec<(usize, &ProtocolCandidate)> = candidates
        .iter()
        .enumerate()
        .filter(|(index, candidate)| {
            !duplicates.contains(index) && !corrected.contains(&candidate.identity)
        })
        .collect();
    let mut effective_times = BTreeSet::new();
    let mut states_by_identity = BTreeMap::<String, BTreeSet<String>>::new();
    for (_, candidate) in &active {
        states_by_identity
            .entry(candidate.identity.clone())
            .or_default()
            .insert(candidate.state.clone());
    }
    let identity_conflicts: BTreeMap<String, BTreeSet<String>> = states_by_identity
        .into_iter()
        .filter(|(_, states)| states.len() > 1)
        .collect();
    let identity_conflict_values: BTreeSet<String> = identity_conflicts
        .values()
        .flat_map(|states| states.iter().cloned())
        .collect();
    for (identity, states) in &identity_conflicts {
        pending.insert(format!(
            "transition identity `{identity}` has incompatible states without an explicit correction"
        ));
        for (_, candidate) in active
            .iter()
            .filter(|(_, candidate)| candidate.identity == *identity)
        {
            set_disposition(&mut history, candidate.history_index, "conflict");
            if let Some(time) = &candidate.effective_time {
                effective_times.insert(time.clone());
            }
        }
        alternatives.extend(states.iter().cloned());
    }
    if active
        .iter()
        .any(|(_, candidate)| candidate.report_time.is_none())
    {
        pending.insert(
            "one or more qualified reports have no exact document report time; undated and partial dates are not ordered".into(),
        );
    }
    let time_classes: BTreeSet<&str> = active
        .iter()
        .filter_map(|(_, candidate)| candidate.report_time.as_deref())
        .filter_map(|time| report_time_class(time))
        .collect();
    let known_times = active
        .iter()
        .filter(|(_, candidate)| candidate.report_time.is_some())
        .count();
    if known_times != 0 && time_classes.len() != 1 {
        pending.insert(
            "report times have mixed precision or unsupported intervals; no total order is assumed"
                .into(),
        );
    }

    let mut outcome = DerivedOutcome::Nothing;
    let mut as_of_report_time = None;
    if !identity_conflict_values.is_empty() {
        outcome = DerivedOutcome::Conflict {
            values: identity_conflict_values.into_iter().collect(),
        };
    } else if !pending.is_empty() {
        for (_, candidate) in &active {
            if let Some(time) = &candidate.effective_time {
                effective_times.insert(time.clone());
            }
            if candidate.report_time.is_none() || time_classes.len() != 1 {
                set_disposition(&mut history, candidate.history_index, "alternative");
            }
        }
        outcome = DerivedOutcome::Pending {
            values: alternatives.iter().cloned().collect(),
            reasons: pending.iter().cloned().collect(),
        };
    } else if !active.is_empty() {
        let latest = active
            .iter()
            .filter_map(|(_, candidate)| candidate.report_time.as_deref())
            .max()
            .ok_or_else(|| "protocol report-time ordering lost all timestamps".to_string())?;
        as_of_report_time = Some(latest.to_string());
        let selected: Vec<&ProtocolCandidate> = active
            .iter()
            .map(|(_, candidate)| *candidate)
            .filter(|candidate| candidate.report_time.as_deref() == Some(latest))
            .collect();
        let states: BTreeSet<String> = selected
            .iter()
            .map(|candidate| candidate.state.clone())
            .collect();
        for candidate in &selected {
            if let Some(time) = &candidate.effective_time {
                effective_times.insert(time.clone());
            }
            rule_ids.insert(candidate.rule_id.clone());
        }
        for (_, candidate) in &active {
            if candidate.report_time.as_deref() != Some(latest) {
                set_disposition(
                    &mut history,
                    candidate.history_index,
                    "superseded_by_report_time",
                );
            } else {
                set_disposition(&mut history, candidate.history_index, "selected");
            }
        }
        if states.len() > 1 {
            let states: Vec<String> = states.into_iter().collect();
            pending.insert(format!("incompatible states share report time `{latest}`"));
            outcome = DerivedOutcome::Conflict {
                values: states.clone(),
            };
            alternatives = states.into_iter().collect();
        } else if let Some(state) = states.into_iter().next() {
            let documents: BTreeSet<String> = selected
                .iter()
                .filter_map(|candidate| {
                    history[candidate.history_index]["document"]
                        .as_str()
                        .map(str::to_string)
                })
                .collect();
            let superseded: BTreeSet<String> = candidates
                .iter()
                .filter(|candidate| candidate.state != state)
                .map(|candidate| candidate.state.clone())
                .collect();
            outcome = DerivedOutcome::Decided {
                values: vec![state],
                input: None,
                documents: documents.into_iter().collect(),
                superseded: superseded.into_iter().collect(),
            };
        }
    }

    let audit = DerivedProtocolAudit {
        rule_fingerprint,
        as_of_report_time,
        effective_times: effective_times.into_iter().collect(),
        rule_ids: rule_ids.into_iter().collect(),
        basis_claims: basis.into_iter().collect(),
        assignment_dependencies: assignments,
        alternatives: alternatives.into_iter().collect(),
        history,
        reasons: pending.into_iter().collect(),
    };
    debug!(
        fold = %fold.id,
        record = %target_id,
        outcome = outcome.label(),
        claims = audit.basis_claims.len(),
        rules = audit.rule_ids.len(),
        alternatives = audit.alternatives.len(),
        reasons = audit.reasons.len(),
        as_of_report_time = ?audit.as_of_report_time,
        "atlas/derive: protocol fold evaluated"
    );
    Ok((outcome, audit))
}

fn matches_rule(
    claim: &crate::enrichment::atlas::atoms::Claim,
    source_document: Option<&crate::enrichment::atlas::SourceDocument>,
    rule: &ProtocolRuleDecl,
) -> Result<bool, Vec<String>> {
    let mut missing = BTreeSet::new();
    let mut mismatch = false;
    for (field, expected) in &rule.when {
        match source_field_value(claim, source_document, field) {
            Ok(actual) if actual == *expected => {}
            Ok(_) => mismatch = true,
            Err(reason) => {
                missing.insert(format!("`{field}`: {reason}"));
            }
        }
    }
    if mismatch {
        Ok(false)
    } else if missing.is_empty() {
        Ok(true)
    } else {
        Err(missing.into_iter().collect())
    }
}

fn source_field_value(
    claim: &crate::enrichment::atlas::atoms::Claim,
    source_document: Option<&crate::enrichment::atlas::SourceDocument>,
    field: &str,
) -> Result<String, String> {
    let carrier = claim
        .attributes
        .get(CLAIM_FIELDS_ATTRIBUTE)
        .ok_or_else(|| "DocumentRead field provenance is unavailable in this cache".to_string())?;
    let fields = carrier
        .as_object()
        .ok_or_else(|| "DocumentRead field provenance is not an object".to_string())?;
    let wire = fields
        .get(field)
        .cloned()
        .ok_or_else(|| "DocumentRead did not provide this qualification".to_string())?;
    let field: DocumentReadField = serde_json::from_value(wire)
        .map_err(|error| format!("DocumentRead qualification is malformed: {error}"))?;
    match field {
        DocumentReadField::Unknown { reason } => Err(format!("source field is unknown: {reason}")),
        DocumentReadField::Supported {
            value, evidence, ..
        } => {
            if evidence.trim().is_empty() {
                return Err("supported field has no field evidence".into());
            }
            let Some(document) = source_document else {
                return Err("claim has no unique cited source document".into());
            };
            if !field_evidence_exists(document, &evidence) {
                return Err("field evidence no longer resolves in the cited document".into());
            }
            match value {
                Value::String(value) => Ok(value),
                Value::Number(value) => Ok(value.to_string()),
                _ => Err("supported qualification is not a scalar string or number".into()),
            }
        }
    }
}

fn history_entry(
    input: usize,
    claim: &crate::enrichment::atlas::atoms::Claim,
    document: Option<&str>,
    report_time: Option<&str>,
    rule: Option<&str>,
    state: Option<&str>,
    identity: Option<&str>,
    effective_time: Option<&str>,
    disposition: &str,
    reason: Option<&str>,
    field_evidence: Value,
) -> Value {
    let mut entry = json!({
        "input": input,
        "claim": claim.id.as_str(),
        "claim_kind": claim.claim_kind,
        "assigned_to": claim.subject.as_ref().map(|subject| subject.as_str()),
        "document": document,
        "report_time": report_time,
        "effective_time": effective_time,
        "identity": identity,
        "rule": rule,
        "state": state,
        "citation": serde_json::to_value(&claim.evidence).unwrap_or(Value::Null),
        "field_evidence": field_evidence,
        "disposition": disposition,
    });
    if let Some(reason) = reason {
        entry["reason"] = Value::String(reason.to_string());
    }
    entry
}

fn set_disposition(history: &mut [Value], index: usize, disposition: &str) {
    if let Some(entry) = history.get_mut(index).and_then(Value::as_object_mut) {
        entry.insert("disposition".into(), Value::String(disposition.to_string()));
    }
}

fn report_time_class(time: &str) -> Option<&'static str> {
    if time.len() == 10
        && time.as_bytes().get(4) == Some(&b'-')
        && time.as_bytes().get(7) == Some(&b'-')
    {
        Some("date")
    } else if time.contains('T') && time.ends_with('Z') {
        Some("utc")
    } else {
        None
    }
}
