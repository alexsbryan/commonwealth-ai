// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Mutex;

use sovereign_contracts::request_priority::{asked, carry};

use super::*;

/// One request's step at a fake of the embed context: the gate's step, then
/// the stage's first-in-first-out lock held for `turn`, then its name in the
/// order the stage served it.
async fn at_the_stage(
    gate: Arc<PriorityGate>,
    stage: Arc<tokio::sync::Mutex<()>>,
    served: Arc<Mutex<Vec<String>>>,
    name: String,
    turn: Duration,
) {
    let _step = gate.step("fake_embed").await;
    let _context = stage.lock().await;
    tokio::time::sleep(turn).await;
    served.lock().unwrap().push(name);
}

/// Where the writer's step was served among five background steps that
/// reached the stage before it, each asked with `background` (or, for the
/// control, with nothing).
async fn writers_place(background: Option<RequestPriority>) -> usize {
    let gate = PriorityGate::new(Duration::from_secs(10));
    let stage = Arc::new(tokio::sync::Mutex::new(()));
    let served = Arc::new(Mutex::new(Vec::new()));
    let turn = Duration::from_millis(40);
    let mut work = Vec::new();
    for i in 0..5 {
        let f = at_the_stage(
            Arc::clone(&gate),
            Arc::clone(&stage),
            Arc::clone(&served),
            format!("check {i}"),
            turn,
        );
        work.push(tokio::spawn(asked(background, f)));
    }
    // Every check has reached the gate or the stage before the writer types.
    tokio::time::sleep(Duration::from_millis(10)).await;
    let f = at_the_stage(gate, stage, Arc::clone(&served), "writer".into(), turn);
    work.push(tokio::spawn(asked(Some(RequestPriority::Interactive), f)));
    for w in work {
        w.await.unwrap();
    }
    let served = served.lock().unwrap();
    assert_eq!(served.len(), 6, "{served:?}");
    served.iter().position(|n| n == "writer").unwrap()
}

/// The order's failing input (orders/v1-pull PV3): an interactive request
/// queued behind background work at the stage PV1 named. Asked as
/// background, at most the one check already at the stage goes before the
/// writer; unasked, all five do.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_writers_step_is_served_ahead_of_background_work_queued_at_the_stage() {
    let place = writers_place(Some(RequestPriority::Background)).await;
    assert!(place <= 1, "the writer was served in place {place}");
    let unasked = writers_place(None).await;
    assert_eq!(
        unasked, 5,
        "the control: unasked, the writer waits its turn"
    );
}

/// A background step goes ahead at the ceiling while an interactive step is
/// still in flight, so a run of interactive work cannot starve it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_background_step_goes_ahead_at_the_hold_ceiling() {
    let gate = PriorityGate::new(Duration::from_millis(100));
    let writer = asked(Some(RequestPriority::Interactive), gate.step("held_open")).await;
    let started = Instant::now();
    let _step = asked(Some(RequestPriority::Background), gate.step("check")).await;
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(90) && waited < Duration::from_secs(1),
        "{waited:?}"
    );
    drop(writer);
}

/// A step inside the one its background request holds starts at once: one
/// request at a time is the rule, and it must not wait on itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_step_inside_its_requests_held_step_does_not_wait_on_it() {
    let gate = PriorityGate::new(Duration::from_secs(10));
    let started = Instant::now();
    asked(Some(RequestPriority::Background), async {
        let _outer = gate.step("completion").await;
        let _inner = gate.step("embed").await;
    })
    .await;
    assert!(started.elapsed() < Duration::from_secs(1));
}

/// A request that named no priority neither waits nor is waited for.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unasked_step_passes_while_interactive_work_is_in_flight() {
    let gate = PriorityGate::new(Duration::from_secs(10));
    let _writer = asked(Some(RequestPriority::Interactive), gate.step("held_open")).await;
    let started = Instant::now();
    let _step = gate.step("unasked").await;
    assert!(started.elapsed() < Duration::from_secs(1));
}

/// How long the writer waited for a fake accelerator while a long step read
/// eight parts of 50 ms through it, asked as `background` (or unasked, the
/// control), and the ranges that step read.
async fn writers_wait_beside_a_long_read(
    background: Option<RequestPriority>,
) -> (Duration, Vec<std::ops::Range<usize>>) {
    let gate = PriorityGate::new(Duration::from_secs(10));
    let accelerator = Arc::new(Mutex::new(()));
    let part_time = Duration::from_millis(50);
    let read = {
        let (gate, accelerator) = (Arc::clone(&gate), Arc::clone(&accelerator));
        asked(background, async move {
            tokio::task::spawn_blocking(carry(move || {
                let mut ranges = Vec::new();
                gate.in_parts("fake_prefill", 8, 1, |r| {
                    let _held = accelerator.lock().unwrap();
                    std::thread::sleep(part_time * r.len() as u32);
                    ranges.push(r);
                    Ok::<_, ()>(())
                })
                .unwrap();
                ranges
            }))
            .await
            .unwrap()
        })
    };
    let read = tokio::spawn(read);
    // The writer types while the second part is being read.
    tokio::time::sleep(Duration::from_millis(75)).await;
    let waited = asked(Some(RequestPriority::Interactive), async {
        let _step = gate.step("fake_embed").await;
        let arrived = Instant::now();
        tokio::task::spawn_blocking(move || {
            let _held = accelerator.lock().unwrap();
            arrived.elapsed()
        })
        .await
        .unwrap()
    })
    .await;
    (waited, read.await.unwrap())
}

/// PV3b's failing input: a writer's step arriving while one long background
/// step reads through the stage waits behind at most one part of it; unasked,
/// it waits the whole remaining read. Either way the step reads every part,
/// in order.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_writer_waits_behind_one_part_of_a_long_background_read() {
    let (waited, ranges) = writers_wait_beside_a_long_read(Some(RequestPriority::Background)).await;
    assert!(
        waited < Duration::from_millis(160),
        "the writer waited {waited:?}"
    );
    assert_eq!(ranges, (0..8).map(|i| i..i + 1).collect::<Vec<_>>());
    let (unasked, ranges) = writers_wait_beside_a_long_read(None).await;
    assert!(
        unasked > Duration::from_millis(250),
        "the control: unasked, the writer waits out the read ({unasked:?})"
    );
    assert_eq!(ranges, vec![0..8]);
}
