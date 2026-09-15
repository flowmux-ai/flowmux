#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Docker-backed compatibility smoke check for the Ubuntu support matrix.
#
# Ubuntu 24.04 and 26.04 use distro packages for native GUI smoke checks under
# Xvfb. On 22.04, check that GTK/libadwaita remain below the native build floor;
# use the Flatpak GNOME runtime there.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DOCKER="${DOCKER:-docker}"
NATIVE_UBUNTU_VERSIONS="${NATIVE_UBUNTU_VERSIONS:-24.04 26.04}"

need_docker() {
    if ! command -v "$DOCKER" >/dev/null 2>&1; then
        echo "error: docker is required; set DOCKER=/path/to/docker to override" >&2
        exit 1
    fi
}

check_jammy_flatpak_path() {
    echo "==> ubuntu:22.04 package floor"
    "$DOCKER" run --rm ubuntu:22.04 bash -lc '
set -euo pipefail
apt-get update >/dev/null
gtk="$(apt-cache policy libgtk-4-dev | awk "/Candidate:/ {print \$2}")"
adw="$(apt-cache policy libadwaita-1-dev | awk "/Candidate:/ {print \$2}")"
# flowmux needs gtk4 >= 4.12 (v4_12) and libadwaita >= 1.5 (v1_5); Ubuntu 22.04
# ships ~4.6 / ~1.1, below the floor, so the native build is impossible and
# 22.04 must use the Flatpak GNOME runtime. VTE >= 0.76 is provided there.
if dpkg --compare-versions "$gtk" ge 4.12 && dpkg --compare-versions "$adw" ge 1.5; then
    echo "error: ubuntu:22.04 unexpectedly meets the GTK/libadwaita floor (gtk=$gtk adw=$adw)" >&2
    exit 1
fi
echo "gtk=$gtk libadwaita=$adw below the v4_12/v1_5 floor; use Flatpak GNOME runtime for 22.04"
'
}

check_native_ubuntu() {
    local version="$1"
    echo "==> ubuntu:$version native GUI smoke"
    "$DOCKER" run --rm \
        --mount "type=bind,source=$REPO_ROOT,target=/workspace,readonly" \
        -e DEBIAN_FRONTEND=noninteractive \
        "ubuntu:$version" bash -lc '
set -euo pipefail
apt-get update >/dev/null
apt-get install -y --no-install-recommends \
    ca-certificates curl git build-essential pkg-config \
    meson ninja-build python3 xvfb xauth \
    libgtk-4-dev libadwaita-1-dev libvte-2.91-gtk4-dev \
    libwebkitgtk-6.0-dev libssl-dev \
    libdbus-1-dev libsecret-1-dev >/dev/null
curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal >/dev/null
. "$HOME/.cargo/env"
echo "rustc $(rustc --version)"
CARGO_HOME=/tmp/cargo CARGO_TARGET_DIR=/tmp/flowmux-target \
    cargo build --manifest-path /workspace/Cargo.toml \
    -p flowmux -p flowmux-cli --locked
export XDG_RUNTIME_DIR=/tmp/flowmux-runtime
export XDG_STATE_HOME=/tmp/flowmux-state
export XDG_DATA_HOME=/tmp/flowmux-data
export XDG_CONFIG_HOME=/tmp/flowmux-config
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_STATE_HOME" "$XDG_DATA_HOME" "$XDG_CONFIG_HOME"
chmod 700 "$XDG_RUNTIME_DIR"
xvfb-run -a -s "-screen 0 1280x800x24" /tmp/flowmux-target/debug/flowmux \
    >/tmp/flowmux-gui.log 2>&1 &
gui_pid=$!
cleanup() {
    status=$?
    kill "$gui_pid" >/dev/null 2>&1 || true
    wait "$gui_pid" >/dev/null 2>&1 || true
    if [ "$status" -ne 0 ]; then
        cat /tmp/flowmux-gui.log >&2 || true
    fi
    exit "$status"
}
trap cleanup EXIT
for _ in $(seq 1 120); do
    if /tmp/flowmux-target/debug/flowmux ping >/tmp/flowmux-ping.out 2>/tmp/flowmux-ping.err; then
        break
    fi
    if ! kill -0 "$gui_pid" 2>/dev/null; then
        echo "flowmux GUI exited before ping" >&2
        exit 1
    fi
    sleep 0.25
done
/tmp/flowmux-target/debug/flowmux workspace new \
    --name "Linux GUI Smoke" --root /workspace --json >/tmp/flowmux-workspace.json
pane=""
for _ in $(seq 1 120); do
    /tmp/flowmux-target/debug/flowmux tree --json >/tmp/flowmux-tree.json
    pane=$(python3 - <<PY
import json
try:
    data = json.load(open("/tmp/flowmux-tree.json"))
    print(data["tree"]["workspaces"][0]["panes"][0]["id"])
except Exception:
    pass
PY
)
    if [ -n "$pane" ]; then
        break
    fi
    sleep 0.25
done
if [ -z "$pane" ]; then
    echo "no pane created" >&2
    cat /tmp/flowmux-tree.json >&2
    exit 1
fi
keys=$(printf "printf \"FLOWMUX_LINUX_GUI_SMOKE_OK\\\\n\"\\n")
/tmp/flowmux-target/debug/flowmux send-keys "$pane" "$keys" >/tmp/flowmux-send.out
sleep 0.5
/tmp/flowmux-target/debug/flowmux read-screen "$pane" >/tmp/flowmux-screen.txt
grep -q FLOWMUX_LINUX_GUI_SMOKE_OK /tmp/flowmux-screen.txt
echo "ubuntu GUI smoke passed pane=$pane"
'
}

need_docker
check_jammy_flatpak_path
for version in $NATIVE_UBUNTU_VERSIONS; do
    check_native_ubuntu "$version"
done
echo "==> ubuntu compatibility smoke checks passed"
