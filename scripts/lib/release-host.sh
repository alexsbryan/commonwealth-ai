#!/usr/bin/env bash
# release-host.sh — one decider for "which host am I releasing from, which
# legs can it build, and where do the BSD and GNU tools disagree?"
#
# WHY THIS EXISTS
#
# The three release drivers (release-all.sh, release-cli-local.sh,
# release-desktop-local.sh) were written on, and hard-gated to, one arm64
# Mac: each opened with `[[ "$(uname -sm)" == "Darwin arm64" ]] || die`. That
# gate was honest — the scripts really did assume BSD `df -g`, BSD `du -sg`,
# `shasum`, `stat -f %m`, `xcrun`, and a `podman machine` VM that only exists
# on macOS. But it also meant a Fedora workstation could not cut ANY leg of a
# release, including the three legs it is strictly better at:
#
#   • The Linux desktop and Linux CLI legs run `--platform linux/amd64`. On
#     the arm64 Mac that is qemu emulation, which is why the shader compile is
#     pinned to a SINGLE cpu (the glslc-reap deadlock that stalled v0.3.0 for
#     10.5 hours). On an x86_64 Linux host the same container is NATIVE: no
#     emulation, no deadlock, no cap.
#   • The Windows leg is cargo-xwin in a host-arch container; it cross-compiles
#     from anywhere.
#
# The Apple legs genuinely cannot move: they need the macOS SDK (`xcrun`),
# `codesign`, `hdiutil`, and `plutil`. So this is not "make Linux able to cut a
# release" — it is "let each host cut the legs it can, into the same draft
# tags, with the completeness gate moved to the one place that can see both
# halves" (release-all.sh's publish gate, which counts assets ON THE RELEASE).
#
# THE RULE
#
# A host declares its capability once, here. Callers ask; they never re-derive
# it from `uname`, and they never silently skip a leg — an auto-skip is always
# announced by name (ARCH §18.3: never silently substitute).

# Idempotent: the drivers source this, and release-all.sh also execs drivers
# that source it again.
[[ -n "${_RELEASE_HOST_SH:-}" ]] && return 0
_RELEASE_HOST_SH=1

# ─── Host identity ────────────────────────────────────────────────────
RELEASE_HOST_UNAME="$(uname -sm)"
case "$RELEASE_HOST_UNAME" in
    "Darwin arm64") RELEASE_HOST_KIND=mac-arm64 ;;
    "Linux x86_64") RELEASE_HOST_KIND=linux-x86_64 ;;
    *)              RELEASE_HOST_KIND=unsupported ;;
esac

# Capability, derived once. APPLE covers both macOS legs of both artifacts;
# there is no partial state (an Intel-mac cross needs the same SDK as native).
case "$RELEASE_HOST_KIND" in
    mac-arm64)
        RELEASE_CAN_APPLE=1
        RELEASE_CAN_CONTAINER=1
        # The amd64 Linux container is qemu-emulated here. This is the single
        # fact that the glslc concurrency cap keys off.
        RELEASE_LINUX_LEG_EMULATED=1
        # The Windows image is built for the HOST arch (cargo-xwin cross-
        # compiles from any arch, so running it native is free speed).
        RELEASE_HOST_CONTAINER_PLATFORM=linux/arm64
        ;;
    linux-x86_64)
        RELEASE_CAN_APPLE=0
        RELEASE_CAN_CONTAINER=1
        RELEASE_LINUX_LEG_EMULATED=0
        RELEASE_HOST_CONTAINER_PLATFORM=linux/amd64
        ;;
    *)
        RELEASE_CAN_APPLE=0
        RELEASE_CAN_CONTAINER=0
        RELEASE_LINUX_LEG_EMULATED=0
        RELEASE_HOST_CONTAINER_PLATFORM=linux/amd64
        ;;
esac

RELEASE_HOST_UNSUPPORTED_MSG="unsupported release host '$RELEASE_HOST_UNAME'. \
The release drivers run on 'Darwin arm64' (Apple + container legs) or \
'Linux x86_64' (container legs only)."

# Are we inside a toolbox/podman container right now? Load-bearing on Linux:
# the container legs need podman, which is NOT reachable from inside a
# toolbox, while the native-cargo steps (test gate, router-cache check) need
# the build deps that only the toolbox has. See release_native_run below.
if [[ -f /run/.containerenv ]]; then
    RELEASE_INSIDE_CONTAINER=1
    RELEASE_CONTAINER_NAME="$(sed -n 's/^name="\(.*\)"$/\1/p' /run/.containerenv 2>/dev/null | head -1)"
else
    RELEASE_INSIDE_CONTAINER=0
    RELEASE_CONTAINER_NAME=""
fi

# ─── BSD vs GNU shims ─────────────────────────────────────────────────
# Each of these had a BSD-only spelling inlined in at least one driver, and
# each fails DIFFERENTLY on Linux rather than erroring cleanly:
#   df -g    → "df: invalid option -- 'g'" (empty field ⇒ arithmetic error)
#   du -sg   → same
#   stat -f  → on Linux means "stat the FILESYSTEM"; %m prints a mount point,
#              which then explodes inside (( … > epoch ))
#   shasum   → simply absent on Fedora

