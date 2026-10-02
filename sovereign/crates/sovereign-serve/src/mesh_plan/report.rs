// SPDX-License-Identifier: AGPL-3.0-or-later
//! The plan's report: its types, the split and the speed lookup (split from
//! `mesh_plan.rs` at the move to serve).

use super::*;

pub(crate) const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

pub(crate) fn gb(bytes: u64) -> f64 {
    bytes as f64 / GIB
}

/// Everything `build_report` needs, already read from disk and validated.
pub(crate) struct PlanInput {
    /// Display name of the model file.
    pub(crate) model_name: String,
    /// Transformer block count from the GGUF header.
    pub(crate) n_layer: u32,
    /// `(tensor_name, layer, nbytes)` from the GGUF tensor table.
    pub(crate) sizes: Vec<(String, Option<u32>, u64)>,
    /// Per-device usable VRAM in GB, in caller order. The
    /// [`Possible`](CapacityBasis::Possible) basis: device totals.
    pub(crate) devices_gb: Vec<f64>,
    /// Per-device LIVE FREE memory in GB, in the same order as `devices_gb` —
    /// the [`SafeNow`](CapacityBasis::SafeNow) basis.
    ///
    /// `Some` only when a live reading exists for EVERY device. That
    /// all-or-nothing rule is deliberate: a cut apportioned from a mix of
    /// live-free and device-total readings is neither basis, and would produce a
    /// third split matching nothing the loader would execute — the exact class of
    /// silent disagreement this field was added to end. Partial knowledge is
    /// reported as "unknown", not averaged into a plausible-looking number.
    pub(crate) devices_free_gb: Option<Vec<f64>>,
    /// The daemon's `SOVEREIGN_RPC_BLOCK_SPLIT` pin, verbatim, when one is set.
    ///
    /// A pin makes both capacity bases irrelevant as predictions: the loader
    /// obeys the pin and ignores VRAM. Carried as the raw string so this side
    /// validates it with the SAME `parse_block_split` the loader runs — a pin the
    /// loader would reject must be rejected here too, or the plan would confidently
    /// preview a split nothing honours.
    pub(crate) block_split_pin: Option<String>,
    /// Index into `devices_gb` of the host — the node that holds the output
    /// head. Validated in range by the caller.
    pub(crate) host: usize,
    /// Headroom multiplier applied to each device's share.
    pub(crate) headroom: f64,
    /// Whether `headroom` came from `--headroom` (a what-if) rather than the
    /// configuration the live load will actually use.
    pub(crate) headroom_from_flag: bool,
    /// Live mesh identities, in the same order as `devices_gb`. `Some` only
    /// under `--from-mesh`; a `--devices` plan describes hardware that is not
    /// here and therefore has no measurement to find.
    pub(crate) mesh: Option<Vec<MeshDevice>>,
    /// Context length the plan assumes. Part of the measurement key, because
    /// decode rate is a function of KV size.
    pub(crate) n_ctx: u32,
    /// llama.cpp's projected non-weight terms (KV + compute per device) for
    /// this model at `n_ctx` — the SAME projection the live fit gate judges
    /// with (`projected_overheads`), so the preview's fit column and the
    /// load's refusal carry identical numbers. `None` when the projection was
    /// unavailable (no backend in this process, or it failed); the fit is then
    /// weights-only, exactly like the live gate's own fallback.
    pub(crate) overheads: Option<sovereign_inference::embedded::PlanOverheads>,
}

/// What `mesh plan` can honestly say about speed.
pub(crate) enum SpeedSection {
    /// A real run against exactly this configuration.
    Measured {
        summary: Box<crate::mesh_measurements::MeasurementSummary>,
    },
    /// This configuration could be measured; nobody has. `near` names
    /// measurements of the same model in *other* configurations — as context
    /// for the operator, never as a number for this one.
    NotMeasured {
        near: Vec<crate::mesh_measurements::NearMiss>,
    },
    /// There is nothing here to have measured.
    NotMeasurable(NotMeasurable),
}

