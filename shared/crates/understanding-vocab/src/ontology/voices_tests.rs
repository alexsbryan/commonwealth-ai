use super::*;

#[test]
fn voice_folding_is_case_and_article_insensitive() {
    assert_eq!(fold_voice("  The Cataloguer "), "cataloguer");
    assert_eq!(fold_voice("cataloguer"), "cataloguer");
    assert_eq!(fold_voice("the narrator"), "narrator");
}

#[test]
fn a_declared_voice_is_a_voice_however_it_is_spelled() {
    let voices = VoicesDecl {
        not_entities: vec!["the Narrator".into(), " ".into()],
        ..VoicesDecl::default()
    };
    for name in ["the narrator", "Narrator", "  THE NARRATOR "] {
        assert!(voices.is_voice(name), "{name}");
    }
    for name in ["Ana", "the narrators", "", " "] {
        assert!(!voices.is_voice(name), "{name:?}");
    }
    assert!(!VoicesDecl::default().is_voice("the narrator"));
}
