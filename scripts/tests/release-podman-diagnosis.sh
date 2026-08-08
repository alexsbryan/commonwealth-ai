#!/usr/bin/env bash
# The linux leg of scripts/release-cli-local.sh must tell "podman is
# unreachable" apart from "the image is absent".
#
# WHY THIS EXISTS. The image probe used to run BEFORE `podman machine start`,
# so a stopped VM failed `podman image exists` and was reported as
# "container image missing — run scripts/build-desktop-linux.sh". That sends
# you to rebuild a 3.3GB image you already have, to fix a VM that only needed
# starting. Observed 2026-08-08 cutting cli-v0.5.0, NINE MINUTES into the build
# and after both mac legs had already been compiled and packaged.
#
# Absence of an answer is not the answer "no" (ARCH §18.3). Both directions are
# asserted here: a down VM must NOT say "image missing", and a genuinely
# missing image must NOT say "unreachable" — otherwise the fix would just move
# the misdiagnosis rather than remove it.
#
# `podman` is stubbed; no VM is started and no container runs.
set -uo pipefail

ROOT="$(git rev-parse --show-toplevel)"
REAL_SCRIPT="$ROOT/scripts/release-cli-local.sh"
[[ -f "$REAL_SCRIPT" ]] || { echo "cannot find $REAL_SCRIPT"; exit 2; }

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/root/scripts" "$T/root/dist" "$T/bin"
cp "$REAL_SCRIPT" "$T/root/scripts/release-cli-local.sh"
printf '[workspace.package]\nversion = "0.5.0"\n' > "$T/root/Cargo.toml"

printf '#!/usr/bin/env bash\nexit 0\n' > "$T/bin/gh"
chmod +x "$T/bin/gh"
export PATH="$T/bin:$PATH"

cd "$T/root" || exit 2
git init -q .
git -c user.email=t@t -c user.name=t commit -q --allow-empty -m init

rc=0

mk_podman() {  # mk_podman <info-succeeds 0|1> <image-exists-succeeds 0|1>
    cat > "$T/bin/podman" <<EOF
#!/usr/bin/env bash
case "\$1" in
  info)    exit $(( $1 ? 0 : 1 )) ;;
  image)   exit $(( $2 ? 0 : 1 )) ;;
  machine) exit 0 ;;
  run)     echo "!!! CONTAINER RAN"; exit 0 ;;
  *)       exit 0 ;;
esac
EOF
    chmod +x "$T/bin/podman"
}

run_case() {  # run_case <name> <needle> <forbidden>
    local name="$1" needle="$2" forbidden="$3" out
    out="$(bash scripts/release-cli-local.sh --skip-macos-arm --skip-macos-intel 2>&1)"
    if grep -qi -- "$needle" <<<"$out" && ! grep -qi -- "$forbidden" <<<"$out"; then
        echo "  ok    $name"
    else
        echo "  FAIL  $name — wanted '$needle' and NOT '$forbidden'"
        sed 's/^/          /' <<<"$out" | tail -5
        rc=1
    fi
}

echo "release-podman-diagnosis:"

mk_podman 0 1   # VM down; the image would be there if we could ask
run_case "VM down says unreachable, not 'rebuild the image'" \
    "cannot reach podman" "run scripts/build-desktop-linux.sh"

mk_podman 1 0   # VM up; image genuinely absent
run_case "reachable + no image says the image is absent" \
    "genuinely absent" "cannot reach podman"

exit "$rc"
