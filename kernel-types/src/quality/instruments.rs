// SPDX-License-Identifier: AGPL-3.0-or-later
//! Schema + reader for `quality/instruments.toml` — the declared registry of
//! every instrument that verifies anything in this repo.
//!
//! WHY IT EXISTS. Before this file the quality surface was eleven private
//! lists that never read each other: two git hooks, an xtask table, three CI
//! jobs, two shell harnesses, a scheduler, a posture command, a `package.json`
//! and a prose doc. None could answer "what verifies this repo", so nothing
//! could answer "what verifies it NOWHERE" — and
//! `sovereign-desktop/QUALITY_SURFACE.md` records the cost in its own
//! postmortem: `wizard-verify.sh`, the only coverage of the packaged boot
//! chain, sat on no executable list for ten days after catching a
//! ship-blocking bug on its first run.
//!
//! Every other open set in this repo already became a registry
//! (`env-flags.toml`, `operational-anchors.toml`, `arch-probes.toml`,
//! `requirements.toml`, `check-lanes.toml`). This is the last one that was
//! prose. ARCH §4: open sets are registries.
//!
//! WHAT IS CLOSED HERE, and what deliberately is not. [`Kind`],
//! [`Enforcement`], [`Fidelity`], [`Precondition`], [`RunsIn`] and
//! [`BaselineKind`] are closed sums — an unknown word is a parse ERROR naming
//! the row, never a silently-dropped field (ARCH §18.3). `command`, `doc` and
//! `negative_control` are open text, because they name things outside this
//! schema's authority.

use std::collections::BTreeMap;

/// One instrument: something you can run that renders a verdict about this
/// repo, plus everything a reader needs to decide whether its green is worth
/// anything.
#[derive(Clone, Debug, PartialEq)]
pub struct Instrument {
    /// Stable id — identity from essence (ARCH §7.5), never a position in the
    /// file. It is the key `svrn quality map` sorts by and the name a gate
    /// failure prints.
    pub id: String,
    pub kind: Kind,
    /// The literal command a human types. This is also the census key: the
    /// closure gate matches an observed invocation against these strings, so
    /// a command written differently here than it is invoked is a gate
    /// failure, not a cosmetic difference.
    pub command: String,
    pub cost: Cost,
    pub enforcement: Enforcement,
    pub fidelity: Fidelity,
    pub preconditions: Vec<Precondition>,
    pub baseline: Baseline,
    /// The mutant (`quality/sabotage/*.toml`) or `control`-kind instrument
    /// that has been watched making this one go red. `None` is the honest
    /// answer for most rows today and is what posture counts — a check with
    /// no failing input you can name is ARCH §18.1's first smell.
    pub negative_control: Option<String>,
    /// Every place that runs it. EMPTY IS LEGAL AND IS THE POINT: an
    /// instrument nothing runs is the finding this registry exists to make
    /// visible, so it is representable rather than unstateable.
    pub runs_in: Vec<RunsIn>,
    /// The owning section a reader should open for the "why".
    pub doc: String,
    /// Other spellings of the same command at real call sites — `npx
    /// playwright test` for what `package.json` calls `npm run test:e2e`,
    /// `cargo run -p xtask -- api-gate` for `cargo xtask api-gate`. Declared
    /// rather than guessed: the closure gate matches an OBSERVED invocation
    /// against these strings, and a gate that infers equivalence would be
    /// deciding on its own that two commands are one instrument.
    pub also_invoked_as: Vec<String>,
    /// Flags and env knobs that change what this instrument PROVES — the
    /// half `QUALITY_SURFACE.md` carried as prose because no schema could
    /// hold it. Optional; most instruments have none.
    pub load_bearing: Vec<LoadBearing>,
}

/// A flag whose absence silently weakens an instrument.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadBearing {
    pub flag: String,
    pub why: String,
}

/// What an instrument IS. Closed: adding a seventh shape is a schema change
/// with a reviewable diff, not a new string somebody typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// Pass/fail on the repo as it stands. A red one means "your code".
    Gate,
    /// A body of tests run together.
    Suite,
    /// Produces numbers compared against a baseline or a band.
    Bench,
    /// Observes and reports; no verdict of its own to fail on.
    Probe,
    /// A NEGATIVE control — breaks something on purpose and requires the
    /// instrument above it to notice. The only kind that measures what the
    /// others would CATCH rather than what they reached.
    Control,
    /// A composed lane runner with its own lane table (`svrn quality check`).
    Check,
}

/// Whether a not-passed verdict may fail the run that hosts it. Matches the
/// three words `check-lanes.toml` and `quality_cmd.rs` already use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Enforcement {
    Hard,
    Advisory,
    Tracked,
}

/// How far an instrument sits from what a user actually runs — generalised
/// from `QUALITY_SURFACE.md`'s desktop table so a daemon lane and a Playwright
/// suite can be compared on one axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Fidelity {
    /// Unit: no process boundary, no backend.
    F0,
    /// Mocked backend — real frontend or real caller, fabricated answers.
    F1,
    /// Real binary against a fixture daemon.
    F2,
    /// Real daemon, real models.
    F3,
    /// A supervised child process.
    F4,
    /// The packaged boot chain a shipped install takes.
    F5,
}

