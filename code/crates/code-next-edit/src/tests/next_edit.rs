use super::*;

fn unit(before: &str, after: &str, left: &str, right: &str) -> HistoryUnit {
    HistoryUnit {
        before: before.into(),
        after: after.into(),
        left: left.into(),
        right: right.into(),
    }
}

fn console_rule() -> GuardedRule {
    expand_rule(&unit("log", "debug", "  console.", "(\"a\");")).unwrap()
}

#[test]
fn expand_absorbs_member_access_and_call_paren() {
    let r = console_rule();
    assert_eq!(r.find, "console.log(");
    assert_eq!(r.replace, "console.debug(");
    assert!(r.guard_left, "identifier start must be guarded");
    assert!(!r.guard_right, "call paren needs no guard");
}

#[test]
fn expand_guards_pure_identifier_rename_both_ends() {
    // inserted "Next" after "count": `count` → `countNext`
    let r = expand_rule(&unit("", "Next", "let count", " = count + 1;")).unwrap();
    assert_eq!(r.find, "count");
    assert_eq!(r.replace, "countNext");
    assert!(r.guard_left && r.guard_right);
    // the guards prevent the rule re-matching its own output
    assert_eq!(
        find_guarded_sites("let countNext = countNext + 1;", &r, 0),
        Vec::<usize>::new()
    );
}

#[test]
fn expand_declines_noops_and_multiline() {
    assert!(expand_rule(&unit("b", "b", "a", "c")).is_none());
    assert!(expand_rule(&unit("x", "\n", "a", "c")).is_none());
}

#[test]
fn induce_anchors_most_recent_and_window_counts() {
    let a = Some(console_rule());
    let b = expand_rule(&unit("foo", "bar", " ", " "));
    let got = induce(&[a.clone(), b, None, a.clone()]).unwrap();
    assert_eq!(got.0, console_rule());
    assert_eq!(got.1, 2, "support survives interleaved noise");
    assert!(induce(&[]).is_none());
    assert!(induce(&[None, None]).is_none());
}

#[test]
fn threshold_table() {
    let specific = console_rule(); // find len 12
    let short = GuardedRule {
        find: "ab".into(),
        replace: "ax".into(),
        guard_left: true,
        guard_right: true,
    };
    let single = GuardedRule {
        find: "a".into(),
        replace: "x".into(),
        guard_left: true,
        guard_right: true,
    };
    let at_bar = GuardedRule {
        find: "abcde".into(), // exactly MIN_RULE_CHARS
        replace: "abcdx".into(),
        guard_left: true,
        guard_right: true,
    };
    let under_bar = GuardedRule {
        find: "abcd".into(), // one short
        replace: "abcx".into(),
        guard_left: true,
        guard_right: true,
    };
    assert!(!should_fire(&specific, 5, 0), "never without a site");
    assert!(should_fire(&specific, 2, 1));
    assert!(!should_fire(&specific, 1, 10), "one edit never fires");
    // The minimum `find` is 5 trimmed chars, RAISED from 2 on the
    // measured frontier (see `should_fire`): at 2 the config is
    // Pareto-dominated — 138 strict-useful against 52 wrong fires,
    // where 5 gets 141 against 39. Pin both sides of the bar so the
    // constant cannot drift without a test saying so.
    assert!(should_fire(&at_bar, 2, 10), "a rule at the bar fires");
    assert!(
        !should_fire(&under_bar, 2, 10),
        "one char under the bar does not"
    );
    assert!(
        !should_fire(&short, 2, 10),
        "a two-char rule is the literal trap"
    );
    assert!(
        !should_fire(&single, 2, 10),
        "one-char rule is never specific enough"
    );
    assert!(
        !should_fire(&single, 9, 10),
        "and no amount of support rescues it"
    );
    // The support tier stayed collapsed across the 2→5 move: more
    // support still does not buy a shorter rule.
    assert_eq!(should_fire(&short, 2, 10), should_fire(&short, 7, 10));
    assert_eq!(
        should_fire(&under_bar, 2, 10),
        should_fire(&under_bar, 9, 10)
    );
}

/// Two insertions of one block, at siblings differing only in the
/// value on the anchor line.
fn insert_pair() -> Vec<HistoryUnit> {
    vec![
        unit(
            "",
            "  retries: 3,\n",
            "a = {\n  port: 1,\n  os: linux,\n",
            "};\n",
        ),
        unit(
            "",
            "  retries: 3,\n",
            "b = {\n  port: 2,\n  os: linux,\n",
            "};\n",
        ),
    ]
}

const INSERT_ANCHOR: &str = "  os: linux,\n";

