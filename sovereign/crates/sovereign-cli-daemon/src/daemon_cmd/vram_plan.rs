// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn daemon vram-plan` — what VRAM does a slot loadout need, and what is
//! the smallest card that holds it?
//!
//! The daemon already asks the first half of this question at every boot
//! (`build/preflight.rs` → `capacity::check_fit`), but only ever about THIS
//! machine's detected GPU and only about GGUFs already on this disk. Renting
//! hardware asks it the other way round: the models are not here yet, and the
//! card is the unknown being solved for.
//!
//! `scripts/dev-pod.sh` is the caller. Its offer search used to carry a
//! hardcoded `gpu_ram>=46`, a number that was correct for one loadout and
//! silently wrong for any other — configure a bigger primary and the script
//! would go on renting 48 GB boxes that cannot hold it, discovering the
//! mismatch only after the pull, the boot and the bill. Deriving the floor
//! from the loadout makes the two move together.
//!
//! Sizes may be given as bytes (the rental case — nothing is on this disk) or
//! as a path to a local GGUF (the ordinary case). Both land in the same
//! estimator; there is no second formula here (ARCH §10.6).

use sovereign_inference::capacity::{check_fit_sized, min_total_vram_mb, SizedSlot};

use sovereign_cli_shared::help::{Help, HelpSection};

const HELP: Help = Help {
    command: "svrn daemon vram-plan",
    summary: "Size a slot loadout, and name the smallest card that holds it",
    sections: &[
        HelpSection::Usage(
            "svrn daemon vram-plan --slot <role>:<bytes|path>[:<ctx>[:<nseq>]] ... [OPTIONS]",
        ),
        HelpSection::Flags(&[
            (
                "--slot <spec>",
                "A slot to plan for; repeatable. Size is a byte count (for \
                 models not on this disk) or a path to a GGUF. <ctx> defaults \
                 to --ctx, <nseq> to 1.",
            ),
            (
                "--ctx <n>",
                "Default context window for slots that don't name one \
                 (default 32768). KV cache scales linearly with it.",
            ),
            (
                "--vram-mb <n>",
                "Judge the fit against a card of this raw VRAM instead of only \
                 reporting the minimum. Exits 1 when it does not fit.",
            ),
            ("--json", "Machine-readable output."),
        ]),
        HelpSection::Examples(&[
            (
                "svrn daemon vram-plan --slot primary:30011242784 --slot fast:4261908800",
                "What card does this loadout need?",
            ),
            (
                "svrn daemon vram-plan --slot primary:30011242784 --vram-mb 49152",
                "Would it fit a 48 GB card?",
            ),
        ]),
        HelpSection::Notes(
            "The daemon asks the same question at every boot (build/preflight.rs), \
             but only about THIS machine's GPU and only about GGUFs already on \
             disk. Renting asks it the other way round: the models are not here \
             yet and the card is the unknown. scripts/dev-pod.sh derives its \
             offer-search floor from this so the box it rents tracks the loadout.",
        ),
    ],
};

/// One `--slot` spec, parsed. Kept separate from `SizedSlot` because a path
/// still has to be stat'd, and a stat that fails must REFUSE rather than
/// budget the slot at zero — a loadout silently planned as smaller than it is
/// would rent an undersized box and look correct doing it (ARCH §18.3).
fn parse_slot(spec: &str, default_ctx: u32) -> Result<SizedSlot, String> {
    // role:size[:ctx[:nseq]] — split from the LEFT on the first colon only for
    // the role, because a Windows-ish or absolute path may itself contain
    // colons on some hosts. Everything after the role is re-split from the
    // RIGHT for the two optional numeric tails.
    let (role, rest) = spec
        .split_once(':')
        .ok_or_else(|| format!("--slot {spec}: expected <role>:<bytes|path>[:<ctx>[:<nseq>]]"))?;
    if role.is_empty() {
        return Err(format!("--slot {spec}: empty role"));
    }

    // Peel at most two trailing all-numeric fields as ctx and nseq. A bare
    // byte count is itself numeric, so stop peeling once only one field is
    // left — otherwise `primary:30011242784` would read its own size as ctx.
    let mut fields: Vec<&str> = rest.split(':').collect();
    let mut nseq: u32 = 1;
    let mut ctx: u32 = default_ctx;
    if fields.len() > 1 && fields.last().is_some_and(|f| f.parse::<u32>().is_ok()) {
        let v: u32 = fields.pop().unwrap().parse().unwrap();
        if fields.len() > 1 && fields.last().is_some_and(|f| f.parse::<u32>().is_ok()) {
            // Two tails present: the peeled one was nseq, the next is ctx.
            nseq = v;
            ctx = fields.pop().unwrap().parse().unwrap();
        } else {
            ctx = v;
        }
    }
    let size = fields.join(":");

    let weights_bytes = match size.parse::<u64>() {
        Ok(b) => b,
        Err(_) => std::fs::metadata(&size)
            .map_err(|e| format!("--slot {spec}: cannot size {size}: {e}"))?
            .len(),
    };
    if weights_bytes == 0 {
        return Err(format!("--slot {spec}: zero bytes"));
    }
    if ctx == 0 {
        return Err(format!("--slot {spec}: context window must be > 0"));
    }

    Ok(SizedSlot {
        role: role.to_string(),
        weights_bytes,
        n_seq_max: nseq,
        n_ctx: ctx,
    })
}