/// Why a configuration can carry no measurement at all.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum NotMeasurable {
    /// `--devices` describes hardware that is not present.
    HypotheticalDevices,
    /// The host advertises no hardware fingerprint (an older daemon), so there
    /// is no key under which a measurement could have been filed.
    HostUnidentified,
    /// A *peer* carrying part of the model advertises no hardware fingerprint,
    /// so the placement cannot say which silicon held that share.
    ///
    /// Distinct from [`HostUnidentified`](NotMeasurable::HostUnidentified)
    /// because the repair is on a different machine, and the operator needs to
    /// be told which one. Falling back to a name-only key instead would let a
    /// peer swap its GPU and keep answering with the old number.
    PeerUnidentified {
        /// The mesh member to go upgrade.
        name: String,
    },
}

/// One device's row in the plan.
pub(crate) struct DeviceRow {
    /// Index into the caller's device list — the number the operator typed in
    /// `--devices`, and the one displayed. **Not** the same as
    /// `fit.device_index`, which is in plan order (workers first, host last).
    /// Confusing the two silently attributes every row to the wrong machine,
    /// and nothing downstream can catch it.
    pub(crate) dev: usize,
    pub(crate) is_host: bool,
    pub(crate) blocks: Option<(u32, u32)>,
    pub(crate) holds_output: bool,
    /// This device's share weighed against its memory.
    ///
    /// Comes from `shard_fits` — the SAME decider the live load's per-device
    /// gate runs. That is the whole point: this command exists to preview a
    /// load, and a preview computing its own answer is a preview that can
    /// disagree with the thing it previews. It did, until 2026-07-28.
    pub(crate) fit: sovereign_inference::embedded::ShardFit,
}

impl DeviceRow {
    /// What this device has.
    pub(crate) fn vram(&self) -> u64 {
        self.fit.capacity_bytes
    }
    /// Bytes of weights this device holds.
    pub(crate) fn weight(&self) -> u64 {
        self.fit.held_bytes
    }
    /// `weight × headroom` — what must fit.
    pub(crate) fn need(&self) -> u64 {
        self.fit.need_bytes
    }
    /// Whether this device can hold its share.
    pub(crate) fn fits(&self) -> bool {
        self.fit.fits()
    }
}

/// Spread of per-block byte mass — the "is heterogeneous VRAM safe here" signal.
pub(crate) struct BlockMass {
    pub(crate) min: u64,
    pub(crate) max: u64,
    pub(crate) mean: u64,
    pub(crate) spread: f64,
    pub(crate) uniform: bool,
}

/// Hot/cold split for a mixture-of-experts model.
pub(crate) struct MoeReport {
    /// Routed-expert bytes — cold, only the router's top-k are read per token.
    pub(crate) routed_expert_bytes: u64,
    /// Resident mass touched on every token.
    pub(crate) hot_bytes: u64,
}

/// Which answer to "how much memory does each device have" a layout was built
/// from. The two are not interchangeable and the report never merges them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CapacityBasis {
    /// Device TOTAL — what the silicon could hold if nothing else were resident.
    /// Answers "is this configuration viable at all", which is a property of the
    /// hardware and does not change because something is loaded right now.
    Possible,
    /// Live FREE — what is available at this instant, and therefore what the
    /// running loader's fit gate judges against. Answers "would a load started
    /// right now succeed, and with which cut".
    SafeNow,
    /// Not derived from capacity at all: the operator pinned the per-device block
    /// counts with `SOVEREIGN_RPC_BLOCK_SPLIT`, and the loader honours the pin
    /// over any VRAM apportionment.
    ///
    /// When a pin is active it OUTRANKS both other bases as a description of what
    /// will load, because it is the only one the loader will actually obey.
    Pinned,
}

impl CapacityBasis {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Possible => "possible (device total)",
            Self::SafeNow => "safe now (live free)",
            Self::Pinned => "pinned (SOVEREIGN_RPC_BLOCK_SPLIT)",
        }
    }
}

