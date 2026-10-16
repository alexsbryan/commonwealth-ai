#!/bin/sh
# FIM probe: BOS and special-token rendering on llama-server /completion.
repo=/home/alexbryan/dev/commonwealth-ai
port=48311
$repo/target/debug/llama-server -m $repo/sovereign/models/Qwen3.5-0.8B-UD-Q6_K_XL.gguf --port $port -ngl 99 -c 4096 > $1/fim-probe-server.log 2>&1 &
pid=$!
trap 'kill $pid 2>/dev/null; wait $pid 2>/dev/null' EXIT
for i in $(seq 1 60); do curl -sf localhost:$port/health >/dev/null && break; sleep 1; done
P='<|fim_prefix|>def add(a, b):\n    <|fim_suffix|>\n\nprint(add(1, 2))\n<|fim_middle|>'
echo "== tokenize add_special=true (what /completion does)"
curl -s localhost:$port/tokenize -d "{\"content\":\"$P\",\"add_special\":true,\"with_pieces\":true}" | python3 -c "import json,sys; t=json.load(sys.stdin)['tokens']; print(len(t), [x['piece'] for x in t[:4]])"
echo "== completion, default"
curl -s localhost:$port/completion -d "{\"prompt\":\"$P\",\"n_predict\":40,\"temperature\":0,\"return_tokens\":true}" | python3 -c "import json,sys; d=json.load(sys.stdin); print(repr(d['content']), d.get('stop_type'), d['tokens'][-3:])"
echo "== completion, preserved_tokens + stop"
curl -s localhost:$port/completion -d "{\"prompt\":\"$P\",\"n_predict\":40,\"temperature\":0,\"preserved_tokens\":[\"<|file_sep|>\",\"<|fim_pad|>\",\"<|repo_name|>\",\"<|endoftext|>\"],\"stop\":[\"<|file_sep|>\",\"<|fim_pad|>\",\"<|repo_name|>\",\"<|endoftext|>\"]}" | python3 -c "import json,sys; d=json.load(sys.stdin); print(repr(d['content']), d.get('stop_type'), repr(d.get('stopping_word')))"
echo "== a prompt that invites a special token in the middle"
Q='<|fim_prefix|>x = 1<|fim_suffix|><|fim_middle|>'
curl -s localhost:$port/completion -d "{\"prompt\":\"$Q\",\"n_predict\":30,\"temperature\":0,\"return_tokens\":true}" | python3 -c "import json,sys; d=json.load(sys.stdin); print('default  ', repr(d['content']), d['tokens'])"
curl -s localhost:$port/completion -d "{\"prompt\":\"$Q\",\"n_predict\":30,\"temperature\":0,\"return_tokens\":true,\"preserved_tokens\":[\"<|file_sep|>\",\"<|fim_pad|>\",\"<|repo_name|>\"]}" | python3 -c "import json,sys; d=json.load(sys.stdin); print('preserved', repr(d['content']), d['tokens'])"
