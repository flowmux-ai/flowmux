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
suite=${2:-regression}
# Native browser dialogs and pointer navigation follow desktop timing: extended only.
if [[ $suite == extended ]]; then export FLOWMUX_BROWSER_NATIVE_DIALOGS=1; fi
case "$suite" in
    regression|extended) ;;
    *) echo "Usage: $0 linux|macos [regression|extended]" >&2; exit 2 ;;
esac
# Linux CI runs each group on its own instance; see scripts/test-linux.sh.
group=${3:-all}
case "$mode:$group" in
    *:all|*:scripts) ;;
    linux:build|linux:workspace|linux:team-usage|linux:team|linux:agent-host|linux:agent-session) ;;
    linux:browser|linux:terminal|macos:headless|macos:app|macos:native) ;;
    *) echo "Usage: $0 linux|macos [regression|extended] [group]" >&2; exit 2 ;;
esac
in_group() { [[ $group == all || $group == "$1" ]]; }

export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$PWD/target"}
mkdir -p "$CARGO_TARGET_DIR"
CARGO_TARGET_DIR=$(cd "$CARGO_TARGET_DIR" && pwd)
report_dir="$CARGO_TARGET_DIR/ci/$mode"
mkdir -p "$report_dir"
rm -f -- "$report_dir/"*.json "$report_dir/tests/"*.json
exec > >(tee "$report_dir/run.log") 2>&1

run_phase() {
    local phase=$1 budget=$2
    shift 2
    python3 scripts/ci-phase.py "$report_dir" "$phase" "$budget" "$@"
}

# Never inherit the invoking pane's socket, identity, or persistent state.
for variable in ${!FLOWMUX_@}; do unset "$variable"; done
unset CODEX_THREAD_ID CODEX_SESSION_ID CLAUDECODE CLAUDE_CODE_ENTRYPOINT
# A provider CLI or Team worker exports its real homes; tests must never resolve them.
unset CLAUDE_CONFIG_DIR CODEX_HOME GEMINI_CLI_HOME
test_root=$(mktemp -d /tmp/fm-ci.XXXXXX)
# Desktop services the gate's private bus and display started can outlive an interrupted run:
# their FUSE mounts make the runtime directory unremovable and they keep running.
release_test_root() {
    if [[ -r /proc/self/mountinfo ]]; then
        local mount
        awk -v root="$test_root/" 'index($5, root) == 1 { print $5 }' /proc/self/mountinfo |
            sort -r | while IFS= read -r mount; do
                fusermount3 -u -z "$mount" 2>/dev/null || fusermount -u -z "$mount" 2>/dev/null ||
                    umount -l "$mount" 2>/dev/null || true
            done
    fi
    local pids pid
    pids=$(pgrep -u "$(id -u)" -f -- "$test_root/" 2>/dev/null || true)
    [[ -n $pids ]] || return 0
    for pid in $pids; do
        [[ $pid == "$$" ]] || kill "$pid" 2>/dev/null || true
    done
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        pgrep -u "$(id -u)" -f -- "$test_root/" >/dev/null 2>&1 || return 0
        sleep 0.2
    done
    for pid in $(pgrep -u "$(id -u)" -f -- "$test_root/" 2>/dev/null || true); do
        [[ $pid == "$$" ]] || kill -KILL "$pid" 2>/dev/null || true
    done
}
cleanup() {
    local result=$?
    # On Ctrl-C the log's tee can end first; writing to it must not kill cleanup with SIGPIPE.
    trap '' PIPE
    echo "CI_GATE_EXIT=$result" 2>/dev/null || true
    release_test_root
    if [[ $result == 0 ]]; then
        rm -rf "$test_root" || echo "CI cleanup incomplete: $test_root" >&2
    else
        local evidence="$report_dir/failure-${test_root##*/}"
        if mv "$test_root" "$evidence"; then
            echo "CI_FAILURE_ARTIFACTS=$evidence"
        else
            echo "CI failure artifacts remain at: $test_root" >&2
        fi
    fi
    exit "$result"
}
trap cleanup EXIT
# An interrupted gate still releases its test root: bash runs no EXIT trap when it dies of a signal.
trap 'trap - INT; exit 130' INT
trap 'trap - TERM; exit 143' TERM
export TMPDIR="$test_root/tmp" FLOWMUX_RUNTIME_DIR="$test_root/run"
export XDG_RUNTIME_DIR="$test_root/run" XDG_CONFIG_HOME="$test_root/config"
export XDG_STATE_HOME="$test_root/state" XDG_DATA_HOME="$test_root/data"
export XDG_CACHE_HOME="$test_root/cache"
mkdir -p "$TMPDIR" "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME" "$XDG_STATE_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"
chmod 700 "$XDG_RUNTIME_DIR"
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} RUST_BACKTRACE=1 G_DEBUG=fatal-criticals