pub fn run(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP);
        return 0;
    }

    let mut specs: Vec<String> = Vec::new();
    let mut default_ctx: u32 = 32_768;
    let mut vram_mb: Option<u64> = None;
    let mut json = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--slot" => {
                let Some(v) = args.get(i + 1) else {
                    eprintln!("error: --slot needs a value");
                    return 2;
                };
                specs.push(v.clone());
                i += 2;
            }
            "--ctx" => {
                let Some(v) = args.get(i + 1).and_then(|v| v.parse().ok()) else {
                    eprintln!("error: --ctx needs a positive integer");
                    return 2;
                };
                default_ctx = v;
                i += 2;
            }
            "--vram-mb" => {
                let Some(v) = args.get(i + 1).and_then(|v| v.parse().ok()) else {
                    eprintln!("error: --vram-mb needs a positive integer");
                    return 2;
                };
                vram_mb = Some(v);
                i += 2;
            }
            "--json" => {
                json = true;
                i += 1;
            }
            other => {
                eprintln!("error: unknown vram-plan flag '{other}'");
                sovereign_cli_shared::help::print(&HELP);
                return 2;
            }
        }
    }

    if specs.is_empty() {
        eprintln!("error: vram-plan needs at least one --slot");
        sovereign_cli_shared::help::print(&HELP);
        return 2;
    }

    let mut slots = Vec::with_capacity(specs.len());
    for spec in &specs {
        match parse_slot(spec, default_ctx) {
            Ok(s) => slots.push(s),
            Err(e) => {
                eprintln!("error: {e}");
                return 2;
            }
        }
    }

    // `check_fit_sized` against 0 VRAM to get the requirement without a
    // verdict; the verdict comes from the real card below, if one was named.
    let required_mb = check_fit_sized(&slots, 0).total_required_mb;
    let min_mb = min_total_vram_mb(required_mb);
    // Disk needs the raw bytes, not the working set: the pull is what fills
    // the volume. Rounded UP to a GB and given the same headroom the image
    // and the daemon's own data dir want.
    let weights_bytes: u64 = slots.iter().map(|s| s.weights_bytes).sum();
    let disk_gb = weights_bytes.div_ceil(1_000_000_000) + 20;

    let report = vram_mb.map(|mb| check_fit_sized(&slots, mb));

    if json {
        let per_slot: Vec<String> = check_fit_sized(&slots, 0)
            .per_slot
            .iter()
            .map(|(role, e)| {
                format!(
                    r#"{{"role":"{}","weights_mb":{},"kv_cache_mb":{},"scratch_mb":{},"total_mb":{}}}"#,
                    role,
                    e.weights_mb,
                    e.kv_cache_mb,
                    e.scratch_mb,
                    e.total_mb()
                )
            })
            .collect();
        print!(
            r#"{{"required_mb":{},"min_total_vram_mb":{},"min_total_vram_gb":{},"weights_bytes":{},"disk_gb":{},"slots":[{}]"#,
            required_mb,
            min_mb,
            min_mb.div_ceil(1024),
            weights_bytes,
            disk_gb,
            per_slot.join(",")
        );
        if let Some(r) = &report {
            print!(
                r#","available_mb":{},"safety_reserved_mb":{},"fits":{}"#,
                r.available_mb, r.safety_reserved_mb, r.fits
            );
        }
        println!("}}");
    } else {
        // The table is `CapacityReport`'s own — the daemon's refusal message
        // renders the identical block, and two copies of the column widths
        // drift the moment either is touched.
        print!("{}", check_fit_sized(&slots, 0).slot_table());
        println!("  {:<22} {:>8}", "required", required_mb);
        println!(
            "  {:<22} {:>8} MiB ({} GB card)",
            "smallest card",
            min_mb,
            min_mb.div_ceil(1024)
        );
        println!("  {:<22} {:>8} GB", "disk for the pull", disk_gb);
        if let Some(r) = &report {
            println!(
                "  {:<22} {:>8} MiB available after {} reserved",
                "against card", r.available_mb, r.safety_reserved_mb
            );
            println!(
                "  {:<22} {:>8}",
                "verdict",
                if r.fits { "FITS" } else { "DOES NOT FIT" }
            );
        }
    }

    match report {
        Some(r) if !r.fits => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_byte_count_is_not_mistaken_for_a_context_window() {
        // `primary:30011242784` has exactly one field after the role and it
        // is numeric — the tail-peeling must NOT read the size as ctx and
        // leave the slot with no weights.
        let s = parse_slot("primary:30011242784", 32_768).unwrap();
        assert_eq!(s.weights_bytes, 30_011_242_784);
        assert_eq!(s.n_ctx, 32_768);
        assert_eq!(s.n_seq_max, 1);
    }

    #[test]
    fn the_optional_tails_are_read_in_order() {
        let one = parse_slot("embed:639150592:8192", 32_768).unwrap();
        assert_eq!(one.weights_bytes, 639_150_592);
        assert_eq!(one.n_ctx, 8_192);
        assert_eq!(one.n_seq_max, 1);

        let two = parse_slot("embed:639150592:8192:16", 32_768).unwrap();
        assert_eq!(two.weights_bytes, 639_150_592);
        assert_eq!(two.n_ctx, 8_192);
        assert_eq!(two.n_seq_max, 16);
    }

    #[test]
    fn an_unsizeable_path_refuses_rather_than_budgeting_zero() {
        // The failure that would rent an undersized box while looking fine.
        let e = parse_slot("primary:/nonexistent/model.gguf", 32_768).unwrap_err();
        assert!(e.contains("cannot size"), "{e}");
    }

    #[test]
    fn a_missing_size_is_a_usage_error() {
        assert!(parse_slot("primary", 32_768).is_err());
        assert!(parse_slot(":30011242784", 32_768).is_err());
    }
}
