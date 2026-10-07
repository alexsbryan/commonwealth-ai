// SPDX-License-Identifier: AGPL-3.0-or-later
use serde_json::{json, Value};
use std::{collections::BTreeSet, fs, path::Path};

pub const TASK: &str = "Place the six-method CorpusReadPort extension beside the historical IndexSource contract so the fixed svrn reader can compile without reaching the corpus engine.";
pub const POLICY: &str = include_str!("../../tests/fixtures/core-read/policy.toml");
pub const PACKAGES: &[&str] = &[
    "core-probe",
    "engine-probe",
    "corpus-index",
    "corpus-engine-yield",
];
pub const TEST: &str = "core_read_surface_compiles";
pub const SOURCE_CONTRACT: &str =
    include_str!("../../tests/fixtures/core-read/source-contract.toml");

const BASE_INDEX_SOURCE: &str = include_str!("../../tests/fixtures/core-read/base-index-source.rs");
const CONTRACT_PORT_PREFIX: &str =
    include_str!("../../tests/fixtures/core-read/contract-port-prefix.rs");
const PORT_SUFFIX: &str = include_str!("../../tests/fixtures/core-read/port-suffix.rs");
const ENGINE_BASE: &str = include_str!("../../tests/fixtures/core-read/engine-base.rs");
const ENGINE_CONTRACT_REEXPORT: &str =
    include_str!("../../tests/fixtures/core-read/engine-contract-reexport.rs");
const ENGINE_PORT_PREFIX: &str =
    include_str!("../../tests/fixtures/core-read/engine-port-prefix.rs");
const ENGINE_IMPL_PREFIX: &str =
    include_str!("../../tests/fixtures/core-read/engine-impl-prefix.rs");
const ENGINE_IMPL_SUFFIX: &str =
    include_str!("../../tests/fixtures/core-read/engine-impl-suffix.rs");
const CALLER_PREFIX: &str = include_str!("../../tests/fixtures/core-read/core-probe-prefix.rs");
const CALLER_PORT_IMPORT: &str =
    include_str!("../../tests/fixtures/core-read/core-probe-port-import.rs");
const CALLER_ENGINE_IMPORT: &str =
    include_str!("../../tests/fixtures/core-read/core-probe-engine-import.rs");
const CALLER_CONSUMER: &str = include_str!("../../tests/fixtures/core-read/core-probe-consumer.rs");

struct Method {
    id: &'static str,
    declaration: &'static str,
    implementation: &'static str,
}

const METHODS: &[Method] = &[
    Method {
        id: "embed",
        declaration: include_str!("../../tests/fixtures/core-read/methods/embed.decl.rs"),
        implementation: include_str!("../../tests/fixtures/core-read/methods/embed.impl.rs"),
    },
    Method {
        id: "installed_indexes",
        declaration: include_str!(
            "../../tests/fixtures/core-read/methods/installed_indexes.decl.rs"
        ),
        implementation: include_str!(
            "../../tests/fixtures/core-read/methods/installed_indexes.impl.rs"
        ),
    },
    Method {
        id: "open_index_for_corpus",
        declaration: include_str!(
            "../../tests/fixtures/core-read/methods/open_index_for_corpus.decl.rs"
        ),
        implementation: include_str!(
            "../../tests/fixtures/core-read/methods/open_index_for_corpus.impl.rs"
        ),
    },
    Method {
        id: "index_dir",
        declaration: include_str!("../../tests/fixtures/core-read/methods/index_dir.decl.rs"),
        implementation: include_str!("../../tests/fixtures/core-read/methods/index_dir.impl.rs"),
    },
    Method {
        id: "foreground_lease",
        declaration: include_str!(
            "../../tests/fixtures/core-read/methods/foreground_lease.decl.rs"
        ),
        implementation: include_str!(
            "../../tests/fixtures/core-read/methods/foreground_lease.impl.rs"
        ),
    },
    Method {
        id: "builtin_corpora",
        declaration: include_str!("../../tests/fixtures/core-read/methods/builtin_corpora.decl.rs"),
        implementation: include_str!(
            "../../tests/fixtures/core-read/methods/builtin_corpora.impl.rs"
        ),
    },
];

pub fn properties() -> Value {
    json!({
        "target": {"type": "string", "enum": ["contract", "engine"]},
        "methods": {
            "type": "array",
            "items": {
                "type": "string",
                "enum": METHODS.iter().map(|method| method.id).collect::<Vec<_>>()
            },
            "uniqueItems": true
        },
        "binding": {"type": "string", "enum": ["port", "engine"]}
    })
}

pub fn write_baseline_probe(root: &Path) -> Result<(), String> {
    write(root, "contract", &[], "port")?;
    let consumer = format!(
        "{}\n{}",
        CALLER_PREFIX,
        CALLER_CONSUMER.replace("&dyn CorpusReadPort", "&dyn IndexSource")
    );
    write_file(root, "core-probe/src/lib.rs", &consumer)
}

