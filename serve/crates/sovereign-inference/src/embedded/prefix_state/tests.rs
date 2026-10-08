// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn toks(head: i32, body: &[i32]) -> Vec<LlamaToken> {
    // First PROBE_TOKENS identical per `head` (the family
    // fingerprint), then the body.
    let mut v: Vec<LlamaToken> = (0..PROBE_TOKENS as i32)
        .map(|i| LlamaToken(head * 10_000 + i))
        .collect();
    v.extend(body.iter().map(|&t| LlamaToken(t)));
    v
}

/// Two request shapes that share a family fingerprint AND a common
/// core, then diverge — the shape that thrashed on Flash-Next
/// (2026-08-26). `own_len` is stable text only THIS shape declares.
fn shape(shared_core: usize, own: i32, own_len: usize, tail_len: usize) -> Vec<LlamaToken> {
    let core: Vec<i32> = (0..shared_core as i32).collect();
    let mut v = toks(1, &core);
    v.extend((0..own_len as i32).map(|i| LlamaToken(500_000 + own * 10_000 + i)));
    v.extend((0..tail_len as i32).map(|i| LlamaToken(900_000 + own * 1_000 + i)));
    v
}

/// A sibling call: the identical declared `window`, then a tail only
/// this call carries (a different claim under the same evidence).
fn sibling(window: &[LlamaToken], tail_seed: i32, tail_len: usize) -> Vec<LlamaToken> {
    let mut v = window.to_vec();
    v.extend((0..tail_len as i32).map(|i| LlamaToken(950_000 + tail_seed * 1_000 + i)));
    v
}

/// Two directed windows that share the 48-token probe but diverge
/// inside the declared prefix are two families: each learns its own
/// FULL declared prefix, and a sibling of either restores all of it —
/// never a common-prefix compromise. This is the 2026-09-01 gate defect
/// in miniature: turn N+1's judges open like turn N's (same scaffold,
/// same first chunk) and diverge at the second chunk.
///
/// Watched red on the probe-keyed code: turn 2 planned
/// `Learn { pin_len: 248 }` — the 200-token core it shares with turn 1,
/// not its own 300.
/// ISSUE #57, found live on 2026-09-02 by instrumenting the undirected
/// path's silent `Pass` returns. The DeepQuery synthesis call sends the
/// SAME 9,891-token prompt every turn, and the cache refused it a pin
/// every turn: the learn guard was `lcp < tokens.len()`, so the one case
/// where two sightings share everything fell through to `Pass` and the
/// family never formed. The gate's judges, four inches away in the same
/// turn, were restoring 4,881 tokens in 45 ms off the directed path,
/// which had always backed off by `PIN_TAIL_MARGIN` instead of refusing.
///
/// Watched red on the old code: the second sighting returned `Pass`, and
/// so did the third, and the tenth.
#[test]
fn two_identical_sightings_learn_a_pin_rather_than_refusing_one() {
    let mut cache = PrefixStateCache::new_for_test(64);
    let prompt = toks(1, &(0..600).collect::<Vec<i32>>());
    assert_eq!(
        lcp_len(&prompt, &prompt),
        prompt.len(),
        "fixture: the two sightings are byte-identical"
    );

    assert!(
        matches!(cache.plan(&prompt), PrefixPlan::Pass),
        "first sighting has nothing to compare against"
    );

    let want = prompt.len() - PIN_TAIL_MARGIN;
    match cache.plan(&prompt) {
        PrefixPlan::Learn { pin_len, .. } => assert_eq!(
            pin_len, want,
            "an identical repeat pins all but the decodable tail"
        ),
        other => panic!("identical repeat must learn, got {other:?}"),
    }
}

/// The pin the case above learns has to be RESTORABLE, or the fix just
/// moves the full prefill one turn later. `Restore` needs a strict
/// prefix with a non-empty tail, which is what the margin buys.
#[test]
fn the_pin_learned_from_an_identical_repeat_is_then_restorable() {
    let mut cache = PrefixStateCache::new_for_test(64);
    let prompt = toks(1, &(0..600).collect::<Vec<i32>>());
    cache.plan(&prompt);
    let PrefixPlan::Learn { key, pin_len } = cache.plan(&prompt) else {
        panic!("second sighting must learn");
    };
    cache.commit(
        key,
        prompt[..pin_len].to_vec(),
        std::path::PathBuf::from("/tmp/x"),
    );

    match cache.plan(&prompt) {
        PrefixPlan::Restore { prefix_len, .. } => assert_eq!(
            prefix_len, pin_len,
            "the third sighting restores the whole pin"
        ),
        other => panic!("expected Restore, got {other:?}"),
    }
}

