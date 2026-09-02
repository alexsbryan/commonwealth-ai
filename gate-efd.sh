set -u
cd /home/alexbryan/dev/cw-ont-p0
git log --oneline -1; git status --short
echo '=== E lint --full ==='
./scripts/sovereign-lint.sh --human --full; echo "lint exit=$?"
echo '=== F arch-gate ==='
(cd corpus-engine && cargo run -q -p xtask -- arch-gate); echo "arch-gate exit=$?"
echo '=== D daemon binary size, BEFORE the dep flip, at this commit ==='
cargo build -p sovereign-cli-daemon --features corpus-engine/treesitter 2>&1 | tail -40; echo "build exit=$?"
stat -c '%s bytes  %n' target/debug/sovereign-cli-daemon
