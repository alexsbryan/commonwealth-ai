#!/usr/bin/env bash
# bootstrap-linux.sh — one-shot installer for the Linux build deps.
#
# Handles Fedora (dnf) and Ubuntu/Debian (apt). Idempotent. Expects
# to run inside a Strix Halo toolbox (kyuz0 ROCm 7.2.1 or Vulkan
# image), though it also works on any Fedora 40+ / Ubuntu 24.04+
# host with the matching GPU stack installed.
#
# Usage:
#   ./scripts/bootstrap-linux.sh                 # autodetect backend
#   ./scripts/bootstrap-linux.sh --backend=rocm
#   ./scripts/bootstrap-linux.sh --backend=vulkan
#   ./scripts/bootstrap-linux.sh --no-sudo       # skip package install (caller handled it)
#   ./scripts/bootstrap-linux.sh --revert-cargo  # undo the Vulkan Cargo.toml swap
#
# What it does:
#   1. Preflights the container image for two known kyuz0 vulkan-radv
#      issues (broken sudo, dangling /usr/bin/ld alternative) and
#      prints a host-side `podman exec` fix if either is detected.
#   2. Detects the GPU backend from the toolbox (or honours --backend).
#   3. Installs Rust (via rustup if no system toolchain, + rustfmt which
#      llama-cpp-sys-2's bindgen needs), clang/libclang, cmake, binutils,
#      protobuf, OpenSSL, the GTK/WebKit deps Tauri 2 needs for
#      sovereign-desktop, and the backend-specific bits:
#        ROCm  : rocm-hip-sdk7.2.1 (Fedora) / rocm-hip-sdk (Ubuntu)
#        Vulkan: vulkan-loader-devel + headers + glslc
#   4. ROCm only: writes /etc/ld.so.conf.d/sovereign-rocm.conf so the
#      runtime linker finds libamdhip64 without LD_LIBRARY_PATH, and
#      /etc/profile.d/sovereign-rocm.sh so new shells have
#      ROCM_PATH / HIP_PATH / PATH / CMAKE_PREFIX_PATH pre-set.
#   5. Rewrites the `llama-cpp-2` backend feature in
#      crates/sovereign-inference/Cargo.toml to match the resolved backend
#      (rocm ↔ vulkan, leaving llguidance and friends alone). The repo
#      default is "rocm"; the vulkan swap is a LOCAL EDIT — don't commit
#      it. `--revert-cargo` puts it back to the rocm default.
#   6. Wipes target/*/build/llama-cpp-sys-2-* if the previous build used
#      a different backend, so cmake reconfigures from scratch.
#
# --no-sudo mode is for when you've already installed system packages
# from the host via `podman exec --user root <container> dnf install ...`
# (the workaround for the broken-sudo kyuz0 vulkan-radv image). The
# rustup + Cargo.toml swap + target-wipe steps still run.
#
# After it finishes, open a fresh shell (or `source` the profile drop
# on ROCm) and run `cargo build --release` from the sovereign/ workspace root.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
INFERENCE_TOML="$REPO_ROOT/crates/sovereign-inference/Cargo.toml"
BACKEND_MARKER="$REPO_ROOT/target/.sovereign-backend"

BACKEND="auto"
REVERT_CARGO=0
NO_SUDO=0

ROCM_PATH="${ROCM_PATH:-/opt/rocm}"

die() { echo "bootstrap-linux: $*" >&2; exit 1; }
warn() { echo "bootstrap-linux: warning: $*" >&2; }

parse_args() {
    for arg in "$@"; do
        case "$arg" in
            --backend=rocm|--backend=vulkan|--backend=auto) BACKEND="${arg#--backend=}" ;;
            --revert-cargo) REVERT_CARGO=1 ;;
            --no-sudo) NO_SUDO=1 ;;
            -h|--help)
                sed -n '2,/^set -/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//;/^set -/d'
                exit 0 ;;
            *) die "unknown flag: $arg (see --help)" ;;
        esac
    done
}