release_free_gb() {  # release_free_gb <dir> → whole GB free, or 0
    local dir="$1"
    if [[ "$RELEASE_HOST_KIND" == mac-arm64 ]]; then
        df -g "$dir" 2>/dev/null | awk 'NR==2{print $4+0}'
    else
        df -BG "$dir" 2>/dev/null | awk 'NR==2{gsub(/G/,"",$4); print $4+0}'
    fi
}

release_dir_gb() {  # release_dir_gb <dir> → whole GB used, or 0
    local dir="$1"
    [[ -d "$dir" ]] || { echo 0; return; }
    if [[ "$RELEASE_HOST_KIND" == mac-arm64 ]]; then
        du -sg "$dir" 2>/dev/null | awk '{print $1+0}'
    else
        du -sBG "$dir" 2>/dev/null | awk '{gsub(/G/,"",$1); print $1+0}'
    fi
}

# `stat` arguments that print a file's APPARENT SIZE in bytes — what gets
# tar-streamed, not on-disk blocks. Used with `find -exec … {} +` so one stat
# covers a batch. The fourth BSD/GNU wrapper, and it exists because GNU
# `find -printf '%s\n'` has no BSD equivalent at all: on a Mac that spelling
# does not degrade, it errors, and the caller sees an empty walk.
if [[ "$RELEASE_HOST_KIND" == mac-arm64 ]]; then
    RELEASE_STAT_SIZE=(-f %z)
else
    RELEASE_STAT_SIZE=(-c %s)
fi

release_file_mtime() {  # release_file_mtime <file> → mtime epoch, or 0
    local f="$1"
    [[ -f "$f" ]] || { echo 0; return; }
    if [[ "$RELEASE_HOST_KIND" == mac-arm64 ]]; then
        stat -f %m "$f" 2>/dev/null || echo 0
    else
        stat -c %Y "$f" 2>/dev/null || echo 0
    fi
}

# The sha256 sidecar must stay byte-identical to what CI writes, because
# SHA256SUMS is concatenated from a MIX of CI-built and locally-built
# sidecars. `shasum -a 256` and GNU `sha256sum` both emit "<hash>  <name>",
# so only the command name differs.
if command -v shasum >/dev/null 2>&1; then
    release_sha256() { shasum -a 256 "$@"; }
elif command -v sha256sum >/dev/null 2>&1; then
    release_sha256() { sha256sum "$@"; }
else
    release_sha256() { echo "release-host: neither shasum nor sha256sum on PATH" >&2; return 127; }
fi

# ─── Container runtime readiness ──────────────────────────────────────
# On macOS podman is a VM ("podman machine") that has to exist, be sized, and
# be started. On Linux podman is native — there is no machine, and asking for
# one returns nothing, which the Mac-shaped check read as 0MiB and refused.
# Sets RELEASE_CONTAINER_ERR and returns non-zero on failure.
release_container_ready() {
    RELEASE_CONTAINER_ERR=""
    if ! command -v podman >/dev/null 2>&1; then
        if (( RELEASE_INSIDE_CONTAINER )); then
            RELEASE_CONTAINER_ERR="podman is not on PATH, and you are INSIDE the '${RELEASE_CONTAINER_NAME:-unknown}' container. \
The container legs cannot be driven from in here (no nested podman, no flatpak-spawn). \
Run the release from the host shell; the native-cargo steps will re-enter the toolbox by themselves."
        else
            RELEASE_CONTAINER_ERR="'podman' is required on PATH for the container legs."
        fi
        return 1
    fi

    if [[ "$RELEASE_HOST_KIND" == mac-arm64 ]]; then
        local mem
        mem="$(podman machine inspect --format '{{.Resources.Memory}}' 2>/dev/null || echo 0)"
        mem="${mem:-0}"
        if (( mem == 0 )); then
            RELEASE_CONTAINER_ERR="no podman machine. One-time setup: podman machine init --cpus 8 --memory 24576 --disk-size 120 && podman machine start"
            return 1
        fi
        if (( mem < 16384 )); then
            RELEASE_CONTAINER_ERR="podman machine has ${mem}MiB; ggml-vulkan's shader compile OOMs below ~16GiB. Resize: podman machine stop && podman machine set --memory 24576 && podman machine start"
            return 1
        fi
        podman machine start >/dev/null 2>&1 || true
    else
        # Native podman: the same ~16GiB shader-compile floor applies, but it
        # is the HOST's memory, not a VM allocation.
        local kb=0
        kb="$(awk '/^MemTotal:/{print $2}' /proc/meminfo 2>/dev/null || echo 0)"
        if (( kb > 0 && kb / 1024 < 16384 )); then
            RELEASE_CONTAINER_ERR="host has $((kb / 1024))MiB of RAM; ggml-vulkan's shader compile OOMs below ~16GiB."
            return 1
        fi
    fi

    if ! podman info >/dev/null 2>&1; then
        if [[ "$RELEASE_HOST_KIND" == mac-arm64 ]]; then
            RELEASE_CONTAINER_ERR="podman is installed but unreachable even after 'podman machine start'. Fix it before starting a release: podman machine start (or 'podman machine init' if there is no VM)."
        else
            RELEASE_CONTAINER_ERR="podman is installed but 'podman info' fails. Fix the rootless podman setup before starting a release."
        fi
        return 1
    fi
    return 0
}

