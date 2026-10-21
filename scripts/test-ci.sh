#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# The same gates run locally and in .github/workflows/test.yml.
set -euo pipefail
cd "$(dirname "$0")/.."

mode=${1:-}
case "$mode:$(uname -s)" in
    linux:Linux|macos:Darwin) ;;
    *) echo "Usage: $0 linux|macos (on the matching OS)" >&2; exit 2 ;;
esac

export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$PWD/target"}
mkdir -p "$CARGO_TARGET_DIR"
CARGO_TARGET_DIR=$(cd "$CARGO_TARGET_DIR" && pwd)
report_dir="$CARGO_TARGET_DIR/ci/$mode"
mkdir -p "$report_dir"
exec > >(tee "$report_dir/run.log") 2>&1

run_phase() {
    local phase=$1 budget=$2
    shift 2
    python3 scripts/ci-phase.py "$report_dir" "$phase" "$budget" "$@"
}

# Never inherit the invoking pane's socket, identity, or persistent state.
for variable in ${!FLOWMUX_@}; do unset "$variable"; done
test_root=$(mktemp -d /tmp/fm-ci.XXXXXX)
trap 'result=$?; echo "CI_GATE_EXIT=$result"; rm -rf "$test_root"' EXIT
export TMPDIR="$test_root/tmp" FLOWMUX_RUNTIME_DIR="$test_root/run"
export XDG_RUNTIME_DIR="$test_root/run" XDG_CONFIG_HOME="$test_root/config"
export XDG_STATE_HOME="$test_root/state" XDG_DATA_HOME="$test_root/data"
export XDG_CACHE_HOME="$test_root/cache"
mkdir -p "$TMPDIR" "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME" "$XDG_STATE_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"
chmod 700 "$XDG_RUNTIME_DIR"
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} RUST_BACKTRACE=1 G_DEBUG=fatal-criticals

printf 'CI_GATE=%s\n' "$mode"
uname -a
rustc -Vv
cargo --version
pkg-config --modversion gtk4 libadwaita-1
run_phase ci-runner-tests 120 python3 scripts/test-ci-runner.py
run_phase installer-routing 60 python3 scripts/test-install.py
run_phase agent-runner-tests 120 python3 -B scripts/test-agent-unit-runner.py
run_phase agent-team-tests 120 python3 scripts/test-agent-team.py
run_phase format 180 cargo fmt --all -- --check

if [[ $mode == linux ]]; then
    cargo llvm-cov --version
    rustup component add llvm-tools-preview
    export GDK_BACKEND=x11 GTK_A11Y=test LANG=C.UTF-8 LC_ALL=C.UTF-8
    locale
    # Keep a fixed virtual desktop. AppArmor is provisioned by the host/CI;
    # this runner does not disable the WebKit sandbox or change host policy.
    result=0
    run_phase coverage 1500 xvfb-run -a -s '-screen 0 1280x1024x24' dbus-run-session -- \
        bash scripts/test-coverage.sh -- --nocapture || result=$?
    # Preserve failure evidence even when a test fails; never turn it green.
    mkdir -p "$CARGO_TARGET_DIR/llvm-cov"
    run_phase coverage-json 180 cargo llvm-cov report --json --summary-only \
        --output-path "$CARGO_TARGET_DIR/llvm-cov/summary.json" || result=1
    run_phase coverage-html 180 cargo llvm-cov report --html || result=1
    run_phase coverage-thresholds 180 cargo llvm-cov report --fail-under-lines 79 --fail-under-regions 78 \
        --fail-under-functions 78 || result=1
    exit "$result"
fi

# Hosted macOS VMs compile OpenGL shaders on the GTK thread for seconds.
# Keep local and CI functional checks independent of inherited GPU settings.
export GSK_RENDERER=cairo
printf 'GSK_RENDERER=%s\n' "$GSK_RENDERER"

