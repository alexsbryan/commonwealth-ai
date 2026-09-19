// The struct-literal door: both fields of `AtomsFile` are `pub` today
// (understanding-vocab/src/atoms.rs:1529-1530), so a caller who supplies
// everything is not refused. This fixture COMPILES today; the seal privatises
// the fields (the private wire twin) and flips the assertion to
// `t.compile_fail`.
use understanding_vocab::atoms::AtomsFile;

fn main() {
    let _file = AtomsFile {
        schema_version: "2.5".to_string(),
        atoms: Vec::new(),
    };
}
