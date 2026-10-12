// SPDX-License-Identifier: AGPL-3.0-or-later
/// The provider build, by source: the text before the first test module.
/// This module spells every monitor it looks for, so scanning the whole
/// file would pass with all four production calls deleted.
fn production_src() -> &'static str {
    let src = include_str!("../assembly.rs");
    let end = src
        .find("#[cfg(test)]")
        .expect("assembly.rs has a test module");
    &src[..end]
}

/// Every idle monitor the embedded engine exposes. Adding a slot with
/// an idle monitor means adding it here, which is the point: the list
/// is the checklist.
const MONITORS: [&str; 4] = [
    "start_idle_monitor(",
    "start_extras_idle_monitor(",
    "start_fast_idle_monitor(",
    "start_embed_idle_monitor(",
];

/// A call, not a mention: comments explaining a monitor must not
/// satisfy the gate. A gate that would pass with the ability it
/// guards fully removed is not a gate.
fn calls(src: &str, monitor: &str) -> bool {
    src.lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with("//") && !t.starts_with("///") && !t.starts_with('*')
        })
        .any(|l| l.contains(monitor))
}

#[test]
fn the_cold_start_path_arms_every_idle_monitor() {
    let missing: Vec<&str> = MONITORS
        .iter()
        .copied()
        .filter(|m| !calls(production_src(), m))
        .collect();
    assert!(
        missing.is_empty(),
        "cold-start provider build never calls: {missing:?} — those slots stay \
         resident for the life of the daemon"
    );
}

#[test]
fn each_monitor_reads_its_own_configured_window() {
    // One knob per monitor, read from config — not a literal, and not
    // another slot's knob. A monitor wired to the wrong field looks
    // armed and sweeps on someone else's schedule.
    for (monitor, knob) in [
        ("start_idle_monitor(", "primary_idle_secs"),
        ("start_extras_idle_monitor(", "extras_idle_secs"),
        ("start_fast_idle_monitor(", "fast_idle_secs"),
        ("start_embed_idle_monitor(", "embed_idle_secs"),
    ] {
        let wired = production_src()
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !t.starts_with("//") && !t.starts_with("///")
            })
            .any(|l| l.contains(monitor) && l.contains(knob));
        assert!(
            wired,
            "`{monitor}` is never called with `{knob}` — it is either hardcoded or \
             reading another slot's window"
        );
    }
}
