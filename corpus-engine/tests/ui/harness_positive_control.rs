// POSITIVE CONTROL — names no corpus-engine type on purpose.
//
// This fixture cannot compile under any feature resolution, so it must ALWAYS
// be reported as failing. If it is ever reported as compiling, the harness is
// not evaluating fixtures at all and every other verdict in this suite is
// worthless. Keeping it is what makes that condition loud instead of silent:
// on 2026-08-20 all five real fixtures reported "expected to fail, but
// succeeded" because they were being judged against a corpus-engine that
// itself did not build, and nothing in the output said so.
fn main() {
    let _x: i32 = "this is not an i32";
}
