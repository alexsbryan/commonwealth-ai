// SPDX-License-Identifier: AGPL-3.0-or-later
//! BusinessEmailDomain — entity extraction tuned for RFC5322 email
//! corpora (the Enron substrate, Firm Inbox vertical, sales-intel,
//! etc).
//!
//! The `ConversationalDomain` is the right shape for chat exports
//! (one user + one assistant, treat the user as implicit and skip
//! them from extraction). It is the WRONG shape for email: the
//! mailbox owner is a first-class entity who should appear in every
//! cluster, and the From: / To: / Cc: headers are the canonical
//! source of identity ground truth. Running `enron-sample-multi-tiny`
//! through `conversational` matched 7/35 train-split gold entities —
//! the conversational prompt skipped Lay-in-Lay's-mailbox and
//! Skilling-in-Skilling's-mailbox as "the user", leaving the
//! ground-truth canonicals like `jeff.skilling@enron.com` un-emitted.
//!
//! BusinessEmailDomain pairs with the email extractor's header
//! preamble (`From:` / `To:` / `Cc:` / `Date:` / `Subject:` lines
//! prepended to chunk content): the prompt below treats those lines
//! as primary identity signals, asks for sender + every recipient as
//! `persons`, and lifts email-domain organizations from the address
//! domains. Non-entity prompts (skeleton, cluster labeling, fault
//! lines, open questions) delegate to `ConversationalDomain` — they
//! work fine on email passages as-is and the duplication would rot
//! independently.

use std::sync::Arc;

use super::super::domain::{
    AlignmentConfig, Chunk, ChunkFilter, ClusteringConfig, Domain, FaultLineConfig,
    PositionStatusVocab, QuestionType, SkeletonStorage,
};
use super::conversational::ConversationalDomain;

pub struct BusinessEmailDomain {
    inner: Arc<ConversationalDomain>,
}

impl BusinessEmailDomain {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ConversationalDomain),
        }
    }
}

impl Default for BusinessEmailDomain {
    fn default() -> Self {
        Self::new()
    }
}

impl Domain for BusinessEmailDomain {
    fn id(&self) -> &str {
        "business_email"
    }

    fn name(&self) -> &str {
        "Business email"
    }

    fn position_statuses(&self) -> &PositionStatusVocab {
        self.inner.position_statuses()
    }

    fn question_types(&self) -> &[QuestionType] {
        self.inner.question_types()
    }

    fn overview_filter(&self) -> ChunkFilter {
        self.inner.overview_filter()
    }

    fn skeleton_extraction_prompt(&self, chunks: &[&Chunk]) -> String {
        self.inner.skeleton_extraction_prompt(chunks)
    }

    fn cluster_labeling_prompt(&self, representative_chunks: &[&Chunk]) -> String {
        self.inner.cluster_labeling_prompt(representative_chunks)
    }

    fn fault_line_detection_prompt(
        &self,
        chunks_a: &[&Chunk],
        chunks_b: &[&Chunk],
        position_a: &str,
        position_b: &str,
    ) -> String {
        self.inner
            .fault_line_detection_prompt(chunks_a, chunks_b, position_a, position_b)
    }

    fn open_question_prompt(&self, chunks: &[&Chunk]) -> String {
        self.inner.open_question_prompt(chunks)
    }

    fn clustering_config(&self) -> ClusteringConfig {
        self.inner.clustering_config()
    }

    fn alignment_config(&self) -> AlignmentConfig {
        self.inner.alignment_config()
    }

    fn fault_line_config(&self) -> FaultLineConfig {
        self.inner.fault_line_config()
    }

    fn skeleton_storage(&self) -> SkeletonStorage {
        self.inner.skeleton_storage()
    }

