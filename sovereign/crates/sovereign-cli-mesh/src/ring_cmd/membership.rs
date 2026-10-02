// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn ring membership` — who is in, per the record.
//!
//! The answer is the daemon's own fold, read whole ([`rail_membership`]):
//! the seed beside the one membership walk. The render's one job is the
//! contrast between the halves — a key standing through an `Admit` the seed
//! never names, and a key whose `Admit` was voided (never in at all), are
//! both one glance here and both grep-prose elsewhere.

use commonwealth_rail::short_id;

use super::rail_membership;

/// The dispatch target for `svrn ring membership <namespace> [--json]`.
pub(super) async fn run(args: &[String]) -> i32 {
    let Some(namespace) = args
        .first()
        .filter(|a| !a.starts_with("--"))
        .map(String::as_str)
    else {
        eprintln!("ring membership: which ring? `svrn ring membership <namespace>`");
        return 2;
    };
    let v = match rail_membership(namespace).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ring membership: {e}");
            return 1;
        }
    };
    if args.iter().any(|a| a == "--json") {
        println!("{v}");
        return 0;
    }
    println!("{namespace} — who is in, per the record");
    for line in rows(&v) {
        println!("{line}");
    }
    0
}

/// One binding per line, sorted by person then key: the name, the key, and
/// whether it counts NOW. `via act` marks standing the seed does not carry —
/// the leak-undo's subject — and `out` marks a binding whose standing a
/// `Remove` cut (the name stays; only standing moves, leg 4).
///
/// Pure so the shape is testable without a daemon, like `op_line`: the two
/// marks are the facts a reader concludes from, and dropping either changes
/// the conclusion without changing the output's success.
pub(super) fn rows(v: &serde_json::Value) -> Vec<String> {
    let empty = serde_json::Map::new();
    let bindings = v
        .get("membership")
        .and_then(|m| m.get("bindings"))
        .and_then(|b| b.as_object())
        .unwrap_or(&empty);
    let empty_arr = Vec::new();
    let standing = v
        .get("membership")
        .and_then(|m| m.get("standing"))
        .and_then(|s| s.as_array())
        .unwrap_or(&empty_arr);
    let seed = v
        .get("roster")
        .and_then(|r| r.get("members"))
        .and_then(|m| m.as_object())
        .unwrap_or(&empty);
    let seed_keys: std::collections::BTreeSet<&str> = seed
        .values()
        .filter_map(|keys| keys.as_array())
        .flat_map(|keys| keys.iter().filter_map(|k| k.as_str()))
        .collect();

    let mut rows: Vec<(String, String, bool, bool)> = bindings
        .iter()
        .map(|(key, person)| {
            let stands = standing.iter().any(|s| s.as_str() == Some(key.as_str()));
            let via_act = stands && !seed_keys.contains(key.as_str());
            (
                person.as_str().unwrap_or("?").to_string(),
                key.clone(),
                stands,
                via_act,
            )
        })
        .collect();
    rows.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let width = rows.iter().map(|r| r.0.len()).max().unwrap_or(3).max(3);
    rows.into_iter()
        .map(|(person, key, stands, via_act)| {
            format!(
                "  {person:<width$}  {}  {}{}",
                short_id(&key),
                if stands { "in" } else { "out" },
                if via_act { "  via act" } else { "" }
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(
        bindings: serde_json::Value,
        standing: &[&str],
        seed_keys: &[&str],
    ) -> serde_json::Value {
        serde_json::json!({
            "membership": {
                "bindings": bindings,
                "standing": standing,
            },
            "roster": { "members": { "Ada": seed_keys } },
        })
    }

    /// The two marks a reader concludes from: a seed member is `in` bare, an
    /// act-admitted key is `in via act` — the leak-undo's subject — and a
    /// `Remove`-cut key keeps its name and reads `out`.
    #[test]
    fn a_row_names_the_person_the_key_and_the_two_marks() {
        let v = answer(
            serde_json::json!({
                "aa11": "Ada",
                "bb22": "Sam",
                "cc33": "Rowan",
            }),
            &["aa11", "bb22"],
            &["aa11"],
        );
        assert_eq!(
            rows(&v),
            vec![
                "  Ada    aa11  in".to_string(),
                "  Rowan  cc33  out".to_string(),
                "  Sam    bb22  in  via act".to_string(),
            ]
        );
    }

    /// No membership half (an older daemon, or a shape this build cannot
    /// read) renders NOTHING — not an empty ring, which would read as
    /// "nobody is in" and be a confident claim about a shape it did not
    /// understand (ARCH §18.3).
    #[test]
    fn a_shape_with_no_walk_renders_no_rows() {
        assert!(rows(&serde_json::json!({ "roster": {} })).is_empty());
    }
}
