//! Smoke test for the GliNER extractor wrapper. Loads the model
//! from `~/.sovereign/models/gliner/gliner_small-v2.1/` and runs it
//! against a sample conv chunk (with role markers, to verify the
//! preprocess strip works).
//!
//! Run with:
//!   cargo run --example gliner_smoke --features gliner-ner -p sovereign-tools

#[cfg(feature = "gliner-ner")]
fn main() -> sovereign_core::error::Result<()> {
    use sovereign_tools::gliner_ner::GlinerExtractor;
    eprintln!("Loading GliNER (this is the first-call cost)…");
    let start = std::time::Instant::now();
    let extractor = GlinerExtractor::new_default()?;
    eprintln!("Loaded in {:.2?}", start.elapsed());

    // Simulate a real conv chunk with role markers.
    let raw = r#"### [2025-08-05 14:22] user
I want to develop my theory that finance is modeled on cocaine production —
derivatives are 'cut' products, central banks function as dealers, and Jonathan
Swift's satirical pithiness is what I need to bring it together.

### [2025-08-05 14:23] assistant
That's a striking allegory. Swift's signature technique in *A Modest Proposal*
was deadpan logic applied to a horrific premise. You could mirror that by
treating the financial industry's progressive abstractions — from coca leaves
through CDOs to central bank liquidity — as increasingly diluted products in
the same supply chain. The Volcker Rule and Glass-Steagall could feature as
failed regulatory attempts to control the cartel.
"#;

    let start = std::time::Instant::now();
    let mentions = extractor.extract(raw)?;
    let elapsed = start.elapsed();
    eprintln!("Extracted {} mentions in {:.2?}", mentions.len(), elapsed);

    for m in &mentions {
        eprintln!(
            "  {:14} | {:18} | {:.1}% | [{}..{}]",
            m.label,
            m.text,
            m.score * 100.0,
            m.char_start,
            m.char_end
        );
    }

    // Assertion checks: must NOT tag "user" or "assistant" as
    // Person (role markers stripped); MUST find Swift + Jonathan
    // Swift + Volcker Rule + Glass-Steagall + Modest Proposal.
    let has = |needle: &str| {
        mentions
            .iter()
            .any(|m| m.text.to_lowercase().contains(needle))
    };
    let none = |needle: &str| !mentions.iter().any(|m| m.text.to_lowercase() == needle);
    eprintln!();
    eprintln!("Sanity checks:");
    eprintln!("  has Jonathan Swift or Swift: {}", has("swift"));
    eprintln!("  has Volcker:                 {}", has("volcker"));
    eprintln!("  has Glass-Steagall:          {}", has("glass"));
    eprintln!("  has Modest Proposal:         {}", has("modest"));
    eprintln!("  NO 'assistant' as Person:    {}", none("assistant"));
    eprintln!("  NO 'user' as Person:         {}", none("user"));

    Ok(())
}

#[cfg(not(feature = "gliner-ner"))]
fn main() {
    eprintln!("gliner_smoke requires `--features gliner-ner`");
    std::process::exit(2);
}