/// What must be true before an instrument can judge anything. The four wire
/// spellings are `quality/check-lanes.toml`'s
/// (`quality_check_cmd::Precondition`); `container:` and `host-quiet` are
/// declared here FIRST because the shell harnesses need them and the lane
/// runner does not yet have them. Phase 1 merges the two tables and the
/// superset becomes the one closed set (ARCH §10.6 — stated here so the
/// duplication is a scheduled merge rather than a discovery).
///
/// No `Eq`/`Ord`: the load bound is an `f64`, and a total order over a type
/// holding a float would be a lie. Preconditions are declared in the order the
/// author wrote them and rendered that way.
#[derive(Clone, Debug, PartialEq, PartialOrd)]
pub enum Precondition {
    PortListening(u16),
    SlotDecodes(String),
    CorpusInstalled(String),
    Binary(String),
    /// A toolbox/container the command must run inside.
    Container(String),
    /// The host's 1-minute load average is at or under this bound.
    ///
    /// The ARGUMENT is not decoration. A latency bar measured on a contended
    /// box is could-not-judge, never failed, and the bound has to be per-lane:
    /// the same binary and bank produced 50.7 tok/s at load 3.7 and 17.8 at
    /// load 32 on this host (note d596639c). Wire form and semantics are
    /// `quality_check_cmd::Precondition::HostQuiet`'s — this schema follows
    /// the runner rather than inventing a bare spelling beside it (ARCH §10.6).
    HostQuiet(f64),
}

/// What an instrument compares against, and in what currency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Baseline {
    pub path: Option<String>,
    pub kind: BaselineKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BaselineKind {
    /// A ratchet file of counts that may only shrink.
    Count,
    /// Recorded measurements compared with a band.
    Metrics,
    /// Nothing — the instrument's verdict is absolute.
    None,
}

/// Where an instrument runs. The open half (`ci:<job>`, `weekly:<job>`,
/// `smoke:<phase>`) carries a name because "in CI" is not a fact you can act
/// on; "in `ci:gates`" is.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RunsIn {
    Prepush,
    Precommit,
    Ci(String),
    Weekly(String),
    Smoke(String),
    /// A lane of `svrn quality check`.
    Check,
    /// The CLI-contract nightly lane.
    Nightly,
    /// Fired at login by `scripts/run-if-stale.sh` when its marker is stale.
    RunIfStale,
    /// A human, when they remember. The verdict this registry exists to count.
    ByHand,
}

/// What an instrument costs. `Unmeasured` is a first-class value, not a
/// missing field: a cost nobody has timed is a fact about the instrument
/// (ARCH §18.3 — absence is reported, never defaulted).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cost {
    Secs(f64),
    Unmeasured,
}

/// A command reachable from a scanned surface that verifies nothing — a dev
/// server, a lister, a build step. Declared WITH a reason so the closure gate
/// has no silent skip-list of its own (the failure mode that let four
/// harnesses sit off every map).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotAnInstrument {
    pub key: String,
    pub why: String,
}

/// The parsed registry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Registry {
    pub instruments: Vec<Instrument>,
    pub not_instruments: Vec<NotAnInstrument>,
    /// The files the closure gate censuses, declared HERE rather than
    /// hardcoded in the gate: how far a gate reaches is policy, and policy in
    /// this repo is data (ARCH §6). It is also the field a reader checks when
    /// they want to know whether a surface is actually covered.
    pub censused_surfaces: Vec<String>,
}

/// The line posture prints every session — the closure trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Coverage {
    pub total: usize,
    pub with_negative_control: usize,
    pub unmeasured_cost: usize,
    pub by_hand_only: usize,
}

impl Instrument {
    /// True when some CI job runs it.
    pub fn in_ci(&self) -> bool {
        self.runs_in.iter().any(|r| matches!(r, RunsIn::Ci(_)))
    }

    /// True when a human is the ONLY thing that runs it — the population the
    /// `wizard-verify.sh` postmortem is about.
    pub fn by_hand_only(&self) -> bool {
        self.runs_in.as_slice() == [RunsIn::ByHand]
    }
}

