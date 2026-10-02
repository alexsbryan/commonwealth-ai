// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn file(rel: &str, text: &str) -> NormFile {
    NormFile {
        rel: rel.to_string(),
        lines: normalize(text),
    }
}

/// A 10-statement body, enough for three overlapping windows.
fn body(tag: &str) -> String {
    (0..10)
        .map(|i| format!("    let {tag}{i} = compute_{tag}({i});\n"))
        .collect()
}

#[test]
fn comments_blanks_uses_and_lone_brackets_are_not_compared() {
    let src = "use a::b;\npub(crate) use c::{\n    d,\n};\n// note\n\nfn f() {\n    g();\n}\n";
    let lines: Vec<String> = normalize(src).into_iter().map(|(_, l)| l).collect();
    assert_eq!(lines, vec!["fn f() {", "g();"]);
}

#[test]
fn normalized_lines_keep_their_source_line_numbers() {
    let src = "// c\n\nfn f() {\n    g();\n}\n";
    assert_eq!(
        normalize(src),
        vec![(3, "fn f() {".to_string()), (4, "g();".to_string())]
    );
}

/// The whole `#[cfg(test)]` item is test mass, and production code after it
/// is still read — unlike size-gate's to-end-of-file approximation.
#[test]
fn a_cfg_test_item_is_skipped_to_its_closing_brace() {
    let src = "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn t() { let s = \"}\"; }\n}\nfn b() {}\n#[cfg(test)]\nuse x::y;\nfn c() {}\n";
    let lines: Vec<String> = normalize(src).into_iter().map(|(_, l)| l).collect();
    assert_eq!(lines, vec!["fn a() {}", "fn b() {}", "fn c() {}"]);
}

#[test]
fn test_paths_are_not_production() {
    assert!(is_test_path("a/src/tests/x.rs"));
    assert!(is_test_path("a/src/foo_tests.rs"));
    assert!(is_test_path("a/src/tests.rs"));
    assert!(!is_test_path("a/src/contests.rs"));
}

#[test]
fn braces_ignore_strings_chars_and_comments() {
    assert_eq!(braces("if x { \"{\" } // }"), vec![1, -1]);
    assert_eq!(braces("let c = '{'; fn f<'a>() {"), vec![1]);
}

/// The planted copy: one body in two files is a family, and every line of
/// both copies counts.
#[test]
fn a_body_in_two_files_is_a_clone() {
    let files = vec![
        file("a/src/x.rs", &format!("fn x() {{\n{}}}\n", body("v"))),
        file("b/src/y.rs", &format!("fn y() {{\n{}}}\n", body("v"))),
    ];
    let (total, families) = census(&files);
    assert_eq!(total, 20);
    let key = "a/src/x.rs <-> b/src/y.rs";
    assert_eq!(family_lines(&families[key]), 20);
    assert_eq!(
        render(&files, &families[key]),
        "a/src/x.rs:2-11  <->  b/src/y.rs:2-11"
    );
}

/// A repeat inside one file is not a twin of anything.
#[test]
fn a_repeat_inside_one_file_is_not_a_clone() {
    let text = format!("fn x() {{\n{}}}\nfn y() {{\n{}}}\n", body("v"), body("v"));
    let (total, families) = census(&[file("a/src/x.rs", &text)]);
    assert_eq!((total, families.len()), (0, 0));
}

/// Seven shared lines are under the window.
#[test]
fn a_run_shorter_than_the_window_is_not_a_clone() {
    let short: String = body("v")
        .lines()
        .take(WINDOW - 1)
        .collect::<Vec<_>>()
        .join("\n");
    let files = vec![
        file("a/src/x.rs", &format!("{short}\nlet p = 1;\n")),
        file("b/src/y.rs", &format!("{short}\nlet q = 2;\n")),
    ];
    assert_eq!(census(&files).0, 0);
}

/// The negative control: moving a file re-keys its family and leaves the
/// ratcheted total where it was.
#[test]
fn a_moved_file_keeps_the_total() {
    let before = vec![
        file("a/src/x.rs", &body("v")),
        file("b/src/y.rs", &body("v")),
    ];
    let after = vec![
        file("a/src/x.rs", &body("v")),
        file("c/src/moved.rs", &body("v")),
    ];
    assert_eq!(census(&before).0, census(&after).0);
}

#[test]
fn the_snapshot_carries_the_total_first() {
    let files = vec![
        file("a/src/x.rs", &body("v")),
        file("b/src/y.rs", &body("v")),
    ];
    let (total, families) = census(&files);
    let map = snapshot(total, &families);
    assert_eq!(map.keys().next().map(String::as_str), Some(TOTAL_KEY));
    assert_eq!(map[TOTAL_KEY], 20);
}
