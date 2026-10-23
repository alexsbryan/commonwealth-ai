// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn chunk_range_count() {
    let r = ChunkRange::new(0, 1000);
    assert_eq!(r.count(), 1000);
}

#[test]
fn chunk_range_serde_roundtrip() {
    let r = ChunkRange::new(500, 1500);
    let json = serde_json::to_string(&r).unwrap();
    let back: ChunkRange = serde_json::from_str(&json).unwrap();
    assert_eq!(r, back);
}

#[test]
fn corpus_shard_info_serde_roundtrips_atlas_fields() {
    let info = CorpusShardInfo {
        corpus_id: "wikipedia".into(),
        chunk_range: None,
        is_replica: false,
        last_updated: 0,
        chunk_count: 1_000_000,
        canonical_fingerprint: Some("abc123".into()),
        total_shards: Some(38),
        processed_shards: vec![0, 1, 2],
        atlas_atom_count: 51_280,
        atlas_tier2_count: 612,
        atlas_fingerprint: Some(
            "7c3f8e9b1f0a2d3c4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d".into(),
        ),
    };
    let json = serde_json::to_string(&info).unwrap();
    let back: CorpusShardInfo = serde_json::from_str(&json).unwrap();
    assert_eq!(back.atlas_atom_count, 51_280);
    assert_eq!(back.atlas_tier2_count, 612);
    assert!(back.atlas_fingerprint.unwrap().starts_with("7c3f"));
}

/// A blob from an older peer (pre-C1) won't carry the atlas
/// fields. Deserialize must succeed with zero counts and `None`
/// fingerprint so the upgrade window is graceful.
#[test]
fn corpus_shard_info_back_compat_without_atlas_fields() {
    let json = r#"{
        "corpus_id": "wikipedia",
        "chunk_range": null,
        "is_replica": false,
        "last_updated": 0,
        "chunk_count": 0,
        "processed_shards": []
    }"#;
    let back: CorpusShardInfo = serde_json::from_str(json).unwrap();
    assert_eq!(back.atlas_atom_count, 0);
    assert_eq!(back.atlas_tier2_count, 0);
    assert!(back.atlas_fingerprint.is_none());
}

#[test]
fn knowledge_result_legacy_json_deserialises_with_none_chunk_id() {
    // Older peers (pre reading-surface plumbing) emit
    // KnowledgeResult JSON without the chunk_id / source_doc_id
    // fields. Verify they deserialise cleanly to None so
    // wire-compat is preserved across mixed-version meshes.
    let legacy = r#"{
        "content": "Alyosha Karamazov is a novice",
        "title": "The Brothers Karamazov",
        "corpus_id": "brothers_karamazov",
        "url": null,
        "score": 0.87,
        "metadata": {}
    }"#;
    let parsed: KnowledgeResult = serde_json::from_str(legacy).expect("deserialise");
    assert_eq!(parsed.chunk_id, None);
    assert_eq!(parsed.source_doc_id, None);
    assert_eq!(parsed.corpus_id, "brothers_karamazov");
    // A peer that predates the provenance fields says NOTHING about them,
    // and the requester must read that as absence — never as a class.
    // `acquired_from_peer` turns both `None`s into the refusing value
    // (`Custody::Unknown` through the join, `Grain::Summary`), so an
    // un-upgraded peer's hits stay exactly as unquotable as they were.
    assert_eq!(parsed.custody, None);
    assert_eq!(parsed.grain, None);

    // And a forward-compat round-trip preserves every field.
    let modern = KnowledgeResult {
        content: "passage".into(),
        title: Some("title".into()),
        corpus_id: "bk".into(),
        url: None,
        score: 0.5,
        metadata: Default::default(),
        chunk_id: Some(42),
        source_doc_id: Some("bk-ch01".into()),
        custody: Some("public-web".into()),
        grain: Some("summary".into()),
        peer_name: None,
        peer_node_id: None,
        document: None,
        document_absent: None,
        span: None,
        span_absent: None,
    };
    let json = serde_json::to_string(&modern).unwrap();
    let back: KnowledgeResult = serde_json::from_str(&json).unwrap();
    assert_eq!(back.chunk_id, Some(42));
    assert_eq!(back.source_doc_id.as_deref(), Some("bk-ch01"));
    assert_eq!(back.custody.as_deref(), Some("public-web"));
    assert_eq!(back.grain.as_deref(), Some("summary"));

    // An OLD peer reading a NEW payload must not choke on the additions —
    // the other half of interop, and the half a `skip_serializing_if`
    // does not prove on its own.
    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct LegacyShape {
        content: String,
        corpus_id: String,
        score: f32,
    }
    let old_reader: LegacyShape =
        serde_json::from_str(&json).expect("a new payload still parses as the old shape");
    assert_eq!(old_reader.corpus_id, "bk");

    // A recorded-nothing stamp stays ABSENT on the wire rather than
    // becoming the string "unknown" — the two mean different things to
    // the join on the far side.
    let silent = KnowledgeResult {
        custody: None,
        grain: None,
        ..modern
    };
    let json = serde_json::to_string(&silent).unwrap();
    assert!(
        !json.contains("custody"),
        "absence must not be serialised: {json}"
    );
    assert!(
        !json.contains("grain"),
        "absence must not be serialised: {json}"
    );
}

