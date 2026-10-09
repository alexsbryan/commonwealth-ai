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
    let (e, c) = answering(Asker::Daemon, dir.path(), embed, chat).unwrap();
    let q = ChatPrompt::new("sys", "which kind?").with_phase_id("document_passes_locate");
    let first = c(&q, None).await.unwrap();
    // The same question again in the run: the one answer, no second call.
    assert_eq!(c(&q, None).await.unwrap(), first);
    let v = e("a line").await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let (embed, chat) = refusing_daemon();
    let (e, c) = answering(Asker::Replay, dir.path(), embed, chat).unwrap();
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
    assert!(answering(Asker::Replay, dir.path(), embed.clone(), chat.clone()).is_err());
    let q = ChatPrompt::new("sys", "same?");
    let line = Answer {
        key: chat_key(&q, None),
        port: "chat".into(),
        phase: None,
        answer: Value::String(r#"{"A":1.0,"0":0.0}"#.into()),
    };
    std::fs::write(
        dir.path().join(GOLD_FILE),
        format!("{}\n", serde_json::to_string(&line).unwrap()),
    )
    .unwrap();
    let (_, c) = answering(Asker::Gold, dir.path(), embed, chat).unwrap();
    assert_eq!(c(&q, None).await.unwrap(), r#"{"A":1.0,"0":0.0}"#);
}

#[test]
fn the_asker_flag_is_a_closed_set() {
    for s in ["daemon", "replay", "gold"] {
        assert_eq!(Asker::parse(s).unwrap().label(), s);
    }
    assert!(Asker::parse("cache").is_err());
}