printf 'CI_GATE=%s SUITE=%s GROUP=%s\n' "$mode" "$suite" "$group"
uname -a
# An instance that receives prebuilt binaries has no need for the toolchain.
if [[ ${CI_TEST_PREBUILT:-} != 1 ]]; then
    rustc -Vv
    cargo --version
fi
# The script self-tests need neither the native libraries nor the SSH fixture.
if [[ $group != scripts ]]; then
    pkg-config --modversion glib-2.0 gtk4 libadwaita-1
    if [[ $mode == linux ]]; then
        run_phase ssh-prerequisites 30 python3 scripts/ssh-workspace-fixture.py --check
        # WebKit's sandbox needs user namespaces. Where AppArmor restricts them, an unprofiled
        # gate fails only when its first browser starts, long after this point.
        if [[ $group != build && $(cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns 2>/dev/null) == 1 ]] &&
            command -v bwrap >/dev/null && ! bwrap --unshare-user --ro-bind / / true 2>/dev/null; then
            echo "error: this host restricts user namespaces and the gate is not in the WebKit profile." >&2
            echo "       Run: aa-exec -p flowmux-webkit -- bash scripts/test-ci.sh $mode $suite $group" >&2
            echo "       (install the profile with packaging/apparmor/flowmux-webkit; see docs/setup.md)" >&2
            exit 2
        fi
    fi
fi
# The full failure/marker matrix starts many mock processes on hosted macOS.
if in_group scripts; then
    run_phase ci-runner-tests 300 python3 scripts/test-ci-runner.py
    run_phase installer-routing 60 python3 scripts/test-install.py
    run_phase thorvg-installer 60 python3 -B scripts/test-thorvg-macos.py
    run_phase agent-runner-tests 120 python3 -B scripts/test-agent-unit-runner.py
    run_phase agent-team-tests 120 python3 scripts/test-agent-team.py
    run_phase benchmark-agent-tests 60 python3 -B scripts/benchmark-browser-agent.py self-test
    run_phase stability-harness-tests 60 python3 -B scripts/test-macos-stability.py --self-check
    run_phase format 180 cargo fmt --all -- --check
fi
[[ $group != scripts ]] || exit 0

if [[ $mode == linux ]]; then
    export GDK_BACKEND=x11 GTK_A11Y=test LANG=C.UTF-8 LC_ALL=C.UTF-8
    # Xvfb must not select a host GNOME portal that requires the desktop session.
    export XDG_CURRENT_DESKTOP= XDG_SESSION_DESKTOP=
    locale
    # Keep a fixed virtual desktop. AppArmor is provisioned by the host/CI;
    # this runner does not disable the WebKit sandbox or change host policy.
    # Browser, Team and managed-Agent live phases run sequentially in one gate.
    run_phase regression 4200 xvfb-run -a -s '-screen 0 1280x1024x24' dbus-run-session -- \
        env FLOWMUX_TEST_X11_LOCKS=1 bash scripts/test-linux.sh "$suite" "$group" -- --nocapture
    exit 0
fi

# Hosted macOS VMs compile OpenGL shaders on the GTK thread for seconds.
# Keep local and CI functional checks independent of inherited GPU settings.
export GSK_RENDERER=cairo
printf 'GSK_RENDERER=%s\n' "$GSK_RENDERER"