/// One capacity basis laid across the devices: the cut that follows from ONE
/// answer to how much room each device has, plus the verdicts on it.
///
/// Exists because the two bases genuinely produce DIFFERENT cuts, and the
/// difference was invisible before 2026-07-30. On the mesh that produced the
/// first valid two-node 122B record, the totals basis apportioned 14/34 while
/// the loader — reading live free — ran 12/36. The plan therefore looked up a
/// measurement key that no run could ever file under, and reported "not
/// measured" about a configuration it had a real 10.48 tok/s number for.
pub(crate) struct Allocation {
    pub(crate) basis: CapacityBasis,
    /// Per-device rows, sorted by the operator-facing device index. Each row's
    /// `vram()` IS the capacity this basis fed in, so the basis needs no second
    /// copy of it.
    pub(crate) rows: Vec<DeviceRow>,
    pub(crate) pooled: u64,
    pub(crate) gate_pass: bool,
    pub(crate) nodes: NodesReport,
}

impl Allocation {
    /// Devices whose share does not fit their own memory on this basis.
    pub(crate) fn overflows(&self) -> Vec<&DeviceRow> {
        self.rows.iter().filter(|r| !r.fits()).collect()
    }

    /// Whether this basis clears both gates.
    pub(crate) fn fits(&self) -> bool {
        self.gate_pass && self.overflows().is_empty()
    }
}

/// Node count and the per-token hop cost that follows from it.
pub(crate) struct NodesReport {
    pub(crate) active_nodes: usize,
    pub(crate) hops_now: usize,
    pub(crate) min_nodes: usize,
    pub(crate) hops_min: usize,
}

/// The finished plan, ready to render.
pub(crate) struct PlanReport {
    pub(crate) model_name: String,
    pub(crate) n_layer: u32,
    pub(crate) total_weight: u64,
    pub(crate) output_bytes: u64,
    pub(crate) embd_bytes: u64,
    pub(crate) block_mass: BlockMass,
    pub(crate) moe: Option<MoeReport>,
    pub(crate) headroom: f64,
    pub(crate) headroom_from_flag: bool,
    pub(crate) pooled: u64,
    pub(crate) gate_need: u64,
    pub(crate) gate_pass: bool,
    pub(crate) rows: Vec<DeviceRow>,
    pub(crate) nodes: NodesReport,
    /// The same layout recomputed against LIVE FREE memory — the cut a load
    /// started right now would actually run, and the basis its fit gate judges.
    ///
    /// `None` when no live reading is available for every device: a `--devices`
    /// what-if (hardware that is not here), a daemon predating
    /// `/v1/mesh/status.device_memory`, or a peer with no discovered RPC worker.
    ///
    /// The fields above (`rows`, `pooled`, `gate_pass`, `nodes`) remain the
    /// [`Possible`](CapacityBasis::Possible) basis whether or not this is
    /// present, so their meaning never depends on what happens to be loaded.
    pub(crate) safe_now: Option<Allocation>,
    /// The cut the operator PINNED, when `SOVEREIGN_RPC_BLOCK_SPLIT` is set and
    /// valid for this model and device count.
    ///
    /// `Some` here means neither capacity basis predicts what will load, and this
    /// one does. Built with the same `plan_shards_explicit` the loader calls, so
    /// the block ranges are identical rather than merely similar.
    pub(crate) pinned: Option<Allocation>,
    /// The raw pin as the daemon reported it, even when it could NOT be applied
    /// (wrong device count, counts not summing to the block count). A pin the
    /// loader will reject is worth naming: the operator set it expecting it to
    /// take effect, and silence would let them believe it had.
    pub(crate) block_split_pin: Option<String>,
    /// What we can honestly say about how fast this configuration runs.
    ///
    /// Resolved against the cut that would ACTUALLY run — `safe_now` when we have
    /// it, else the possible basis. A measurement is filed under the placement
    /// that produced it, so looking it up under a cut the loader would not make
    /// is a guaranteed miss.
    pub(crate) speed: SpeedSection,
    /// The measurement key this plan looked up, when it had one. Emitted in
    /// `--json` so a script can correlate a plan with a `mesh bench` record.
    pub(crate) speed_key: Option<crate::mesh_measurements::MeasurementKey>,
}

