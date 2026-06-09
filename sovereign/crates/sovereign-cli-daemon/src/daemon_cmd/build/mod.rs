//! `run_daemon` bootstrap phase-builders (§3.3 decomposition of the
//! daemon startup path). Each builder is a **pure relocation** of a
//! self-contained early phase — same statements, same order — called in
//! place from `run_daemon`.
//!
//! Only the genuinely self-contained early phases live here: the VRAM
//! `preflight` check (no outputs) and the inference `provider` load
//! (returns the provider + engine handle + resolved embed family). The
//! later phases — work-atlas wiring, the CorpusEngine, the
//! `EmbeddedDaemon`, NoteStore propagation, the project/watched-folder
//! pipelines — are deeply **interleaved** (they mutate the daemon, the
//! tool registry, and the note store across many steps with ordering
//! constraints), so they stay inline in `run_daemon`, the same call the
//! desktop `state.rs` decomposition made for its `tools`/`embedded_daemon`
//! phases. This startup path has no GGUF-free CI coverage, so extraction
//! is limited to relocations the compiler can fully type-check.

pub mod inference;
pub mod preflight;