# Each group is independent of the others' results; CI gives each its own instance.
if in_group app; then
    run_phase macos-installer 180 python3 scripts/test-install-macos.py
    run_phase build 900 cargo build --workspace --locked
    # Negative readiness cases now exhaust bounded pre-input waits.
    run_phase browser-control 240 python3 scripts/test-browser-control.py "$CARGO_TARGET_DIR/debug/flowmux"
    export FLOWMUX_BUNDLED_CLI_PATH="$CARGO_TARGET_DIR/debug/flowmuxctl"
    # Run the production GUI in separate processes, outside the native test harness.
    run_phase agent-gui-crash 180 python3 scripts/test-agent-gui-crash.py --gui "$CARGO_TARGET_DIR/debug/flowmux" \
        2>&1 | tee "$report_dir/agent-gui-crash.log"
    grep -q '^AGENT_GUI_CRASH_OK$' "$report_dir/agent-gui-crash.log"
    run_phase webdriver 180 python3 scripts/test-macos-webdriver.py "$CARGO_TARGET_DIR/debug/flowmux" \
        2>&1 | tee "$report_dir/webdriver.log"
    grep -q '^MACOS_WEBDRIVER_OK$' "$report_dir/webdriver.log"
    # Later groups start as they do on their own instance.
    unset FLOWMUX_BUNDLED_CLI_PATH
fi

if in_group headless; then
    # agent-unit below runs the core, daemon, CLI and procmon unit tests once.
    run_phase native-state 180 cargo test -p flowmux-state --locked
    run_phase cli 300 cargo test -p flowmux-cli --locked --test '*'
    git rev-parse HEAD
    python3 -c 'import hashlib, pathlib, sys; print("CLI_SHA256=" + hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())' "$CARGO_TARGET_DIR/debug/flowmuxctl"
    run_phase agent-unit 960 python3 scripts/test-agent-unit.py 2>&1 | tee "$report_dir/agent-unit.log"
    run_phase markdown 180 cargo test -p flowmux-md-viewer --locked -- --test-threads=1 \
        2>&1 | tee "$report_dir/markdown.log"
    grep -q '^MACOS_MARKDOWN_NATIVE_OK$' "$report_dir/markdown.log"
    run_phase notifications 120 cargo test -p flowmux-notify --features native-smoke --locked \
        2>&1 | tee "$report_dir/notifications.log"
    grep -q '^MACOS_NATIVE_DOCK_BADGE_OK$' "$report_dir/notifications.log"
fi

