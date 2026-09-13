# WHAT ~/Library/Application Support/svrnmesh IS FOR — settled empirically 2026-08-24, and the answer is "one live role, plus residue."…

WHAT `~/Library/Application Support/svrnmesh` IS FOR — settled empirically 2026-08-24, and the answer is "one live role, plus residue." Supersedes 81e9f605 (which was right that the data is vestigial but imprecise: it implied the whole directory is deletable, and it is not).

THE ONE LIVE ROLE: it is `rebrand::mesh_config_dir()` — the platform CONFIG dir,
home of `desktop.toml`, read at `desktop/state/config.rs:389`. That is
legitimate and correct, and `rebrand.rs:163-171` already explains why it must
stay distinct from the data dir:

  "Deliberately NOT collapsed into mesh_data_dir. On macOS the two resolve to
   the same directory, which makes them look interchangeable; on Linux and
   Windows they do not (~/.config vs ~/.local/share), so routing a settings
   file like desktop.toml through the data-dir [is wrong]."

THAT IS THE WHOLE SOURCE OF THE CONFUSION, and it is macOS-specific: on this
platform `dirs::config_dir()` == `dirs::data_dir()`, so a LIVE config directory
and a DEAD data root are the same folder. On Linux they would be visibly
separate and nobody would have conflated them.

EVERYTHING ELSE IN IT IS RESIDUE (measured):
  mesh.json      2026-04-24   node_id 2026-04-24   notes.db 2026-05-25
  sovereign.db   2026-07-24   — vs ~/.svrnmesh written the same day (14:35/14:47)
  lsof: zero open handles anywhere under it; the live daemon holds
        ~/.svrnmesh/sovereign.db
  indexes/: 6 entries vs 1847 in the live root; the only entries unique to it
        are project_docs.db-shm / -wal (SQLite sidecars, not corpora). The 15G
        is redundant.

SO, PRECISELY:
  KEEP    mesh_config_dir() and desktop.toml. Live, correct, cross-platform.
  DELETE  mesh_data_dir() AS A ROOT SOURCE in code. That is the bug.
  ARCHIVE the stale data in the folder (indexes/, sovereign.db, notes.db,
          mesh.json, node_id, join_key.secret, local-corpora*, vault-snapshots)
          — operator action, out of band, and NOT desktop.toml.

THE LIVE CODE BUG, unchanged from 81e9f605 and still the point:
1. `cli-shared/src/dirs.rs:49-56` — `mesh_data_dir()` used UNCONDITIONALLY by
   `svrn mesh create` / `join` (mesh_cmd.rs:2570,2691). Its comment claims the
   dir is "shared with sovereign-desktop so a mesh created from either surface
   is picked up by the other." FALSE in practice: the desktop resolves data to
   ~/.svrnmesh via desktop.toml (`data_dir = "~/.sovereign"`, itself a symlink
   to ~/.svrnmesh created 2026-06-30) and the daemon uses svrnmesh_root(). So
   `svrn mesh` writes live mesh identity where nothing reads it. The 2026-04-24
   mesh.json is likely the last `svrn mesh create`.
2. `desktop/state/config.rs:301` — `DesktopConfig::default_data_dir()` returns
   `mesh_data_dir()`. Only the existing desktop.toml override stops it biting;
   A FRESH INSTALL lands data in the platform root while the daemon uses
   ~/.svrnmesh. That is how this residue was created, and it is reproducible.

CONSEQUENCE FOR conv-1 (Phase 1): no mesh merge, no identity choice. One
accessor for the DATA root (~/.svrnmesh); `mesh_data_dir()` stops being a data
root; `mesh_config_dir()` survives untouched. The loud-refusal tier still earns
its place for other machines, where a platform-root data set may be live.

DOES NOT GENERALISE: one machine. Another host may have live data in its
platform root. Deletion is a local operator action, never a migration the code
performs.
