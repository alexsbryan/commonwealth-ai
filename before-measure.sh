set -u
cd /home/alexbryan/dev/cw-ont-p0
echo '=== A daemon -i sovereign-eval (BEFORE) ==='
cargo tree -p sovereign-cli-daemon -i sovereign-eval -e normal; echo "A exit=$?"
echo '=== B desktop -i sovereign-eval (BEFORE) ==='
cargo tree -p sovereign-desktop -i sovereign-eval -e normal; echo "B exit=$?"
echo '=== C arch-gate (BEFORE) ==='
cargo run -q -p xtask -- arch-gate; echo "C exit=$?"
echo '=== D daemon binary size (BEFORE) ==='
cargo build -p sovereign-cli-daemon --features corpus-engine/treesitter 2>&1 | tail -3
stat -c '%s bytes  %n' target/debug/sovereign-cli-daemon