if in_group native; then
    # The app group builds the whole workspace; alone, the smokes need only the helpers.
    if [[ $group == native ]]; then
        run_phase helper-build 600 cargo build -p flowmux-cli -p flowmux-daemon --locked
    fi
    # The first test phase of this group compiles the test binary.
    run_phase macos-layer 900 cargo test -p flowmux --bin flowmux --locked ui::macos_layer::tests
    run_phase macos-update-unit 180 cargo test -p flowmux --bin flowmux --locked update::
    run_phase macos-update-options 180 cargo test -p flowmux --bin flowmux --locked dmg_update_tab
    run_phase macos-dmg-update 300 python3 scripts/test-update-macos-dmg.py
    run_phase ipc 300 cargo test -p flowmux --bin flowmux --locked ipc_handler::tests
    run_phase bridge 180 cargo test -p flowmux --bin flowmux --locked bridge::tests
    run_phase session-path-unit 180 cargo test -p flowmux --bin flowmux --locked ui::window::session
    export FLOWMUX_BUNDLED_CLI_PATH="$CARGO_TARGET_DIR/debug/flowmuxctl"
    run_phase native 600 cargo test -p flowmux --test macos_native --features native-smoke --locked \
        2>&1 | tee "$report_dir/native-smoke.log"
    grep -q '^MACOS_NATIVE_SMOKE_OK$' "$report_dir/native-smoke.log"
    grep -q '^MACOS_MANAGED_AGENT_DRAG_OK$' "$report_dir/native-smoke.log"
    FLOWMUX_OPTIONS_ZOOM_SMOKE_ONLY=1 run_phase options-zoom 180 cargo test -p flowmux --test macos_native --features native-smoke --locked \
        2>&1 | tee "$report_dir/options-zoom.log"
    grep -q '^MACOS_OPTIONS_ZOOM_DISPLAY_OK$' "$report_dir/options-zoom.log"
    FLOWMUX_RESTORED_ROOTS_SMOKE_ONLY=1 run_phase boot-restore 180 cargo test -p flowmux --test macos_native --features native-smoke --locked \
        2>&1 | tee "$report_dir/boot-restore.log"
    grep -q '^MACOS_BOOT_RESTORE_POLICY_OK$' "$report_dir/boot-restore.log"
    ssh_tmp="$test_root/macos-native-long-temporary-directory"
    mkdir -p "$ssh_tmp"
    TMPDIR="$ssh_tmp" FLOWMUX_SSH_PREVIEW_SMOKE_ONLY=1 run_phase ssh-preview 180 cargo test -p flowmux --test macos_native --features native-smoke --locked \
        2>&1 | tee "$report_dir/ssh-preview.log"
    grep -q '^MACOS_SSH_PREVIEW_EXPIRY_OK$' "$report_dir/ssh-preview.log"
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
    FLOWMUX_SESSION_FLOW_SMOKE_ONLY=1 run_phase session-flow 180 cargo test -p flowmux --test macos_native --features native-smoke --locked \
        2>&1 | tee "$report_dir/session-flow.log"
    grep -q '^MACOS_SESSION_FLOW_LAYOUT_OK$' "$report_dir/session-flow.log"
    run_phase thorvg-runtime 60 python3 scripts/check-thorvg.py
    run_phase image-unit 180 cargo test -p flowmux --bin flowmux --locked ui::image_viewer::tests -- --test-threads=1
    run_phase image-routing 180 cargo test -p flowmux --bin flowmux --locked macos_terminal_routes_lottie_json_to_the_viewer
    FLOWMUX_IMAGE_VIEWER_SMOKE_ONLY=1 run_phase image-viewer 180 cargo test -p flowmux --test macos_native --features native-smoke --locked \
        2>&1 | tee "$report_dir/image-viewer.log"
    grep -q '^MACOS_IMAGE_LOTTIE_VIEWER_OK$' "$report_dir/image-viewer.log"
    FLOWMUX_NOTIFICATION_CLEANUP_SMOKE_ONLY=1 run_phase notification-cleanup 120 cargo test -p flowmux --test macos_native --features native-smoke --locked \
        2>&1 | tee "$report_dir/notification-cleanup.log"
    grep -q '^MACOS_NOTIFICATION_CLEANUP_OK$' "$report_dir/notification-cleanup.log"
    run_phase notification-routing 120 cargo test -p flowmux --bin flowmux --locked native_notification_target_opens_the_source_pid_socket
    run_phase webdriver-address 120 cargo test -p flowmux --bin flowmux --locked macos_webdriver::tests
    run_phase webdriver-native 180 cargo test -p flowmux --test macos_webdriver --features native-smoke --locked
    FLOWMUX_BROWSER_CHROME_SMOKE_ONLY=1 run_phase browser-chrome 180 cargo test -p flowmux --test macos_native --features native-smoke --locked \
        2>&1 | tee "$report_dir/browser-chrome.log"
    grep -q '^BROWSER_CHROME_LAYOUT_OK$' "$report_dir/browser-chrome.log"
    # Closing the command palette must release its dialog tree; rows hold the window.
    FLOWMUX_PALETTE_RELEASE_SMOKE_ONLY=1 run_phase palette-release 180 cargo test -p flowmux --test macos_native --features native-smoke --locked \
        2>&1 | tee "$report_dir/palette-release.log"
    grep -q '^MACOS_PALETTE_RELEASE_OK$' "$report_dir/palette-release.log"
    FLOWMUX_EDITOR_RECOVERY_SMOKE_ONLY=1 run_phase editor-recovery 180 cargo test -p flowmux --test macos_native --features native-smoke --locked \
        2>&1 | tee "$report_dir/editor-recovery.log"
    grep -q '^MACOS_EDITOR_RENDERER_RETRY_OK$' "$report_dir/editor-recovery.log"
    if [[ $suite == extended ]]; then
        # Exercise actual office drawing and release after repeated status/resize churn.
        FLOWMUX_OFFICE_STRESS_SMOKE_ONLY=1 run_phase office-stability 180 cargo test -p flowmux --test macos_native --features native-smoke --locked \
            2>&1 | tee "$report_dir/office-stability.log"
        grep -q '^OFFICE_STABILITY_OK ' "$report_dir/office-stability.log"
    fi
    run_phase browser-completion-unit 180 cargo test -p flowmux --bin flowmux --locked completion_tests
    FLOWMUX_BROWSER_DEADLINE_SMOKE_ONLY=1 run_phase browser-deadline 120 cargo test -p flowmux --test macos_native --features native-smoke --locked \
        2>&1 | tee "$report_dir/browser-deadline.log"
    grep -q '^MACOS_BROWSER_SCRIPT_DEADLINE_OK$' "$report_dir/browser-deadline.log"
fi
