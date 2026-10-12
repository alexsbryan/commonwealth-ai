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

/// A forced-choice daemon that answers a wave natively and counts requests
/// and questions: every single question gets a distribution whose top label
/// is the last character of its user turn.
fn wave_daemon() -> (EmbedFn, InferenceFn, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let requests = Arc::new(AtomicUsize::new(0));
    let questions = Arc::new(AtomicUsize::new(0));
    let (r, q) = (Arc::clone(&requests), Arc::clone(&questions));
    let answer = move |p: &ChatPrompt| {
        q.fetch_add(1, Ordering::SeqCst);
        let top = p.user.chars().last().unwrap().to_string();
        let mut d = std::collections::BTreeMap::new();
        d.insert("A".to_string(), 0.1f32);
        d.insert("B".to_string(), 0.1f32);
        d.insert("0".to_string(), 0.1f32);
        d.insert(top, 0.8f32);
        oicp_types::forced_choice::answer(&d)
    };
    let chat: InferenceFn = Arc::new(move |p: &ChatPrompt, _| {
        r.fetch_add(1, Ordering::SeqCst);
        let text = match crate::enrichment::atlas::resolve_records::wave_singles(p) {
            Some(singles) => {
                let texts: Vec<std::result::Result<String, String>> =
                    singles.unwrap().iter().map(|s| Ok(answer(s))).collect();
                crate::enrichment::atlas::resolve_records::wave_text(&texts)
            }
            None => answer(p),
        };
        Box::pin(async move { Ok(text) })
    });
    let embed: EmbedFn = Arc::new(|_| Box::pin(async { Ok(vec![0.0]) }));
    (embed, chat, requests, questions)
}

fn line_question(line: &str) -> ChatPrompt {
    ChatPrompt::new("sys", format!("doc\nLine {line}"))
        .with_response_schema("read", oicp_types::forced_choice::schema(&["A", "B", "0"]))
        .with_phase_id("document_passes_locate")
        .with_temperature(0.0)
}

#[tokio::test]
async fn a_wave_records_each_question_under_its_own_key_and_replays_either_way() {
    use crate::enrichment::atlas::resolve_records::{decision_call, decision_wave};
    let labels = vec!["A", "B", "0"];
    let ask_wave = |c: InferenceFn, lines: Vec<&'static str>| {
        let labels = labels.clone();
        async move {
            let qs: Vec<ChatPrompt> = lines.iter().map(|l| line_question(l)).collect();
            let ls = vec![labels; qs.len()];
            let items: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
            decision_wave(&c, &qs, &ls, "doc", &items).await
        }
    };
    let dir = tempfile::tempdir().unwrap();
    let (embed, chat, requests, questions) = wave_daemon();
    let (_, c) = answering(Asker::Daemon, dir.path(), embed, chat).unwrap();
    // One question asked alone first; the wave then asks only the other two,
    // as one request.
    let alone = decision_call(&c, &line_question("1 A"), &labels, "doc", "1").await.unwrap();
    let waved = ask_wave(Arc::clone(&c), vec!["1 A", "2 B", "3 0"]).await;
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    assert_eq!(questions.load(Ordering::SeqCst), 3);
    assert_eq!(waved[0].as_ref().unwrap(), &alone);
    // Each answer is on file under its single question's key, in the bytes a
    // single request is answered with.
    let store = std::fs::read_to_string(dir.path().join(ANSWERS_FILE)).unwrap();
    for line in ["2 B", "3 0"] {
        assert!(store.contains(&chat_key(&line_question(line), None)), "{line}");
    }

    // Replayed with no daemon: each question alone, and the wave again, give
    // the recorded answers.
    let (embed, chat) = refusing_daemon();
    let (_, c) = answering(Asker::Replay, dir.path(), embed, chat).unwrap();
    for (i, line) in ["1 A", "2 B", "3 0"].iter().enumerate() {
        let single = decision_call(&c, &line_question(line), &labels, "doc", line).await;
        assert_eq!(single.as_ref().unwrap(), waved[i].as_ref().unwrap());
    }
    assert_eq!(ask_wave(Arc::clone(&c), vec!["1 A", "2 B", "3 0"]).await, waved);
    // A wave holding one question the run never asked refuses that one alone.
    let partly = ask_wave(Arc::clone(&c), vec!["2 B", "4 A"]).await;
    assert_eq!(partly[0], waved[1]);
    assert!(
        matches!(&partly[1], Err(crate::enrichment::atlas::resolve_records::Refusal::NoAnswer { reason }) if reason.contains("no answer recorded"))
    );
}
