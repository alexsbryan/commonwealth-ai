// A failed Judgement with no reason. nc-10-judgement red #2 — the whole
// point of the type: you cannot report a failure you cannot explain.
use kernel_types::Judgement;

fn main() {
    let _ = Judgement::failed("drift");
}
