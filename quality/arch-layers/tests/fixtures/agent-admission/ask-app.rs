pub fn answer() -> String {
    formatter::render(42)
}

#[test]
fn task_returns_expected_answer() {
    assert_eq!(answer(), "answer: 42");
}