#[test]
fn directed_windows_sharing_the_probe_get_their_own_entries() {
    let mut cache = PrefixStateCache::new_for_test(64);
    let turn1 = shape(200, 1, 100, 40);
    let turn2 = shape(200, 2, 100, 40);
    let pin = PROBE_TOKENS + 200 + 100;
    assert_eq!(
        lcp_len(&turn1, &turn2),
        PROBE_TOKENS + 200,
        "fixture: the windows share the probe and diverge inside the declared prefix"
    );

    let PrefixPlan::Learn { key: key1, pin_len } = cache.plan_directed(&turn1, pin) else {
        panic!("turn 1 learns on first sight")
    };
    assert_eq!(pin_len, pin);
    cache.commit(key1, turn1[..pin].to_vec(), cache.state_path(key1));

    let plan = cache.plan_directed(&turn2, pin);
    let PrefixPlan::Learn { key: key2, pin_len } = plan else {
        panic!("turn 2 must learn its own window, got {plan:?}")
    };
    assert_eq!(
        pin_len, pin,
        "turn 2 learns its FULL declared prefix, not the prefix it shares with turn 1"
    );
    assert_ne!(key2, key1, "a different window is a different family key");
    cache.commit(key2, turn2[..pin].to_vec(), cache.state_path(key2));
    assert_eq!(cache.entries.len(), 2, "two windows, two entries");

    // A sibling of EITHER turn restores that turn's whole declared prefix.
    assert_eq!(
        cache.plan_directed(&sibling(&turn1[..pin], 1, 55), pin),
        PrefixPlan::Restore {
            key: key1,
            prefix_len: pin
        }
    );
    assert_eq!(
        cache.plan_directed(&sibling(&turn2[..pin], 2, 55), pin),
        PrefixPlan::Restore {
            key: key2,
            prefix_len: pin
        }
    );
}

/// Siblings declaring the identical window share ONE entry — the second
/// call restores `prefix_len == directed_pin` — and that entry is not
/// shrunk by a nested SHORTER window that shares its probe (the
/// 2026-08-24 shape: an audit window that grew between passes, so the
/// small window and the grown one are both declared for a while).
///
/// Watched red on the probe-keyed code: the nested window came back
/// under the long window's key, then replaced its entry with the
/// 248-token compromise.
#[test]
fn directed_siblings_with_one_declared_window_share_one_entry() {
    let mut cache = PrefixStateCache::new_for_test(64);
    let long = shape(200, 1, 100, 40);
    let pin_long = PROBE_TOKENS + 300;
    let PrefixPlan::Learn { key, .. } = cache.plan_directed(&long, pin_long) else {
        panic!("first sighting learns")
    };
    cache.commit(key, long[..pin_long].to_vec(), cache.state_path(key));

    for seed in 2..5 {
        assert_eq!(
            cache.plan_directed(
                &sibling(&long[..pin_long], seed, 30 + seed as usize),
                pin_long
            ),
            PrefixPlan::Restore {
                key,
                prefix_len: pin_long
            },
            "sibling {seed} restores the whole declared window"
        );
    }
    assert_eq!(
        cache.entries.len(),
        1,
        "identical declared windows share one entry"
    );

    // A nested shorter window: the same bytes up to the shared core,
    // declared as the whole prefix.
    let pin_short = PROBE_TOKENS + 200;
    let short = sibling(&long[..pin_short], 9, 40);
    let plan = cache.plan_directed(&short, pin_short);
    let PrefixPlan::Learn {
        key: key_short,
        pin_len,
    } = plan
    else {
        panic!("a shorter window is its own family, got {plan:?}")
    };
    assert_eq!(pin_len, pin_short);
    assert_ne!(key_short, key, "a nested window is a different family key");
    cache.commit(
        key_short,
        short[..pin_short].to_vec(),
        cache.state_path(key_short),
    );

    assert_eq!(
        cache.plan_directed(&sibling(&long[..pin_long], 7, 33), pin_long),
        PrefixPlan::Restore {
            key,
            prefix_len: pin_long
        },
        "the long window was not shrunk to the nested one"
    );
    assert_eq!(cache.entries.len(), 2);
}

