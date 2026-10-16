use super::*;

fn line(of: &[&str]) -> Fifo {
    let mut f = Fifo::default();
    for c in of {
        f.push(c);
    }
    f
}

fn scoped(of: &[(&str, u64)]) -> Vec<(String, u64)> {
    of.iter().map(|(c, n)| (c.to_string(), *n)).collect()
}

/// Failing input: a corpus queued behind another answered as embedding.
#[test]
fn a_corpus_behind_the_one_embedding_is_queued_never_embedding() {
    let mut f = line(&["a", "b"]);
    f.take();
    let got = places(&f.snapshot(), &scoped(&[("a", 40), ("b", 7)]), |_| None);
    assert_eq!(got[0].corpus, "a");
    assert_eq!(got[0].place, IndexPlace::Embedding);
    assert!(got[0].behind.is_empty());
    assert_eq!(got[1].corpus, "b");
    assert_eq!(got[1].place, IndexPlace::Queued);
    assert_eq!(
        got[1].behind,
        [QueuedCorpus {
            corpus: "a".into(),
            pending: Some(40)
        }]
    );
}

/// Failing input: a corpus named ahead that has already finished.
#[test]
fn a_finished_corpus_is_named_ahead_of_nobody() {
    let mut f = line(&["a", "b"]);
    f.take();
    let got = places(&f.snapshot(), &scoped(&[("b", 7)]), |_| None);
    assert_eq!(got[0].behind.len(), 1, "a is embedding: {got:?}");
    f.finish();
    let got = places(&f.snapshot(), &scoped(&[("b", 7)]), |_| None);
    assert_eq!(got.len(), 1);
    assert!(got[0].behind.is_empty(), "a has finished: {got:?}");
    f.take();
    let got = places(&f.snapshot(), &scoped(&[("b", 7)]), |_| None);
    assert_eq!(got[0].place, IndexPlace::Embedding);
    assert!(got[0].behind.is_empty());
}

/// Nothing ahead and the worker idle: the enqueue woke it, so the corpus is
/// embedding, not queued behind nothing.
#[test]
fn a_corpus_with_nothing_ahead_is_embedding_before_the_worker_takes_it() {
    let f = line(&["a"]);
    let got = places(&f.snapshot(), &scoped(&[("a", 4)]), |_| None);
    assert_eq!(got[0].place, IndexPlace::Embedding);
    assert!(got[0].behind.is_empty());
}

/// The order is the queue's: a corpus out of scope ahead is named in turn,
/// with the count the host has (or none for one it has not opened), and the
/// corpus embedding now is not named twice when a search re-queued it.
#[test]
fn the_corpora_ahead_come_in_the_workers_order() {
    let mut f = line(&["x", "y", "z", "b"]);
    f.take();
    f.push("x");
    let got = places(&f.snapshot(), &scoped(&[("b", 3)]), |c| {
        (c == "y").then_some(11)
    });
    let ahead: Vec<_> = got[0]
        .behind
        .iter()
        .map(|q| (q.corpus.as_str(), q.pending))
        .collect();
    assert_eq!(ahead, [("x", None), ("y", Some(11)), ("z", None)]);
}

/// A pending corpus in no list is placed behind the whole line, not as
/// embedding.
#[test]
fn a_pending_corpus_in_no_list_is_queued_behind_the_whole_line() {
    let mut f = line(&["a"]);
    f.take();
    let got = places(&f.snapshot(), &scoped(&[("q", 2)]), |_| None);
    assert_eq!(got[0].place, IndexPlace::Queued);
    assert_eq!(got[0].behind.len(), 1);
}