# Run a command "in the build VM" for diagnostics. On macOS the container work
# happens inside the podman machine and is invisible from the host; on Linux
# the host IS the machine.
release_vm_exec() {  # release_vm_exec <shell-command-string>
    if [[ "$RELEASE_HOST_KIND" == mac-arm64 ]]; then
        podman machine ssh "$1" 2>/dev/null
    else
        bash -c "$1" 2>/dev/null
    fi
}

# Is a build actually making progress? Used by the stall watchdog, where a
# wrong answer either kills a healthy build or lets a hung one run for hours.
# On Linux rootless podman the container's rustc/cargo are visible in the
# host PID namespace, so the pgrep arm is authoritative for BOTH host and
# container legs; the loadavg arm is the backstop.
release_build_busy() {  # sets RELEASE_BUILD_LOAD for logging; 0 = busy
    RELEASE_BUILD_LOAD=""
    if pgrep -x cargo >/dev/null 2>&1 || pgrep -x rustc >/dev/null 2>&1 \
       || pgrep -x cargo-tauri >/dev/null 2>&1; then
        return 0
    fi
    RELEASE_BUILD_LOAD="$(release_vm_exec 'cat /proc/loadavg' | awk '{print $1+0}' | tail -1)"
    awk "BEGIN{exit !(${RELEASE_BUILD_LOAD:-1} >= 0.5)}"
}

# ─── Native cargo, on a host whose build deps live in a toolbox ───────
# The Fedora host cannot compile this workspace: llama-cpp-sys-4's build
# script needs clang + Vulkan headers, which live in the sovereign-vulkan
# toolbox. But podman — and therefore every container leg — is unreachable
# from inside that toolbox. So a Linux release straddles the boundary: the
# container legs run on the host, and the two native-cargo steps (the
# workspace test gate and the router-cache freshness check) re-enter the
# toolbox. This resolves which, once, and says so.
#
# Override with RELEASE_NATIVE_RUN_PREFIX (set it empty to force direct).
SOVEREIGN_TOOLBOX="${SOVEREIGN_TOOLBOX:-sovereign-vulkan}"
if [[ -n "${RELEASE_NATIVE_RUN_PREFIX+x}" ]]; then
    read -r -a _RELEASE_NATIVE_RUN <<<"${RELEASE_NATIVE_RUN_PREFIX}"
    RELEASE_NATIVE_RUN_VIA="RELEASE_NATIVE_RUN_PREFIX override"
elif (( RELEASE_INSIDE_CONTAINER )); then
    _RELEASE_NATIVE_RUN=()
    RELEASE_NATIVE_RUN_VIA="directly (already inside '${RELEASE_CONTAINER_NAME:-a container}')"
elif [[ "$RELEASE_HOST_KIND" == linux-x86_64 ]] && command -v toolbox >/dev/null 2>&1 \
     && toolbox list --containers 2>/dev/null | grep -q "[[:space:]]${SOVEREIGN_TOOLBOX}[[:space:]]"; then
    _RELEASE_NATIVE_RUN=(toolbox run -c "$SOVEREIGN_TOOLBOX")
    RELEASE_NATIVE_RUN_VIA="toolbox run -c $SOVEREIGN_TOOLBOX"
else
    _RELEASE_NATIVE_RUN=()
    RELEASE_NATIVE_RUN_VIA="directly on the host"
fi

release_native_run() {  # release_native_run <cmd> [args…]
    if (( ${#_RELEASE_NATIVE_RUN[@]} )); then
        "${_RELEASE_NATIVE_RUN[@]}" "$@"
    else
        "$@"
    fi
}

# ─── Linux-leg shader concurrency ─────────────────────────────────────
# The cap exists for ONE reason: under qemu the ggml-vulkan build script's
# glslc pool deadlocks in wait(). Natively there is no deadlock and no reason
# to give a 32-core box one core. Honour an explicit override either way.
release_linux_build_cpus() {
    if [[ -n "${SOVEREIGN_LINUX_BUILD_CPUS:-}" ]]; then
        echo "$SOVEREIGN_LINUX_BUILD_CPUS"
    elif (( RELEASE_LINUX_LEG_EMULATED )); then
        echo 1
    else
        # Native: no cap. Reported as the host's cpu count so the log says a
        # number rather than "unlimited", and so taskset stays a no-op.
        (command -v nproc >/dev/null 2>&1 && nproc) || echo 8
    fi
}