impl PlanReport {
    /// Devices whose share does not fit their own memory.
    pub(crate) fn overflows(&self) -> Vec<&DeviceRow> {
        self.rows.iter().filter(|r| !r.fits()).collect()
    }

    /// `0` when the model fits both gates, `1` when it does not.
    ///
    /// Speed never participates: a plan that fits but would run slowly is still
    /// a plan that fits, and picking a tokens-per-second threshold on someone
    /// else's behalf is not this command's job.
    pub(crate) fn exit_code(&self) -> i32 {
        if self.gate_pass && self.overflows().is_empty() {
            0
        } else {
            1
        }
    }
}

/// Lay a model's blocks across a set of devices and judge the fit.
///
/// Pure. Uses the same `plan_shards_weighted` the live load uses, over the same
/// device order (workers first, host last), so the preview and the load cannot
/// disagree about where a block lands.
pub(crate) fn build_report(
    input: PlanInput,
    measurements: &crate::mesh_measurements::MeasurementFile,
    peers: &[crate::mesh_measurements::ForeignRecord],
    current_build: &str,
) -> PlanReport {
    use sovereign_inference::embedded as inf;

    let PlanInput {
        model_name,
        n_layer,
        sizes,
        devices_gb,
        devices_free_gb,
        block_split_pin,
        host,
        headroom,
        headroom_from_flag,
        mesh,
        n_ctx,
        overheads,
    } = input;

    // Per-block byte mass + global tensors (output head → last block-holder;
    // token_embd → host system RAM; other globals lumped as host overhead).
    // Routed-expert (`_exps`) mass is the COLD part of an MoE model — only the
    // router's top-k experts are read per token, so it can be ~90% of the bytes
    // yet a small fraction of the per-token work. `model_mass_from_sizes` is the
    // same decomposition the live load's planner uses.
    let mass = inf::model_mass_from_sizes(&sizes, n_layer);
    let total_weight: u64 = mass.total_bytes();

    let gate_need = (total_weight as f64 * headroom) as u64;

    // Lay the model across one capacity basis and judge it.
    //
    // Factored out so BOTH bases go through the identical arithmetic. If
    // "possible" and "safe now" were computed by two code paths, a divergence
    // between them would be indistinguishable from a divergence in the
    // capacities — which is the confusion this whole two-basis report exists to
    // remove.
    // `counts`: `Some` pins the per-device block counts (plan order) instead of
    // apportioning by capacity — the loader's `SOVEREIGN_RPC_BLOCK_SPLIT` path.
    // The capacities still matter even then, because the FIT verdict is about
    // whether the pinned share fits the memory available.
    let allocate =
        |basis: CapacityBasis, devices_gb: &[f64], counts: Option<&[u32]>| -> Allocation {
            let vram: Vec<u64> = devices_gb.iter().map(|&g| (g * GIB) as u64).collect();

            // Mirror the daemon's device order (RPC workers first, host/local GPU last) so
            // plan_shards places the output head on the host — the SAME functions the live
            // load uses, so the dry run matches reality.
            let mut order: Vec<usize> = (0..vram.len()).filter(|&d| d != host).collect();
            order.push(host);
            let weights: Vec<f32> = order
                .iter()
                .map(|&d| inf::quantize_vram(vram[d]) as f32)
                .collect();
            // Byte-mass-aware split — apportion each device a contiguous block range whose
            // BYTES (not count) are proportional to its VRAM, folding the output head onto
            // the host. The IDENTICAL call the live load makes, so the preview matches it.
            //
            // Under a pin we call `plan_shards_explicit` instead — again the identical
            // call, so the pinned ranges here ARE the ranges the loader computes, not a
            // reconstruction of them. A pin that fails to tile falls back rather than
            // wedging; `pinned` is only built from a pin `parse_block_split` accepted,
            // so this fallback is unreachable in practice and defensive only.
            let plan = counts
                .and_then(|c| inf::plan_shards_explicit(n_layer, &weights, c))
                .unwrap_or_else(|| {
                    inf::plan_shards_weighted(n_layer, &weights, &mass.block_bytes, mass.head_bytes)
                });

            // The per-device verdict, from the decider the live gate runs. Capacities go
            // in PLAN order (`order[pos]`), and the display maps back through `order`
            // below — the two index spaces look interchangeable and are not.
            let capacities: Vec<u64> = order.iter().map(|&d| vram[d]).collect();
            let fits = inf::shard_fits(&plan, &capacities, &mass, headroom, overheads.as_ref());

            let mut rows: Vec<DeviceRow> = Vec::with_capacity(vram.len());
            for (pos, &d) in order.iter().enumerate() {
                let shard = &plan[pos];
                rows.push(DeviceRow {
                    dev: d,
                    is_host: d == host,
                    blocks: shard.blocks,
                    holds_output: shard.holds_output,
                    // `shard_fits` declines to judge when the inputs don't describe each
                    // other. Every such input is validated away before we get here
                    // except one: a GGUF whose tensor table carries no per-layer mass at
                    // all. For that model every device genuinely holds zero block bytes,
                    // so a zero row is the right answer rather than a papered-over gap.
                    fit: fits
                        .as_ref()
                        .and_then(|f| f.get(pos).copied())
                        .unwrap_or(inf::ShardFit {
                            device_index: pos,
                            held_bytes: 0,
                            overhead_bytes: 0,
                            need_bytes: 0,
                            capacity_bytes: capacities[pos],
                        }),
                });
            }
            rows.sort_by_key(|r| r.dev);

            // Aggregate gate (the live daemon's model×1.2, with YOUR headroom).
            let pooled: u64 = vram.iter().sum();

            // Minimum nodes to hold the model: fewest of the LARGEST devices whose pooled
            // VRAM covers model×headroom. Single-stream pipeline decode costs (nodes-1)
            // hops/token, so fewer nodes = fewer hops. Aggregate lower bound — a very
            // skewed model may need one more node for per-device fit.
            let mut vram_desc: Vec<u64> = vram.clone();
            vram_desc.sort_unstable_by(|a, b| b.cmp(a));
            let (mut min_nodes, mut acc) = (0usize, 0u64);
            for v in &vram_desc {
                if acc >= gate_need {
                    break;
                }
                acc += *v;
                min_nodes += 1;
            }
            min_nodes = min_nodes.max(1);
            let active_nodes = rows.iter().filter(|r| r.blocks.is_some()).count().max(1);

            Allocation {
                basis,
                rows,
                pooled,
                gate_pass: pooled >= gate_need,
                nodes: NodesReport {
                    active_nodes,
                    hops_now: active_nodes - 1,
                    min_nodes,
                    hops_min: min_nodes.saturating_sub(1),
                },
            }
        };

    let possible = allocate(CapacityBasis::Possible, &devices_gb, None);
    // Only when a live reading covers EVERY device — see `PlanInput::devices_free_gb`.
    let safe_now = devices_free_gb
        .as_ref()
        .filter(|free| free.len() == devices_gb.len())
        .map(|free| allocate(CapacityBasis::SafeNow, free, None));

    // A pin outranks both derived bases as a prediction, because the loader obeys
    // it and ignores VRAM. Validated with the loader's own parser, so a pin it
    // would reject produces no `pinned` allocation here either — the report then
    // names the pin as INVALID rather than previewing a cut nobody honours.
    //
    // Judged against live free when we have it: the question a pinned split raises
    // is not "how should the blocks be divided" (that is settled) but "does the
    // share the operator pinned still fit the memory available".
    let pin_counts = block_split_pin
        .as_deref()
        .and_then(|raw| inf::parse_block_split(raw, n_layer, devices_gb.len()));
    let pinned = pin_counts.as_ref().map(|counts| {
        let basis_gb = devices_free_gb
            .as_ref()
            .filter(|f| f.len() == devices_gb.len())
            .unwrap_or(&devices_gb);
        allocate(CapacityBasis::Pinned, basis_gb, Some(counts))
    });

    // Block-mass uniformity → the "does heterogeneity stay safe" verdict.
    let nz: Vec<u64> = mass
        .block_bytes
        .iter()
        .copied()
        .filter(|&b| b > 0)
        .collect();
    let bmin = nz.iter().copied().min().unwrap_or(0);
    let bmax = nz.iter().copied().max().unwrap_or(0);
    let bmean = if nz.is_empty() {
        0
    } else {
        nz.iter().sum::<u64>() / nz.len() as u64
    };
    let spread = if bmin > 0 {
        bmax as f64 / bmin as f64
    } else {
        1.0
    };
    let block_mass = BlockMass {
        min: bmin,
        max: bmax,
        mean: bmean,
        spread,
        uniform: spread <= 1.15,
    };

    // Hot = resident mass touched every token: all block bytes minus the cold
    // routed experts, plus the output head (token_embd lives in host RAM).
    let moe = if mass.routed_expert_bytes > 0 {
        Some(MoeReport {
            routed_expert_bytes: mass.routed_expert_bytes,
            hot_bytes: mass
                .block_bytes
                .iter()
                .sum::<u64>()
                .saturating_sub(mass.routed_expert_bytes)
                + mass.head_bytes,
        })
    } else {
        None
    };

    // Speed is looked up against the cut that would ACTUALLY run — `safe_now`
    // when a live reading gave us one, else the possible basis.
    //
    // This is the fix for a silent, total miss. A measurement is filed under the
    // placement that produced it; if the plan predicts a different cut it queries
    // a key nothing will ever be stored at, and reports "not measured" about a
    // configuration it holds a real number for. Observed 2026-07-29 on the very
    // mesh whose 10.48 tok/s two-node record had just been written: totals said
    // 14/34, the loader ran 12/36, so the lookup missed by construction.
    // Precedence: a pin wins (the loader obeys it), else live free, else totals.
    let executed = pinned.as_ref().or(safe_now.as_ref()).unwrap_or(&possible);
    let (speed, speed_key) = resolve_speed(
        &executed.rows,
        mesh.as_deref(),
        &sizes,
        n_layer,
        executed.nodes.active_nodes,
        n_ctx,
        measurements,
        peers,
        current_build,
    );

    let Allocation {
        rows,
        pooled,
        gate_pass,
        nodes,
        ..
    } = possible;

    PlanReport {
        model_name,
        n_layer,
        total_weight,
        output_bytes: mass.head_bytes,
        embd_bytes: mass.embd_bytes,
        block_mass,
        moe,
        headroom,
        headroom_from_flag,
        pooled,
        gate_need,
        gate_pass,
        rows,
        nodes,
        safe_now,
        pinned,
        block_split_pin,
        speed,
        speed_key,
    }
}

