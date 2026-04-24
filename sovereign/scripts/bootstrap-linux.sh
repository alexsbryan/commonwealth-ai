#!/usr/bin/env bash
# bootstrap-linux.sh — one-shot installer for the Linux build deps.
#
# Handles Fedora (dnf) and Ubuntu/Debian (apt). Idempotent. Expects
# to run inside a Strix Halo toolbox (kyuz0 ROCm 7.2.1 image or
# equivalent), though it also works on any Fedora 40+ / Ubuntu 24.04+
# host with a ROCm 7.x apt/dnf repo configured.
#
# Usage:
#   ./scripts/bootstrap-linux.sh
#
# What it does:
#   1. Installs Rust, clang/libclang, cmake, protobuf, OpenSSL,
#      ROCm HIP SDK (hipcc + headers + rocBLAS/hipBLAS dev), and the
#      GTK/WebKit deps Tauri 2 needs for sovereign-desktop.
#   2. Writes /etc/ld.so.conf.d/sovereign-rocm.conf so the runtime
#      linker can find libamdhip64 etc. without LD_LIBRARY_PATH.
#   3. Writes /etc/profile.d/sovereign-rocm.sh so new shells have
#      ROCM_PATH / HIP_PATH / PATH / CMAKE_PREFIX_PATH pre-set for
#      cargo build.
#
# After it finishes, open a fresh shell (or `source` the profile drop)
# and run `cargo build --release` from the sovereign/ workspace root.

set -euo pipefail

ROCM_PATH="${ROCM_PATH:-/opt/rocm}"

die() { echo "bootstrap-linux: $*" >&2; exit 1; }

detect_distro() {
    [[ -f /etc/os-release ]] || die "can't detect distro (/etc/os-release missing)"
    # shellcheck source=/dev/null
    . /etc/os-release
    echo "$ID"
}

install_fedora() {
    echo "== Installing Fedora build deps =="
    sudo dnf install -y \
        rust cargo \
        clang clang-devel \
        cmake gcc gcc-c++ pkg-config \
        protobuf-compiler protobuf-devel \
        openssl-devel \
        rocm-hip-sdk7.2.1 \
        webkit2gtk4.1-devel gtk3-devel libsoup3-devel librsvg2-devel \
        libayatana-appindicator-gtk3
}

install_ubuntu() {
    echo "== Installing Ubuntu/Debian build deps =="
    sudo apt-get update
    sudo apt-get install -y \
        cargo rustc \
        clang libclang-dev \
        cmake build-essential pkg-config \
        protobuf-compiler libprotobuf-dev \
        libssl-dev \
        rocm-hip-sdk \
        libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev librsvg2-dev \
        libayatana-appindicator3-1
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

main() {
    sudo -v  # prompt once up front so the rest doesn't stall mid-way

    case "$(detect_distro)" in
        fedora)        install_fedora ;;
        ubuntu|debian) install_ubuntu ;;
        *)             die "unsupported distro — see docs/TOOLBOX_SETUP.md §3 for the manual package list" ;;
    esac

    configure_rocm_runtime

    cat <<EOF

== Done ==

Open a new shell (or run \`source /etc/profile.d/sovereign-rocm.sh\`)
to pick up ROCM_PATH / HIP_PATH, then:

  cargo build --release

For a Vulkan toolbox instead of ROCm, swap the Linux feature in
crates/sovereign-inference/Cargo.toml ("rocm" → "vulkan") — one line,
no other changes.
EOF
}

main "$@"
