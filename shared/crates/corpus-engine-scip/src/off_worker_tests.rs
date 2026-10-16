// SPDX-License-Identifier: AGPL-3.0-or-later
//! Each step of a SCIP rebuild's tail, run the way the reindexer runs it,
//! beside a ticker that must keep ticking.
//!
//! The rebuild task is woken by its exporter's exit, which the runtime's I/O
//! driver delivers, so it runs on the worker that just left the driver. A step
//! run inline there leaves the driver unpolled for its whole length, and the
//! ticker, like every accept and timer in the daemon, waits on it. Each test
//! fails on that shape: the step's rows are sized so it runs well past the
//! ticker's period, and the ticker must never wait on it.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use prost::Message;

use crate::scip_graph::{ScipGraph, ScipRefRecord, ScipSymbolRecord};
use crate::scip_proto::{Document, Index, Occurrence, SymbolInformation, SymbolRole};

const TICK: Duration = Duration::from_millis(20);

/// Run `step` on a multi-worker runtime like the daemon's, beside a `TICK`
/// ticker, woken where the rebuild is: alone, in the LIFO slot of the worker
/// that has just left the driver. The ticker wakes it right after a tick, so
/// no other task shares that wake (two would make tokio hand the driver to
/// another worker, which the exporter's lone exit does not). Returns how long
/// the step ran and the ticker's worst gap while it did.
fn beside_a_ticker<F, Fut>(step: F) -> (Duration, Duration)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async move {
        let done = Arc::new(AtomicBool::new(false));
        let (go, gone) = tokio::sync::oneshot::channel::<()>();
        let ticker = tokio::spawn({
            let done = Arc::clone(&done);
            async move {
                let mut go = Some(go);
                let mut worst = Duration::ZERO;
                let mut last = Instant::now();
                while !done.load(Ordering::Relaxed) {
                    tokio::time::sleep(TICK).await;
                    let now = Instant::now();
                    worst = worst.max(now - last);
                    last = now;
                    if let Some(go) = go.take() {
                        let _ = go.send(());
                    }
                }
                worst
            }
        });
        let ran = tokio::spawn(async move {
            gone.await.expect("the ticker wakes the step");
            let started = Instant::now();
            step().await;
            started.elapsed()
        })
        .await
        .expect("step task");
        done.store(true, Ordering::Relaxed);
        (ran, ticker.await.expect("ticker task"))
    })
}

/// The verdict. A step too short to outlast several ticks cannot show a
/// stall, which is a failure to judge, never a pass.
fn assert_driver_stayed_polled(step: &str, ran: Duration, worst: Duration) {
    eprintln!("{step}: ran {ran:?}, worst {TICK:?}-tick gap {worst:?}");
    assert!(
        ran >= TICK * 10,
        "{step} ran {ran:?}, too short to judge against a {TICK:?} ticker; grow its rows"
    );
    assert!(
        worst * 2 < ran,
        "{step} held the runtime: the {TICK:?} ticker waited {worst:?} while it ran {ran:?}"
    );
}

/// `files × per_file` symbols, each with one call edge to its neighbour.
fn rows(files: usize, per_file: usize) -> (Vec<ScipSymbolRecord>, Vec<ScipRefRecord>) {
    let mut symbols = Vec::with_capacity(files * per_file);
    let mut refs = Vec::with_capacity(files * per_file);
    for f in 0..files {
        let file_path = format!("src/m{f}.rs");
        for s in 0..per_file {
            let line = (s * 10) as i32;
            let (caller, callee) = (format!("f{s}"), format!("f{}", (s + 1) % per_file));
            symbols.push(ScipSymbolRecord {
                name: caller.clone(),
                qualified_name: format!("rust-analyzer cargo t 0.1.0 m{f}/{caller}()."),
                kind: "function".into(),
                file_path: file_path.clone(),
                line_start: line,
                line_end: line + 9,
                language: "rust".into(),
            });
            refs.push(ScipRefRecord {
                caller_qualified: format!("rust-analyzer cargo t 0.1.0 m{f}/{caller}()."),
                callee_qualified: format!("rust-analyzer cargo t 0.1.0 m{f}/{callee}()."),
                caller_symbol: caller,
                callee_symbol: callee,
                file_path: file_path.clone(),
                line: line + 1,
                ref_kind: "direct".into(),
                start_col: 4,
                end_line: line + 1,
                end_col: 12,
            });
        }
    }
    (symbols, refs)
}

