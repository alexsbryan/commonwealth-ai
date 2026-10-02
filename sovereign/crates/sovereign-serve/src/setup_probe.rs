// SPDX-License-Identifier: AGPL-3.0-or-later
//! `<loader> --setup-probe <question> …`: first-run setup's questions to the
//! program that loads the model (FIVE_PROGRAMS §2c; five-programs fp-25 and
//! HUMAN-fp25-setup-host (a), with the loader the stock binary;
//! pb-distribution-setup). A machine with no config has no serve to dial, so
//! `svrn setup` and `svrn doctor` exec this and read ONE JSON answer on
//! stdout. The verdict is the exit code: 0 answered, 1 refused with the
//! reason on stderr, 2 a question this probe does not know.
//!
//! The answers are the planner's own functions (`setup_planner`,
//! `hardware`, `engine_factory`), the ones serve's `/v1/admin/setup/*` reads
//! answer from; this is the same planner reached by exec instead of HTTP.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use sovereign_contracts::daemon_wire::{
    ByomSource, FetchedBytes, FimPlan, HardwareProfile, PrimaryPlacement, ProbedPlan, SetupPlan,
    SlotConfig,
};
use sovereign_inference::hardware::{self, detect_hardware};
use sovereign_inference::setup_planner::{self as planner, SlotKind};

/// A question and the function that answers it.
type Question = (&'static str, fn(&[String]) -> i32);

/// Every question the probe answers: a registry, not a match (principle 9).
const QUESTIONS: &[Question] = &[
    ("plan", plan),
    ("fim", fim),
    ("byom-url", byom_url),
    ("download", download),
    ("placement", placement),
];

/// The probe's entry, routed by [`crate::child_launch`].
pub(crate) fn run(args: &[String]) -> i32 {
    // ggml logs go through `tracing` before detection touches a backend
    // device: without it, Metal's library-compile lines land in the middle of
    // an onboarding screen (the reason setup installed this before the probe
    // moved here).
    let _ = sovereign_inference::llama_logs::LlamaLogs::from_env().install_global();
    let (question, rest) = match args.split_first() {
        Some((q, rest)) => (q.as_str(), rest),
        None => ("", args),
    };
    match QUESTIONS.iter().find(|(name, _)| *name == question) {
        Some((_, answer)) => {
            tracing::debug!(target: "serve", question, "setup probe answering");
            answer(rest)
        }
        None => {
            let known: Vec<&str> = QUESTIONS.iter().map(|(n, _)| *n).collect();
            eprintln!(
                "setup probe: unknown question `{question}` (expected {})",
                known.join(" | ")
            );
            2
        }
    }
}

/// Print one answer as a JSON line; exit 0.
fn answer<T: Serialize>(value: &T) -> i32 {
    match serde_json::to_string(value) {
        Ok(line) => {
            println!("{line}");
            0
        }
        Err(e) => refuse(format!("setup probe: cannot render the answer: {e}")),
    }
}

fn refuse(reason: impl std::fmt::Display) -> i32 {
    eprintln!("{reason}");
    1
}

/// Download URL by slot file.
fn urls<'a>(slots: impl IntoIterator<Item = &'a SlotConfig>) -> BTreeMap<String, String> {
    slots
        .into_iter()
        .map(|s| (s.file.clone(), planner::hf_download_url(s)))
        .collect()
}

/// The plan for a probed machine: the four lookups the wizard makes, taken
/// apart from detection so a test can pin the shape against a hardware
/// profile it chose rather than one it inherited from the host.
fn build_plan(hardware: HardwareProfile) -> SetupPlan {
    let profile = hardware::select_profile(&hardware);
    SetupPlan {
        catalog: planner::build_primary_catalog(&profile),
        // Absent, not substituted: a manifest with no fast slot for this tier
        // is a fact the caller has to see, and `null` says it (principle 6).
        fast: planner::resolve_slot(&profile, SlotKind::Fast),
        embed: planner::resolve_slot(&profile, SlotKind::Embed),
        hardware,
        profile,
    }
}

