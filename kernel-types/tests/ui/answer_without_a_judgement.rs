// An Answer with no Judgement — nc-thesis declared illegal construction #1,
// proven at rung nc-11-answer.
//
// Two doors are tried and both are shut. The fields are private, so "but I
// filled in everything else" is not a path; and the release door takes the
// judgements as a POSITIONAL argument, so omitting them is an arity error
// rather than a defaulted empty verdict.
use kernel_types::{Answer, Attribution, Draft, Server};

fn attribution() -> Attribution {
    Attribution {
        model: "qwen3-30b".to_string(),
        build: "b4321".to_string(),
        quantization: None,
        host: Server::Local,
    }
}

fn main() {
    let _ = Answer {
        text: "Whales are mammals.".to_string(),
        citations: Vec::new(),
        provenance: attribution(),
    };

    let _ = Draft::composed("Whales are mammals.", Vec::new()).release(attribution());
}