/// Two windows sharing the probe must not evict each other, and neither
/// may be shortened to what they share.
///
/// Rewritten from `two_shapes_sharing_a_family_converge_instead_of_thrashing`
/// (2026-08-27). That test asserted the COMPROMISE: once A and B had each
/// learned under the one probe key, A re-pinned at their common prefix
/// and both shapes restored `pin_a` forever — B paying its own 100-token
/// tail on every call. The compromise stopped the eviction thrash it was
/// built for (Flash-Next, [3998, 4612, 3998, 4612]) and then broke the
/// grounding gate: every later TURN on one corpus shares the probe with
/// the previous turn, so the gate pinned the ~500-1300 tokens two turns
/// share and re-prefilled ~12K per judge (2026-09-01). Under content keys
/// the two windows are two entries. The property that survives is "once
/// both are pinned, nothing re-learns"; the one that changed is "each
/// restores ITS OWN full declared prefix".
///
/// Watched red on the probe-keyed code: round 0, A restored 248 tokens
/// (the compromise) instead of its declared 348.
#[test]
fn alternating_directed_windows_never_relearn_once_both_are_pinned() {
    let mut cache = PrefixStateCache::new_for_test(64);
    let a = shape(200, 1, 100, 40);
    let b = shape(200, 2, 150, 40);
    let pin_a = PROBE_TOKENS + 200 + 100;
    let pin_b = PROBE_TOKENS + 200 + 150;

    let PrefixPlan::Learn {
        key: key_a,
        pin_len,
    } = cache.plan_directed(&a, pin_a)
    else {
        panic!("A learns first")
    };
    cache.commit(key_a, a[..pin_len].to_vec(), cache.state_path(key_a));
    let PrefixPlan::Learn {
        key: key_b,
        pin_len,
    } = cache.plan_directed(&b, pin_b)
    else {
        panic!("B learns its own window")
    };
    cache.commit(key_b, b[..pin_len].to_vec(), cache.state_path(key_b));

    for round in 0..4 {
        let plan_a = cache.plan_directed(&a, pin_a);
        assert!(
            !matches!(plan_a, PrefixPlan::Learn { .. }),
            "round {round}: A re-learned — the eviction thrash is back"
        );
        assert_eq!(
            plan_a,
            PrefixPlan::Restore {
                key: key_a,
                prefix_len: pin_a
            },
            "round {round}: A must restore its whole declared prefix"
        );
        let plan_b = cache.plan_directed(&b, pin_b);
        assert!(
            !matches!(plan_b, PrefixPlan::Learn { .. }),
            "round {round}: B re-learned — the eviction thrash is back"
        );
        assert_eq!(
            plan_b,
            PrefixPlan::Restore {
                key: key_b,
                prefix_len: pin_b
            },
            "round {round}: B must restore its whole declared prefix"
        );
    }
    assert_eq!(cache.entries.len(), 2, "two windows, two entries, no churn");
}

/// Distinct directed windows accumulate only up to the LRU cap.
///
/// Rewritten from `a_drifted_family_still_replaces_its_pin`
/// (2026-08-27), which asserted the compromise's escape hatch: evidence
/// sharing only the probe (`lcp < min_pin`) learned under the SAME key
/// and replaced the entry in place. Under content keys nothing is ever
/// replaced in place — each drifted window is its own key — so what has
/// to be proven instead is the closure: the entry count is bounded by
/// `MAX_ENTRIES` (and the byte budget, `byte_budget_evicts_lru_until_under_cap`)
/// and the oldest window is the one retired.
///
/// Watched red on the probe-keyed code: all eight windows came back
/// under one key.
#[test]
fn distinct_directed_windows_are_bounded_by_the_lru() {
    let mut cache = PrefixStateCache::new_for_test(64);
    let pin = PROBE_TOKENS + 200;
    let mut keys = Vec::new();
    for turn in 0..(MAX_ENTRIES as i32 + 2) {
        // Same 48-token opening, different evidence from token 48 on.
        let mut w = toks(1, &[]);
        w.extend((0..300i32).map(|i| LlamaToken(770_000 + turn * 1_000 + i)));
        let plan = cache.plan_directed(&w, pin);
        let PrefixPlan::Learn { key, pin_len } = plan else {
            panic!("turn {turn}: a new window learns immediately, got {plan:?}")
        };
        assert_eq!(
            pin_len, pin,
            "turn {turn}: learns the whole declared prefix"
        );
        cache.commit(key, w[..pin].to_vec(), cache.state_path(key));
        keys.push(key);
    }
    let distinct: std::collections::HashSet<u64> = keys.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        keys.len(),
        "every drifted window is its own family key"
    );
    assert_eq!(cache.entries.len(), MAX_ENTRIES, "bounded by the entry cap");
    assert!(
        !cache.entries.contains_key(&keys[0]),
        "the oldest window was retired"
    );
    assert!(cache.entries.contains_key(keys.last().unwrap()));
}

