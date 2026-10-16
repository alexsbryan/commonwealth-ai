#!/bin/sh
repo=/home/alexbryan/dev/commonwealth-ai
port=48312
for m in Qwen3.5-0.8B-UD-Q6_K_XL.gguf Mellum2-12B-A2.5B-Instruct-Q6_K.gguf; do
$repo/target/debug/llama-server -m $repo/sovereign/models/$m --port $port -ngl 99 -c 4096 > $1/fim-probe2-$m.log 2>&1 &
pid=$!
for i in $(seq 1 120); do curl -sf localhost:$port/health >/dev/null && break; sleep 1; done
echo "=== $m"
grep -E "EOG token|add_bos|special_eog" $1/fim-probe2-$m.log | head -12 | cut -c1-140
case $m in Qwen*) toks='"<|file_sep|>","<|fim_pad|>","<|repo_name|>","<|endoftext|>"'; P='<|fim_prefix|>x = 1<|fim_suffix|><|fim_middle|>';;
           *) toks='"<|endoftext|>","<|im_end|>","<fim_pad>"'; P='<fim_prefix>x = 1<fim_suffix><fim_middle>';; esac
curl -s localhost:$port/tokenize -d "{\"content\":\"$P\",\"add_special\":true,\"with_pieces\":true}" | python3 -c "import json,sys; t=json.load(sys.stdin)['tokens']; print('tokens', len(t), [x['piece'] for x in t[:3]])"
for t in $(echo $toks | tr ',' ' '); do
  id=$(curl -s localhost:$port/tokenize -d "{\"content\":$t,\"parse_special\":true}" | python3 -c "import json,sys; print(json.load(sys.stdin)['tokens'][0])")
  for pres in no yes; do
    extra=""; [ $pres = yes ] && extra=",\"preserved_tokens\":[$t]"
    curl -s localhost:$port/completion -d "{\"prompt\":\"$P\",\"n_predict\":3,\"temperature\":0,\"return_tokens\":true,\"logit_bias\":[[$id,100]]$extra}" \
      | python3 -c "import json,sys; d=json.load(sys.stdin); print('  ', $t, 'id', $id, 'preserved=$pres', 'content', repr(d['content']), 'stop_type', d.get('stop_type'), 'tokens', d['tokens'])"
  done
done
kill $pid; wait $pid 2>/dev/null
done