impl Registry {
    /// Parse and validate. Returns EVERY error, not the first: a registry with
    /// four bad rows should cost one round-trip to fix, not four.
    pub fn parse(text: &str) -> Result<Registry, Vec<String>> {
        let value: toml::Value = text.parse().map_err(|e| vec![format!("parse: {e}")])?;
        let mut errors = Vec::new();
        let mut instruments = Vec::new();
        let empty = Vec::new();

        // A file with NO `[[instrument]]` rows is a broken instrument, not an
        // empty repo: it is the shape a mistyped table header takes
        // (`[[instruments]]`), and defaulting it to an empty registry would
        // make the closure gate report every command in the repo as
        // unregistered while the real fault is one character (ARCH §18.3).
        let rows = match value.get("instrument") {
            Some(toml::Value::Array(rows)) if !rows.is_empty() => rows,
            Some(toml::Value::Array(_)) | None => {
                return Err(vec![
                    "no `[[instrument]]` rows — an instrument registry with nothing in it is a \
                     broken parse (check the table header spelling), not an empty repo"
                        .to_string(),
                ])
            }
            Some(_) => return Err(vec!["`instrument` must be an array of tables".to_string()]),
        };
        for (i, row) in rows.iter().enumerate() {
            match instrument(row, i) {
                Ok(inst) => instruments.push(inst),
                Err(mut e) => errors.append(&mut e),
            }
        }

        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        for (i, inst) in instruments.iter().enumerate() {
            if let Some(first) = seen.insert(inst.id.as_str(), i) {
                errors.push(format!(
                    "instrument `{}` declared twice (rows {} and {})",
                    inst.id,
                    first + 1,
                    i + 1
                ));
            }
        }

        // Zero exemptions IS a legal state — unlike zero instruments, it is
        // what a registry that exempts nothing looks like.
        let mut not_instruments = Vec::new();
        let rows = value
            .get("not_instrument")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty);
        for (i, row) in rows.iter().enumerate() {
            let key = str_field(row, "key");
            let why = str_field(row, "why");
            match (key, why) {
                (Some(key), Some(why)) if !why.trim().is_empty() => {
                    not_instruments.push(NotAnInstrument { key, why })
                }
                (Some(key), _) => errors.push(format!(
                    "[[not_instrument]] `{key}`: `why` is required and must say something — an \
                     exemption with no reason is a silent skip-list"
                )),
                (None, _) => errors.push(format!("[[not_instrument]] #{}: missing `key`", i + 1)),
            }
        }

        let censused_surfaces: Vec<String> = value
            .get("censused_surfaces")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if censused_surfaces.is_empty() {
            errors.push(
                "missing `censused_surfaces` — a closure gate whose reach is unstated cannot be \
                 reviewed, and an empty reach finds nothing while reporting green (ARCH §18.1)"
                    .to_string(),
            );
        }

