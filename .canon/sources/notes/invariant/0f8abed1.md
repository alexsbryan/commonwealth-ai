# SCRIPTS MUST NAME THE NOTES-STORE PATH, NEVER DISCOVER IT FROM CWD — the stray-db caveat is worse and quieter than filed.

SCRIPTS MUST NAME THE NOTES-STORE PATH, NEVER DISCOVER IT FROM CWD — the stray-db caveat is worse and quieter than filed.

MEASURED 2026-08-09 (order seat-backlog-protocol, D1). Same command, same argument, only cwd differs:

  cwd=~/dev/commonwealth-ai  ->  resolves ./sovereign/.sovereign/notes.db  (68 notes)
  cwd=$HOME                                  ->  resolves ~/.sovereign/notes.db          (6811 notes)

`sovereign notes list --id 0807272f` returns A HIT AND EXIT 0 FROM BOTH — a DIFFERENT note each time. There is no error, no warning, no "no notes matched". The comaintainer skill's filed reflection (2026-08-06) describes the failure as reporting "no notes matched" against the wrong store; that understates it. The observable failure is a confident correct-looking answer from the wrong store.

Note also it is NOT the repo-root stray that wins: from the repo root the resolver walks to ./sovereign/.sovereign/notes.db, not ./.sovereign/notes.db (17 notes). So "delete the stray at the repo root" would not have fixed it either. Seven notes.db files exist under the repo (excluding target/ and test-artifacts).

THE RULE for any script reading the notes store: resolve an EXPLICITLY NAMED path — env override first (test), then $SOVEREIGN_DATA_DIR/notes.db, then ~/.sovereign/notes.db — and open it read-only (`sqlite3.connect("file:<path>?mode=ro", uri=True)` works against the live WAL store; verified). Print the resolved path and the row count in the output so a read against the wrong store is VISIBLE rather than plausible. scripts/co-backlog.py does exactly this (notes_db_path(), and the page footer names path + counts); it follows co-closeout.py:164-171, which resolves its own sources the same way.

Agents (not scripts) should keep preferring the MCP `notes` tool — it always hits the daemon's store. This invariant is the script-side form of the same fix, because a script cannot call MCP.

~/.sovereign/notes.db and ~/.svrnmesh/notes.db are HARDLINKS to one file (same inode, verified) — they are not two stores.
