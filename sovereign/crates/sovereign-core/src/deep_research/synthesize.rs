// SPDX-License-Identifier: AGPL-3.0-or-later
//! R8 — local synthesis: the draft, URL-constrained.
//!
//! The draft is produced through the port's constrained draft surface
//! (`ResearchPort::draft`) with the URL constraint enabled over the
//! window's source URLs — invented citations are structurally
//! impossible (the renderer then verifies every span anyway, the
//! always-on guarantee). The evidence is assembled into the prompt by
//! this code, never by the model.

use super::estate::ResearchPort;
use super::icd::{Draft, DraftCitation, EvidenceWindow, UrlConstraintPolicy};

/// Assemble the round's evidence text (chunk id → content) for the
/// prompt. Deterministic: chunks in window order, bounded by the
/// charter's window cap (the window was already capped at build).
pub fn evidence_block(window: &EvidenceWindow) -> String {
    let mut out = String::new();
    for chunk in &window.chunks {
        out.push_str(&format!(
            "[{}] {}",
            chunk.id,
            chunk.content.replace('\n', " ")
        ));
        out.push('\n');
    }
    out
}

/// The allowed citation set for the draft: the window's source URLs.
pub fn allowed_urls(window: &EvidenceWindow) -> Vec<String> {
    window.chunks.iter().map(|c| c.source_url.clone()).collect()
}

/// The draft's deterministic figure inventory (order deep-research-t1h,
/// H2 — pre-registered): per window chunk, its `figure_tokens` — the
/// ONE figure decider (mod.rs) — under a fixed header, so the model is
/// never left to volunteer the evidence's digits (the t1f residual:
/// keys whose figures sat in the window while the sub-questions did
/// not carry them; the t1g v1 flight: the window carried era figures
/// the draft's era-years restated). The inventory is code-enforced
/// into the PROMPT; the model's carrying of the figures into the
/// answer is measured by the battery, never assumed (§7.6). Empty
/// window → empty block (nothing to enumerate, nothing to invent).
pub fn figure_inventory(window: &EvidenceWindow) -> String {
    let mut out = String::new();
    let mut any = false;
    for chunk in &window.chunks {
        let tokens = super::figure_tokens(&chunk.content);
        if tokens.is_empty() {
            continue;
        }
        any = true;
        out.push_str(&format!("- [{}]: {}\n", chunk.id, tokens.join(", ")));
    }
    if !any {
        return String::new();
    }
    format!(
        "Figures present in the evidence (every evidence-supported figure must appear in the answer):\n{out}"
    )
}

/// The corruption-class marker set (order deep-research-t6c, REV-2,
/// pre-registered): inner-monologue and evidence-self-interrogation
/// shapes measured in the seed-07 r3 draft (flight record
/// dr-1787102765 — the rev-1 2->38 ledger blowout). Rules describe
/// SHAPES, never content: the marker class is the documented
/// corruption signature, and a clean draft with one occurrence cannot
/// trip the bar (>= 2 distinct or >= 3 total required).
pub(crate) const DEGENERATE_MARKERS: [&str; 10] = [
    "(Wait",
    "Let me re-",
    "Let me read",
    "Let me look",
    "I must ",
    "Actually,",
    "Note: Evidence",
    "the exact string",
    "in the snippet",
    "? no",
];

/// The degenerate-draft detector (pure, deterministic — no model, no
/// battery-learned thresholds). Degenerate iff >= 2 DISTINCT markers
/// OR >= 3 total occurrences OR >= 8 "**" per 1k chars. Measured on
/// the flight records: the seed-07 corruption draft = 10 distinct /
/// 27 total / 12.8 per 1k; the clean synthesis class (v1 draft-2/3,
/// seed-02 draft-2) = 0 distinct / 0 total / <= 3.2 per 1k — a >= 2.5x
/// margin on the density bar.
pub(crate) fn draft_is_degenerate(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let mut distinct = 0usize;
    let mut total = 0usize;
    for marker in DEGENERATE_MARKERS {
        let n = text.matches(marker).count();
        if n > 0 {
            distinct += 1;
            total += n;
        }
    }
    let bold_per_1k = text.matches("**").count() as f64 * 1000.0 / text.len() as f64;
    distinct >= 2 || total >= 3 || bold_per_1k >= 8.0
}

