// The `Deserialize` door: `AtomsFile` derives `Deserialize` today
// (corpus-engine-vocab/src/atoms.rs:1527), so any caller outside vocab can
// mint one from a string. This fixture COMPILES today; the seal removes the
// derive and flips the assertion to `t.compile_fail`.
use corpus_engine_vocab::atoms::AtomsFile;

fn main() {
    let _file: AtomsFile = serde_json::from_str(r#"{"schema_version":"2.5","atoms":[]}"#).unwrap();
}
