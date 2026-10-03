// A Judgement is not a mutable accumulator: its verdict cannot be
// reassigned after the fact. nc-10-judgement red #5.
use kernel_types::{Judgement, Reason, Verdict};

fn main() {
    let mut j = Judgement::failed("drift", Reason::literal("two claims contradicted"));
    j.verdict = Verdict::Passed;
}
