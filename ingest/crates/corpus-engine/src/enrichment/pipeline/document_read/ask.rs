// SPDX-License-Identifier: AGPL-3.0-or-later
//! One statement being asked its fields, each by the pass its declared family
//! names: Choose a closed value, Point at an open one, Pick a reference
//! (ONTOLOGY_METHOD §Reading). Split from `passes.rs`; Point and Pick are
//! `point.rs` and `pick.rs`.

use std::collections::BTreeMap;
use std::ops::Range;

use serde_json::Value;
use tracing::debug;

use super::passes::{argmax, render, FieldPlan, CHOOSE_PHASE};
use super::pick::Candidates;
use super::prefill;
use super::DocumentReadField;
use crate::enrichment::atlas::precision::{Precision, SourcePrecision};
use crate::enrichment::atlas::resolve_records::{
    choice_question, decision_call, ClosedAttr, LABELS, NONE,
};
use crate::enrichment::ontology::OntologyTypeDecl;
use crate::InferenceFn;

/// One statement being asked: where it is in the folded body, the raw lines
/// that are its evidence, and what its document offers a reference.
pub(super) struct Ask<'q> {
    pub(super) infer: &'q InferenceFn,
    /// The document's declared facts, first in every question (`prefill`).
    pub(super) facts: &'q str,
    pub(super) document: &'q str,
    pub(super) statement: &'q str,
    pub(super) body: &'q str,
    pub(super) at: Range<usize>,
    pub(super) evidence: &'q str,
    /// Per referenced type, the candidates its document offers (`pick.rs`).
    pub(super) candidates: &'q BTreeMap<String, Candidates>,
}

impl Ask<'_> {
    pub(super) async fn field(
        &self,
        owner: &OntologyTypeDecl,
        field: &FieldPlan<'_>,
        calls: &mut u32,
    ) -> (String, DocumentReadField) {
        match field {
            FieldPlan::Unasked(attr) => (
                attr.name.clone(),
                DocumentReadField::Unknown {
                    reason: format!(
                        "not asked: `{}` declares more values than one forced choice shows ({})",
                        attr.name,
                        LABELS.len()
                    ),
                },
            ),
            FieldPlan::Choose(attr) => (attr.name.clone(), self.choose(owner, attr, calls).await),
            FieldPlan::Point(attr) => (
                attr.name.clone(),
                super::point::point(self, owner, attr, calls).await,
            ),
            FieldPlan::Pick(attr, of) => (
                attr.name.clone(),
                super::pick::pick(self, owner, attr, of, calls).await,
            ),
        }
    }

    async fn choose(
        &self,
        owner: &OntologyTypeDecl,
        attr: &ClosedAttr,
        calls: &mut u32,
    ) -> DocumentReadField {
        let labels: Vec<&str> = LABELS[..attr.values.len()]
            .iter()
            .copied()
            .chain([NONE])
            .collect();
        let prompt = prefill::carrying(
            choice_question(
                &owner.name,
                &owner.description,
                attr,
                &labels,
                self.body,
                self.at.clone(),
                CHOOSE_PHASE,
            ),
            self.facts,
        );
        *calls += 1;
        match decision_call(self.infer, &prompt, &labels, self.document, self.statement).await {
            Err(refusal) => DocumentReadField::Unknown {
                reason: format!("refused: {refusal:?}"),
            },
            Ok(dist) => match argmax(&labels, &dist) {
                (best, p) if best < attr.values.len() => {
                    let value = &attr.values[best];
                    debug!(document = self.document, statement = self.statement, field = %attr.name, %value, p, dist = %render(&dist), "document_read/passes: chose");
                    // No read precision is declared yet: the argmax decides
                    // so that it can be measured (`passes.rs` module doc).
                    DocumentReadField::Supported {
                        value: Value::String(value.clone()),
                        evidence: self.evidence.to_string(),
                        by: Some(SourcePrecision::new("reader_choose", Precision::Unmeasured)),
                    }
                }
                (_, p) => {
                    debug!(document = self.document, statement = self.statement, field = %attr.name, p, dist = %render(&dist), "document_read/passes: chose none of the values");
                    DocumentReadField::Unknown {
                        reason: format!("the reader chose none of the values: {}", render(&dist)),
                    }
                }
            },
        }
    }
}
