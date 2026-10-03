// `Reason("unknown".into())` — the newtype's field is private, so the
// placeholder refusal cannot be routed around. nc-10-judgement red #4.
use kernel_types::Reason;

fn main() {
    let _ = Reason("unknown".to_string());
}