run_phase macos-installer 180 python3 scripts/test-install-macos.py
run_phase build 900 cargo build --workspace --locked
run_phase cli 300 cargo test -p flowmux-cli --locked
git rev-parse HEAD
python3 -c 'import hashlib, pathlib, sys; print("CLI_SHA256=" + hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())' "$CARGO_TARGET_DIR/debug/flowmuxctl"
run_phase agent-unit 960 python3 scripts/test-agent-unit.py 2>&1 | tee "$report_dir/agent-unit.log"
run_phase ipc 300 cargo test -p flowmux --bin flowmux --locked ipc_handler::tests
run_phase bridge 180 cargo test -p flowmux --bin flowmux --locked bridge::tests
run_phase session-path-unit 180 cargo test -p flowmux --bin flowmux --locked ui::window::sessions::
export FLOWMUX_BUNDLED_CLI_PATH="$CARGO_TARGET_DIR/debug/flowmuxctl"
run_phase native 600 cargo test -p flowmux --test macos_native --features native-smoke --locked \
    2>&1 | tee "$report_dir/native-smoke.log"
grep -q '^MACOS_NATIVE_SMOKE_OK$' "$report_dir/native-smoke.log"

FLOWMUX_EDITOR_FOCUS_SMOKE_ONLY=1 run_phase editor-keyboard 180 cargo test -p flowmux --test macos_native --features native-smoke --locked \
    2>&1 | tee "$report_dir/editor-keyboard-smoke.log"
grep -q '^MACOS_FOUR_PANE_EDITING_KEYS_OK$' "$report_dir/editor-keyboard-smoke.log"

# Install skills through the mapped Options UI using a private agent home.
FLOWMUX_SKILLS_SMOKE_ONLY=1 run_phase skills 180 cargo test -p flowmux --test macos_native --features native-smoke --locked \
    2>&1 | tee "$report_dir/skills-smoke.log"
grep -q '^MACOS_NATIVE_SKILLS_INSTALL_UPDATE_OK$' "$report_dir/skills-smoke.log"

# The same CLI/PTY/VTE hook matrix used by the Linux SSH GUI gate.
FLOWMUX_AGENT_SMOKE_ONLY=1 run_phase agent-hooks 300 cargo test -p flowmux --test macos_native --features native-smoke --locked \
    2>&1 | tee "$report_dir/agent-hooks.log"
grep -q '^MACOS_NATIVE_AGENT_HOOKS_OK$' "$report_dir/agent-hooks.log"

FLOWMUX_SESSION_PATHS_SMOKE_ONLY=1 run_phase session-paths 180 cargo test -p flowmux --test macos_native --features native-smoke --locked \
    2>&1 | tee "$report_dir/session-paths.log"
grep -q '^MACOS_SESSION_PATHS_OK$' "$report_dir/session-paths.log"

run_phase markdown 180 cargo test -p flowmux-md-viewer --locked -- --test-threads=1 \
    2>&1 | tee "$report_dir/markdown.log"
grep -q '^MACOS_MARKDOWN_NATIVE_OK$' "$report_dir/markdown.log"

run_phase notifications 120 cargo test -p flowmux-notify --features native-smoke --locked \
    2>&1 | tee "$report_dir/notifications.log"
grep -q '^MACOS_NATIVE_DOCK_BADGE_OK$' "$report_dir/notifications.log"
FLOWMUX_NOTIFICATION_CLEANUP_SMOKE_ONLY=1 run_phase notification-cleanup 120 cargo test -p flowmux --test macos_native --features native-smoke --locked \
    2>&1 | tee "$report_dir/notification-cleanup.log"
grep -q '^MACOS_NOTIFICATION_CLEANUP_OK$' "$report_dir/notification-cleanup.log"
run_phase notification-routing 120 cargo test -p flowmux --bin flowmux --locked native_notification_target_opens_the_source_pid_socket

run_phase webdriver-address 120 cargo test -p flowmux --bin flowmux --locked macos_webdriver::tests
run_phase webdriver-native 180 cargo test -p flowmux --test macos_webdriver --features native-smoke --locked
run_phase webdriver 180 python3 scripts/test-macos-webdriver.py "$CARGO_TARGET_DIR/debug/flowmux" \
    2>&1 | tee "$report_dir/webdriver.log"
grep -q '^MACOS_WEBDRIVER_OK$' "$report_dir/webdriver.log"
