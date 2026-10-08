// SPDX-License-Identifier: AGPL-3.0-or-later
//! `plan_conversation`: a conversation's pin follows the conversation.

use super::*;

/// A conversation turn: the previous prompt, then `n` new tokens (the
/// reply and the next tool result, as far as the planner can tell).
fn next_turn(prev: &[LlamaToken], seed: i32, n: usize) -> Vec<LlamaToken> {
    let mut v = prev.to_vec();
    v.extend((0..n as i32).map(|i| LlamaToken(700_000 + seed * 10_000 + i)));
    v
}

/// The 2026-10-08 battery defect: `plan` pinned turn 1 and restored that
/// pin for the whole task while the prompt grew 23k → 39k. The
/// conversation planner's restored prefix must follow the conversation.
#[test]
fn a_conversation_pin_follows_the_conversation() {
    let mut cache = PrefixStateCache::new_for_test(200);
    let turn1 = toks(1, &(0..600).collect::<Vec<i32>>());
    let (plan, repin) = cache.plan_conversation(&turn1);
    assert_eq!(plan, PrefixPlan::Pass, "turn 1 has nothing to restore");
    let key = repin.expect("turn 1 is pinned whole after its prefill");
    cache.commit(key, turn1.clone(), cache.repin_path(key, &turn1));

    let turn2 = next_turn(&turn1, 2, 500);
    assert_eq!(
        cache.plan_conversation(&turn2),
        (
            PrefixPlan::Restore {
                key,
                prefix_len: turn1.len()
            },
            Some(key)
        ),
        "turn 2 restores all of turn 1 and re-pins itself"
    );
    cache.commit(key, turn2.clone(), cache.repin_path(key, &turn2));

    let turn3 = next_turn(&turn2, 3, 100);
    assert_eq!(
        cache.plan_conversation(&turn3),
        (
            PrefixPlan::Restore {
                key,
                prefix_len: turn2.len()
            },
            None
        ),
        "growth under min_pin restores without paying a save"
    );
    let turn4 = next_turn(&turn3, 4, 150);
    assert_eq!(
        cache.plan_conversation(&turn4),
        (
            PrefixPlan::Restore {
                key,
                prefix_len: turn2.len()
            },
            Some(key)
        ),
        "growth accumulated past min_pin since the entry re-pins"
    );
    assert_eq!(
        cache.entries.len(),
        1,
        "one family, one entry: each re-pin supersedes"
    );
}

/// The battery's second defect: four tasks share the agent's system head,
/// so the probe key collides across tasks. `plan` re-learned at the shared
/// head (1,099 tokens) and every later task restored only that. A new task
/// is a full prefill pinned whole, so its own next turn extends it.
#[test]
fn a_new_task_under_the_same_system_head_is_pinned_whole_not_at_the_head() {
    let mut cache = PrefixStateCache::new_for_test(200);
    let task_a = shape(300, 1, 400, 0);
    let (_, repin) = cache.plan_conversation(&task_a);
    let key = repin.unwrap();
    cache.commit(key, task_a.clone(), cache.repin_path(key, &task_a));

    let task_b = shape(300, 2, 900, 0);
    assert_eq!(
        PrefixStateCache::key(&task_b),
        key,
        "fixture: the tasks share the probe"
    );
    let mut sighting_planner = PrefixStateCache::new_for_test(200);
    sighting_planner.commit(
        key,
        task_a.clone(),
        sighting_planner.repin_path(key, &task_a),
    );
    assert_eq!(
        sighting_planner.plan(&task_b),
        PrefixPlan::Learn {
            key,
            pin_len: PROBE_TOKENS + 300
        },
        "fixture: the sighting planner pins only the shared head"
    );
    assert_eq!(
        cache.plan_conversation(&task_b),
        (PrefixPlan::Pass, Some(key))
    );
    cache.commit(key, task_b.clone(), cache.repin_path(key, &task_b));
    let b2 = next_turn(&task_b, 1, 50);
    assert_eq!(
        cache.plan_conversation(&b2).0,
        PrefixPlan::Restore {
            key,
            prefix_len: task_b.len()
        },
        "task B's second turn restores all of task B's first"
    );
}

/// A re-pin saves to a path of its own, so a pin the byte budget refuses
/// costs that save and nothing else: the entry it would have replaced
/// keeps serving, as the frozen pin did before re-pinning existed.
#[test]
fn a_refused_repin_leaves_the_previous_pin_serving() {
    let mut cache = PrefixStateCache::new_for_test(200);
    cache.max_bytes = 1_000;
    let turn1 = toks(1, &(0..600).collect::<Vec<i32>>());
    let key = cache.plan_conversation(&turn1).1.unwrap();
    assert!(cache.commit_sized(key, turn1.clone(), cache.repin_path(key, &turn1), 600));

    let turn2 = next_turn(&turn1, 2, 500);
    assert_ne!(cache.repin_path(key, &turn2), cache.repin_path(key, &turn1));
    assert!(
        !cache.commit_sized(key, turn2.clone(), cache.repin_path(key, &turn2), 1_100),
        "over the whole budget: refused"
    );
    let turn3 = next_turn(&turn2, 3, 50);
    assert_eq!(
        cache.plan_conversation(&turn3).0,
        PrefixPlan::Restore {
            key,
            prefix_len: turn1.len()
        },
        "the refused re-pin did not take turn 1's pin with it"
    );
}

#[test]
fn short_or_disabled_conversations_take_no_pin() {
    let mut cache = PrefixStateCache::new_for_test(200);
    let short = toks(1, &(0..100).collect::<Vec<i32>>());
    assert_eq!(cache.plan_conversation(&short), (PrefixPlan::Pass, None));
    cache.enabled = false;
    let long = toks(1, &(0..600).collect::<Vec<i32>>());
    assert_eq!(cache.plan_conversation(&long), (PrefixPlan::Pass, None));
}
