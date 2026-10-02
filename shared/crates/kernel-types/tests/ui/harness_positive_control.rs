// Names no kernel-types type and cannot compile under any feature
// resolution. If trybuild ever reports this as COMPILING, the suite is not
// evaluating anything and says so out loud (ARCH §18.4 — validate the
// instrument before the result). Copied deliberately from
// `corpus-engine/tests/ui/harness_positive_control.rs`: the hour that
// fixture was written for is recorded in `evidence_reds.rs`, and a second
// compile-fail suite with no control would have to relive it.
fn main() {
    let _: NoSuchTypeAnywhere = there_is_no_such_function();
}
