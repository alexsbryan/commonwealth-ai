#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# sample-daemon-mem.sh — attribute the daemon's memory growth while it serves.
#
# The daemon reclaims ~38 GiB on restart after ~24 min of compose-replay
# serving (measured 2026-08-27), and an OOM kill the day before showed
# anon-rss 58.7 GiB against ~29 GB of weights. A dead process cannot be
# asked what it was holding, so this samples the LIVE one.
#
# RssAnon vs RssFile is the whole question: file-backed pages are the mmap'd
# GGUF and are RECLAIMABLE under pressure; anonymous pages are not, and are
# what the OOM killer counts. Weights that show up as anon are the defect.
#
# Re-resolves the pid every tick ON PURPOSE — the sweep restarts the daemon
# between arms, and those boundaries are the most informative rows in the file.
#
# Columns after memavail_gib were added 2026-09-13 (appended, so older tsvs keep
# their positions):
#   gtt_daemon_gib    GTT the daemon's own DRM clients hold (/proc/<pid>/fdinfo)
#   gtt_clients_gib   GTT held by EVERY DRM client on the box, deduped by client id
#   gtt_ownerless_gib mem_info_gtt_used minus gtt_clients — 35 GB of this was
#                     observed with no sovereign process alive (2026-09-13)
#   heap_gib          anon in [heap] (brk: glibc main arena)
#   anon_big_gib      anon in unnamed mappings >= 64 MiB (ggml host buffers, mmap'd
#                     large mallocs)
#   anon_small_gib    anon in unnamed mappings < 64 MiB (glibc thread arenas, stacks)
# The last three split RssAnon by allocation CLASS, so a staircase can be pinned
# to malloc vs direct mmap before a heap profile is read.
set -u
OUT=${1:?usage: sample-daemon-mem.sh <out.tsv> [interval_secs]}
INT=${2:-20}
printf 'ts\tpid\tetime\tvmrss_gib\tanon_gib\tfile_gib\tshm_gib\tpgtbl_gib\tgtt_gib\tswap_gib\tmemavail_gib\tgtt_daemon_gib\tgtt_clients_gib\tgtt_ownerless_gib\theap_gib\tanon_big_gib\tanon_small_gib\n' > "$OUT"

# GTT per DRM client, as "pid client_id kib" rows. A client id can appear on
# several fds of one process, so callers dedupe on the id.
drm_gtt_rows () {
  for fd in /proc/[0-9]*/fdinfo/*; do
    awk -v f="$fd" '
      /^drm-client-id:/ {id=$2}
      /^drm-memory-gtt:/ {kib=$2}
      END { if (id != "") { split(f, p, "/"); print p[3], id, kib+0 } }' "$fd" 2>/dev/null
  done
}

while :; do
  # By max RssAnon, not `pgrep -f | head -1`: under heaptrack the wrapper shell's
  # command line also names the daemon binary, and the probe-buffer-threshold
  # run lost a whole column to a 2 MB process resolved that way.
  pid=$(for p in $(pgrep -x sovereign-cli-d); do
          awk -v p="$p" '$1=="RssAnon:"{print $2, p}' "/proc/$p/status" 2>/dev/null
        done | sort -rn | awk 'NR==1{print $2}')
  if [ -z "${pid:-}" ]; then
    printf '%s\t-\t-\t\t\t\t\t\t\t\t\t\t\t\t\t\t\n' "$(date +%H:%M:%S)" >> "$OUT"
    sleep "$INT"; continue
  fi
  g () { awk -v k="$1" '$1==k":"{printf "%.2f",$2/1048576}' "/proc/$pid/status" 2>/dev/null; }
  et=$(ps -o etime= -p "$pid" 2>/dev/null | tr -d ' ')
  gtt_bytes=$(head -1 /sys/class/drm/card*/device/mem_info_gtt_used 2>/dev/null | head -1)
  gtt=$(awk -v b="$gtt_bytes" 'BEGIN{printf "%.2f", b/1073741824}')
  read -r sw av < <(free -m | awk '/^Swap:/{s=$3} /^Mem:/{a=$7} END{printf "%.2f %.2f",s/1024,a/1024}')
  read -r gd gc go < <(drm_gtt_rows | awk -v d="$pid" -v used="$gtt_bytes" '
      !seen[$2]++ { all += $3; if ($1 == d) mine += $3 }
      END { printf "%.2f %.2f %.2f", mine/1048576, all/1048576, (used/1024 - all)/1048576 }')
  read -r hp ab as < <(awk '
      /^[0-9a-f]+-[0-9a-f]+ / { name = $6 }
      /^Anonymous:/ {
        if (name == "[heap]") heap += $2
        else if (name == "") { if ($2 >= 65536) big += $2; else small += $2 }
      }
      END { printf "%.2f %.2f %.2f", heap/1048576, big/1048576, small/1048576 }' "/proc/$pid/smaps" 2>/dev/null)
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$(date +%H:%M:%S)" "$pid" "$et" \
    "$(g VmRSS)" "$(g RssAnon)" "$(g RssFile)" "$(g RssShmem)" "$(g VmPTE)" \
    "$gtt" "$sw" "$av" "${gd:-}" "${gc:-}" "${go:-}" "${hp:-}" "${ab:-}" "${as:-}" >> "$OUT"
  sleep "$INT"
done
