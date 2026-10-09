// SPDX-License-Identifier: AGPL-3.0-or-later
//! `[[document]]` blocks: the load rules, and the one metadata rule.

use super::*;
use kernel_types::Sha256Hash;

fn recipe(acquire: &str, documents: &str) -> std::result::Result<Recipe, String> {
    let toml = format!(
        "[corpus]\nid = \"c\"\nname = \"C\"\ndescription = \"d\"\nlicense = \"MIT\"\n\
         mesh_sharing = false\n\n[acquire]\n{acquire}\n\n[extract]\ntype = \"plaintext\"\n\n\
         [chunk]\ntype = \"paragraph\"\n{documents}"
    );
    Recipe::from_toml(&toml).map_err(|e| e.to_string())
}

const INLINE: &str = "type = \"inline\"";
const FILES: &str = "type = \"local_file\"\npath = \"/tmp/x\"";

/// Two inline documents, one declaring metadata. The conformance fixture
/// (the normative example) is loaded by `tests/main/inline_recipe_e2e.rs`:
/// a package's `src/` embeds nothing outside its crate (boundary-gate 3b).
const TWO: &str = "[[document]]\nname = \"folio\"\ntext = \"An archive.\"\n\
                   metadata = '{\"title\":\"The Folio and the Box\"}'\n\
                   [[document]]\nname = \"notes\"\ntext = \"Tuesday.\"\n";

#[test]
fn a_block_carries_text_or_source_never_both_never_neither() {
    let both = recipe(
        FILES,
        "[[document]]\nname = \"a\"\ntext = \"t\"\nsource = \"a.txt\"\n",
    );
    assert!(both.unwrap_err().contains("both `text` and `source`"));
    let neither = recipe(FILES, "[[document]]\nmetadata = '{}'\n");
    assert!(neither.unwrap_err().contains("neither `text` nor `source`"));
    let nameless = recipe(INLINE, "[[document]]\ntext = \"t\"\n");
    assert!(nameless.unwrap_err().contains("needs a `name`"));
    let named_file = recipe(FILES, "[[document]]\nname = \"a\"\nsource = \"a.txt\"\n");
    assert!(named_file.unwrap_err().contains("named by its `source`"));
    let unknown = recipe(
        INLINE,
        "[[document]]\nname = \"a\"\ntext = \"t\"\ntitle = \"x\"\n",
    );
    assert!(unknown.unwrap_err().contains("unknown field"));
}

#[test]
fn metadata_must_be_json_and_is_kept_verbatim() {
    let bad = recipe(
        INLINE,
        "[[document]]\nname = \"a\"\ntext = \"t\"\nmetadata = '{\"id\": '\n",
    );
    assert!(bad.unwrap_err().contains("not valid JSON"));
    let spaced = r#"{"id":  "a",   "z": 1, "b": [1.50]}"#;
    let r = recipe(
        INLINE,
        &format!("[[document]]\nname = \"a\"\ntext = \"t\"\nmetadata = '{spaced}'\n"),
    )
    .unwrap();
    let DeclaredDocument::Inline { metadata, .. } = &r.documents[0] else {
        panic!("inline");
    };
    assert_eq!(metadata.as_deref(), Some(spaced));
}

#[test]
fn an_inline_recipe_holds_only_inline_documents_and_at_least_one() {
    assert!(recipe(INLINE, "")
        .unwrap_err()
        .contains("this recipe has none"));
    let file_in_inline = recipe(INLINE, "[[document]]\nsource = \"a.txt\"\n");
    assert!(file_in_inline.unwrap_err().contains("reads no files"));
    let text_in_files = recipe(FILES, "[[document]]\nname = \"a\"\ntext = \"t\"\n");
    assert!(text_in_files.unwrap_err().contains("only [acquire] type"));
    let twice = recipe(
        INLINE,
        "[[document]]\nname = \"a\"\ntext = \"t\"\n[[document]]\nname = \"a\"\ntext = \"u\"\n",
    );
    assert!(twice.unwrap_err().contains("declared twice"));
    for bad in ["a/b", "..", "."] {
        let r = recipe(
            INLINE,
            &format!("[[document]]\nname = \"{bad}\"\ntext = \"t\"\n"),
        );
        assert!(r.unwrap_err().contains("one file-name component"), "{bad}");
    }
    let escapes = recipe(FILES, "[[document]]\nsource = \"../a.txt\"\n");
    assert!(escapes.unwrap_err().contains("relative to the source root"));
}

#[test]
fn a_recipe_without_blocks_serializes_as_before() {
    let r = recipe(FILES, "").unwrap();
    assert!(r.documents.is_empty());
    assert!(!toml::to_string(&r).unwrap().contains("[[document]]"));
    let r = recipe(INLINE, TWO).unwrap();
    let again = Recipe::from_toml(&toml::to_string(&r).unwrap()).unwrap();
    assert_eq!(again.documents, r.documents, "the blocks round-trip");
}

#[test]
fn declared_metadata_replaces_the_extractors_for_its_source_only() {
    let r = recipe(
        FILES,
        "[[document]]\nsource = \"sub/a.txt\"\nmetadata = '{\"k\": 1}'\n",
    )
    .unwrap();
    let root = Path::new("/srv/root");
    let declared = DeclaredMetadata::of(&r, Some(root));
    let extracted = serde_json::json!({"from": "extractor"});

    let a = DocSource::File(root.join("sub/a.txt"));
    let got = declared.input("t", "a", 0, &a, Some(&extracted));
    assert_eq!(got.metadata.as_deref(), Some(r#"{"k": 1}"#), "verbatim");

    let b = DocSource::File(root.join("sub/b.txt"));
    let got = declared.input("t", "b", 0, &b, Some(&extracted));
    assert_eq!(got.metadata.as_deref(), Some(r#"{"from":"extractor"}"#));

    let got = declared.input("t", "a", 0, &DocSource::Record, None);
    assert_eq!(got.metadata, None, "neither declared nor extracted");

    // Without a root, no file declaration can be matched.
    let rootless = DeclaredMetadata::of(&r, None);
    let got = rootless.input("t", "a", 0, &a, None);
    assert_eq!(got.metadata, None);
}

#[test]
fn an_inline_documents_metadata_is_found_by_its_name() {
    let r = recipe(INLINE, TWO).unwrap();
    let declared = DeclaredMetadata::of(&r, None);
    let stated = DocSource::Hashed {
        sha256: Sha256Hash::of_str("x"),
        extractor: "plaintext@0".into(),
    };
    let got = declared.input("t", "folio", 0, &stated, None);
    assert!(got
        .metadata
        .as_deref()
        .is_some_and(|m| m.contains("The Folio and the Box")));
    let got = declared.input("t", "notes", 0, &stated, None);
    assert_eq!(got.metadata, None);
}
