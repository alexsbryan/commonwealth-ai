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

/// What the slot does with a re-pin: save `tokens[..pin_len]` under its key.
fn pin(cache: &mut PrefixStateCache, repin: Option<Repin>, tokens: &[LlamaToken]) -> u64 {
    let Repin { key, pin_len } = repin.expect("the planner asked for a re-pin");
    let pinned = tokens[..pin_len].to_vec();
    let path = cache.repin_path(key, &pinned);
    cache.commit(key, pinned, path);
    key
}

/// The 2026-10-08 replay defect (arm B2): the next turn renders this
/// turn's generation prompt again, but not as the same tokens — Qwen3.8's
/// prompt ends `<think>\n` (198), a reply without reasoning renders back
/// `<think>\n\n</think>` (271). A whole-prompt pin missed every such turn
/// by one token and the turn was a full prefill.
#[test]
fn a_turn_that_retokenizes_the_generation_prompt_still_extends_the_pin() {
    let mut cache = PrefixStateCache::new_for_test(200);
    let turn1 = toks(1, &(0..600).collect::<Vec<i32>>());
    let (_, repin) = cache.plan_conversation(&turn1);
    let key = pin(&mut cache, repin, &turn1);

    let mut turn2 = turn1[..turn1.len() - 1].to_vec();
    turn2.push(LlamaToken(271));
    let turn2 = next_turn(&turn2, 2, 500);
    assert_eq!(
        cache.plan_conversation(&turn2).0,
        PrefixPlan::Restore {
            key,
            prefix_len: turn1.len() - CONVERSATION_PIN_TAIL
        },
        "the pin stops short of the generation prompt, so the re-tokenized tail is prefilled"
    );
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
    let key = pin(&mut cache, repin, &turn1);
    let tail = CONVERSATION_PIN_TAIL;

    let turn2 = next_turn(&turn1, 2, 500);
    let (plan, repin) = cache.plan_conversation(&turn2);
    assert_eq!(
        (plan, repin),
        (
            PrefixPlan::Restore {
                key,
                prefix_len: turn1.len() - tail
            },
            Some(Repin {
                key,
                pin_len: turn2.len() - tail
            })
        ),
        "turn 2 restores turn 1 short of its generation prompt and re-pins itself"
    );
    pin(&mut cache, repin, &turn2);

    let turn3 = next_turn(&turn2, 3, 100);
    assert_eq!(
        cache.plan_conversation(&turn3),
        (
            PrefixPlan::Restore {
                key,
                prefix_len: turn2.len() - tail
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
                prefix_len: turn2.len() - tail
            },
            Some(Repin {
                key,
                pin_len: turn4.len() - tail
            })
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
/// is a full prefill, pinned, so its own next turn extends it.
#[test]
fn a_new_task_under_the_same_system_head_is_pinned_whole_not_at_the_head() {
    let mut cache = PrefixStateCache::new_for_test(200);
    let task_a = shape(300, 1, 400, 0);
    let (_, repin) = cache.plan_conversation(&task_a);
    let key = pin(&mut cache, repin, &task_a);

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
    let (plan, repin) = cache.plan_conversation(&task_b);
    assert_eq!(plan, PrefixPlan::Pass);
    pin(&mut cache, repin, &task_b);
    let b2 = next_turn(&task_b, 1, 50);
    assert_eq!(
        cache.plan_conversation(&b2).0,
        PrefixPlan::Restore {
            key,
            prefix_len: task_b.len() - CONVERSATION_PIN_TAIL
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
    let key = cache.plan_conversation(&turn1).1.unwrap().key;
    let pin1 = turn1[..turn1.len() - CONVERSATION_PIN_TAIL].to_vec();
    assert!(cache.commit_sized(key, pin1.clone(), cache.repin_path(key, &pin1), 600));

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
            prefix_len: pin1.len()
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
