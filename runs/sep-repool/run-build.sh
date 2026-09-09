#!/bin/bash
cd /home/alexbryan/dev/commonwealth-ai
S0=$(date +%s)
python3 runs/sep-repool/build_last_pooled.py sep sep-last
rc=$?
echo "WALL build: $(( $(date +%s) - S0 ))s rc=$rc"
