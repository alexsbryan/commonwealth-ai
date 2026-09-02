set -u
cd /home/alexbryan/dev/cw-ont-p0
echo '=== 1. solo -p daemon build, FULL errors ==='
cargo build -p sovereign-cli-daemon --features corpus-engine/treesitter 2>&1 | grep -E "^error|^warning: unused|-->|E0[0-9]+" | head -40
echo '=== 2. same, but with the workspace feature contract ==='
cargo build -p sovereign-cli-daemon --features corpus-engine/treesitter,sovereign-cli/dev-tools 2>&1 | tail -5
