#!/bin/sh
# loop.sh <arm> [corpus...|inner|all]: one arm of the ontology loop, every step and its record, so no session rebuilds them.
#
# The corpora run concurrently (each has its own index and answer store; the daemon serves remote calls in parallel).
# `inner` is the inner ring (corpora.py INNER: the corpora that carry most of the one-shot gap), for iterating;
# `all` (the default) is the five worlds and ward-story, for an arm that is a candidate to adopt.
#
# Per corpus it writes runs/loop/<arm>/<corpus>/:
#   run.json        what ran: HEAD, the dirty diff's hash, the svrn-ingest binary's mtime, engine, asker, NOTE
#   chapters.json   the window cut the run read (indexes/<id>/chapters.json), so the arm replays: RESTORE=<arm>
#   extract.*, resolve.*  the public commands' output; extract.err carries every window prompt and raw answer (trace)
#   windows/        each window's labels, prompt and answer, split out of extract.err
#   score.json      score.py's report; misses.jsonl + misses.txt, each unserved gold deal's chain and verdict (misses.py)
#                   (a bank: records.json + bank.json; a held-out corpus, corpora.HELD_OUT, is scored and never diagnosed)
#   decisions.jsonl every RESOLVE question judged against gold, the model's own pick beside the decision (decisions.py)
#   atlas/          the records: the atlas's atoms.json and edges.json after RESOLVE (table.py --same compares two arms')
# then table.py prints the arm's totals and categories and appends a row to runs/loop/journal.tsv.
#
# Env: NOTE="what this arm changes" (required). ASKER=replay|recorded|daemon for extract (default recorded: a
# question asked before reuses its answer, a new one goes to the daemon); resolve always runs --asker recorded.
# INIT=1 re-cuts the windows (enrich init). RESTORE=<arm> puts that arm's cut back first, for an exact replay.
# SOVEREIGN_READER is passed through as given: unset runs the product default (the window reader since 5f2711847),
# so an arm exercises the path a user gets; run.json records it ("default" when unset).
# NOSCORE=1 scores and diagnoses nothing and writes no journal row: a deletion's replay, judged by
# `table.py --same <reference arm> <arm>` on its records, asks no bank question (a bank's answerer is a model call).
cd "$(dirname "$0")/../../.." || exit 1
L=research/ontology-apps/loop
arm=$1; shift
[ -n "$arm" ] && [ -n "$NOTE" ] || { echo "usage: NOTE='what this arm changes' $0 <arm> [corpus...]" >&2; exit 2; }
case "$1" in ""|all) set -- $(python3 $L/corpora.py --set all) ;; inner) set -- $(python3 $L/corpora.py --set inner) ;; banks) set -- $(python3 $L/corpora.py --set banks) ;; esac
root=runs/loop/$arm
[ -e $root ] && [ -z "$FORCE" ] && { echo "$root exists; name a new arm (FORCE=1 overwrites)" >&2; exit 2; }
mkdir -p $root
export SOVEREIGN_WAVES_ONE_BY_ONE=1 SOVEREIGN_NO_STALE_WARN=1 NO_COLOR=1
export RUST_LOG=info,corpus_engine::enrichment::pipeline::document_read::window=trace,corpus_engine::enrichment::atlas::resolve_records=debug
asker=${ASKER:-recorded}
head=$(git rev-parse --short HEAD)
dirty=$(git diff HEAD -- . ':!runs' | shasum | cut -c1-12); [ -z "$(git diff HEAD --stat -- . ':!runs')" ] && dirty=clean
bin=$(stat -f '%Sm' -t '%Y-%m-%dT%H:%M:%S' target/debug/svrn-ingest)
engine=$(curl -s -m 5 127.0.0.1:9741/v1/models | python3 -c "import json,sys; print(','.join(m['id'] for m in json.load(sys.stdin)['data']))" 2>/dev/null)
git diff HEAD -- . ':!runs' > $root/dirty.diff
one() {
  id=$1; out=$root/$id; mkdir -p $out; s=$(date +%s)
  cut=~/.svrnmesh/indexes/$id/chapters.json; cfg=~/.svrnmesh/enrichment/$id/config.json
  if [ -n "$RESTORE" ]; then
    cp runs/loop/$RESTORE/$id/chapters.json $cut || { echo "$id: no saved cut in runs/loop/$RESTORE"; return; }
  elif [ -n "$INIT" ]; then  # re-cut the windows; init over a literary-pipeline config is put back (run_world.sh)
    cp $cfg $out/config.before.json
    sovereign enrich init $id --from-corpus $id --chat-model commonwealth/primary --embed-model qwen3-embedding-0.6b --force > $out/init.out 2> $out/init.err || { echo "$id: init failed"; return; }
    if python3 -c "import json,sys; sys.exit(0 if json.load(open(sys.argv[1])).get('pipeline_id') == 'literary' else 1)" $cfg; then
      python3 -c "import json,sys; c=json.load(open(sys.argv[1])); c['corpus_id']=sys.argv[3]; json.dump(c, open(sys.argv[2],'w'), indent=2)" $out/config.before.json $cfg $id
    fi
  fi
  cp $cut $out/chapters.json; cp $cfg $out/config.json
  store=~/.svrnmesh/enrichment/$id/answers.jsonl; before=$(wc -l < $store 2>/dev/null | tr -d ' ')
  sovereign enrich extract $id --full --asker $asker > $out/extract.out 2> $out/extract.err; rx=$?
  [ $rx = 0 ] && { sovereign enrich resolve $id --asker recorded > $out/resolve.out 2> $out/resolve.err; rr=$?; }
  after=$(wc -l < $store 2>/dev/null | tr -d ' ')
  [ "$rr" = 0 ] && mkdir -p $out/atlas && cp ~/.svrnmesh/indexes/$id/atlas/atoms.json ~/.svrnmesh/indexes/$id/atlas/edges.json $out/atlas/
  kind=$(python3 $L/corpora.py $id kind); [ -n "$NOSCORE" ] && kind=none
  if [ "$kind" = none ]; then :
  elif [ "$kind" = bank ]; then  # a question bank's corpus: its records through the public query, asked the bank
    [ $rx = 0 ] && [ "$rr" = 0 ] && python3 $L/records_of.py $id $(python3 $L/corpora.py $id recipe) $out/records.json > $out/records.out 2>&1 \
      && python3 $L/score_bank.py $out/records.json $(python3 $L/corpora.py $id bank) $out/bank.json > $out/score.out 2>&1
  else
    [ $rx = 0 ] && [ "$rr" = 0 ] && python3 $L/score.py $(python3 $L/corpora.py $id msgids) $out/score.json $(python3 $L/corpora.py $id score-args) > $out/score.out 2>&1
  fi
  python3 -c "import json,sys; json.dump(dict(zip(sys.argv[2::2], sys.argv[3::2])), open(sys.argv[1], 'w'), indent=1)" $out/run.json \
    arm "$arm" corpus "$id" note "$NOTE" head "$head" dirty "$dirty" svrn_ingest_mtime "$bin" engine "$engine" asker "$asker" \
    reader "${SOVEREIGN_READER:-default}" \
    init "${INIT:-}" restore "${RESTORE:-}" extract_exit "$rx" resolve_exit "${rr:-not run}" seconds "$(( $(date +%s) - s ))" \
    answers_before "$before" answers_after "$after" noscore "${NOSCORE:-}" finished "$(date -u +%FT%TZ)"
  [ "$kind" = none ] && { echo "$id: extract exit $rx, resolve exit ${rr:-not run}; answers $before -> $after (not scored)"; return; }
  if [ "$kind" = bank ]; then
    [ -f $out/bank.json ] && echo "$id $(python3 -c "import json,sys; s=json.load(open(sys.argv[1]))['summary']; print(f\"bank {s['right']}/{s['judged']} right; records {s['records']}\")" $out/bank.json)" \
      || echo "$id: extract exit $rx, resolve exit ${rr:-not run}; see $out"
    return
  fi
  [ -f $out/score.json ] || { echo "$id: extract exit $rx, resolve exit ${rr:-not run}; see $out"; return; }
  python3 $L/misses.py $out $id > $out/misses.txt
  python3 $L/decisions.py $out $id > $out/decisions.txt
  echo "$id $(python3 $L/table.py --line $out)"
}
for id in "$@"; do one $id & done
wait
[ -n "$NOSCORE" ] || python3 $L/table.py $root