/// A graph file on disk holding `rows(files, per_file)`, written on a
/// runtime of its own before any measurement starts.
fn written_graph(dir: &Path, files: usize, per_file: usize) -> PathBuf {
    let path = dir.join("source").join("scip_graph.db");
    let graph = ScipGraph::open_with_integrity(&path, "source").expect("open source graph");
    let (symbols, refs) = rows(files, per_file);
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("setup runtime")
        .block_on(graph.replace_all(symbols, refs))
        .expect("write source graph");
    path
}

/// The instrument's own failing input: an inline step must read as a held
/// runtime, or every test below passes without having seen anything.
#[test]
fn the_ticker_sees_an_inline_step_hold_the_runtime() {
    let (ran, worst) = beside_a_ticker(|| async { std::thread::sleep(TICK * 25) });
    eprintln!("inline sleep: ran {ran:?}, worst {TICK:?}-tick gap {worst:?}");
    assert!(
        worst * 2 >= ran,
        "an inline {ran:?} step left the {TICK:?} ticker free (worst gap {worst:?}): \
         the harness no longer reproduces the stall, so its passes mean nothing"
    );
}

#[test]
fn parse_leaves_the_runtime_driver_polled() {
    // Documents shaped like rust-analyzer's: each symbol defined once and
    // referenced from its neighbours' bodies.
    let (docs, per_doc) = (60, 400);
    let mut documents = Vec::with_capacity(docs);
    for d in 0..docs {
        let sym = |s: usize| format!("rust-analyzer cargo t 0.1.0 m{d}/f{}().", s % per_doc);
        let mut occurrences = Vec::new();
        let mut symbols = Vec::new();
        for s in 0..per_doc {
            let line = (s * 10) as i32;
            occurrences.push(Occurrence {
                range: vec![line, 3, 8],
                symbol: sym(s),
                symbol_roles: SymbolRole::DEFINITION,
                enclosing_range: vec![line, 0, line + 9, 1],
                ..Default::default()
            });
            for k in 1..=3 {
                occurrences.push(Occurrence {
                    range: vec![line + k as i32, 4, 12],
                    symbol: sym(s + k),
                    ..Default::default()
                });
            }
            symbols.push(SymbolInformation {
                symbol: sym(s).into_bytes(),
                display_name: format!("f{s}"),
                ..Default::default()
            });
        }
        documents.push(Document {
            language: "rust".into(),
            relative_path: format!("src/m{d}.rs"),
            occurrences,
            symbols,
        });
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("index.scip");
    let index = Index {
        documents,
        ..Default::default()
    };
    std::fs::write(&path, index.encode_to_vec()).expect("write index");

    let (ran, worst) = beside_a_ticker(move || async move {
        let (symbols, refs) = crate::scip_export::parse_scip_file_off_worker(path, "rust")
            .await
            .expect("parse");
        assert_eq!(symbols.len(), docs * per_doc, "every definition parsed");
        assert!(!refs.is_empty(), "the references resolved to callers");
    });
    assert_driver_stayed_polled("parse_scip_file", ran, worst);
}

#[test]
fn replace_all_leaves_the_runtime_driver_polled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let graph = Arc::new(ScipGraph::open(&dir.path().join("g.db"), "c").expect("open graph"));
    let (symbols, refs) = rows(100, 1000);
    let g = Arc::clone(&graph);
    let (ran, worst) = beside_a_ticker(move || async move {
        g.replace_all(symbols, refs).await.expect("replace_all");
    });
    assert_driver_stayed_polled("replace_all", ran, worst);
}

#[test]
fn integrity_checked_open_leaves_the_runtime_driver_polled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = written_graph(dir.path(), 100, 1000);
    let (ran, worst) = beside_a_ticker(move || async move {
        let graph = ScipGraph::open_with_integrity_off_worker(path, "source".into())
            .await
            .expect("a sound graph opens");
        assert_eq!(graph.symbol_count().await, 100 * 1000);
    });
    assert_driver_stayed_polled("open_with_integrity", ran, worst);
}

#[test]
fn merged_import_leaves_the_runtime_driver_polled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = written_graph(dir.path(), 100, 1000);
    let merged = Arc::new(ScipGraph::open_in_memory("merged").expect("merged graph"));
    let m = Arc::clone(&merged);
    let (ran, worst) = beside_a_ticker(move || async move {
        let (symbols, refs) = m.import_from_path(&path).await.expect("import");
        assert_eq!((symbols, refs), (100 * 1000, 100 * 1000));
    });
    assert_driver_stayed_polled("import_from_path", ran, worst);
}