/// A v0.4 hit reads into the v0.5 type and writes back unchanged; a
/// v0.5 hit carries its document, metadata verbatim.
#[test]
fn knowledge_result_document_is_additive_over_v04() {
    let v04 = serde_json::json!({
        "content": "passage",
        "corpus_id": "bk",
        "score": 0.5,
        "metadata": {"author": "Dostoevsky"},
        "chunk_id": 42
    });
    let hit: KnowledgeResult = serde_json::from_value(v04.clone()).unwrap();
    assert!(hit.document.is_none());
    assert_eq!(serde_json::to_value(&hit).unwrap(), v04);

    let mut v05 = v04;
    v05["document"] = serde_json::json!({
        "text_sha256": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
        "extractor": "plaintext@0.1.0",
        "source": {"id": "bk-ch01", "sha256": null},
        "metadata": {"id": "okafor2019", "type": "article-journal"}
    });
    let hit: KnowledgeResult = serde_json::from_value(v05.clone()).unwrap();
    let doc = hit.document.as_ref().unwrap();
    assert_eq!(doc.source.sha256, None, "a record has no source bytes");
    assert_eq!(doc.metadata.as_ref().unwrap()["type"], "article-journal");
    assert_eq!(serde_json::to_value(&hit).unwrap(), v05);
}

#[test]
fn knowledge_search_thin_client_shape_deserializes() {
    // OICP v0.4 §6.1: a thin client sends only `query` — no embedding,
    // and the OICP field name `query` (not `query_text`).
    let req: KnowledgeSearchRequest =
        serde_json::from_value(serde_json::json!({"query": "stoic virtue", "limit": 3})).unwrap();
    assert_eq!(req.query_text, "stoic virtue");
    assert!(req.query_embedding.is_empty(), "host embeds when absent");
    assert_eq!(req.effective_limit(), 3);
}

#[test]
fn knowledge_search_mesh_shape_still_deserializes() {
    // The mesh-internal shape (pre-embedded, `query_text`) is unchanged.
    let req: KnowledgeSearchRequest = serde_json::from_value(serde_json::json!({
        "query_embedding": [0.1, 0.2, 0.3],
        "query_text": "stoic virtue",
    }))
    .unwrap();
    assert_eq!(req.query_embedding, vec![0.1, 0.2, 0.3]);
    assert_eq!(req.query_text, "stoic virtue");
}

#[test]
fn knowledge_search_empty_embedding_omitted_from_wire() {
    // An absent embedding must not serialize as `query_embedding: []`.
    let req = KnowledgeSearchRequest {
        query_embedding: Vec::new(),
        query_text: "q".into(),
        corpora: None,
        limit: None,
        documents: Vec::new(),
        granularity: Granularity::Passage,
        priority: None,
    };
    let v = serde_json::to_value(&req).unwrap();
    assert!(v.get("query_embedding").is_none());
}

/// v0.6 §2.11: a request that does not name a granularity is a passage
/// search and writes back without the key, so old clients and hosts see
/// the wire they knew; `sentence` round-trips; a value outside the set
/// is an error, never a passage search.
#[test]
fn granularity_defaults_to_passage_and_stays_off_the_wire() {
    let old = serde_json::json!({"query": "q", "limit": 3, "documents": ["ab"]});
    let req: KnowledgeSearchRequest = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(req.granularity, Granularity::Passage);
    let back = serde_json::to_value(&req).unwrap();
    assert!(back.get("granularity").is_none(), "{back}");

    let mut asks = old;
    asks["granularity"] = "sentence".into();
    let req: KnowledgeSearchRequest = serde_json::from_value(asks).unwrap();
    assert_eq!(req.granularity, Granularity::Sentence);
    assert_eq!(
        serde_json::to_value(&req).unwrap()["granularity"],
        "sentence"
    );

    let bad = serde_json::json!({"query": "q", "granularity": "paragraph"});
    assert!(serde_json::from_value::<KnowledgeSearchRequest>(bad).is_err());
}

