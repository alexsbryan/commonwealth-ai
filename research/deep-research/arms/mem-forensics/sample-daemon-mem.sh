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
set -u
OUT=${1:?usage: sample-daemon-mem.sh <out.tsv> [interval_secs]}
INT=${2:-20}
printf 'ts\tpid\tetime\tvmrss_gib\tanon_gib\tfile_gib\tshm_gib\tpgtbl_gib\tgtt_gib\tswap_gib\tmemavail_gib\n' > "$OUT"
while :; do
  pid=$(pgrep -f "debug/sovereign-cli-daemon daemon run" | head -1)
  if [ -z "${pid:-}" ]; then
    printf '%s\t-\t-\t\t\t\t\t\t\t\t\n' "$(date +%H:%M:%S)" >> "$OUT"
    sleep "$INT"; continue
  fi
  g () { awk -v k="$1" '$1==k":"{printf "%.2f",$2/1048576}' "/proc/$pid/status" 2>/dev/null; }
  et=$(ps -o etime= -p "$pid" 2>/dev/null | tr -d ' ')
  gtt=$(awk '{printf "%.2f",$1/1073741824}' /sys/class/drm/card*/device/mem_info_gtt_used 2>/dev/null | head -1)
  read -r sw av < <(free -m | awk '/^Swap:/{s=$3} /^Mem:/{a=$7} END{printf "%.2f %.2f",s/1024,a/1024}')
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$(date +%H:%M:%S)" "$pid" "$et" \
    "$(g VmRSS)" "$(g RssAnon)" "$(g RssFile)" "$(g RssShmem)" "$(g VmPTE)" \
    "$gtt" "$sw" "$av" >> "$OUT"
  sleep "$INT"
done