        if errors.is_empty() {
            Ok(Registry {
                instruments,
                not_instruments,
                censused_surfaces,
            })
        } else {
            Err(errors)
        }
    }

    /// The coverage line's four numbers, computed ONCE here so posture and
    /// the map cannot disagree about what "covered" means (ARCH §10.6).
    pub fn coverage(&self) -> Coverage {
        Coverage {
            total: self.instruments.len(),
            with_negative_control: self
                .instruments
                .iter()
                .filter(|i| i.negative_control.is_some())
                .count(),
            unmeasured_cost: self
                .instruments
                .iter()
                .filter(|i| i.cost == Cost::Unmeasured)
                .count(),
            by_hand_only: self.instruments.iter().filter(|i| i.by_hand_only()).count(),
        }
    }

    /// Instruments grouped by fidelity, each group id-sorted — the shape both
    /// the fidelity table and the layer table render from.
    pub fn by_fidelity(&self) -> BTreeMap<Fidelity, Vec<&Instrument>> {
        let mut out: BTreeMap<Fidelity, Vec<&Instrument>> = BTreeMap::new();
        for i in &self.instruments {
            out.entry(i.fidelity).or_default().push(i);
        }
        for v in out.values_mut() {
            v.sort_by(|a, b| a.id.cmp(&b.id));
        }
        out
    }

    /// Every declared venue → the instruments that run there. A venue with no
    /// instruments does not appear; an instrument with no venue appears in no
    /// group, which is why [`Registry::nowhere`] exists beside this.
    pub fn by_venue(&self) -> BTreeMap<String, Vec<&Instrument>> {
        let mut out: BTreeMap<String, Vec<&Instrument>> = BTreeMap::new();
        for i in &self.instruments {
            for r in &i.runs_in {
                out.entry(r.label()).or_default().push(i);
            }
        }
        for v in out.values_mut() {
            v.sort_by(|a, b| a.id.cmp(&b.id));
        }
        out
    }

    /// Instruments no CI job runs — `QUALITY_SURFACE.md`'s "What CI does not
    /// run" section, as a query instead of a paragraph.
    pub fn not_in_ci(&self) -> Vec<&Instrument> {
        let mut v: Vec<&Instrument> = self.instruments.iter().filter(|i| !i.in_ci()).collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    /// Instruments nothing at all runs — not CI, not a hook, not a harness,
    /// not even a declared by-hand step. The empty `runs_in` population.
    pub fn nowhere(&self) -> Vec<&Instrument> {
        let mut v: Vec<&Instrument> = self
            .instruments
            .iter()
            .filter(|i| i.runs_in.is_empty())
            .collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    /// Every instrument carrying at least one load-bearing flag, id-sorted.
    pub fn load_bearing(&self) -> Vec<&Instrument> {
        let mut v: Vec<&Instrument> = self
            .instruments
            .iter()
            .filter(|i| !i.load_bearing.is_empty() || !i.preconditions.is_empty())
            .collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    pub fn get(&self, id: &str) -> Option<&Instrument> {
        self.instruments.iter().find(|i| i.id == id)
    }
}

// ─── Row parsing ────────────────────────────────────────────────────

fn str_field(row: &toml::Value, key: &str) -> Option<String> {
    row.get(key).and_then(|v| v.as_str()).map(String::from)
}

/// Read one `[[instrument]]` row. Reports EVERY problem in the row, because a
/// registry with four bad rows should cost one round-trip to fix.
fn instrument(row: &toml::Value, index: usize) -> Result<Instrument, Vec<String>> {
    let mut errors = Vec::new();
    // Only the error PREFIX falls back to a row number; the missing `id`
    // itself is pushed as its own error below, never defaulted away.
    let id = str_field(row, "id").unwrap_or_else(|| format!("#{}", index + 1));
    let at = |what: &str| format!("instrument `{id}`: {what}");

    if str_field(row, "id").is_none() {
        errors.push(at("missing `id`"));
    }
    let kind = enum_field(row, "kind", &id, &mut errors, Kind::parse, KIND_WORDS);
    let command = required(row, "command", &id, &mut errors);
    let doc = required(row, "doc", &id, &mut errors);
    let enforcement = enum_field(
        row,
        "enforcement",
        &id,
        &mut errors,
        Enforcement::parse,
        ENFORCEMENT_WORDS,
    );
    let fidelity = enum_field(row, "fidelity", &id, &mut errors, Fidelity::parse, "F0..F5");

    let cost = match row.get("cost_secs") {
        Some(toml::Value::Float(f)) => Some(Cost::Secs(*f)),
        Some(toml::Value::Integer(n)) => Some(Cost::Secs(*n as f64)),
        Some(toml::Value::String(s)) if s == "unmeasured" => Some(Cost::Unmeasured),
        Some(other) => {
            errors.push(at(&format!(
                "cost_secs must be a number of seconds or the string \"unmeasured\", got {other}"
            )));
            None
        }
        None => {
            errors.push(at(
                "missing `cost_secs` — an untimed instrument declares \"unmeasured\", it does not \
                 omit the field",
            ));
            None
        }
    };

    let mut preconditions = Vec::new();
    for s in string_list(row, "preconditions") {
        match Precondition::parse(&s) {
            Some(p) => preconditions.push(p),
            None => errors.push(at(&format!(
                "precondition `{s}` is not one of {PRECONDITION_WORDS}"
            ))),
        }
    }

    let mut runs_in = Vec::new();
    match row.get("runs_in") {
        Some(toml::Value::Array(_)) => {
            for s in string_list(row, "runs_in") {
                match RunsIn::parse(&s) {
                    Some(r) => runs_in.push(r),
                    None => {
                        errors.push(at(&format!("runs_in `{s}` is not one of {RUNS_IN_WORDS}")))
                    }
                }
            }
        }
        Some(_) => errors.push(at("runs_in must be an array")),
        None => errors.push(at(
            "missing `runs_in` — an instrument nothing runs declares `runs_in = []`, which is a \
             finding, not an omission",
        )),
    }

    let baseline = baseline_field(row, &id, &mut errors);
    let negative_control = negative_control_field(row, &id, &mut errors);

    let also_invoked_as = string_list(row, "also_invoked_as");

    let mut load_bearing = Vec::new();
    if let Some(toml::Value::Array(rows)) = row.get("load_bearing") {
        for lb in rows {
            match (str_field(lb, "flag"), str_field(lb, "why")) {
                (Some(flag), Some(why)) => load_bearing.push(LoadBearing { flag, why }),
                _ => errors.push(at("each load_bearing entry needs `flag` and `why`")),
            }
        }
    }

    // Every `None` arm above pushed an error, so a complete row here is exactly
    // an empty error list. The `else` restates that rather than unwrapping.
    let (
        Some(kind),
        Some(command),
        Some(doc),
        Some(enforcement),
        Some(fidelity),
        Some(cost),
        Some(baseline),
    ) = (kind, command, doc, enforcement, fidelity, cost, baseline)
    else {
        if errors.is_empty() {
            errors.push(at(
                "a required field was absent and reported nothing — parser bug",
            ));
        }
        return Err(errors);
    };
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(Instrument {
        id,
        kind,
        command,
        cost,
        enforcement,
        fidelity,
        preconditions,
        baseline,
        negative_control,
        runs_in,
        doc,
        also_invoked_as,
        load_bearing,
    })
}

const KIND_WORDS: &str = "gate|suite|bench|probe|control|check";
const ENFORCEMENT_WORDS: &str = "hard|advisory|tracked";
const PRECONDITION_WORDS: &str = "port-listening:<port>, slot-decodes:<slot>, \
     corpus-installed:<id>, binary:<name>, container:<name>, host-quiet";
const RUNS_IN_WORDS: &str = "prepush, precommit, ci:<job>, weekly:<job>, smoke:<phase>, \
     check, nightly, run-if-stale, by-hand";

fn required(row: &toml::Value, key: &str, id: &str, errors: &mut Vec<String>) -> Option<String> {
    match str_field(row, key) {
        Some(v) => Some(v),
        None => {
            errors.push(format!("instrument `{id}`: missing `{key}`"));
            None
        }
    }
}

/// A closed-set field: absent is one error, unknown is another, and the
/// legal words are named in BOTH so a red is self-serviceable.
fn enum_field<T>(
    row: &toml::Value,
    key: &str,
    id: &str,
    errors: &mut Vec<String>,
    parse: fn(&str) -> Option<T>,
    words: &str,
) -> Option<T> {
    let raw = required(row, key, id, errors)?;
    match parse(&raw) {
        Some(v) => Some(v),
        None => {
            errors.push(format!(
                "instrument `{id}`: {key} `{raw}` is not one of {words}"
            ));
            None
        }
    }
}

fn baseline_field(row: &toml::Value, id: &str, errors: &mut Vec<String>) -> Option<Baseline> {
    let at = |what: &str| format!("instrument `{id}`: {what}");
    let Some(v) = row.get("baseline") else {
        errors.push(at(
            "missing `baseline` (use `{ kind = \"none\" }` when there is none)",
        ));
        return None;
    };
    let Some(kind) = BaselineKind::parse(v.get("kind").and_then(|k| k.as_str()).unwrap_or(""))
    else {
        errors.push(at("baseline.kind must be one of count|metrics|none"));
        return None;
    };
    let path = v.get("path").and_then(|p| p.as_str()).map(String::from);
    if kind != BaselineKind::None && path.is_none() {
        errors.push(at(
            "baseline.kind is not `none` but no baseline.path is given",
        ));
        return None;
    }
    if kind == BaselineKind::None && path.is_some() {
        errors.push(at("baseline.kind = `none` but a baseline.path is given"));
        return None;
    }
    Some(Baseline { path, kind })
}

/// `none` is a WORD here, not an omission: the count of instruments with no
/// negative control is the number posture prints, so leaving the field out
/// would let a row dodge being counted (ARCH §18.3).
fn negative_control_field(row: &toml::Value, id: &str, errors: &mut Vec<String>) -> Option<String> {
    match str_field(row, "negative_control") {
        Some(s) if s == "none" => None,
        Some(s) if s.trim().is_empty() => {
            errors.push(format!(
                "instrument `{id}`: negative_control must name a mutant id, a control instrument, \
                 or the literal \"none\" — an empty string is neither"
            ));
            None
        }
        Some(s) => Some(s),
        None => {
            errors.push(format!(
                "instrument `{id}`: missing `negative_control` — say \"none\" and be counted, \
                 rather than leaving it unstated"
            ));
            None
        }
    }
}

fn string_list(row: &toml::Value, key: &str) -> Vec<String> {
    row.get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

// ─── Closed-set wire forms ──────────────────────────────────────────

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "gate" => Kind::Gate,
            "suite" => Kind::Suite,
            "bench" => Kind::Bench,
            "probe" => Kind::Probe,
            "control" => Kind::Control,
            "check" => Kind::Check,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Gate => "gate",
            Kind::Suite => "suite",
            Kind::Bench => "bench",
            Kind::Probe => "probe",
            Kind::Control => "control",
            Kind::Check => "check",
        }
    }
}