#[test]
fn insertion_induces_an_anchored_rule() {
    let r = induce_insertion(&insert_pair()).expect("pair induces");
    // The anchor is the common line-aligned tail — the shared line
    // that precedes both insertion points. The differing `port:`
    // lines above it are not part of what the contexts agree on.
    assert_eq!(r.find, INSERT_ANCHOR);
    assert_eq!(r.replace, format!("{}{}", r.find, "  retries: 3,\n"));
    assert!(
        !r.guard_left && !r.guard_right,
        "line anchors have no word boundaries"
    );
}

#[test]
fn insertion_refuses_what_it_cannot_know() {
    // Per-site-varying payload: which variant belongs at an unseen
    // site is a guess, and this lane does not guess.
    let varying = vec![
        unit("", "  retries: 3,\n", "a = {\n  port: 1,\n", "};\n"),
        unit("", "  retries: 9,\n", "b = {\n  port: 2,\n", "};\n"),
    ];
    assert!(
        induce_insertion(&varying).is_none(),
        "varying payload is param_insert's shape"
    );

    // Not a pure insertion — that is a rewrite, and `expand_rule`
    // already owns it.
    let rewrite = vec![
        unit("log", "debug", "console.", "(1);"),
        unit("log", "debug", "console.", "(2);"),
    ];
    assert!(induce_insertion(&rewrite).is_none());

    // Nothing in common to anchor on.
    let unanchored = vec![
        unit("", "x\n", "totally\n", ""),
        unit("", "x\n", "different\n", ""),
    ];
    assert!(
        induce_insertion(&unanchored).is_none(),
        "an anchor must be specific"
    );

    assert!(induce_insertion(&[]).is_none());
}

#[test]
fn insertion_is_a_fallback_and_never_preempts_the_literal_lane() {
    // Literal lane declines (no rule from a bare insertion), so the
    // anchor lane fires on the remaining sibling.
    let text = "a = {\n  port: 1,\n  os: linux,\n  retries: 3,\n};\n\
                b = {\n  port: 2,\n  os: linux,\n  retries: 3,\n};\n\
                c = {\n  port: 3,\n  os: linux,\n};\n";
    let p = predict(&insert_pair(), text, 0);
    assert!(
        p.reason_silent.is_none(),
        "anchor lane fires where the literal lane cannot"
    );
    assert_eq!(p.edits.len(), 1, "only the site still missing the block");
    assert_eq!(
        p.edits[0].new_text,
        format!("{INSERT_ANCHOR}  retries: 3,\n")
    );

    // A site that already carries the payload is excluded, so a
    // fully-propagated document stays silent rather than stacking.
    let done = "a = {\n  port: 1,\n  os: linux,\n  retries: 3,\n};\n";
    let q = predict(&insert_pair(), done, 0);
    assert!(
        q.edits.is_empty(),
        "already-applied sites are not re-proposed"
    );

    // And where the literal lane DOES fire it keeps the floor: the
    // more specific claim wins.
    let owns = vec![
        unit("log", "debug", "console.", "(1);"),
        unit("log", "debug", "console.", "(2);"),
    ];
    let r = predict(&owns, "console.log(3);\n", 0);
    assert_eq!(r.rule.as_ref().unwrap().find, "console.log(");
}

#[test]
fn deletion_repeats_only_multi_line_blocks() {
    let block = "  legacy: true,\n  deprecated: yes,\n";
    let pair = vec![
        unit(block, "", "a = {\n", "};\n"),
        unit(block, "", "b = {\n", "};\n"),
    ];
    let r = induce_deletion(&pair).expect("multi-line repeat induces");
    assert_eq!(r.find, block);
    assert!(r.replace.is_empty(), "a deletion is an empty replace");

    // ONE line is refused: measured at 13 wrong fires across the
    // bank's negatives, because a short literal recurs innocently.
    let one = vec![
        unit("  legacy: true,\n", "", "a = {\n", "};\n"),
        unit("  legacy: true,\n", "", "b = {\n", "};\n"),
    ];
    assert!(
        induce_deletion(&one).is_none(),
        "single-line deletion repeat is not safe"
    );

    // Differing blocks are a bulk-removal run, not a repeat — the
    // shape this lane explicitly does not attempt.
    let differing = vec![
        unit("  a: 1,\n  b: 2,\n", "", "x = {\n", "};\n"),
        unit("  c: 3,\n  d: 4,\n", "", "y = {\n", "};\n"),
    ];
    assert!(induce_deletion(&differing).is_none());

    // Not a deletion at all.
    assert!(induce_deletion(&insert_pair()).is_none());
}

fn bare_rule(find: &str, replace: &str) -> GuardedRule {
    GuardedRule {
        find: find.into(),
        replace: replace.into(),
        guard_left: false,
        guard_right: false,
    }
}

