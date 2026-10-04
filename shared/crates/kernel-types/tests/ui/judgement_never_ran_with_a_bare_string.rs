// `Judgement::never_ran(subject, "unknown")` — a String is not a Reason, so
// the placeholder cannot slip past by skipping the checked constructor.
// nc-10-judgement red #3.
use kernel_types::Judgement;

fn main() {
    let _ = Judgement::never_ran("bench-baselines", "unknown");
}
