#!/bin/sh
# Spike build: the shipped source with GGML_RPC on, for llama-server --rpc and rpc-server.
cd /home/alexbryan/dev/commonwealth-ai || exit 1
src=target/llama-server/src-b904a3745bfa
b=target/llama-server/build-rpc-spike
cmake -S $src -B $b -DGGML_VULKAN=ON -DGGML_RPC=ON -DLLAMA_CURL=OFF -DLLAMA_LLGUIDANCE=ON \
  -DBUILD_SHARED_LIBS=OFF -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF -DCMAKE_BUILD_TYPE=Release \
  > $b.configure.log 2>&1 || { echo "configure failed"; tail -20 $b.configure.log; exit 1; }
cmake --build $b --target llama-server ggml-rpc-server llama-bench -j "$(nproc)" > $b.build.log 2>&1 \
  || { echo "build failed"; grep -m5 -i error $b.build.log; exit 1; }
ls -la $b/bin/