/// A family: shared stable core of `core_len` tokens, then a
/// per-request variable tail.
fn family_member(core_len: usize, tail_seed: i32, tail_len: usize) -> Vec<LlamaToken> {
    let core: Vec<i32> = (0..core_len as i32).collect();
    let mut v = toks(1, &core);
    v.extend((0..tail_len as i32).map(|i| LlamaToken(900_000 + tail_seed * 1_000 + i)));
    v
}

#[test]
fn learns_boundary_from_two_sightings_then_restores() {
    let mut cache = PrefixStateCache::new_for_test(64);
    let a = family_member(200, 1, 40);
    let b = family_member(200, 2, 55);

    // First sighting: pass (nothing to compare against).
    assert_eq!(cache.plan(&a), PrefixPlan::Pass);

    // Second sighting: learn at the exact divergence boundary.
    let plan = cache.plan(&b);
    let PrefixPlan::Learn { key, pin_len } = plan else {
        panic!("expected Learn, got {plan:?}");
    };
    assert_eq!(
        pin_len,
        PROBE_TOKENS + 200,
        "pin lands at the divergence point"
    );

    // Commit the pin; a third member restores.
    cache.commit(key, b[..pin_len].to_vec(), cache.state_path(key));
    let c = family_member(200, 3, 70);
    assert_eq!(
        cache.plan(&c),
        PrefixPlan::Restore {
            key,
            prefix_len: pin_len
        }
    );
}

#[test]
fn short_prompts_and_short_overlap_pass() {
    let mut cache = PrefixStateCache::new_for_test(64);
    // Too short to consider at all.
    let tiny = toks(2, &[1, 2, 3]);
    assert_eq!(cache.plan(&tiny), PrefixPlan::Pass);

    // Same fingerprint but the shared prefix is under min_pin:
    // never pins.
    let a = family_member(10, 1, 400);
    let b = family_member(10, 2, 400);
    assert_eq!(cache.plan(&a), PrefixPlan::Pass);
    assert_eq!(cache.plan(&b), PrefixPlan::Pass);
}

#[test]
fn identical_full_prompts_never_restore_with_empty_tail() {
    // The tail carries the fresh logits, so an exact-match prompt must
    // never plan a ZERO-TAIL restore. It used to get there by refusing
    // the pin outright (`lcp < tokens.len()`), which also refused the
    // ~9.9k-token DeepQuery synthesis prompt on every turn forever
    // (issue #57). It now pins all but `PIN_TAIL_MARGIN` instead — the
    // invariant this test is named for, without the full prefill.
    let mut cache = PrefixStateCache::new_for_test(64);
    let a = family_member(200, 1, 40);
    cache.plan(&a);
    let PrefixPlan::Learn { key, pin_len } = cache.plan(&a.clone()) else {
        panic!("an identical repeat is the strongest evidence for a pin");
    };
    assert!(
        pin_len < a.len(),
        "the pin must leave a tail: pin_len={pin_len} len={}",
        a.len()
    );
    cache.commit(key, a[..pin_len].to_vec(), cache.state_path(key));
    // And the restore it enables still leaves that tail to decode.
    assert_eq!(
        cache.plan(&a),
        PrefixPlan::Restore {
            key,
            prefix_len: pin_len
        }
    );
    assert!(a.len() > pin_len, "restore keeps a non-empty tail");
}

#[test]
fn drift_relearns_at_shorter_boundary() {
    let mut cache = PrefixStateCache::new_for_test(64);
    let a = family_member(200, 1, 40);
    let b = family_member(200, 2, 55);
    cache.plan(&a);
    let PrefixPlan::Learn { key, pin_len } = cache.plan(&b) else {
        panic!("expected Learn");
    };
    cache.commit(key, b[..pin_len].to_vec(), cache.state_path(key));

    // The family drifts: only the first 120 core tokens survive
    // (e.g. daily anchor rotated mid-core). Next sighting re-learns
    // at the shorter boundary instead of restoring stale state.
    let drifted = family_member(120, 9, 60);
    let plan = cache.plan(&drifted);
    assert_eq!(
        plan,
        PrefixPlan::Learn {
            key,
            pin_len: PROBE_TOKENS + 120
        }
    );
}

