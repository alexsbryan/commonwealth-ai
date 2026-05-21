use group_anagrams::group_anagrams;

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

#[test]
fn empty_input() {
    let out = group_anagrams(vec![]);
    assert!(out.is_empty());
}

#[test]
fn single_string() {
    let out = group_anagrams(s(&["a"]));
    assert_eq!(out, vec![s(&["a"])]);
}

#[test]
fn no_anagrams_each_singleton() {
    let out = group_anagrams(s(&["abc", "def", "ghi"]));
    assert_eq!(out, vec![s(&["abc"]), s(&["def"]), s(&["ghi"])]);
}

#[test]
fn all_anagrams_one_group() {
    let out = group_anagrams(s(&["abc", "bca", "cab"]));
    assert_eq!(out, vec![s(&["abc", "bca", "cab"])]);
}

#[test]
fn classic_three_groups_preserve_order() {
    let out = group_anagrams(s(&["eat", "tea", "tan", "ate", "nat", "bat"]));
    // Group 1: starts with "eat" (idx 0). Members: eat, tea, ate (idxs 0, 1, 3).
    // Group 2: starts with "tan" (idx 2). Members: tan, nat (idxs 2, 4).
    // Group 3: starts with "bat" (idx 5). Members: bat.
    let expected = vec![
        s(&["eat", "tea", "ate"]),
        s(&["tan", "nat"]),
        s(&["bat"]),
    ];
    assert_eq!(out, expected);
}

#[test]
fn empty_strings_grouped_together() {
    let out = group_anagrams(s(&["", "", "abc"]));
    assert_eq!(out, vec![s(&["", ""]), s(&["abc"])]);
}

#[test]
fn case_sensitive_within_ascii() {
    // All lowercase per spec — these are the same letters, different orderings.
    let out = group_anagrams(s(&["listen", "silent", "enlist"]));
    assert_eq!(out, vec![s(&["listen", "silent", "enlist"])]);
}

#[test]
fn single_char_anagrams() {
    let out = group_anagrams(s(&["a", "a", "b", "a"]));
    assert_eq!(out, vec![s(&["a", "a", "a"]), s(&["b"])]);
}

#[test]
fn long_words() {
    let out = group_anagrams(s(&["theclassroom", "theroomclass", "differentword"]));
    // theclassroom <-> theroomclass: same letters.
    // differentword: different.
    assert_eq!(
        out,
        vec![
            s(&["theclassroom", "theroomclass"]),
            s(&["differentword"]),
        ]
    );
}

#[test]
fn group_ordering_follows_first_appearance() {
    // "z" first → first group. "a" second → second group. "az" third → third.
    let out = group_anagrams(s(&["z", "a", "az", "za", "z"]));
    let expected = vec![
        s(&["z", "z"]),
        s(&["a"]),
        s(&["az", "za"]),
    ];
    assert_eq!(out, expected);
}

#[test]
fn duplicate_strings_keep_all_occurrences() {
    let out = group_anagrams(s(&["abc", "abc", "abc"]));
    assert_eq!(out, vec![s(&["abc", "abc", "abc"])]);
}

#[test]
fn varied_lengths() {
    let out = group_anagrams(s(&["a", "ab", "ba", "abc", "cab"]));
    assert_eq!(
        out,
        vec![
            s(&["a"]),
            s(&["ab", "ba"]),
            s(&["abc", "cab"]),
        ]
    );
}