# Two container-image bugs we've hit in kyuz0's vulkan-radv image. Both
# require root in the container, which rootless podman doesn't give our
# toolbox user, so the only remediation is from the host. We detect them
# here so the next dev sees a clear message instead of a cryptic cmake
# error five minutes into their first build.
preflight_image_quirks() {
    local container=""
    [[ -f /run/.containerenv ]] && container=$(. /run/.containerenv 2>/dev/null && echo "${name:-}")

    local issues=()
    # Issue 1: /etc/sudoers and /etc/pam.d/sudo were stripped from the
    # image. `sudo -n` fails with "unable to open /etc/sudoers".
    if [[ ! -f /etc/sudoers ]] || [[ ! -f /etc/pam.d/sudo ]]; then
        issues+=("sudoers-missing")
    fi

    # Issue 2: /usr/bin/ld is a symlink to /etc/alternatives/ld but the
    # alternatives entry was never set up (scriptlet failure during image
    # build). Any cmake compiler check dies with
    # `collect2: fatal error: cannot find 'ld'`.
    if [[ -L /usr/bin/ld && ! -e /usr/bin/ld ]]; then
        issues+=("ld-alternative-dangling")
    fi

    (( ${#issues[@]} )) || return 0

    local name="${container:-<toolbox-name>}"
    cat >&2 <<EOF

== Image preflight found ${#issues[@]} issue(s) in this toolbox ==

The kyuz0 vulkan-radv image ships with a few things broken. These
need root inside the container. The simplest fix is to open a root
shell in the running toolbox — from a **host** terminal (not inside
the toolbox), run:

  toolbox enter -u 0 $name    # Fedora toolbox
  # distrobox enter --root $name    # Ubuntu/distrobox equivalent

Then inside that root shell, run the commands for each issue:

EOF
    for issue in "${issues[@]}"; do
        case "$issue" in
            sudoers-missing)
                cat >&2 <<'EOF'
  # Fix missing sudoers + sudo PAM config:
  dnf reinstall -y sudo
  echo '%wheel ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/wheel-nopasswd
  chmod 440 /etc/sudoers.d/wheel-nopasswd

EOF
                ;;
            ld-alternative-dangling)
                cat >&2 <<'EOF'
  # Fix dangling /usr/bin/ld -> /etc/alternatives/ld:
  ln -sf /usr/bin/ld.bfd /etc/alternatives/ld

EOF
                ;;
        esac
    done
    cat >&2 <<EOF
Exit the root shell, re-enter the normal toolbox session, and re-run
this script — preflight will clear and installation will continue.

If \`toolbox enter -u 0\` isn't available on your host, the fallback
is \`podman exec --user root $name <cmd>\` from the host for each
command above (see docs/TOOLBOX_SETUP.md §3a).

EOF
    die "preflight failed — address the image issues above"
}

detect_distro() {
    [[ -f /etc/os-release ]] || die "can't detect distro (/etc/os-release missing)"
    # shellcheck source=/dev/null
    . /etc/os-release
    echo "$ID"
}

detect_backend() {
    # Caller picked one explicitly.
    if [[ "$BACKEND" != "auto" ]]; then
        echo "$BACKEND"
        return
    fi

    # ROCm wins if the runtime is already installed — the kyuz0 ROCm
    # toolbox ships /opt/rocm-* populated even before bootstrap runs.
    if compgen -G "/opt/rocm*/lib/libamdhip64.so*" >/dev/null; then
        echo "rocm"
        return
    fi

    # Vulkan toolbox (kyuz0 vulkan-radv / vulkan-amdvlk) ships the
    # loader + ICDs but no ROCm. Any AMD ICD file is a strong signal.
    if compgen -G "/usr/share/vulkan/icd.d/*amd*.json" >/dev/null \
       || compgen -G "/usr/share/vulkan/icd.d/*radeon*.json" >/dev/null \
       || compgen -G "/usr/share/vulkan/icd.d/*radv*.json" >/dev/null; then
        echo "vulkan"
        return
    fi

    # Last resort: if /dev/kfd exists, assume ROCm; else Vulkan if the
    # Vulkan loader is present; else bail.
    if [[ -e /dev/kfd ]]; then
        warn "autodetect: /dev/kfd present but no ROCm runtime found — assuming ROCm (will install)"
        echo "rocm"
        return
    fi

    if [[ -f /usr/lib64/libvulkan.so.1 || -f /usr/lib/x86_64-linux-gnu/libvulkan.so.1 ]]; then
        echo "vulkan"
        return
    fi

    die "could not autodetect backend — pass --backend=rocm or --backend=vulkan"
}

ensure_rust() {
    if ! (command -v cargo >/dev/null 2>&1 && command -v rustc >/dev/null 2>&1); then
        echo "== Installing Rust via rustup =="
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal
        # shellcheck source=/dev/null
        . "$HOME/.cargo/env"
    fi
    # llama-cpp-sys-2's bindgen pipes its output through rustfmt; rustup's
    # minimal profile omits it. Without it the build errors with
    # "'rustfmt' is not installed for the toolchain".
    if ! command -v rustfmt >/dev/null 2>&1; then
        echo "== Installing rustfmt component =="
        rustup component add rustfmt
    fi
}

install_fedora_common() {
    # binutils: provides ld (the kyuz0 vulkan-radv image ships the symlink
    # but not a working target; see preflight_image_quirks).
    sudo dnf install -y \
        clang clang-devel \
        cmake gcc gcc-c++ pkg-config binutils \
        protobuf-compiler protobuf-devel \
        openssl-devel \
        webkit2gtk4.1-devel gtk3-devel libsoup3-devel librsvg2-devel \
        libayatana-appindicator-gtk3
}

install_fedora_rocm() {
    echo "== Installing Fedora ROCm build deps =="
    install_fedora_common
    sudo dnf install -y rocm-hip-sdk7.2.1
}

install_fedora_vulkan() {
    echo "== Installing Fedora Vulkan build deps =="
    install_fedora_common
    sudo dnf install -y \
        vulkan-loader-devel vulkan-headers glslc
}

install_ubuntu_common() {
    # binutils is pulled in by build-essential on Ubuntu, so no explicit
    # dep here — the kyuz0 Ubuntu images don't have the alternative issue.
    sudo apt-get update
    sudo apt-get install -y \
        clang libclang-dev \
        cmake build-essential pkg-config \
        protobuf-compiler libprotobuf-dev \
        libssl-dev \
        libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev librsvg2-dev \
        libayatana-appindicator3-1
}

install_ubuntu_rocm() {
    echo "== Installing Ubuntu/Debian ROCm build deps =="
    install_ubuntu_common
    sudo apt-get install -y rocm-hip-sdk
}

install_ubuntu_vulkan() {
    echo "== Installing Ubuntu/Debian Vulkan build deps =="
    install_ubuntu_common
    sudo apt-get install -y libvulkan-dev glslang-tools
}

configure_rocm_runtime() {
    [[ -d "$ROCM_PATH/lib" ]] || die "ROCm not found at $ROCM_PATH/lib — did the HIP SDK install succeed?"

    local ld_conf=/etc/ld.so.conf.d/sovereign-rocm.conf
    if [[ ! -f $ld_conf ]]; then
        echo "== Writing $ld_conf =="
        echo "$ROCM_PATH/lib" | sudo tee "$ld_conf" >/dev/null
        sudo ldconfig
    fi

    local profile=/etc/profile.d/sovereign-rocm.sh
    if [[ ! -s $profile ]]; then
        echo "== Writing $profile =="
        sudo tee "$profile" >/dev/null <<EOF
# Populated by sovereign/scripts/bootstrap-linux.sh — safe to delete;
# re-running the script re-creates it.
export ROCM_PATH="$ROCM_PATH"
export HIP_PATH="\$ROCM_PATH"
export PATH="\$ROCM_PATH/bin:\$PATH"
export CMAKE_PREFIX_PATH="\$ROCM_PATH\${CMAKE_PREFIX_PATH:+:\$CMAKE_PREFIX_PATH}"
EOF
    fi
}

# Swap the llama-cpp-2 feature in sovereign-inference/Cargo.toml.
# Idempotent: no-op if already the target backend.
swap_inference_backend() {
    local want="$1"
    [[ -f "$INFERENCE_TOML" ]] || die "missing $INFERENCE_TOML"

    local have
    have="$(awk '/^\[target\..cfg\(target_os = "linux"\).\.dependencies\]/{flag=1;next} flag && /^llama-cpp-2/{print; exit}' "$INFERENCE_TOML")"
    case "$have" in
        *'"rocm"'*)   current="rocm" ;;
        *'"vulkan"'*) current="vulkan" ;;
        *) die "couldn't parse llama-cpp-2 feature in $INFERENCE_TOML — manual edit?" ;;
    esac

    if [[ "$current" == "$want" ]]; then
        return
    fi

    echo "== Swapping sovereign-inference Linux backend: $current → $want =="
    # Match the Linux [target] block only, to avoid touching the macOS metal line.
    # The features list contains the backend plus other features (e.g. llguidance),
    # so we locate the line under the linux header and swap the rocm/vulkan token
    # within it — tolerant of order and additional features.
    python3 - "$INFERENCE_TOML" "$want" <<'PY'
import re, sys, pathlib
path, want = pathlib.Path(sys.argv[1]), sys.argv[2]
src = path.read_text()
header = '[target.\'cfg(target_os = "linux")\'.dependencies]'
m = re.search(re.escape(header) + r'\s*\n(llama-cpp-2 = \{[^\n]*\})', src)
if not m:
    sys.exit("couldn't find Linux llama-cpp-2 line under " + header)
line = m.group(1)
new_line, n = re.subn(r'"(rocm|vulkan)"', f'"{want}"', line)
if n != 1:
    sys.exit(f"expected exactly one rocm/vulkan token in features list, found {n}")
path.write_text(src[:m.start(1)] + new_line + src[m.end(1):])
PY
}

# Wipe llama-cpp-sys-2 build dirs if the last backend differed.
# Lance/etc. are backend-neutral, so we do NOT blow away all of target/.
wipe_llama_cache_if_backend_changed() {
    local want="$1"
    mkdir -p "$(dirname "$BACKEND_MARKER")"
    local prev=""
    [[ -f "$BACKEND_MARKER" ]] && prev="$(cat "$BACKEND_MARKER")"

    if [[ "$prev" != "" && "$prev" != "$want" ]]; then
        echo "== Backend changed ($prev → $want); wiping llama-cpp-sys-2 build cache =="
        rm -rf "$REPO_ROOT"/target/*/build/llama-cpp-sys-2-* 2>/dev/null || true
    fi
    echo "$want" > "$BACKEND_MARKER"
}

warn_stale_rocm_env() {
    # The previous ROCm toolbox pattern was to dump exports straight into
    # ~/.bashrc. Since $HOME is bind-mounted across toolboxes, those exports
    # follow you into a Vulkan toolbox and point at /opt/rocm-* paths that
    # no longer exist. Harmless for the build, but confusing for debugging.
    if [[ -f "$HOME/.bashrc" ]] && grep -q "ROCM_PATH=/opt/rocm" "$HOME/.bashrc" 2>/dev/null; then
        warn "~/.bashrc has ROCm exports but this is a Vulkan toolbox."
        warn "They won't break the build (paths just don't exist), but consider"
        warn "removing them — the ROCm bootstrap uses /etc/profile.d/ instead."
    fi
}

revert_cargo_swap() {
    # Put the Linux feature back to "rocm" (repo default). Idempotent.
    swap_inference_backend rocm
    echo "Reverted crates/sovereign-inference/Cargo.toml to the rocm default."
    echo "If git status is now clean, you're good to commit."
}

main() {
    parse_args "$@"

    if (( REVERT_CARGO )); then
        revert_cargo_swap
        return
    fi

    preflight_image_quirks

    local resolved
    resolved="$(detect_backend)"
    echo "== Backend: $resolved =="

    if (( NO_SUDO )); then
        echo "== --no-sudo: skipping package install (caller handled it) =="
    else
        sudo -v  # prompt once up front so the rest doesn't stall mid-way
        case "$(detect_distro):$resolved" in
            fedora:rocm)         install_fedora_rocm ;;
            fedora:vulkan)       install_fedora_vulkan ;;
            ubuntu:rocm|debian:rocm)     install_ubuntu_rocm ;;
            ubuntu:vulkan|debian:vulkan) install_ubuntu_vulkan ;;
            *) die "unsupported distro/backend combo — see docs/TOOLBOX_SETUP.md §3" ;;
        esac
    fi

    ensure_rust

    if [[ "$resolved" == "rocm" ]]; then
        if (( NO_SUDO )); then
            warn "--no-sudo: skipping /etc/ld.so.conf.d and /etc/profile.d drops"
        else
            configure_rocm_runtime
        fi
        swap_inference_backend rocm
    else
        warn_stale_rocm_env
        swap_inference_backend vulkan
    fi

    wipe_llama_cache_if_backend_changed "$resolved"

    cat <<EOF

== Done ==

Backend: $resolved

EOF
    if [[ "$resolved" == "rocm" ]]; then
        cat <<EOF
Open a new shell (or run \`source /etc/profile.d/sovereign-rocm.sh\`)
to pick up ROCM_PATH / HIP_PATH, then:

  cargo build --release

EOF
    else
        cat <<EOF
A local edit was applied to crates/sovereign-inference/Cargo.toml to
pick the Vulkan backend for llama-cpp-2. Do NOT commit it — the repo
default stays rocm. Undo with:

  ./scripts/bootstrap-linux.sh --revert-cargo

Then:

  cargo build --release

EOF
    fi
}

main "$@"