#[test]
fn lru_evicts_oldest_family() {
    let mut cache = PrefixStateCache::new_for_test(64);
    let mut keys = Vec::new();
    for fam in 0..(MAX_ENTRIES as i32 + 2) {
        let core: Vec<i32> = (0..200).collect();
        let mut a = toks(fam + 1, &core);
        a.extend([LlamaToken(1)]);
        let mut b = toks(fam + 1, &core);
        b.extend([LlamaToken(2)]);
        cache.plan(&a);
        if let PrefixPlan::Learn { key, pin_len } = cache.plan(&b) {
            cache.commit(key, b[..pin_len].to_vec(), cache.state_path(key));
            keys.push(key);
        } else {
            panic!("expected Learn for family {fam}");
        }
    }
    assert!(cache.entries.len() <= MAX_ENTRIES);
    // The first-committed families were evicted.
    assert!(!cache.entries.contains_key(&keys[0]));
    assert!(cache.entries.contains_key(keys.last().unwrap()));
}

#[test]
fn byte_budget_evicts_lru_until_under_cap() {
    let mut cache = PrefixStateCache::new_for_test(64);
    cache.max_bytes = 1_000;
    let mut keys = Vec::new();
    // Three families of 400 bytes each: the third commit must evict
    // the first (1200 > 1000 → evict LRU front → 800 ≤ 1000).
    for fam in 0..3i32 {
        let core: Vec<i32> = (0..200).collect();
        let mut a = toks(fam + 1, &core);
        a.extend([LlamaToken(1)]);
        let key = PrefixStateCache::key(&a);
        cache.commit_sized(
            key,
            a[..PROBE_TOKENS + 100].to_vec(),
            cache.state_path(key),
            400,
        );
        keys.push(key);
    }
    assert!(!cache.entries.contains_key(&keys[0]), "oldest evicted");
    assert!(cache.entries.contains_key(&keys[1]));
    assert!(cache.entries.contains_key(&keys[2]));
    assert!(cache.entries.values().map(|e| e.bytes).sum::<u64>() <= 1_000);
}

#[test]
fn oversized_pin_is_refused_not_admitted() {
    let mut cache = PrefixStateCache::new_for_test(64);
    cache.max_bytes = 1_000;
    let core: Vec<i32> = (0..200).collect();
    let small = toks(1, &core);
    let k_small = PrefixStateCache::key(&small);
    cache.commit_sized(
        k_small,
        small[..PROBE_TOKENS + 100].to_vec(),
        cache.state_path(k_small),
        400,
    );

    // A pin bigger than the WHOLE budget: refused, and the resident
    // small pin survives (admitting would have flushed everything).
    let big = toks(2, &core);
    let k_big = PrefixStateCache::key(&big);
    cache.commit_sized(
        k_big,
        big[..PROBE_TOKENS + 100].to_vec(),
        cache.state_path(k_big),
        5_000,
    );
    assert!(!cache.entries.contains_key(&k_big));
    assert!(cache.entries.contains_key(&k_small));
}

#[test]
fn stale_dir_decision_only_deletes_dead_foreign_pids() {
    let alive = |p: u32| p == 111 || p == 222;
    // Foreign + dead → stale.
    assert!(dir_is_stale("999-qwen35moe", 111, alive));
    // Own pid → never stale, even if the probe lies.
    assert!(!dir_is_stale("111-qwen35moe", 111, |_| false));
    // Foreign but alive → keep (another daemon / compute child).
    assert!(!dir_is_stale("222-qwen35moe", 111, alive));
    // Unparseable name → keep (only delete what we can attribute).
    assert!(!dir_is_stale("not-a-pid-dir", 111, alive));
}

