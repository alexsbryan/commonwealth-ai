// SPDX-License-Identifier: AGPL-3.0-or-later
use super::guard_tools_on_fast;
use sovereign_contracts::types::Speed;

#[test]
fn tool_request_on_slow_slot_passes() {
    assert!(guard_tools_on_fast(true, Speed::Slow).is_ok());
    assert!(guard_tools_on_fast(true, Speed::Medium).is_ok());
}

#[test]
fn tool_request_on_fast_slot_rejected() {
    let err = guard_tools_on_fast(true, Speed::Fast).unwrap_err();
    assert!(err.contains("fast slot"));
    assert!(err.contains("tool_calls"));
    assert!(err.contains("preferred_speed=Slow"));
}

#[test]
fn toolless_request_on_fast_slot_passes() {
    assert!(guard_tools_on_fast(false, Speed::Fast).is_ok());
}

#[test]
fn toolless_request_on_slow_slot_passes() {
    assert!(guard_tools_on_fast(false, Speed::Slow).is_ok());
}