pub fn write(root: &Path, target: &str, methods: &[String], binding: &str) -> Result<(), String> {
    if !matches!(target, "contract" | "engine") {
        return Err("target must be `contract` or `engine`".into());
    }
    if !matches!(binding, "port" | "engine") {
        return Err("binding must be `port` or `engine`".into());
    }

    let mut selected = BTreeSet::new();
    for id in methods {
        if id.is_empty()
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(format!(
                "method IDs cannot contain source text or attributes: {id:?}"
            ));
        }
        if !METHODS.iter().any(|method| method.id == id.as_str()) {
            return Err(format!("unknown core-read method ID: {id}"));
        }
        if !selected.insert(id.as_str()) {
            return Err(format!("duplicate core-read method ID: {id}"));
        }
    }

    fs::create_dir(root).map_err(|error| format!("fixture root must be new: {error}"))?;
    for package in PACKAGES {
        fs::create_dir_all(root.join(package).join("src")).map_err(|error| error.to_string())?;
    }

    write_file(
        root,
        "Cargo.toml",
        include_str!("../../tests/fixtures/core-read/workspace.toml"),
    )?;
    write_file(
        root,
        "README.md",
        include_str!("../../tests/fixtures/core-read/README.md"),
    )?;
    write_file(root, "policy.toml", POLICY)?;
    write_file(root, "SOURCE_CONTRACT.toml", SOURCE_CONTRACT)?;
    write_file(
        root,
        "Cargo.lock",
        &include_str!("../../tests/fixtures/core-read/Cargo.lock.in").replace(
            "@@CORE_PROBE_DEPENDENCIES@@",
            if binding == "port" {
                "dependencies = [\"corpus-engine-yield\", \"corpus-index\"]"
            } else {
                "dependencies = [\"corpus-engine-yield\", \"corpus-index\", \"engine-probe\"]"
            },
        ),
    )?;

    write_file(
        root,
        "corpus-index/Cargo.toml",
        include_str!("../../tests/fixtures/core-read/corpus-index.Cargo.toml"),
    )?;
    write_file(
        root,
        "corpus-index/src/lib.rs",
        include_str!("../../tests/fixtures/core-read/corpus-index-lib.rs"),
    )?;
    write_file(
        root,
        "corpus-index/src/index.rs",
        include_str!("../../tests/fixtures/core-read/corpus-index-index.rs"),
    )?;
    write_file(
        root,
        "corpus-index/src/types.rs",
        include_str!("../../tests/fixtures/core-read/corpus-index-types.rs"),
    )?;

    let mut source = BASE_INDEX_SOURCE.to_owned();
    if target == "contract" {
        source.push_str(CONTRACT_PORT_PREFIX);
        append_declarations(&mut source, &selected);
        source.push_str(PORT_SUFFIX);
    }
    write_file(root, "corpus-index/src/source.rs", &source)?;

    write_file(
        root,
        "corpus-engine-yield/Cargo.toml",
        include_str!("../../tests/fixtures/core-read/corpus-engine-yield.Cargo.toml"),
    )?;
    write_file(
        root,
        "corpus-engine-yield/src/lib.rs",
        include_str!("../../tests/fixtures/core-read/corpus-engine-yield-lib.rs"),
    )?;

    write_file(
        root,
        "engine-probe/Cargo.toml",
        include_str!("../../tests/fixtures/core-read/engine-probe.Cargo.toml"),
    )?;
    let mut engine = ENGINE_BASE.to_owned();
    if target == "contract" {
        engine.push_str(ENGINE_CONTRACT_REEXPORT);
    } else {
        engine.push_str(ENGINE_PORT_PREFIX);
        append_declarations(&mut engine, &selected);
        engine.push_str(PORT_SUFFIX);
    }
    engine.push_str(ENGINE_IMPL_PREFIX);
    append_implementations(&mut engine, &selected);
    engine.push_str(ENGINE_IMPL_SUFFIX);
    write_file(root, "engine-probe/src/lib.rs", &engine)?;

    let core_manifest = if binding == "port" {
        include_str!("../../tests/fixtures/core-read/core-probe-port.Cargo.toml")
    } else {
        include_str!("../../tests/fixtures/core-read/core-probe-engine.Cargo.toml")
    };
    write_file(root, "core-probe/Cargo.toml", core_manifest)?;
    let mut caller = CALLER_PREFIX.to_owned();
    caller.push_str(if binding == "port" {
        CALLER_PORT_IMPORT
    } else {
        CALLER_ENGINE_IMPORT
    });
    caller.push_str(CALLER_CONSUMER);
    write_file(root, "core-probe/src/lib.rs", &caller)
}

fn append_declarations(output: &mut String, selected: &BTreeSet<&str>) {
    for method in METHODS.iter().filter(|method| selected.contains(method.id)) {
        output.push_str(method.declaration);
    }
}

fn append_implementations(output: &mut String, selected: &BTreeSet<&str>) {
    for method in METHODS.iter().filter(|method| selected.contains(method.id)) {
        output.push_str(method.implementation);
    }
}

fn write_file(root: &Path, relative: &str, contents: &str) -> Result<(), String> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(path, contents).map_err(|error| error.to_string())
}
