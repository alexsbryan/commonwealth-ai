// "No surface returns a pre-release draft", as a type rather than a review
// comment: a Draft's text cannot be read out of it at all. The only exits are
// `release` and `release_ungated`, each of which requires saying what is
// known about the draft. Rung nc-11-answer.
use kernel_types::Draft;

fn main() {
    let draft = Draft::composed("Whales are mammals.", Vec::new());

    let _ = &draft.text;
    let _ = draft.text();
    println!("{draft}");
}