impl Enforcement {
    pub fn parse(s: &str) -> Option<Enforcement> {
        Some(match s {
            "hard" => Enforcement::Hard,
            "advisory" => Enforcement::Advisory,
            "tracked" => Enforcement::Tracked,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Enforcement::Hard => "hard",
            Enforcement::Advisory => "advisory",
            Enforcement::Tracked => "tracked",
        }
    }
}

impl Fidelity {
    pub fn parse(s: &str) -> Option<Fidelity> {
        Some(match s {
            "F0" => Fidelity::F0,
            "F1" => Fidelity::F1,
            "F2" => Fidelity::F2,
            "F3" => Fidelity::F3,
            "F4" => Fidelity::F4,
            "F5" => Fidelity::F5,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Fidelity::F0 => "F0",
            Fidelity::F1 => "F1",
            Fidelity::F2 => "F2",
            Fidelity::F3 => "F3",
            Fidelity::F4 => "F4",
            Fidelity::F5 => "F5",
        }
    }

    /// What the level MEANS, once, so the render cannot re-invent it.
    pub fn meaning(self) -> &'static str {
        match self {
            Fidelity::F0 => "unit — no process boundary, no backend",
            Fidelity::F1 => "mocked backend — real caller, fabricated answers",
            Fidelity::F2 => "real binary against a fixture daemon",
            Fidelity::F3 => "real daemon, real models",
            Fidelity::F4 => "a supervised child process",
            Fidelity::F5 => "the packaged boot chain a shipped install takes",
        }
    }
}

impl BaselineKind {
    pub fn parse(s: &str) -> Option<BaselineKind> {
        Some(match s {
            "count" => BaselineKind::Count,
            "metrics" => BaselineKind::Metrics,
            "none" => BaselineKind::None,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            BaselineKind::Count => "count",
            BaselineKind::Metrics => "metrics",
            BaselineKind::None => "none",
        }
    }
}

impl Precondition {
    pub fn parse(s: &str) -> Option<Precondition> {
        let (head, arg) = s.split_once(':')?;
        if arg.is_empty() {
            return None;
        }
        Some(match head {
            "port-listening" => Precondition::PortListening(arg.parse().ok()?),
            "slot-decodes" => Precondition::SlotDecodes(arg.to_string()),
            "corpus-installed" => Precondition::CorpusInstalled(arg.to_string()),
            "binary" => Precondition::Binary(arg.to_string()),
            "container" => Precondition::Container(arg.to_string()),
            // Same refusal the runner makes: a non-positive or non-finite
            // bound is not a load ceiling.
            "host-quiet" => {
                let max: f64 = arg.parse().ok()?;
                if !(max.is_finite() && max > 0.0) {
                    return None;
                }
                Precondition::HostQuiet(max)
            }
            _ => return None,
        })
    }

