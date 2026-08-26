// `Deserialize` is a constructor. Deriving it on Answer would put a public
// door back on the type — `from_str::<Answer>` would mint one with any
// judgement the caller cared to type — so it is deliberately not derived.
// Same rule, same reasoning as `corpus_engine::Evidence`. Rung nc-11-answer.
use kernel_types::Answer;

fn main() {
    let _: Answer = serde_json::from_str("{}").unwrap();
}