#[test]
fn sites_order_after_cursor_then_wrap() {
    let text = "log a\nlog b\nlog c\n";
    assert_eq!(
        find_guarded_sites(text, &bare_rule("log", "dbg"), 3),
        vec![6, 12, 0]
    );
    // non-overlapping stepping
    assert_eq!(
        find_guarded_sites("aaaa", &bare_rule("aa", "bb"), 0),
        vec![0, 2]
    );
}

#[test]
fn insertion_rule_skips_already_applied_sites() {
    // `fetch(` → `await fetch(`: replace CONTAINS find, so edited
    // lines still match textually — re-proposing them would stack
    // the insertion. Found by gym/next-edit case a11 (2026-07-30).
    let text = "a = await fetch(u1)\nb = await fetch(u2)\nc = fetch(u3)\nd = fetch(u4)\n";
    let u = unit("", "await ", "b = ", "fetch(u2)");
    let p = predict(&[u.clone(), u], text, 0);
    assert!(p.reason_silent.is_none());
    assert_eq!(p.edits.len(), 2, "only the two un-edited sites");
    for e in &p.edits {
        assert_eq!(&text[e.start..e.end], "fetch(");
        assert!(
            !text[..e.start].ends_with("await "),
            "already-edited site re-proposed"
        );
    }
    // Deletion-shaped rules (find contains replace) are unaffected.
    let d = expand_rule(&unit(".unwrap()", "", "res", ";")).unwrap();
    assert_eq!(find_guarded_sites("res;\nres.unwrap();\n", &d, 0), vec![5]);
}

/// The already-applied probe used to re-derive `find`'s positions
/// inside `replace` at EVERY site, so a self-similar rule over a
/// large file was quadratic: a crafted 512 KiB request measured
/// 23 s of blocking CPU on one tokio worker, needing no model lane
/// and no opt-in to reach. Rules this shape are declined, and the
/// site scan is bounded regardless.
#[test]
fn pathological_rules_cannot_wedge_the_scan() {
    let text = "(".repeat(512 * 1024);
    let degenerate = bare_rule("(", &format!("{}{}", "(".repeat(2047), ")"));
    let t0 = std::time::Instant::now();
    assert!(
        find_guarded_sites(&text, &degenerate, 0).is_empty(),
        "a replacement containing its own target 2047 times is not an edit pattern"
    );
    // Same file, a rule that is merely common rather than
    // degenerate: bounded work, still a usable queue.
    let common = bare_rule("(", "(x");
    let sites = find_guarded_sites(&text, &common, 0);
    assert!(!sites.is_empty() && sites.len() <= MAX_SITE_SCAN);
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(2),
        "bounded work, not seconds of blocking CPU (took {:?})",
        t0.elapsed()
    );
    // An oversized induced rule is refused outright.
    assert!(find_guarded_sites("abc", &bare_rule(&"a".repeat(600), "b"), 0).is_empty());
}

#[test]
fn predict_end_to_end_console_case() {
    // Two sites already edited (units), three remain; cursor after
    // the second edit.
    let text =
        "console.debug(1);\nconsole.debug(2);\nconsole.log(3);\nconsole.log(4);\nconsole.log(5);\n";
    let u = unit("log", "debug", "console.", "(2);");
    let p = predict(&[u.clone(), u], text, 30);
    assert!(p.reason_silent.is_none());
    assert_eq!(p.support, 2);
    assert_eq!(p.sites, 3);
    assert_eq!(p.edits.len(), 3);
    let first = &p.edits[0];
    assert_eq!(&text[first.start..first.end], "console.log(");
    assert_eq!(first.new_text, "console.debug(");
}

#[test]
fn predict_silences_carry_reasons() {
    assert_eq!(predict(&[], "x", 0).reason_silent, Some("no_rule"));
    let u = unit("log", "debug", "console.", "(1);");
    // one support only
    assert_eq!(
        predict(&[u.clone()], "console.log(9);", 0).reason_silent,
        Some("below_threshold")
    );
    // rule supported but no sites left
    let p = predict(&[u.clone(), u], "console.debug(9);", 0);
    assert_eq!(p.reason_silent, Some("no_sites"));
    assert_eq!(p.support, 2);
}

#[test]
fn utf16_mapping_round_trips_past_astral_chars() {
    // "a💡b" — '💡' is 4 bytes / 2 UTF-16 units.
    let text = "a💡bc";
    assert_eq!(utf16_to_byte(text, 0), 0);
    assert_eq!(utf16_to_byte(text, 1), 1);
    assert_eq!(utf16_to_byte(text, 3), 5); // past the emoji
    assert_eq!(utf16_to_byte(text, 99), text.len());
    assert_eq!(bytes_to_utf16(text, &[5, 0, 1, 6, 7]), vec![3, 0, 1, 4, 5]);
}