#[test]
fn directed_learns_on_first_sighting_then_restores() {
    let mut cache = PrefixStateCache::new_for_test(64);
    let a = family_member(200, 1, 40);
    let pin = PROBE_TOKENS + 180; // directed boundary inside the shared core

    // First sighting: directed plan learns IMMEDIATELY (no second
    // sighting needed — the whole point).
    let plan = cache.plan_directed(&a, pin);
    let PrefixPlan::Learn { key, pin_len } = plan else {
        panic!("expected immediate Learn, got {plan:?}");
    };
    assert_eq!(pin_len, pin);
    cache.commit(key, a[..pin].to_vec(), cache.state_path(key));

    // Sibling with a different tail restores at the entry boundary.
    let b = family_member(200, 2, 55);
    assert_eq!(
        cache.plan_directed(&b, pin),
        PrefixPlan::Restore {
            key,
            prefix_len: pin
        }
    );
}

/// Drifted evidence learns immediately — no invalidate → Pass → Learn
/// sighting dance — and under its OWN key.
///
/// Until 2026-09-01 this asserted the Learn came back under the same
/// key as the stale entry (probe keying: same 48-token opening, one
/// family, entry replaced in place). A drifted window is now a different
/// family; the stale entry is the LRU's to retire
/// (`distinct_directed_windows_are_bounded_by_the_lru`), not this call's.
///
/// Watched red on the probe-keyed code: `key_c == key`.
#[test]
fn directed_drifted_evidence_learns_immediately_under_its_own_key() {
    let mut cache = PrefixStateCache::new_for_test(64);
    let a = family_member(200, 1, 40);
    let pin_a = PROBE_TOKENS + 180;
    let PrefixPlan::Learn { key, .. } = cache.plan_directed(&a, pin_a) else {
        panic!("expected Learn");
    };
    cache.commit(key, a[..pin_a].to_vec(), cache.state_path(key));

    // Next turn: same family fingerprint, new evidence (core drifts
    // right after the probe).
    let mut c = toks(1, &(500..700).collect::<Vec<i32>>());
    c.extend([LlamaToken(1), LlamaToken(2), LlamaToken(3)]);
    let pin_c = PROBE_TOKENS + 150;
    let plan = cache.plan_directed(&c, pin_c);
    let PrefixPlan::Learn {
        key: key_c,
        pin_len,
    } = plan
    else {
        panic!("drifted evidence learns on first sight, got {plan:?}")
    };
    assert_eq!(pin_len, pin_c, "at the NEW declared boundary");
    assert_ne!(key_c, key, "a drifted window is its own family key");
    assert!(
        cache.entries.contains_key(&key),
        "the previous window is not invalidated by a drift"
    );
}

/// **A grown declared window is its own entry, learned at full length.**
///
/// The 2026-08-24 deep-research task-69 flight logged 124 restores,
/// every one `restored_tokens=1064` against a declared window that had
/// grown past 3,300 — mean `suffix_tokens=2289` re-prefilled, 283,874
/// tokens total, roughly 35 minutes of a 39.5-minute audit leg: the
/// short pin strict-prefix-matched, so it was restored and the growth
/// re-prefilled on every call.
///
/// Until 2026-09-01 this test (`directed_relearns_a_pin_shorter_than_the_declaration`)
/// asserted the cure as a RE-LEARN under the same probe key — the short
/// entry replaced by the grown one, with a `min_pin` margin so a small
/// growth would not churn. Under content keys there is no margin and no
/// replacement: the grown window is a different key, learned once at its
/// full length, and the short window's entry stays until the LRU retires
/// it. The cost that matters — every sibling of the grown window
/// restoring the whole declared prefix — is asserted at the end.
///
/// Watched red on the probe-keyed code: `key_grown == key`.
#[test]
fn a_grown_declared_window_is_its_own_entry_learned_at_full_length() {
    let mut cache = PrefixStateCache::new_for_test(64);
    let short = family_member(200, 1, 40);
    let pin_short = PROBE_TOKENS + 100;
    let PrefixPlan::Learn { key, .. } = cache.plan_directed(&short, pin_short) else {
        panic!("expected Learn on first sighting");
    };
    cache.commit(key, short[..pin_short].to_vec(), cache.state_path(key));

    // The same opening, now declaring a MUCH longer stable prefix — the
    // shape of an evidence window that grew between passes.
    let mut grown = short[..pin_short].to_vec();
    grown.extend((0..900i32).map(|i| LlamaToken(700_000 + i)));
    let directed = pin_short + 800;
    assert!(grown.len() > directed);
    let plan = cache.plan_directed(&grown, directed);
    let PrefixPlan::Learn {
        key: key_grown,
        pin_len,
    } = plan
    else {
        panic!("the grown window learns, got {plan:?}")
    };
    assert_eq!(pin_len, directed, "learned at the full declared length");
    assert_ne!(key_grown, key, "a grown window is a different family key");
    assert!(
        cache.entries.contains_key(&key),
        "the short window's entry is the LRU's to retire, not this call's"
    );
    cache.commit(
        key_grown,
        grown[..directed].to_vec(),
        cache.state_path(key_grown),
    );

    assert_eq!(
        cache.plan_directed(&sibling(&grown[..directed], 3, 60), directed),
        PrefixPlan::Restore {
            key: key_grown,
            prefix_len: directed
        },
        "every sibling of the grown window restores the whole declared prefix"
    );
}

