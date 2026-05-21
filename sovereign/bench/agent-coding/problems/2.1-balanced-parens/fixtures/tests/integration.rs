use balanced_parens::is_balanced;

#[test]
fn empty_is_balanced() {
    assert!(is_balanced(""));
}

#[test]
fn single_pair_each_type() {
    assert!(is_balanced("()"));
    assert!(is_balanced("[]"));
    assert!(is_balanced("{}"));
}

#[test]
fn sequential_pairs() {
    assert!(is_balanced("()[]{}"));
    assert!(is_balanced("{}{}{}"));
}

#[test]
fn nested_same_type() {
    assert!(is_balanced("((()))"));
    assert!(is_balanced("[[[]]]"));
}

#[test]
fn nested_mixed_types() {
    assert!(is_balanced("([{}])"));
    assert!(is_balanced("{[()]}"));
    assert!(is_balanced("([]{})"));
}

#[test]
fn mismatch_returns_false() {
    assert!(!is_balanced("(]"));
    assert!(!is_balanced("[)"));
    assert!(!is_balanced("{)"));
    assert!(!is_balanced("[{)"));
}

#[test]
fn interleaved_not_nested() {
    assert!(!is_balanced("([)]"));
    assert!(!is_balanced("{(})"));
    assert!(!is_balanced("[{)]}"));
}

#[test]
fn unclosed_returns_false() {
    assert!(!is_balanced("("));
    assert!(!is_balanced("(("));
    assert!(!is_balanced("([{"));
    assert!(!is_balanced("([])("));
}

#[test]
fn extra_close_returns_false() {
    assert!(!is_balanced(")"));
    assert!(!is_balanced("())"));
    assert!(!is_balanced("()]"));
    assert!(!is_balanced("()[]}"));
}

#[test]
fn close_before_open_returns_false() {
    assert!(!is_balanced(")("));
    assert!(!is_balanced("][}"));
    assert!(!is_balanced("}{"));
}

#[test]
fn deep_nesting() {
    let mut s = String::new();
    for _ in 0..100 { s.push('('); }
    for _ in 0..100 { s.push(')'); }
    assert!(is_balanced(&s));
}

#[test]
fn long_balanced_sequence() {
    // ()()()...()(){}{}{}[][][]
    let mut s = String::new();
    for _ in 0..50 { s.push_str("()"); }
    for _ in 0..50 { s.push_str("{}"); }
    for _ in 0..50 { s.push_str("[]"); }
    assert!(is_balanced(&s));
}