/// v0.6 §2.11: a passage reply carries none of the sentence fields and
/// writes back unchanged; a sentence reply round-trips them; a scorer
/// outside the closed set fails to decode rather than reading as one.
#[test]
fn sentence_reply_fields_are_additive_and_closed() {
    let passage = serde_json::json!({"results": [], "corpora_searched": ["bk"]});
    let r: KnowledgeSearchResponse = serde_json::from_value(passage.clone()).unwrap();
    assert!(r.scorer.is_none() && r.route.is_none());
    assert!(r.sentences_pending.is_none() && r.truncated.is_none());
    assert_eq!(serde_json::to_value(&r).unwrap(), passage);

    let sentences = serde_json::json!({
        "results": [], "corpora_searched": ["bk"],
        "scorer": {"name": "rerank", "model": "bge-reranker"},
        "route": "passages", "sentences_pending": 412,
        "truncated": {"scanned": 100000, "in_scope": 140000}
    });
    let r: KnowledgeSearchResponse = serde_json::from_value(sentences.clone()).unwrap();
    assert_eq!(
        r.scorer,
        Some(Scorer {
            name: SentenceScorer::Rerank,
            model: "bge-reranker".into()
        })
    );
    assert_eq!(r.route, Some(SentenceRoute::Passages));
    assert_eq!(r.sentences_pending, Some(412));
    assert_eq!(r.truncated.map(|t| t.in_scope), Some(140_000));
    assert_eq!(serde_json::to_value(&r).unwrap(), sentences);
    assert_eq!(SentenceScorer::Rerank.as_str(), "rerank");
    assert_eq!(
        serde_json::to_value(SentenceScorer::Cosine).unwrap(),
        SentenceScorer::Cosine.as_str()
    );

    let mut unknown = sentences;
    unknown["scorer"]["name"] = "bm25".into();
    assert!(serde_json::from_value::<KnowledgeSearchResponse>(unknown).is_err());
}

/// v0.6 §2.11: the queue field is additive, round-trips, and a place
/// outside the closed set fails to decode.
#[test]
fn sentence_queue_is_additive_and_closed() {
    let reply = serde_json::json!({
        "results": [], "corpora_searched": ["a", "b"],
        "route": "passages", "sentences_pending": 9,
        "sentence_queue": [
            {"corpus": "a", "pending": 4, "place": "embedding"},
            {"corpus": "b", "pending": 5, "place": "queued",
             "behind": [{"corpus": "a", "pending": 4}, {"corpus": "c"}]}
        ]
    });
    let r: KnowledgeSearchResponse = serde_json::from_value(reply.clone()).unwrap();
    let q = r.sentence_queue.clone().unwrap();
    assert_eq!(q[0].place, IndexPlace::Embedding);
    assert!(q[0].behind.is_empty());
    assert_eq!(q[1].behind[1].pending, None);
    let mut written = serde_json::to_value(&r).unwrap();
    // `behind` is written empty for the embedding corpus.
    written["sentence_queue"][0]
        .as_object_mut()
        .unwrap()
        .remove("behind");
    assert_eq!(written, reply);
    let old = serde_json::json!({"results": [], "corpora_searched": []});
    let r: KnowledgeSearchResponse = serde_json::from_value(old).unwrap();
    assert!(r.sentence_queue.is_none());
    let mut bad = reply;
    bad["sentence_queue"][0]["place"] = "idle".into();
    assert!(serde_json::from_value::<KnowledgeSearchResponse>(bad).is_err());
}

/// v0.6 §2.17: a request naming no priority writes back without the key, on
/// search and on verify; each value round-trips under its wire name; a value
/// outside the set is an error, never read as absent.
#[test]
fn priority_stays_off_the_wire_when_absent_and_refuses_what_it_does_not_name() {
    let old = serde_json::json!({"query": "q", "limit": 3});
    let req: KnowledgeSearchRequest = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(req.priority, None);
    assert!(serde_json::to_value(&req)
        .unwrap()
        .get("priority")
        .is_none());

    for p in [RequestPriority::Interactive, RequestPriority::Background] {
        let mut asks = old.clone();
        asks["priority"] = p.as_str().into();
        let req: KnowledgeSearchRequest = serde_json::from_value(asks).unwrap();
        assert_eq!(req.priority, Some(p));
        assert_eq!(serde_json::to_value(&req).unwrap()["priority"], p.as_str());
    }

    let mut bad = old;
    bad["priority"] = "urgent".into();
    assert!(serde_json::from_value::<KnowledgeSearchRequest>(bad).is_err());

    let verify = serde_json::json!({"unit": "u", "tier": "fast"});
    let req: crate::verify::VerifyRequest = serde_json::from_value(verify.clone()).unwrap();
    assert!(serde_json::to_value(&req)
        .unwrap()
        .get("priority")
        .is_none());
    let mut asks = verify;
    asks["priority"] = "background".into();
    let req: crate::verify::VerifyRequest = serde_json::from_value(asks).unwrap();
    assert_eq!(req.priority, Some(RequestPriority::Background));
}