    fn entity_extraction_schema(&self) -> Option<serde_json::Value> {
        // **Lean schema with `name` required per entity.** A fully-
        // typed schema (per-field unions like `["string", "null"]`,
        // `additionalProperties: false`, distinct shapes per entity
        // kind) caused llguidance's mask sampler to pathologically
        // branch on enron-sample-multi-wide inbox batches — observed
        // 2026-05-29: 300s per-batch timeouts with `schema=true,
        // n_generated=5801` before the deadline aborted the call.
        // A zero-constraint shape (just enforce top-level arrays)
        // swung the other way: model omitted `name` on most entities
        // and serde rejected 100% of batches with `missing field
        // `name``. Middle path: every entity-array item is an object
        // with `name` required (matches `PersonEntity`'s required
        // field on the Rust side); everything else stays free-form
        // so serde Optional / lenient_string_array soak up variants.
        // One mandatory key per item is well under the mask threshold
        // that exploded the rich schema.
        let entity_object = serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
            },
            "required": ["name"],
        });
        let entity_array = serde_json::json!({
            "type": "array",
            "items": entity_object,
        });
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "persons":       entity_array,
                "organizations": entity_array,
                "works":         entity_array,
                "concepts":      entity_array,
                "initiatives":   entity_array,
            },
        }))
    }

    fn entity_extraction_prompt(&self, chunks: &[&Chunk]) -> Option<String> {
        // Empty-slice probe matches conversational: returns Some("")
        // so the engine treats the domain as opting in before the
        // first real batch arrives.
        if chunks.is_empty() {
            return Some(String::new());
        }

        let passages = chunks
            .iter()
            .enumerate()
            .map(|(i, c)| {
                format!(
                    "[Email {} — {}]\n{}",
                    i + 1,
                    c.title.as_deref().unwrap_or("(no subject)"),
                    c.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        // Prompt layout is prefix-cache-optimised: every line of
        // stable instruction sits BEFORE the dynamic `{passages}`
        // tail (mirrors the conversational fix). The header-aware
        // rules here are the load-bearing change vs the
        // ConversationalDomain prompt — they tell the model to lift
        // every header identity (From:, To:, Cc:) as a `persons`
        // entry whether or not they're mentioned in the body, and to
        // canonicalize email addresses as a surface form.
        Some(format!(
            r#"You are reading business emails — sent and received messages
from a corporate mailbox. Your job is named-entity extraction:
identify every *person*, *organization*, *initiative*, *work*,
and *concept* the messages reference, including identities visible
in the message HEADERS (From, To, Cc, Date, Subject) and the BODY.

Every email passage starts with an RFC5322 header preamble:

  From: Sender Name <sender@example.com>
  To: Recipient One <r1@example.com>, Recipient Two <r2@example.com>
  Cc: Cc One <cc1@example.com>
  Date: ...
  Subject: ...

  <body text>

Treat the headers as the primary identity ground-truth signal.
Every distinct address visible in From:, To:, Cc:, or Bcc: is a
Person — emit one entry per distinct individual, even if their
name never appears in the body, and emit them whether they sent
the message or only received it. Do NOT skip the sender just
because they "speak" in the body; the sender is a first-class
entity in this corpus.

Definitions:

- **Person**: a named human individual referenced by header or
  body. Capture:
    - `name` — the most canonical form available
      ("Jeffrey K. Skilling"); fall back to the local-part of the
      email address ("jeff.skilling") only if no display name
      anywhere ties an identity together
    - `email` — the canonical address when known
      ("jeff.skilling@enron.com"); omit if absent from headers
      and body
    - `role` — capture from the body when explicit ("CEO",
      "CFO", "general counsel", "trader"); omit otherwise
    - `affiliation` — the organization implied by the email
      domain (`@enron.com` ⇒ Enron Corp), or the org named in
      the body's signature block; omit if uncertain
    - `aliases` — **enumerate every distinct textual form for
      this person seen anywhere in the batch**, including: short
      forms ("Jeff", "Andy"), initial+surname ("J. Skilling",
      "A. Fastow"), title-prefixed forms ("Mr. Skilling",
      "Dr. Lay"), email addresses, and any header-pair display
      names ("Jeffrey K. Skilling <jeff.skilling@enron.com>" →
      both forms). When in doubt include the form — downstream
      reconciliation collapses redundant entries safely, but it
      cannot synthesize forms that were never emitted.
  Lift short-form ("Jeff", "Mr. Skilling") to the canonical
  long form ("Jeffrey K. Skilling") in `name`, and list both
  in `aliases`. Cross-batch alias merging is a downstream step
  — emit what each batch supports.

- **Organization**: any named institution, company, regulator,
  counterparty, vendor, customer, agency, or law firm referenced
  by name in the body or implied by an email domain
  (`@dynegy.com` ⇒ Dynegy Inc). Capture:
    - `name` — the most canonical form ("Arthur Andersen LLP",
      "Dynegy Inc.")
    - `relationship` — to the mailbox owner when the body makes
      it explicit ("counterparty", "auditor", "outside counsel",
      "regulator", "customer")
    - `aliases` — **every distinct textual form seen in the
      batch**: short ("Andersen"), abbreviation ("AA"),
      full-name ("Arthur Andersen LLP"), and domain forms
      ("andersen.com"). Lift abbreviations to canonical forms
      in `name`, but list every observed form in `aliases`.

- **Initiative**: a concrete ongoing business effort the messages
  organize around — a transaction, deal, project, task force,
  bid, integration, restructuring, investigation, or product
  push. Use the canonical name the threads adopt ("Project
  Raptor", "the Dynegy merger", "the FAS 140 restructuring",
  "the California PUC filing"). Sub-items inside an initiative
  ("the Houston review meeting", "the Q3 earnings call") are
  not separate initiatives — capture them as `status` updates
  on the parent.

- **Work**: a named piece of created content the messages
  reference — a report, memo, filing, deck, press release,
  10-K, term sheet, PSA, board minutes, news article, opinion
  piece, regulatory order. Capture `kind` ("memo", "filing",
  "press release", "earnings release", "report") and `creator`
  when explicit. Skip generic descriptors ("the attached PDF"
  with no name).

- **Concept**: a named business idea, term of art, framework, or
  load-bearing concept the messages think with — accounting
  treatments ("mark-to-market", "off-balance-sheet financing",
  "round-trip trading"), market structures ("the spark spread",
  "tolling agreement"), regulatory regimes ("Reg FD",
  "Sarbanes-Oxley", the Public Utility Holding Company Act),
  internal practices ("PRC review", "ranking and yanking").
  Distinguish from Claim: "mark-to-market" is a Concept; "the
  mark-to-market gains were aggressive" is a Claim that uses
  the framework. Lift Concepts generously.

When the same person is referenced both by short form ("Jeff",
"Mr. Skilling", "JKS") and a full address
("jeff.skilling@enron.com") in the same batch, emit ONE entry
with the canonical long form and list every short form / address
in `mentions`. Use the [Email N] labels to record where each
entity appeared in `mentions`. If you list a person as a
`participants` entry on an initiative, also emit them in the
`persons` array.

DO NOT skip header identities. DO NOT skip the mailbox owner.
DO NOT extract email addresses as separate Person entries when
the same individual already appears via name — collapse them
into one entry whose `email` field carries the address.

Return ONLY a JSON object. The example below is illustrative ONLY
— its entities are deliberately drawn from a fictitious shipping
company chosen so they could not plausibly appear in real Enron
content. **DO NOT echo any of the example names below in your
output.** They exist solely to show the JSON shape; every entity
in your output must come from the actual email text:

{{
  "persons": [
    {{
      "name": "Aldís Sigurðardóttir",
      "email": "aldis@northfjord-logistics.is",
      "role": "managing director",
      "affiliation": "Northfjord Logistics ehf",
      "aliases": ["Aldís", "A. Sigurðardóttir", "aldis@northfjord-logistics.is"],
      "mentions": ["Email 1"]
    }}
  ],
  "organizations": [
    {{
      "name": "Northfjord Logistics ehf",
      "relationship": "vendor",
      "aliases": ["Northfjord", "Northfjord Logistics", "northfjord-logistics.is"],
      "mentions": ["Email 1"]
    }}
  ],
  "works": [
    {{
      "name": "Q2 freight throughput memo",
      "kind": "memo",
      "creator": "Aldís Sigurðardóttir",
      "mentions": ["Email 1"]
    }}
  ],
  "concepts": [
    {{
      "name": "trans-Atlantic LTL rate card",
      "description": "freight-pricing convention the team is renegotiating ahead of Q4",
      "mentions": ["Email 1"]
    }}
  ],
  "initiatives": [
    {{
      "name": "Project Skerry",
      "status": "ratifying the rate card",
      "participants": ["Aldís Sigurðardóttir"],
      "mentions": ["Email 1"]
    }}
  ]
}}

Empty arrays for any kind that didn't appear. Omit affiliation,
role, email, status, creator, kind, description, or relationship
fields when the message doesn't support them — do not invent.
If you find yourself about to emit `Aldís Sigurðardóttir`,
`Northfjord Logistics ehf`, `Q2 freight throughput memo`,
`trans-Atlantic LTL rate card`, or `Project Skerry`, stop —
those are example names, not corpus content.

Emails:
{passages}"#,
            passages = passages
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn business_email_domain_identity() {
        let d = BusinessEmailDomain::new();
        assert_eq!(d.id(), "business_email");
        assert_eq!(d.name(), "Business email");
    }

    #[test]
    fn business_email_domain_is_object_safe() {
        let domain: std::sync::Arc<dyn Domain> = std::sync::Arc::new(BusinessEmailDomain::new());
        assert_eq!(domain.id(), "business_email");
    }

    #[test]
    fn entity_prompt_opts_in_on_empty_probe() {
        let d = BusinessEmailDomain::new();
        let probe = d.entity_extraction_prompt(&[]);
        assert!(probe.is_some());
        assert!(probe.unwrap().is_empty());
    }

    #[test]
    fn entity_prompt_emphasises_headers() {
        let d = BusinessEmailDomain::new();
        let chunk = Chunk {
            id: 1,
            title: Some("Re: hello".to_string()),
            content: "From: a@x.com\nTo: b@y.com\n\nbody".to_string(),
        };
        let prompt = d.entity_extraction_prompt(&[&chunk]).unwrap();
        // Header rule is load-bearing — keep this assertion strict so
        // a future re-edit can't quietly drop the instruction.
        assert!(prompt.contains("DO NOT skip header identities"));
        assert!(prompt.contains("From:"));
        assert!(prompt.contains("To:"));
        assert!(prompt.contains("[Email 1 — Re: hello]"));
    }
}
