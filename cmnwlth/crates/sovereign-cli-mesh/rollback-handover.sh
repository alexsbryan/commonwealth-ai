#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Roll back `svrn mesh up`'s handover moves to the main-era layout
# (svrn/docs/RUNBOOK.md §9). Stop the daemon and cw-rails first.
# SVRN: the daemon's [data] dir; CONFIG: its config.toml; RAILS: cw-rails' data dir.
# Run by migration_backup_tests.rs against a main-era fixture handed over twice.
set -eu
: "${SVRN:?}" "${CONFIG:?}" "${RAILS:?}"
# Identity (identity_handover.rs) and rails.toml: put cw-rails' first originals
# back; an empty aside means the handover created the file, so remove it.
for aside in "$RAILS"/*.pre-handover; do
  [ -e "$aside" ] || continue
  if [ -s "$aside" ]; then mv "$aside" "${aside%.pre-handover}"
  else rm -f "$aside" "${aside%.pre-handover}"; fi
done
rm -rf "$RAILS/meshes"
if [ -e "$SVRN/node_key.handed-over" ]; then mv "$SVRN/node_key.handed-over" "$SVRN/node_key"; fi
# Ring journals (rail_migration.rs): back under the daemon, never over one there.
for ns in "$RAILS"/rings/*/; do
  [ -d "$ns" ] || continue
  name=$(basename "$ns")
  if [ -e "$SVRN/rings/$name" ]; then echo "kept both: $SVRN/rings/$name"; continue; fi
  mkdir -p "$SVRN/rings" && mv "$ns" "$SVRN/rings/$name"
done
rmdir "$RAILS/rings" 2>/dev/null || true
# Media house credential and viewer id (rail_migration.rs).
house=secrets/media-house
if [ -e "$RAILS/$house/authorization" ] && [ ! -e "$SVRN/$house/authorization" ]; then
  mkdir -p "$SVRN/$house" && mv "$RAILS/$house/authorization" "$SVRN/$house/authorization"
fi
rm -f "$RAILS/$house/viewer_user"
rmdir "$RAILS/$house" "$RAILS/secrets" 2>/dev/null || true
# config.toml: the viewer id, [compute.work_offer] and [iroh] keys. Within one
# run config.toml.bak is taken before config.toml.iroh.bak, so it is the older.
if [ -e "$CONFIG.bak" ]; then cp "$CONFIG.bak" "$CONFIG"
elif [ -e "$CONFIG.iroh.bak" ]; then cp "$CONFIG.iroh.bak" "$CONFIG"; fi
rm -f "$CONFIG.bak" "$CONFIG.iroh.bak"