/// `plan`: this machine's hardware, tier, catalog and single picks.
fn plan(_: &[String]) -> i32 {
    let plan = build_plan(detect_hardware());
    let urls = urls(
        plan.catalog
            .iter()
            .map(|o| &o.slot)
            .chain(plan.fast.iter())
            .chain(plan.embed.iter()),
    );
    answer(&ProbedPlan { plan, urls })
}

/// `fim [--quant <rung>]`: the edit model's rung for this machine, or the
/// rung asked for. An unknown rung is refused naming the input and the
/// ladder, before anything is fetched.
fn fim(args: &[String]) -> i32 {
    let asked = match args {
        [] => None,
        [flag, rung] if flag == "--quant" => Some(rung.to_ascii_lowercase()),
        _ => return refuse("setup probe fim: usage `fim [--quant <rung>]`"),
    };
    match fim_plan(asked, detect_hardware()) {
        Ok(plan) => answer(&plan),
        Err(e) => refuse(e),
    }
}

/// [`fim`]'s answer for a given machine, apart from detection so a test can
/// choose the hardware.
fn fim_plan(asked: Option<String>, hardware: HardwareProfile) -> Result<FimPlan, String> {
    let profile = hardware::select_profile(&hardware);
    let rung = asked.unwrap_or_else(|| planner::fim_rung_for_profile(&profile).to_string());
    let Some(slot) = planner::fim_slot_for_rung(&rung) else {
        let ladder: Vec<&str> = planner::FIM_RUNGS.iter().map(|(n, _)| *n).collect();
        return Err(format!(
            "unknown --quant '{rung}' (expected {})",
            ladder.join(" | ")
        ));
    };
    let embed = planner::resolve_slot(&profile, SlotKind::Embed);
    let next = planner::next_fim_rung(&rung).map(|(r, s)| (r.to_string(), s));
    let urls = urls(
        std::iter::once(&slot)
            .chain(embed.iter())
            .chain(next.iter().map(|(_, s)| s)),
    );
    Ok(FimPlan {
        hardware,
        profile,
        rung,
        slot,
        embed,
        next,
        urls,
    })
}

/// `byom-url <link>`: a pasted model link resolved to its raw download URL.
fn byom_url(args: &[String]) -> i32 {
    let [link] = args else {
        return refuse("setup probe byom-url: usage `byom-url <link>`");
    };
    match planner::resolve_byom_url(link) {
        Ok((url, file)) => answer(&ByomSource { url, file }),
        Err(e) => refuse(e),
    }
}

/// `download <url> <dest> <size_gb>`: fetch one GGUF (resumed, validated
/// against the slot's size floor), printing a [`FetchedBytes`] line per
/// progress tick. A file already on disk that validates is not re-fetched.
fn download(args: &[String]) -> i32 {
    let [url, dest, size_gb] = args else {
        return refuse("setup probe download: usage `download <url> <dest> <size_gb>`");
    };
    let Ok(size_gb) = size_gb.parse::<f64>() else {
        return refuse(format!(
            "setup probe download: size_gb '{size_gb}' is not a number"
        ));
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return refuse(format!("setup probe download: cannot build runtime: {e}")),
    };
    let expected = sovereign_contracts::gguf_validator::GgufExpectation::from_size_gb(size_gb);
    let progress = |downloaded, total| {
        if let Ok(line) = serde_json::to_string(&FetchedBytes { downloaded, total }) {
            println!("{line}");
        }
    };
    match runtime.block_on(planner::download_gguf(
        url,
        Path::new(dest),
        &expected,
        &progress,
    )) {
        Ok(()) => 0,
        Err(e) => refuse(e),
    }
}

/// `placement <config.toml>`: whether that config's primary runs in a
/// supervised compute child. The loader decides it; `svrn doctor` asks.
fn placement(args: &[String]) -> i32 {
    let [config] = args else {
        return refuse("setup probe placement: usage `placement <config.toml>`");
    };
    match sovereign_contracts::setup_config::SetupConfig::load_from(Path::new(config)) {
        Ok(cfg) => answer(&PrimaryPlacement {
            child_owns_primary: sovereign_inference::engine_factory::child_owns_primary(&cfg),
        }),
        Err(e) => refuse(e),
    }
}

#[cfg(test)]
#[path = "setup_probe/tests.rs"]
mod tests;