    pub fn label(&self) -> String {
        match self {
            Precondition::PortListening(p) => format!("port-listening:{p}"),
            Precondition::SlotDecodes(s) => format!("slot-decodes:{s}"),
            Precondition::CorpusInstalled(s) => format!("corpus-installed:{s}"),
            Precondition::Binary(s) => format!("binary:{s}"),
            Precondition::Container(s) => format!("container:{s}"),
            Precondition::HostQuiet(max) => format!("host-quiet:{max}"),
        }
    }
}

impl RunsIn {
    pub fn parse(s: &str) -> Option<RunsIn> {
        if let Some(job) = s.strip_prefix("ci:") {
            return (!job.is_empty()).then(|| RunsIn::Ci(job.to_string()));
        }
        if let Some(job) = s.strip_prefix("weekly:") {
            return (!job.is_empty()).then(|| RunsIn::Weekly(job.to_string()));
        }
        if let Some(phase) = s.strip_prefix("smoke:") {
            return (!phase.is_empty()).then(|| RunsIn::Smoke(phase.to_string()));
        }
        Some(match s {
            "prepush" => RunsIn::Prepush,
            "precommit" => RunsIn::Precommit,
            "check" => RunsIn::Check,
            "nightly" => RunsIn::Nightly,
            "run-if-stale" => RunsIn::RunIfStale,
            "by-hand" => RunsIn::ByHand,
            _ => return None,
        })
    }