/// Decide what this plan may say about speed.
///
/// Pure, and deliberately conservative at every branch. The three outcomes are
/// distinct on purpose: "measured" is a fact, "not measured" is an invitation,
/// and "not measurable" means the question does not apply to what was asked.
/// Collapsing the last two would tell a `--devices` user to run a benchmark
/// that could not produce a record matching their query.
#[allow(clippy::too_many_arguments)]
fn resolve_speed(
    rows: &[DeviceRow],
    mesh: Option<&[MeshDevice]>,
    sizes: &[(String, Option<u32>, u64)],
    n_layer: u32,
    active_nodes: usize,
    n_ctx: u32,
    measurements: &crate::mesh_measurements::MeasurementFile,
    peers: &[crate::mesh_measurements::ForeignRecord],
    current_build: &str,
) -> (
    SpeedSection,
    Option<crate::mesh_measurements::MeasurementKey>,
) {
    use crate::mesh_measurements as mm;

    // A hypothetical mesh has no machines to have measured.
    let Some(mesh) = mesh else {
        return (
            SpeedSection::NotMeasurable(NotMeasurable::HypotheticalDevices),
            None,
        );
    };

    // The host must be identifiable, or there is no key. Substituting a
    // placeholder would collide every unidentified host into one bucket and
    // serve one machine's number on another.
    let host_fp = rows
        .iter()
        .find(|r| r.is_host)
        .and_then(|r| mesh.get(r.dev))
        .and_then(|d| d.hw_fingerprint);
    let Some(host) = mm::HostIdentity::from_live_mesh(host_fp) else {
        return (
            SpeedSection::NotMeasurable(NotMeasurable::HostUnidentified),
            None,
        );
    };

    // Only the devices that actually hold something. A machine that was
    // apportioned no blocks is not part of the placement — it changes nothing
    // about how the model decodes — and including it would make the digest
    // depend on which idle peers happened to be online. It would also put this
    // side permanently out of step with `mesh bench`, which builds its shards
    // from what the daemon reports is loaded and has no idle device to report.
    // A key the producer can never reproduce is a key that never matches.
    //
    // Each shard is keyed on the machine's hardware as well as its name — a
    // peer that swaps a GPU must not keep answering with the number the old one
    // produced. A peer too old to advertise a fingerprint is reported rather
    // than keyed on its name alone, which mirrors `mesh bench`'s refusal to
    // file such a run: neither side invents an identity the other cannot check.
    let mut shards: Vec<mm::PlacementShard> = Vec::new();
    // What each of those machines *is*, so a near miss can say how a measured
    // configuration differs from this one in terms the reader can weigh. Purely
    // descriptive — see `mm::MachineWitness`; it is never hashed, so improving
    // what a peer advertises cannot orphan the records naming it.
    let mut machines: Vec<mm::MachineWitness> = Vec::new();
    for r in rows.iter().filter(|r| r.blocks.is_some() || r.holds_output) {
        let dev = mesh.get(r.dev);
        let name = dev
            .map(|d| d.name.clone())
            .unwrap_or_else(|| format!("dev{}", r.dev));
        let Some(hw) = dev.and_then(|d| d.hw_fingerprint) else {
            return (
                SpeedSection::NotMeasurable(NotMeasurable::PeerUnidentified { name }),
                None,
            );
        };
        machines.push(mm::MachineWitness {
            node_key: name.clone(),
            vram_gb: dev.map(|d| d.vram_gb.round() as u32).unwrap_or(0),
            backend: dev.and_then(|d| d.backend.clone()),
        });
        shards.push(mm::PlacementShard {
            node_key: name,
            hw: Some(hw),
            blocks: r.blocks,
            holds_output: r.holds_output,
        });
    }
    let mode = if active_nodes <= 1 {
        "local"
    } else {
        "distributed"
    };

    // The link, over the same devices the digest describes and excluding the
    // host (which has no link to itself). A peer carrying weight but with no
    // discovered worker classifies `Unknown`, which `lookup` refuses — the plan
    // then says "not measured" instead of quoting a number taken over a link it
    // cannot confirm this placement would use.
    let worker_links: Vec<mm::LinkClass> = rows
        .iter()
        .filter(|r| !r.is_host && (r.blocks.is_some() || r.holds_output))
        .map(|r| {
            mesh.get(r.dev)
                .and_then(|d| d.link)
                .unwrap_or(mm::LinkClass::Unknown)
        })
        .collect();
    let link = mm::LinkClass::summarize(&worker_links);

    // The same three values the digest is built from, so this plan can describe
    // itself to the reader on the other side of a near miss.
    let witness = mm::PlacementWitness {
        mode: mode.to_string(),
        total_blocks: n_layer,
        shards: shards.clone(),
        machines,
    };
    let key = mm::MeasurementKey::for_plan(
        host,
        mm::model_fingerprint(sizes, n_layer),
        mm::placement_digest(mode, n_layer, &shards),
        n_ctx,
        link,
    );
    debug_assert!(
        witness.explains(&key.placement_digest),
        "the witness and the key were built from different inputs"
    );

    let section = match mm::lookup(measurements, &key, current_build) {
        Some(summary) => SpeedSection::Measured {
            summary: Box::new(summary),
        },
        None => SpeedSection::NotMeasured {
            near: mm::near_misses(measurements, peers, &key, Some(&witness)),
        },
    };
    (section, Some(key))
}