/// A declared window that differs from a pinned one by a few tokens is
/// its own entry, and never disturbs the pinned one.
///
/// Until 2026-09-01 this test (`directed_keeps_restoring_when_the_pin_is_close_or_longer`)
/// asserted two riders on probe keying: a declaration 10 tokens longer
/// than the pin RESTORED the pin (the re-learn margin, so a trivial
/// shortfall would not churn), and a declaration 50 tokens shorter
/// restored the pin too ("an entry longer than the directive restores at
/// its own length"). Under content keys `pin + 10` and `pin - 50` name
/// different windows: each learns once under its own key, and the churn
/// the margin guarded against cannot occur because no entry is ever
/// replaced by another window. The real consumer never jitters — the gate
/// asserts one byte-identical boundary across siblings
/// (`the_gate_shares_one_prefix_family`, judge.rs).
///
/// Watched red on the probe-keyed code: `pin + 10` planned a Restore.
#[test]
fn a_declared_window_that_differs_by_a_few_tokens_is_its_own_entry() {
    let mut cache = PrefixStateCache::new_for_test(64);
    let base = family_member(200, 1, 40);
    let pin = PROBE_TOKENS + 200;
    let PrefixPlan::Learn { key, .. } = cache.plan_directed(&base, pin) else {
        panic!("expected Learn");
    };
    cache.commit(key, base[..pin].to_vec(), cache.state_path(key));

    let mut longer = base[..pin].to_vec();
    longer.extend((0..600i32).map(|i| LlamaToken(800_000 + i)));

    for declared in [pin + 10, pin - 50] {
        let plan = cache.plan_directed(&longer, declared);
        let PrefixPlan::Learn { key: k, pin_len } = plan else {
            panic!("a declaration of {declared} is its own window, got {plan:?}")
        };
        assert_eq!(pin_len, declared, "learned at exactly the declared length");
        assert_ne!(k, key, "a different window is a different family key");
        cache.commit(k, longer[..declared].to_vec(), cache.state_path(k));
    }
    assert_eq!(cache.entries.len(), 3, "three windows, three entries");
    assert_eq!(
        cache.plan_directed(&sibling(&base[..pin], 4, 30), pin),
        PrefixPlan::Restore {
            key,
            prefix_len: pin
        },
        "the original window's pin was not disturbed"
    );
}

#[test]
fn directed_out_of_range_falls_back_to_sighting_plan() {
    let mut cache = PrefixStateCache::new_for_test(64);
    let a = family_member(200, 1, 40);
    // Pin below min_pin and pin past the end both degrade to the
    // sighting-based plan. Each uses its OWN family, so what is asserted
    // is the fallback itself — a first sighting Passes — rather than the
    // undirected path's second-sighting rule, which these two calls used
    // to exercise by accident (both passed `a`).
    let b = toks(7, &(0..300).collect::<Vec<i32>>());
    assert_ne!(
        PrefixStateCache::key(&a),
        PrefixStateCache::key(&b),
        "fixture: the two probes must be different families"
    );
    assert_eq!(cache.plan_directed(&a, 8), PrefixPlan::Pass);
    assert_eq!(cache.plan_directed(&b, b.len() + 5), PrefixPlan::Pass);
}

#[test]
fn disabled_via_env_shape_passes_everything() {
    let mut cache = PrefixStateCache::new_for_test(64);
    cache.enabled = false;
    let a = family_member(200, 1, 40);
    let b = family_member(200, 2, 55);
    assert_eq!(cache.plan(&a), PrefixPlan::Pass);
    assert_eq!(cache.plan(&b), PrefixPlan::Pass);
}

// The conversation planner's tests live in a sibling so this file stays out
// of the 800-1200 approach band (ARCH §3.1); they reuse the helpers above.
#[path = "conversation_tests.rs"]
mod conversation_tests;
