set -e
S="$1"
TK=/home/alexbryan/dev/commonwealth-ai/target/llama-cmake-cache/380bf11711a74cf6/bin/llama-tokenize
M=/home/alexbryan/dev/commonwealth-ai/sovereign/models/Qwen3.8-27B-UD-Q6_K_XL.gguf
mkdir -p "$S/chunks"
rm -f "$S/chunks"/*
for c in sep_mine sep_holdout repo_md rust_src; do
  split -C 16m -d -a 3 "$S/$c.txt" "$S/chunks/${c}." 
done
ls "$S/chunks" | wc -l
ls "$S/chunks"/* | xargs -P 3 -I{} sh -c "$TK -m $M -f {} --ids 2>/dev/null > {}.ids"
echo "tokenized"
