// A non-shareable Evidence in a peer-bound reply — nc-thesis declared illegal
// construction #5, proven at rung nc-11-answer.
//
// `PeerAnswer` is what the mesh reply path accepts, and its only door returns
// a Result. So the custody sweep cannot be skipped (there is no other
// constructor) and its refusal cannot be dropped on the floor (the Result is
// not a PeerAnswer). Which answers the sweep REFUSES is a runtime fact and is
// pinned by `answer::tests::a_peer_reply_refuses_estate_material_and_names_
// what_it_withheld`; that the sweep happens at all is this file.
use kernel_types::{Answer, Attribution, Custody, PeerAnswer, Reason, Server};

fn attribution() -> Attribution {
    Attribution {
        model: "qwen3-30b".to_string(),
        build: "b4321".to_string(),
        quantization: None,
        host: Server::Local,
    }
}

fn answer() -> Answer {
    Answer::abstained(
        "I could not find anything about this.",
        attribution(),
        Reason::literal("retrieval returned nothing above the relevance floor"),
    )
}

fn main() {
    // The refusal is not optional.
    let _cleared: PeerAnswer = PeerAnswer::bound_for_peer(answer(), Custody::PublicWeb);

    // Nor is the newtype a door around it.
    let _ = PeerAnswer(answer());
}
