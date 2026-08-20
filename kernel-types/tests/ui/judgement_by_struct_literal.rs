// A Judgement made by struct literal, bypassing the four verdict doors.
// nc-10-judgement red #1: the fields are private, so "but I filled
// everything in" is not a path.
use kernel_types::{Judgement, Reason, Verdict};

fn main() {
    let _ = Judgement {
        subject: "drift".to_string(),
        verdict: Verdict::Failed,
        reason: Reason::literal("two claims contradicted"),
        as_of: None,
        horizon: None,
    };
}