    pub fn label(&self) -> String {
        match self {
            RunsIn::Prepush => "prepush".to_string(),
            RunsIn::Precommit => "precommit".to_string(),
            RunsIn::Ci(j) => format!("ci:{j}"),
            RunsIn::Weekly(j) => format!("weekly:{j}"),
            RunsIn::Smoke(p) => format!("smoke:{p}"),
            RunsIn::Check => "check".to_string(),
            RunsIn::Nightly => "nightly".to_string(),
            RunsIn::RunIfStale => "run-if-stale".to_string(),
            RunsIn::ByHand => "by-hand".to_string(),
        }
    }
}

impl Cost {
    /// Rendered for a human. `unmeasured` is a word, never a zero — a cost of
    /// 0 s and a cost nobody timed are different claims.
    pub fn label(self) -> String {
        match self {
            Cost::Secs(s) if s >= 60.0 => format!("{:.0}m", s / 60.0),
            Cost::Secs(s) if s >= 1.0 => format!("{s:.0}s"),
            // Two decimals below a second: a 0.04s gate rendered as "0.0s"
            // reads as free, and "free" is how a gate stops being priced.
            Cost::Secs(s) => format!("{s:.2}s"),
            Cost::Unmeasured => "unmeasured".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A complete row, so every refusal below differs from it in exactly one
    /// way and the test names what that way is.
    const GOOD: &str = r#"
[[instrument]]
id = "arch-gate"
kind = "gate"
command = "cargo xtask arch-gate"
cost_secs = 4.2
enforcement = "hard"
fidelity = "F0"
preconditions = ["container:sovereign-vulkan"]
baseline = { kind = "count", path = "quality/baselines/oversized.txt" }
negative_control = "none"
runs_in = ["prepush", "ci:gates"]
doc = "ARCH_PRINCIPLES.md §3.1"
"#;

    /// Every fixture needs the declared reach, or `Registry::parse` refuses
    /// it — which is itself the point of the field.
    fn with_surfaces(rows: &str) -> String {
        format!("censused_surfaces = [\".github/workflows/ci.yml\"]\n{rows}")
    }

    fn parse(text: &str) -> Registry {
        match Registry::parse(&with_surfaces(text)) {
            Ok(r) => r,
            Err(e) => panic!("expected a clean parse, got {e:?}"),
        }
    }

    fn errors(text: &str) -> Vec<String> {
        match Registry::parse(&with_surfaces(text)) {
            Ok(_) => panic!("expected a refusal, got a clean parse"),
            Err(e) => e,
        }
    }

    /// A gate whose reach is unstated cannot be reviewed, and an empty reach
    /// finds nothing while reporting green — the §18.1 shape this whole
    /// registry exists to end.
    #[test]
    fn a_registry_that_declares_no_censused_surfaces_refuses() {
        let e = match Registry::parse(GOOD) {
            Ok(_) => panic!("a registry with no declared reach must refuse"),
            Err(e) => e,
        };
        assert!(e.iter().any(|m| m.contains("censused_surfaces")), "{e:?}");
    }

    #[test]
    fn an_alias_is_declared_never_inferred() {
        let text = GOOD.replace(
            r#"command = "cargo xtask arch-gate""#,
            "command = \"cargo xtask arch-gate\"\nalso_invoked_as = [\"./target/debug/xtask arch-gate\"]",
        );
        assert_eq!(
            parse(&text).instruments[0].also_invoked_as,
            vec!["./target/debug/xtask arch-gate".to_string()]
        );
        // Absent means absent — nothing is guessed from the command string.
        assert!(parse(GOOD).instruments[0].also_invoked_as.is_empty());
    }

    #[test]
    fn a_complete_row_round_trips_every_field() {
        let r = parse(GOOD);
        let i = &r.instruments[0];
        assert_eq!(i.id, "arch-gate");
        assert_eq!(i.kind, Kind::Gate);
        assert_eq!(i.cost, Cost::Secs(4.2));
        assert_eq!(i.enforcement, Enforcement::Hard);
        assert_eq!(i.fidelity, Fidelity::F0);
        assert_eq!(
            i.preconditions,
            vec![Precondition::Container("sovereign-vulkan".into())]
        );
        assert_eq!(i.baseline.kind, BaselineKind::Count);
        assert_eq!(i.negative_control, None);
        assert_eq!(i.runs_in, vec![RunsIn::Prepush, RunsIn::Ci("gates".into())]);
        assert!(i.in_ci());
        assert!(!i.by_hand_only());
    }

    /// Each closed set REFUSES an unknown word rather than dropping the field
    /// — the whole reason they are enums (ARCH §2, §18.3). Watched failing,
    /// one arm at a time.
    #[test]
    fn an_unknown_word_in_any_closed_set_is_refused_by_name() {
        for (field, bad, expect) in [
            ("kind", "smoke-test", "kind `smoke-test`"),
            ("enforcement", "soft", "enforcement `soft`"),
            ("fidelity", "F9", "fidelity `F9`"),
        ] {
            let text = GOOD
                .lines()
                .map(|l| {
                    if l.starts_with(&format!("{field} = ")) {
                        format!("{field} = \"{bad}\"")
                    } else {
                        l.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            let e = errors(&text);
            assert!(
                e.iter().any(|m| m.contains(expect)),
                "{field}={bad} must be refused by name, got {e:?}"
            );
        }
    }

    #[test]
    fn an_unknown_precondition_and_an_unknown_venue_are_refused() {
        let text = GOOD.replace(
            r#"preconditions = ["container:sovereign-vulkan"]"#,
            r#"preconditions = ["gpu-warm"]"#,
        );
        assert!(errors(&text).iter().any(|m| m.contains("`gpu-warm`")));

        let text = GOOD.replace(
            r#"runs_in = ["prepush", "ci:gates"]"#,
            r#"runs_in = ["ci:"]"#,
        );
        assert!(errors(&text).iter().any(|m| m.contains("runs_in `ci:`")));
    }

    /// `runs_in = []` is the registry's whole point: an instrument nothing
    /// runs must be STATEABLE, or it goes back to being invisible.
    #[test]
    fn an_instrument_nothing_runs_is_legal_and_is_counted() {
        let text = GOOD.replace(r#"runs_in = ["prepush", "ci:gates"]"#, "runs_in = []");
        let r = parse(&text);
        assert_eq!(r.nowhere().len(), 1);
        assert_eq!(r.not_in_ci().len(), 1);
        // …but OMITTING the field is not the same claim, and is refused.
        let text = GOOD.replace(r#"runs_in = ["prepush", "ci:gates"]"#, "");
        assert!(errors(&text)
            .iter()
            .any(|m| m.contains("missing `runs_in`")));
    }

    /// The three fields whose absence would let a row dodge a count posture
    /// prints. Absence is reported, never defaulted (ARCH §18.3).
    #[test]
    fn cost_and_negative_control_must_be_said_not_omitted() {
        let text = GOOD.replace("cost_secs = 4.2", "");
        assert!(errors(&text)
            .iter()
            .any(|m| m.contains("missing `cost_secs`")));

        let text = GOOD.replace(r#"negative_control = "none""#, "");
        assert!(errors(&text)
            .iter()
            .any(|m| m.contains("missing `negative_control`")));

        let text = GOOD.replace("cost_secs = 4.2", r#"cost_secs = "unmeasured""#);
        assert_eq!(parse(&text).instruments[0].cost, Cost::Unmeasured);
        assert_eq!(parse(&text).coverage().unmeasured_cost, 1);
    }

    #[test]
    fn a_baseline_kind_and_path_must_agree() {
        let text = GOOD.replace(
            r#"baseline = { kind = "count", path = "quality/baselines/oversized.txt" }"#,
            r#"baseline = { kind = "count" }"#,
        );
        assert!(errors(&text).iter().any(|m| m.contains("no baseline.path")));

        let text = GOOD.replace(
            r#"baseline = { kind = "count", path = "quality/baselines/oversized.txt" }"#,
            r#"baseline = { kind = "none", path = "x" }"#,
        );
        assert!(errors(&text)
            .iter()
            .any(|m| m.contains("`none` but a baseline.path is given")));
    }

    #[test]
    fn two_rows_may_not_share_an_id() {
        let text = format!("{GOOD}\n{GOOD}");
        assert!(errors(&text).iter().any(|m| m.contains("declared twice")));
    }

    /// An exemption with no reason is a silent skip-list, which is the exact
    /// failure the registry exists to end.
    #[test]
    fn a_not_instrument_must_carry_a_reason() {
        let text = format!("{GOOD}\n[[not_instrument]]\nkey = \"npm:dev\"\nwhy = \"\"\n");
        assert!(errors(&text).iter().any(|m| m.contains("silent skip-list")));
        let text = format!(
            "{GOOD}\n[[not_instrument]]\nkey = \"npm:dev\"\nwhy = \"the vite dev server\"\n"
        );
        assert_eq!(parse(&text).not_instruments.len(), 1);
    }

    /// EVERY error, not the first: four bad rows cost one round-trip.
    /// A registry with no rows is the shape of a mistyped table header, and
    /// it must refuse rather than hand the closure gate an empty set that
    /// would blame every command in the repo.
    #[test]
    fn a_registry_with_no_instrument_rows_refuses() {
        for text in [
            "",
            "schema_version = 1\n",
            "schema_version = 1\n[[instruments]]\nid = \"x\"\n",
        ] {
            assert!(
                errors(text).iter().any(|m| m.contains("broken parse")),
                "{text:?} must refuse"
            );
        }
    }

    #[test]
    fn a_row_reports_all_of_its_problems_at_once() {
        let text = r#"
[[instrument]]
id = "broken"
kind = "wat"
enforcement = "sorta"
fidelity = "F7"
"#;
        let e = errors(text);
        assert!(e.len() >= 5, "expected several errors, got {e:?}");
        for needle in ["kind `wat`", "missing `command`", "missing `runs_in`"] {
            assert!(
                e.iter().any(|m| m.contains(needle)),
                "missing {needle} in {e:?}"
            );
        }
    }

    #[test]
    fn coverage_counts_the_four_numbers_posture_prints() {
        let by_hand = GOOD
            .replace(r#"id = "arch-gate""#, r#"id = "wizard-verify""#)
            .replace(
                r#"runs_in = ["prepush", "ci:gates"]"#,
                r#"runs_in = ["by-hand"]"#,
            )
            .replace("cost_secs = 4.2", r#"cost_secs = "unmeasured""#)
            .replace(
                r#"negative_control = "none""#,
                r#"negative_control = "dst-fe80-liveness""#,
            );
        let r = parse(&format!("{GOOD}\n{by_hand}"));
        let c = r.coverage();
        assert_eq!(c.total, 2);
        assert_eq!(c.with_negative_control, 1);
        assert_eq!(c.unmeasured_cost, 1);
        assert_eq!(c.by_hand_only, 1);
    }

    /// `by-hand` PLUS a harness is not "by-hand only" — the population the
    /// registry is trying to count is the one a human alone reaches.
    #[test]
    fn by_hand_only_means_by_hand_alone() {
        let text = GOOD.replace(
            r#"runs_in = ["prepush", "ci:gates"]"#,
            r#"runs_in = ["smoke:4", "by-hand"]"#,
        );
        assert!(!parse(&text).instruments[0].by_hand_only());
        let text = GOOD.replace(
            r#"runs_in = ["prepush", "ci:gates"]"#,
            r#"runs_in = ["by-hand"]"#,
        );
        assert!(parse(&text).instruments[0].by_hand_only());
    }

    #[test]
    fn every_wire_form_round_trips_through_its_label() {
        for s in ["gate", "suite", "bench", "probe", "control", "check"] {
            assert_eq!(Kind::parse(s).map(Kind::label), Some(s));
        }
        for s in ["hard", "advisory", "tracked"] {
            assert_eq!(Enforcement::parse(s).map(Enforcement::label), Some(s));
        }
        for s in ["F0", "F1", "F2", "F3", "F4", "F5"] {
            assert_eq!(Fidelity::parse(s).map(Fidelity::label), Some(s));
        }
        for s in ["count", "metrics", "none"] {
            assert_eq!(BaselineKind::parse(s).map(BaselineKind::label), Some(s));
        }
        for s in [
            "port-listening:9741",
            "slot-decodes:primary",
            "corpus-installed:sep",
            "binary:cargo-hack",
            "container:sovereign-vulkan",
            "host-quiet:4",
        ] {
            assert_eq!(
                Precondition::parse(s).map(|p| p.label()),
                Some(s.to_string()),
                "{s}"
            );
        }
        for s in [
            "prepush",
            "precommit",
            "ci:gates",
            "weekly:features",
            "smoke:4",
            "check",
            "nightly",
            "run-if-stale",
            "by-hand",
        ] {
            assert_eq!(
                RunsIn::parse(s).map(|r| r.label()),
                Some(s.to_string()),
                "{s}"
            );
        }
    }

    /// A cost of zero and a cost nobody timed are different claims, and the
    /// render must not blur them.
    #[test]
    fn unmeasured_renders_as_a_word_never_as_a_zero() {
        assert_eq!(Cost::Unmeasured.label(), "unmeasured");
        assert_eq!(Cost::Secs(0.1).label(), "0.10s");
        assert_eq!(Cost::Secs(0.04).label(), "0.04s");
        assert_eq!(Cost::Secs(45.0).label(), "45s");
        assert_eq!(Cost::Secs(1745.0).label(), "29m");
    }
}
