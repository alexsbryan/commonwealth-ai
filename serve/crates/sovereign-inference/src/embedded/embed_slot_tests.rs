// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use sovereign_contracts::request_priority::{asked, carry};
use std::time::Duration;

#[test]
fn only_an_interactive_request_embeds_on_the_writers_context() {
    assert!(on_writers_context(Some(RequestPriority::Interactive)));
    assert!(!on_writers_context(Some(RequestPriority::Background)));
    assert!(!on_writers_context(None));
}

/// How long `priority`'s two-input batch takes while the test holds the GPU
/// context for `held`, and its vectors.
async fn embed_beside_a_held_gpu_context(
    slot: &Arc<EmbedSlot>,
    priority: Option<RequestPriority>,
    held: Duration,
) -> (Duration, Vec<Vec<f32>>) {
    let holder = Arc::clone(slot);
    let (taken, is_taken) = std::sync::mpsc::channel();
    let hold = std::thread::spawn(move || {
        let _lock = holder.contexts[0].blocking_lock();
        taken.send(()).unwrap();
        std::thread::sleep(held);
    });
    is_taken.recv().unwrap();
    let embedder = Arc::clone(slot);
    let inputs = vec![
        "the division of labour raises the productive powers of labour".to_string(),
        "a probe sentence".to_string(),
    ];
    let start = Instant::now();
    let vectors = asked(priority, async move {
        tokio::task::spawn_blocking(carry(move || {
            EmbedSlot::run_embed_batch_sync(&embedder, &inputs)
        }))
        .await
        .unwrap()
        .unwrap()
    })
    .await;
    let took = start.elapsed();
    hold.join().unwrap();
    (took, vectors)
}

/// PV3d's failing input: a writer's embed while the GPU context is held (as
/// a check's batch holds it) is answered from the writer's CPU context; an
/// unasked one, the control, waits out the hold. Their vectors agree.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "manual: needs EMBED_GGUF pointing at a local embedding gguf, and a GPU"]
async fn a_writers_embed_is_not_held_by_the_gpu_context() {
    let path = PathBuf::from(std::env::var("EMBED_GGUF").expect("EMBED_GGUF"));
    let backend = Arc::new(LlamaBackend::init().unwrap());
    let slot = Arc::new(EmbedSlot::load(&backend, &path, 999, None, Writer::OnCpu).unwrap());
    assert!(slot.writer.is_some(), "a slot on the GPU keeps a writer's context");

    let held = Duration::from_millis(1500);
    let (asked_took, on_cpu) =
        embed_beside_a_held_gpu_context(&slot, Some(RequestPriority::Interactive), held).await;
    let (unasked_took, on_gpu) = embed_beside_a_held_gpu_context(&slot, None, held).await;
    eprintln!("interactive {asked_took:?}, unasked {unasked_took:?}");
    assert!(asked_took < Duration::from_millis(500), "{asked_took:?}");
    assert!(unasked_took >= Duration::from_millis(1400), "{unasked_took:?}");
    for (a, b) in on_cpu.iter().zip(&on_gpu) {
        let cos: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        eprintln!("cos {cos}");
        assert!(cos > 0.998, "{cos}");
    }
}
