// SPDX-License-Identifier: AGPL-3.0-or-later
//! Each tier's verdict, four-valued (ARCH §18.2), and how the headline shows a
//! tier that could not judge. A sibling file so `dry_report.rs` stays inside
//! its arch-gate band (ARCH §3.1).

use kernel_types::judgement::{Judgement, Reason, Verdict};

/// The exact tier's verdict. `unreadable` is the SCIP graph's error, if any;
/// `scip_in_scope` is how many SCIP function symbols the scope held at all.
pub(super) fn exact_tier(
    unreadable: Option<String>,
    scip_in_scope: usize,
    groups: usize,
) -> Judgement {
    let tier = match unreadable {
        Some(e) => {
            Judgement::could_not_judge("exact tier", because(format!("SCIP graph unreadable: {e}")))
        }
        // No SCIP symbol in scope is not a clean 0: a language whose SCIP
        // export failed (`typescript: skipped (export failed)`) has none (I4).
        None if scip_in_scope == 0 => Judgement::could_not_judge(
            "exact tier",
            because(
                "no SCIP function symbols in scope — the SCIP export for its language \
                 may have failed (`svrn project refresh` names a skipped one)"
                    .to_string(),
            ),
        ),
        None if groups == 0 => Judgement::passed(
            "exact tier",
            because(format!(
                "no two of {scip_in_scope} SCIP function symbols' bodies hash alike"
            )),
        ),
        None => Judgement::failed(
            "exact tier",
            because(format!(
                "{groups} identical-body group(s) among {scip_in_scope} SCIP function symbols"
            )),
        ),
    };
    tracing::debug!(verdict = %tier.verdict(), reason = %tier.reason(), "dry_report: exact tier");
    tier
}

/// The near tier's verdict. A near tier with no vector to compare could not
/// judge, however clean its 0 edges look (I2).
pub(super) fn near_tier(
    considered: usize,
    skipped_no_embedding: usize,
    min_lines: usize,
    threshold: f32,
    clusters: usize,
) -> Judgement {
    let skipped = format!("{skipped_no_embedding} chunk(s) with an empty or zero vector");
    let tier = if considered == 0 && skipped_no_embedding > 0 {
        Judgement::could_not_judge(
            "near tier",
            because(format!(
                "no symbol in scope has a vector ({skipped}; an `--fts-only` index holds \
                 none) — rebuild the index with an embedder"
            )),
        )
    } else if considered == 0 {
        Judgement::could_not_judge(
            "near tier",
            because(format!(
                "no function/method symbol of ≥{min_lines} lines in scope to compare"
            )),
        )
    } else {
        let partial = if skipped_no_embedding > 0 {
            format!("; {skipped} not compared")
        } else {
            String::new()
        };
        let what = format!("{considered} symbols at cosine ≥ {threshold:.2}{partial}");
        if clusters == 0 {
            Judgement::passed("near tier", because(format!("no near pair among {what}")))
        } else {
            Judgement::failed(
                "near tier",
                because(format!("{clusters} cluster(s) among {what}")),
            )
        }
    };
    tracing::debug!(verdict = %tier.verdict(), reason = %tier.reason(), "dry_report: near tier");
    tier
}

/// Did this tier reach a verdict about the code — passed or failed?
pub(super) fn judged(tier: &Judgement) -> bool {
    matches!(tier.verdict(), Verdict::Passed | Verdict::Failed)
}

/// A tier's headline count — or its verdict when it could not judge, since a
/// `0` there reads as "no clones" (I2, I4). `nc-redundant.py` parses the count
/// form and refuses this one, which is what it should do.
pub(super) fn tier_count(tier: &Judgement, n: usize, noun: &str) -> String {
    if judged(tier) {
        format!("**{n}** {noun}")
    } else {
        format!("{noun} **{}**", tier.verdict())
    }
}

/// A [`Reason`] from text this module formats; never a placeholder in practice.
fn because(text: String) -> Reason {
    Reason::new(text).unwrap_or_else(|| Reason::literal("dry_report formatted an empty reason"))
}
