// SPDX-License-Identifier: AGPL-3.0-or-later
//! sv-surface D6 + D2: the lesson pane and the insight sink status read
//! the DAEMON's stores, not this process's.
//!
//! # The state this makes unrepresentable
//!
//! Two surfaces that answered from the wrong object on an attached boot:
//!
//!   * `commands/lessons.rs` held an `Arc<NoteStore>` off `AppState` and
//!     drove `read_notes` / `write_note_full_v9` / `retire_by_id` /
//!     `update_note_payload` / `delete_note` against it. That store is
//!     `<desktop data_dir>/notes.db`, which on an attached boot is not
//!     necessarily the file the daemon writes — so "What I've learned"
//!     could render one node's lessons while the turn enforced another's.
//!   * `insight_commands.rs::get_sink_status` read THIS process's
//!     `InsightService.sinks` registry, which in attach is empty however
//!     many vaults the daemon has connected, and then hard-coded
//!     `sinks: vec![]` in BOTH modes — a defaulted absence (ARCH §18.3).
//!
//! Both are now `TurnClient` calls over `client_base_url()`:
//! `POST /v1/notes/query`, `POST /v1/notes`, `GET|DELETE /v1/notes/{id}`,
//! `PATCH /v1/notes/{id}/payload`, `POST /v1/notes/{id}/retire`, and
//! `GET /v1/insights/sinks`.
//!
//! # Calibration (ARCH §18.1 — name the failing input)
//!
//! PRODUCTION lines only, and only above `#[cfg(test)]`; comment lines are
//! dropped, because the doc comment that explains why the local store is
//! gone has to NAME it to do so. Prose about a needle is documentation,
//! not a second path — the calibration `reading_wire_types_census`
//! records, and the reason this file's own header may say `NoteStore`.

use std::path::Path;

fn production_source(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let prod = match src.find("\n#[cfg(test)]") {
        Some(i) => &src[..i],
        None => &src[..],
    };
    prod.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `NoteStore` method the four lesson commands used to call. Each
/// one is a write or a read against a db handle this process opened.
const LOCAL_NOTE_OPS: &[&str] = &[
    "read_notes(",
    "read_note_by_id(",
    "write_note_full_v9(",
    "retire_by_id(",
    "update_note_payload(",
    "delete_note(",
];

#[test]
fn the_lesson_pane_holds_no_note_store() {
    let code = production_source("src/commands/lessons.rs");
    assert!(
        !code.contains("NoteStore") && !code.contains("state.notes") && !code.contains(".notes\n"),
        "sv-surface D6: commands/lessons.rs takes a NoteStore handle off \
         AppState again. The four lesson commands are TurnClient's \
         notes_list / note_get / note_create / note_set_payload / note_retire \
         / note_delete \
         over client_base_url() — the daemon owns notes.db, and owns it once. \
         A local handle is a second store for the same lessons, and on an \
         attached boot it is the one the turn does NOT read."
    );
    for op in LOCAL_NOTE_OPS {
        assert!(
            !code.contains(op),
            "sv-surface D6: commands/lessons.rs calls the store op `{op}` \
             directly again. Every lesson read and write crosses the wire; \
             the supersede (one ACTIVE lesson per enforcement rung) is \
             composed from notes_list + note_create + note_retire, which are \
             the same three store ops the daemon runs."
        );
    }
}

#[test]
fn the_sink_status_reads_the_daemons_registry() {
    let code = production_source("src/insight_commands.rs");
    assert!(
        !code.contains("sinks.any_connected("),
        "sv-surface D2: insight_commands.rs reads this process's insight sink \
         registry again. get_sink_status is TurnClient::insight_sinks over \
         GET /v1/insights/sinks — on an attached boot the local registry is \
         empty however many vaults the daemon has connected, so a local read \
         reports `any_connected: false` as though it were the answer."
    );
    assert!(
        !code.contains("sinks: vec![]"),
        "sv-surface D2 / ARCH §18.3: insight_commands.rs hard-codes an empty \
         sink list again. An empty list is a REAL answer the route gives when \
         nobody configured a vault; substituting one for the registry it did \
         not read is a defaulted absence, not a placeholder."
    );
}
