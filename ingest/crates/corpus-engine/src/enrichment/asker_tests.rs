// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::*;

/// A daemon that answers each question with a fresh counter, so a second
/// call is visible as a different answer, and counts its calls.
fn counting_daemon() -> (EmbedFn, InferenceFn, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);
    let chat: InferenceFn = Arc::new(move |p: &ChatPrompt, _| {
        let n = c.fetch_add(1, Ordering::SeqCst);
        let text = format!("{} #{n}", p.user);
        Box::pin(async move { Ok(text) })
    });
    let e = Arc::clone(&calls);
    let embed: EmbedFn = Arc::new(move |text: &str| {
        let n = e.fetch_add(1, Ordering::SeqCst) as f32;
        let v = vec![text.len() as f32, n, 0.1];
        Box::pin(async move { Ok(v) })
    });
    (embed, chat, calls)
}

/// Every question answered by one model, `model`.
fn by(model: &'static str) -> AnsweredBy {
    Arc::new(move |_, _| model.to_string())
}

fn refusing_daemon() -> (EmbedFn, InferenceFn) {
    let chat: InferenceFn =
        Arc::new(|_, _| Box::pin(async { panic!("a replay must never reach the daemon") }));
    let embed: EmbedFn =
        Arc::new(|_| Box::pin(async { panic!("a replay must never reach the daemon") }));
    (embed, chat)
}

#[tokio::test]
async fn a_recorded_run_replays_with_no_daemon_and_refuses_what_it_never_asked() {
    let dir = tempfile::tempdir().unwrap();
    let (embed, chat, calls) = counting_daemon();
    let (e, c) = answering(Asker::Daemon, dir.path(), by("m"), embed, chat).unwrap();
    let q = ChatPrompt::new("sys", "which kind?").with_phase_id("document_passes_locate");
    let first = c(&q, None).await.unwrap();
    // The same question again in the run: the one answer, no second call.
    assert_eq!(c(&q, None).await.unwrap(), first);
    let v = e("a line").await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let (embed, chat) = refusing_daemon();
    let (e, c) = answering(Asker::Replay, dir.path(), by("m"), embed, chat).unwrap();
    assert_eq!(c(&q, None).await.unwrap(), first);
    assert_eq!(e("a line").await.unwrap(), v);
    // A question the run never asked is refused, never answered by default.
    let other = ChatPrompt::new("sys", "which kind?").with_phase_id("document_passes_choose");
    let err = c(&other, None).await.unwrap_err().to_string();
    assert!(err.contains("no answer recorded"), "{err}");
    assert!(e("another line").await.is_err());
    // The per-call budget is part of the question.
    assert!(c(&q, Some(5)).await.is_err());
}

#[tokio::test]
async fn gold_answers_from_its_own_store_and_replay_needs_a_store() {
    let dir = tempfile::tempdir().unwrap();
    let (embed, chat) = refusing_daemon();
    assert!(answering(
        Asker::Replay,
        dir.path(),
        by("m"),
        embed.clone(),
        chat.clone()
    )
    .is_err());
    let q = ChatPrompt::new("sys", "same?");
    let line = Answer {
        key: chat_key(&q, None),
        port: "chat".into(),
        phase: None,
        model: None,
        answer: Value::String(r#"{"A":1.0,"0":0.0}"#.into()),
    };
    std::fs::write(
        dir.path().join(GOLD_FILE),
        format!("{}\n", serde_json::to_string(&line).unwrap()),
    )
    .unwrap();
    let (_, c) = answering(Asker::Gold, dir.path(), by("m"), embed, chat).unwrap();
    assert_eq!(c(&q, None).await.unwrap(), r#"{"A":1.0,"0":0.0}"#);
}

#[test]
fn the_asker_flag_is_a_closed_set() {
    for s in ["daemon", "replay", "recorded", "gold"] {
        assert_eq!(Asker::parse(s).unwrap().label(), s);
    }
    assert!(Asker::parse("cache").is_err());
}

/// `recorded` asks the model only what no earlier run of that model asked:
/// a question a run of model m answered is answered from the store, a new
/// one goes to the daemon and is recorded, stamped; under another model every
/// question is asked again, and a store not yet written is an empty one.
#[tokio::test]
async fn recorded_reuses_only_what_the_same_model_answered_and_asks_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let q = ChatPrompt::new("sys", "which kind?").with_phase_id("document_passes_locate");
    let new = ChatPrompt::new("sys", "which value?").with_phase_id("document_passes_act");

    let (embed, chat, calls) = counting_daemon();
    let (e, c) = answering(Asker::Recorded, dir.path(), by("m"), embed, chat).unwrap();
    let first = c(&q, None).await.unwrap();
    let v = e("a line").await.unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "an empty store asks everything"
    );

    let (embed, chat, calls) = counting_daemon();
    let (e, c) = answering(Asker::Recorded, dir.path(), by("m"), embed, chat).unwrap();
    assert_eq!(c(&q, None).await.unwrap(), first);
    assert_eq!(e("a line").await.unwrap(), v);
    assert_eq!(calls.load(Ordering::SeqCst), 0, "m answered both already");
    let fresh = c(&new, None).await.unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "only the new question is asked"
    );

    let store = std::fs::read_to_string(dir.path().join(ANSWERS_FILE)).unwrap();
    let stamps: Vec<Option<String>> = store
        .lines()
        .map(|l| serde_json::from_str::<Answer>(l).unwrap().model)
        .collect();
    assert_eq!(stamps, vec![Some("m".to_string()); 3]);

    let (embed, chat, calls) = counting_daemon();
    // One call outside the store, so n's answers are told from m's by count.
    chat(&ChatPrompt::new("sys", "warm"), None).await.unwrap();
    let (_, c) = answering(Asker::Recorded, dir.path(), by("n"), embed, chat).unwrap();
    let by_n = c(&q, None).await.unwrap();
    assert_ne!(by_n, first);
    assert_eq!(calls.load(Ordering::SeqCst), 2, "n never answered it");

    // A replay gives the latest recording of each question, whoever gave it.
    let (embed, chat) = refusing_daemon();
    let (_, c) = answering(Asker::Replay, dir.path(), by("m"), embed, chat).unwrap();
    assert_eq!(c(&q, None).await.unwrap(), by_n);
    assert_eq!(c(&new, None).await.unwrap(), fresh);
}
