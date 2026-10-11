// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use serde_json::json;

const SHA: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

fn document(metadata: Option<serde_json::Value>) -> Document {
    Document {
        text_sha256: SHA.into(),
        extractor: "plaintext@0.1.0".into(),
        source: SourceRef {
            id: "okafor2019".into(),
            sha256: Some(SHA.into()),
        },
        metadata,
    }
}

#[test]
fn difference_kinds_are_snake_case_on_the_wire_and_as_str_agrees() {
    for kind in DifferenceKind::ALL {
        let wire = serde_json::to_value(kind).unwrap();
        assert_eq!(wire, json!(kind.as_str()), "{kind:?}");
        let back: DifferenceKind = serde_json::from_value(wire).unwrap();
        assert_eq!(back, kind);
    }
    assert!(
        serde_json::from_value::<DifferenceKind>(json!("paraphrased")).is_err(),
        "the set is closed: an unknown kind is an error, never a default"
    );
}

#[test]
fn a_record_states_its_missing_source_hash_as_null() {
    let record = SourceRef {
        id: "row-17".into(),
        sha256: None,
    };
    let v = serde_json::to_value(&record).unwrap();
    assert_eq!(v, json!({"id": "row-17", "sha256": null}));
}

#[test]
fn align_response_round_trips_with_every_field() {
    let resp = AlignResponse {
        alignments: vec![Alignment {
            span: Span {
                corpus_id: "fixture".into(),
                document: document(Some(json!({"id": "okafor2019", "type": "article-journal"}))),
                start: 10,
                end: 42,
                exact: "the river keeps its own counsel".into(),
                prefix: "and ".into(),
                suffix: ".".into(),
            },
            differences: vec![Difference {
                kind: DifferenceKind::Substituted,
                quote: [4, 9],
                source: [14, 19],
            }],
            coverage: 0.875,
        }],
        aligner: "align/2 norm/0".into(),
        corpora: vec![CorpusTexts {
            corpus_id: "fixture".into(),
            texts_digest: SHA.into(),
        }],
        corpora_unavailable: vec![Unavailable {
            corpus_id: "old".into(),
            reason: reasons::TEXTS_NOT_STORED.into(),
        }],
    };
    let wire = serde_json::to_string(&resp).unwrap();
    let back: AlignResponse = serde_json::from_str(&wire).unwrap();
    assert_eq!(back, resp);
    let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
    assert_eq!(v["alignments"][0]["differences"][0]["kind"], "substituted");
    assert_eq!(v["alignments"][0]["differences"][0]["quote"], json!([4, 9]));
}

#[test]
fn an_empty_unavailable_list_is_omitted_and_reads_back_empty() {
    let resp = AlignResponse {
        alignments: vec![],
        aligner: "align/2 norm/0".into(),
        corpora: vec![],
        corpora_unavailable: vec![],
    };
    let v = serde_json::to_value(&resp).unwrap();
    assert!(v.get("corpora_unavailable").is_none());
    let back: AlignResponse = serde_json::from_value(v).unwrap();
    assert!(back.corpora_unavailable.is_empty());
}

#[test]
fn text_slice_and_align_request_round_trip() {
    let slice = TextSlice {
        document: document(None),
        start: 0,
        end: 5,
        text: "hello".into(),
        before: String::new(),
        after: " world".into(),
    };
    let back: TextSlice = serde_json::from_str(&serde_json::to_string(&slice).unwrap()).unwrap();
    assert_eq!(back, slice);
    assert!(
        serde_json::to_value(&slice).unwrap()["document"]
            .get("metadata")
            .is_none(),
        "no declared metadata is absent, not null"
    );

    let bare: AlignRequest = serde_json::from_value(json!({"quote": "a b c"})).unwrap();
    assert!(
        bare.corpora.is_empty(),
        "empty corpora means every readable one"
    );
    assert_eq!(bare.effective_limit(), AlignRequest::DEFAULT_LIMIT);
    assert_eq!(bare.effective_context(), DEFAULT_CONTEXT);
    assert_eq!(
        serde_json::to_value(&bare).unwrap(),
        json!({"quote": "a b c"})
    );
}

#[test]
fn evidence_endpoints_omit_an_absent_align_endpoint() {
    let e = EvidenceEndpoints {
        text_endpoint: "/oicp/v1/text".into(),
        align_endpoint: None,
    };
    assert_eq!(
        serde_json::to_value(&e).unwrap(),
        json!({"text_endpoint": "/oicp/v1/text"})
    );
}

#[test]
fn the_digest_preimage_is_sorted_distinct_lines_with_records_as_dash() {
    let b = "b".repeat(64);
    let a = "a".repeat(64);
    let pre = texts_digest_preimage([
        (b.as_str(), None, "jsonl@0.8.0"),
        (a.as_str(), Some(SHA), "plaintext@0.8.0"),
        (b.as_str(), None, "jsonl@0.8.0"),
    ]);
    assert_eq!(
        pre,
        format!("{a} {SHA} plaintext@0.8.0\n{b} - jsonl@0.8.0\n"),
        "order of arrival and repeats do not change the digest"
    );
    assert_eq!(
        texts_digest_preimage([]),
        "",
        "an empty corpus digests the empty string"
    );
}

#[test]
fn sha256_names_are_64_lowercase_hex() {
    assert!(is_sha256_hex(SHA));
    assert!(
        !is_sha256_hex(&SHA.to_uppercase()),
        "one encoding: lowercase"
    );
    assert!(!is_sha256_hex(&SHA[..63]));
    assert!(!is_sha256_hex(&format!("{}g", &SHA[..63])));
}