/// Produce the round's draft through the constrained surface. Round 1
/// drafts from the estate answer alone; later rounds draft from the
/// evidence + the still-open gaps. `strict_shape` (REV-2: the
/// degenerate-draft guard's re-draft) appends a plain-prose shape
/// constraint — the default prompt is byte-shaped exactly as before.
pub async fn draft_round(
    port: &dyn ResearchPort,
    run_id: &str,
    charter_hash: &str,
    round: u32,
    question: &str,
    evidence: &EvidenceWindow,
    open_gaps: &[String],
    strict_shape: bool,
) -> Result<Draft, String> {
    let system = "You are a local research synthesist. Answer the question from the evidence provided. \
                  Cite EVERY factual claim with [Source: ev-<id>] where <id> is the evidence chunk id \
                  the claim rests on (each chunk is labelled [id] in the evidence block, and its \
                  figures are listed in the inventory). Use only chunk ids present in the evidence \
                  block. If the evidence cannot answer a part, say so explicitly rather than guessing."
        .to_string();
    let mut prompt = String::new();
    if round == 1 {
        prompt.push_str(&format!("Estate evidence:\n{}", evidence_block(evidence)));
    } else {
        prompt.push_str(&format!(
            "Evidence gathered so far:\n{}\n\nQuestion: {question}",
            evidence_block(evidence)
        ));
        if !open_gaps.is_empty() {
            prompt.push_str(
                "\n\nStill-open specifics to resolve (answer only if the evidence supports it):",
            );
            for gap in open_gaps {
                prompt.push_str(&format!("\n- {gap}"));
            }
        }
    }
    // The deterministic figure inventory (t1h — H2): the evidence's
    // figures are enumerated for the model, never left to the draft's
    // discretion. Both round shapes carry it.
    let inventory = figure_inventory(evidence);
    if !inventory.is_empty() {
        prompt.push_str(&format!("\n\n{inventory}"));
    }
    if evidence.chunks.is_empty() {
        prompt.push_str("\n\n(No evidence was retrieved this round. Say so plainly.)");
    }
    // REV-2 (pre-registered): the re-draft's shape constraint — the
    // seed-07 corruption class violated every one of these shapes;
    // the constraint is appended LAST so the model sees it last.
    if strict_shape {
        prompt.push_str(
            "\n\nShape constraint (re-draft): plain prose only — complete \
             sentences, no markdown, no bold, no bullet lists, no \
             parenthetical asides, and no self-interrogation or asides \
             about the evidence text itself; state each fact at most once.",
        );
    }
    let urls = allowed_urls(evidence);
    let text = port
        .draft(&prompt, Some(&system), &urls)
        .await
        .map_err(|e| format!("draft failed: {e}"))?;
    let citations: Vec<DraftCitation> = evidence
        .chunks
        .iter()
        .map(|c| DraftCitation {
            evidence_id: c.id.clone(),
            url: c.source_url.clone(),
            custody: Some(c.custody.clone()),
        })
        .collect();
    Ok(Draft {
        icd: "draft".to_string(),
        version: super::icd::ICD_VERSION,
        run_id: run_id.to_string(),
        charter_hash: charter_hash.to_string(),
        round,
        provider: "port:draft".to_string(),
        url_constraint: UrlConstraintPolicy {
            enabled: true,
            layer: "sovereign-inference:UrlAllowlistConstraint".to_string(),
        },
        text,
        citations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deep_research::estate::{EstateListing, PortHit};
    use crate::types::Custody;
    use std::sync::{Arc, Mutex};

    /// A recording fake port: captures the prompt it was asked to
    /// complete. Everything else is unreachable — the test drives
    /// draft_round directly.
    struct RecordingPort {
        prompt: Arc<Mutex<Option<String>>>,
    }

    impl RecordingPort {
        fn new() -> Self {
            RecordingPort {
                prompt: Arc::new(Mutex::new(None)),
            }
        }
        fn last_prompt(&self) -> String {
            self.prompt.lock().unwrap().clone().unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl ResearchPort for RecordingPort {
        async fn estate_listing(&self, _c: &[String]) -> Result<EstateListing, String> {
            unimplemented!("unreachable: draft_round calls only draft")
        }
        async fn estate_search(
            &self,
            _c: &[String],
            _q: &str,
            _l: usize,
        ) -> Result<Vec<PortHit>, String> {
            unimplemented!("unreachable")
        }
        async fn web_search(&self, _b: &str, _q: &str, _l: usize) -> Result<Vec<PortHit>, String> {
            unimplemented!("unreachable")
        }
        async fn web_fetch(&self, _u: &str) -> Result<String, String> {
            unimplemented!("unreachable")
        }
        async fn terminal_poll(&self) -> Result<(), String> {
            Ok(())
        }
        async fn draft(
            &self,
            prompt: &str,
            _s: Option<&str>,
            _a: &[String],
        ) -> Result<String, String> {
            *self.prompt.lock().unwrap() = Some(prompt.to_string());
            Ok("draft".to_string())
        }
    }

    fn window() -> EvidenceWindow {
        EvidenceWindow {
            icd: "evidence_window".to_string(),
            version: 1,
            run_id: "r".to_string(),
            charter_hash: "h".to_string(),
            round: 1,
            chunks: vec![super::super::icd::WindowChunk {
                id: "ev-1".to_string(),
                locator: "https://example.com/a".to_string(),
                source_url: "https://example.com/a".to_string(),
                custody: Custody::PublicWeb.as_str().to_string(),
                provenance_class: "known".to_string(),
                content: "The Meridian Bridge was completed in 1873.".to_string(),
                ingested_into: None,
                tags: Vec::new(),
            }],
            fetch_failures: Vec::new(),
            dedup_refused: Vec::new(),
            derived_custody: Custody::PublicWeb.as_str().to_string(),
        }
    }

    #[test]
    fn evidence_block_is_deterministic() {
        let w = window();
        let block = evidence_block(&w);
        assert!(block.contains("[ev-1] The Meridian Bridge"));
        assert_eq!(evidence_block(&w), evidence_block(&w));
    }

    #[test]
    fn allowed_urls_is_the_window() {
        assert_eq!(
            allowed_urls(&window()),
            vec!["https://example.com/a".to_string()]
        );
    }

    /// RED (order deep-research-t1h, H2 — draft figure-completeness,
    /// pre-registered in adversarial/pre-registration.md): "a
    /// window-held figure the plan's sub-questions missed enters the
    /// draft". The drafting surface must carry a deterministic figure
    /// inventory — figure_tokens per window chunk, the one decider —
    /// so the model is never left to volunteer the evidence's digits.
    /// The t1f residual: keys whose figures sat in the window while
    /// the draft's sub-questions did not carry them (20 Class-A keys,
    /// t1h-failure-taxonomy.md). Watched red: fails at HEAD — the
    /// prompt carries the evidence block with no inventory.
    #[tokio::test]
    async fn draft_prompt_carries_the_window_figure_inventory() {
        let mut w = window();
        w.chunks[0].content =
            "Gini coefficients in the largest metro areas exceeded 0.5469 in 2019.".to_string();
        let port = RecordingPort::new();
        draft_round(&port, "r", "h", 1, "How did cities change?", &w, &[], false)
            .await
            .unwrap();
        let prompt = port.last_prompt();
        assert!(
            prompt.contains("Figures present in the evidence"),
            "the draft prompt must carry the figure inventory: {prompt}"
        );
        assert!(
            prompt.contains("0.5469"),
            "the window's figure must be enumerated in the inventory: {prompt}"
        );
    }

    // --- REV-2 (order deep-research-t6c, pre-registered): the
    // degenerate-draft guard. RED: these reference
    // `draft_is_degenerate` and the `strict_shape` param, which do not
    // exist at HEAD — the tests must fail to COMPILE before the fix
    // (watched red, then green).

    /// RED (a): the real seed-07 corruption window (flight record
    /// dr-1787102765/draft-3.json — the rev-1 2→38 ledger blowout) must
    /// be detected. The window trips both shape rules: 3 distinct
    /// inner-monologue markers (>= 2) and 29 "**" per 1500 chars
    /// (19.3/k >= 8/k).
    #[test]
    fn degenerate_corruption_draft_is_detected() {
        let text = r#"*announced** **?** per Bloomberg)`
Actually, I cannot see the date clearly in my internal simulation because the user pasted it as plain text. Let me re-read the *User* input carefully.

Input Text: `... The resolution was an oracle-led deal (**announced** **?** ...`
Wait, looking at the very bottom of the evidence block `[ev-1]`:
It ends with `... completed through **?**. `

Okay, let's look at the "Figures present" list again. It contains dates like `2045`, `2076`. This suggests the evidence might have typos or specific future-dated scenarios (perhaps a hypothetical scenario?). Or maybe I am misreading the year 2025 as something else?
No, the text explicitly says `signed by President Biden on **2024**-**04**-**24**`. And `Supreme Court ... on **2025**-**01**-**17**`. These are consistent with real-world recent events (though the Jan 18/19 blackout is fictional/hypothetical in this context compared to reality where it was delayed further).

Let's find the announcement date of the Oracle deal in [ev-1].
Text: `(announced **[DATE]** per Bloomberg)`
I will look for a date pattern. The figure list has `... , ?, ? , 12.5%, ? .`
Actually, I don't need to guess if the text isn't clear? No, I must use the evidence.
Let me re-read the specific sentence in [ev-1]:
"The resolution was an oracle-led deal (**announced** **?** per Bloomberg) giving Oracle about a **12.5%** stake..."

Is it possible the date is missing or obscured?
"Still-open specifics to resolve (answer only if the evide"#;
        assert!(
            draft_is_degenerate(text),
            "the seed-07 corruption class must be detected as degenerate"
        );
    }

    /// RED (b): the real clean synthesis class (v1 flight record
    /// dr-1787104761/draft-3.json) — markdown headings, bold labels,
    /// bullet lists, dense citations — must NOT be flagged. Zero
    /// markers, 6 "**" per 1900 chars (3.2/k < 8/k).
    #[test]
    fn clean_synthesis_draft_is_not_flagged() {
        let text = r#"American cities have undergone a fundamental transformation over the last four decades (1980–2024), characterized by accelerated gentrification, widening economic inequality, deteriorating housing affordability, and distinct demographic shifts.

### Gentrification
Gentrification has become significantly more prevalent since 2000, although it remains geographically concentrated in specific regions [Source: ev-2]. The term was first coined in 1963, but rates accelerated sharply as Americans pursued urban lifestyles; for the period following the 2000 Census, nearly 20% of lower-income neighborhoods experienced gentrification compared to only 9% during the 1990s [Source: ev-1] [Source: ev-2]. This represents a doubling of the rate from the previous decade [Source: ev-2].

*   **Geographic Concentration:** A select group of cities saw extensive changes. Portland, Oregon led with 58.1% of eligible tracts gentrifying (36 out of 142 total tracts) [Source: ev-1] [Source: ev-2]. Washington, D.C. followed at 51.9%, Minneapolis at 50.6%, and Seattle at 50% [Source: ev-1] [Source: ev-2]. In terms of raw numbers, New York City recorded the highest total with 128 gentrified tracts [Source: ev-1].
*   **Limited Reach:** Conversely, cities like Detroit (2.8%), Las Vegas (2%), El Paso (0%), and Arlington, Texas (0%) experienced little to no gentrification [Source: ev-1]. Nationally, only 8% of all neighborhoods reviewed experienced gentrification since the 2000 Census [Source: ev-1].

### Demographic Shifts in Gentrifying Areas
Gentrified neighborhoods typically saw increases in non-Hispanic white populations and declines in poverty rates, whereas lower-income areas that did not gentrify often saw population losses and rising minority concentrations [Source: ev-1]. Specifically, between 2009 and 2013 data points:
*   **Gentrifying Tracts (n=948):** Experienced a +6.5% population change"#;
        assert!(
            !draft_is_degenerate(text),
            "the clean synthesis class must not be flagged"
        );
    }

    /// RED (c): the density bar is a SHAPE rule, not "any bold": a
    /// heading-and-emphasis draft below 8 "**" per 1k chars stays
    /// clean even though it is heavily structured. The test validates
    /// its own precondition (density < 8/k) before asserting the guard
    /// lets it pass — the near-boundary behavior is pinned.
    #[test]
    fn markdown_heading_bold_does_not_trip_density_bar() {
        let mut text = String::new();
        for i in 0..20 {
            // One bold pair in 8 of the 20 sections: 16 "**" occurrences.
            let emphasis = if i % 5 == 0 { "The **headline figure** was 42.7%. " } else { "" };
            text.push_str(&format!(
                "### Section {i}\n{emphasis}The district reported a 42.7% change in the \
                 eligible population, the highest in the region, against the 2019 baseline.\n"
            ));
        }
        let per_1k = text.matches("**").count() as f64 * 1000.0 / text.len() as f64;
        assert!(
            per_1k < 8.0,
            "fixture precondition: {per_1k:.1} bold per 1k chars must sit under the 8/k bar"
        );
        assert!(
            !draft_is_degenerate(&text),
            "bold structure alone under the density bar must not trip the guard"
        );
    }

    /// RED (d): a single monologue marker in a long clean draft is NOT
    /// the corruption signature — the bar is >= 2 DISTINCT markers or
    /// >= 3 total. One "Actually," (a terse transition) must pass.
    #[test]
    fn single_monologue_word_does_not_trip_marker_bar() {
        let mut text = String::new();
        for i in 0..24 {
            text.push_str(&format!(
                "District {i} recorded a 12.4% change in the eligible population, \
                 the highest in the region. The figure reflects the 2019 baseline. "
            ));
        }
        text.push_str("Actually, the 2019 baseline appears twice in the evidence.");
        assert!(
            !draft_is_degenerate(&text),
            "one marker occurrence must not trip the >=2-distinct / >=3-total bar"
        );
    }

    /// RED (e): the shape-constrained re-draft prompt carries the
    /// plain-prose constraint ONLY when strict_shape is set; the
    /// default prompt is the pre-rev-2 shape (evidence block +
    /// inventory, no constraint).
    #[tokio::test]
    async fn shape_constraint_appears_only_on_retry_prompt() {
        let mut w = window();
        w.chunks[0].content =
            "Gini coefficients in the largest metro areas exceeded 0.5469 in 2019.".to_string();
        let port = RecordingPort::new();
        draft_round(&port, "r", "h", 1, "How did cities change?", &w, &[], false)
            .await
            .unwrap();
        let default_prompt = port.last_prompt();
        assert!(
            !default_prompt.contains("Shape constraint"),
            "the default prompt must stay byte-shaped as before: {default_prompt}"
        );
        assert!(
            default_prompt.contains("Figures present in the evidence"),
            "the default prompt must still carry the figure inventory"
        );

        draft_round(&port, "r", "h", 1, "How did cities change?", &w, &[], true)
            .await
            .unwrap();
        let retry_prompt = port.last_prompt();
        assert!(
            retry_prompt.contains("Shape constraint"),
            "the retry prompt must carry the plain-prose constraint: {retry_prompt}"
        );
        assert!(
            retry_prompt.contains("Figures present in the evidence"),
            "the constraint APPENDS; the inventory must survive it"
        );
    }
}
