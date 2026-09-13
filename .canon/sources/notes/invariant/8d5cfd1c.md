# nc-22a: a Tauri command that re-types a daemon REQUEST type silently discards user choices, and the daemon's own serde(default)s hide it.

nc-22a: a Tauri command that re-types a daemon REQUEST type silently discards user choices, and the daemon's own serde(default)s hide it.

sovereign-desktop's `lc_watch_register` took a hand-copied 5-field `WatchedFolderConfigWire` while `src/lib/types.ts` declared 10 fields NON-OPTIONAL and `WatchedFolderRegisterFlow.svelte` bound five of the missing ones to live controls. serde dropped with_ocr / sync_mode / sensitive / additional_roots / enrichment at the command boundary; `RegisterRequest.config`'s per-field #[serde(default)] then re-filled them on the daemon side. Net effect: the sensitive toggle (a PRIVACY control), the manual-sync radio, the OCR checkbox and the additional-roots picker were all inert in a shipped build. Fixed by importing sovereign_tools::local_corpus::config::{WatchedFolderConfig, DeletionGuardConfig}.

THE GENERAL RULE: serde(default) is honest version negotiation only for the READ direction (an older peer that genuinely lacks the field). On a REQUEST type it masks a value the client DID send. nc-21 fixed this file's READ path and left the WRITE path — check both directions when converging a wire type.

Guarded by watched_folder_commands::tests::register_config_survives_the_command_boundary, which was watched failing against the restored fork (with_ocr / sync_mode came back Null) before it passed.

Second defect, same order: types.ts::RelatedAtom declared `display_name` while both producers (sovereign_mesh::RelatedAtom, RelatedAtomDto) emit `canonical_name`, so AtomDetail.svelte's related-atom chips rendered undefined. Its covering test fed the fixture `display_name`, so it stayed green over the break — the §18.1 smell "a guard asserting on a field the subject supplies".
