// No public constructor of any other spelling. `Citation::pointing_into` is
// the one door and it takes a `Seal`; there is no `new`, no `from_text`, no
// `Default`. Beyond nc-thesis's declared list — see `answer_reds.rs`.
use kernel_types::Citation;

fn main() {
    let _ = Citation::new("the whale is a fish");
}
