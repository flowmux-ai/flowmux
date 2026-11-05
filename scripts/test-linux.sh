#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
set -euo pipefail
cd "$(dirname "$0")/.."

suite=${1:-regression}
case "$suite" in
    regression|extended) ;;
    *) echo "Usage: $0 [regression|extended] [cargo test arguments...]" >&2; exit 2 ;;
esac
if (( $# )); then shift; fi
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$PWD/target"}
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
test_log="$CARGO_TARGET_DIR/ci/linux/tests.jsonl"
mkdir -p "$(dirname "$test_log")"
: > "$test_log"

# Each suite owns and reaps its detached GUI/PTY children before the next starts.
run_test() {
    local name=$1 started=$SECONDS status=0
    shift
    printf 'CI_TEST_START=%s\n' "$name"
    python3 scripts/ci-phase.py "$CARGO_TARGET_DIR/ci/linux/tests" "$name" 2400 "$@" || status=$?
    local elapsed=$((SECONDS - started))
    printf 'CI_TEST_RESULT={"test":"%s","elapsed_seconds":%d,"exit_code":%d}\n' \
        "$name" "$elapsed" "$status"
    printf '{"test":"%s","elapsed_seconds":%d,"exit_code":%d}\n' \
        "$name" "$elapsed" "$status" >> "$test_log"
    return "$status"
}

run_test gui-harness python3 scripts/test-gui-harness.py
run_test ssh-fixture-unit python3 scripts/test-ssh-workspace-fixture.py
# Keep workspace tests, examples and doctests; no coverage instrumentation.
run_test workspace cargo test --workspace --locked --no-fail-fast "$@"
run_test build cargo build --workspace --locked
run_test browser-control python3 scripts/test-browser-control.py "$CARGO_TARGET_DIR/debug/flowmux"
run_test browser-bookmarks python3 scripts/test-browser-bookmarks-gui.py \
  --gui "$CARGO_TARGET_DIR/debug/flowmux" \
  --cli "$CARGO_TARGET_DIR/debug/flowmuxctl"
run_test browser-import python3 scripts/test-browser-import-gui.py \
  --gui "$CARGO_TARGET_DIR/debug/flowmux" \
  --cli "$CARGO_TARGET_DIR/debug/flowmuxctl"
run_test skills python3 scripts/test-skills-gui.py \
  --gui "$CARGO_TARGET_DIR/debug/flowmux" \
  --cli "$CARGO_TARGET_DIR/debug/flowmuxctl"
run_test notifications python3 scripts/test-notifications-gui.py \
  --gui "$CARGO_TARGET_DIR/debug/flowmux" \
  --cli "$CARGO_TARGET_DIR/debug/flowmuxctl"
run_test team-pingpong python3 scripts/test-team-pingpong-gui.py \
  --gui "$CARGO_TARGET_DIR/debug/flowmux" \
  --cli "$CARGO_TARGET_DIR/debug/flowmuxctl"
run_test team-recovery python3 scripts/test-team-recovery-gui.py \
  --gui "$CARGO_TARGET_DIR/debug/flowmux" \
  --cli "$CARGO_TARGET_DIR/debug/flowmuxctl"
run_test team-reuse python3 scripts/test-team-reuse-gui.py \
  --gui "$CARGO_TARGET_DIR/debug/flowmux" \
  --cli "$CARGO_TARGET_DIR/debug/flowmuxctl"
run_test team-usage python3 scripts/test-team-usage-gui.py \
  --gui "$CARGO_TARGET_DIR/debug/flowmux" \
  --cli "$CARGO_TARGET_DIR/debug/flowmuxctl"
run_test session-save python3 scripts/test-session-save-gui.py \
  --gui "$CARGO_TARGET_DIR/debug/flowmux" \
  --cli "$CARGO_TARGET_DIR/debug/flowmuxctl"
# Keep failure logs outside the CI temporary tree, which is deleted on exit.
TMPDIR=/tmp run_test session-flow python3 scripts/test-session-flow-gui.py \
  --gui "$CARGO_TARGET_DIR/debug/flowmux" \
  --cli "$CARGO_TARGET_DIR/debug/flowmuxctl"
run_test pty-spawn-failure python3 scripts/test-pty-spawn-failure-gui.py \
  --gui "$CARGO_TARGET_DIR/debug/flowmux" \
  --cli "$CARGO_TARGET_DIR/debug/flowmuxctl"
run_test terminal-read-screen python3 scripts/test-terminal-read-screen-gui.py \
  --gui "$CARGO_TARGET_DIR/debug/flowmux" \
  --cli "$CARGO_TARGET_DIR/debug/flowmuxctl"
TMPDIR=/tmp run_test terminal-image-links python3 scripts/test-terminal-image-links-gui.py \
  --gui "$CARGO_TARGET_DIR/debug/flowmux" \
  --cli "$CARGO_TARGET_DIR/debug/flowmuxctl"
run_test ipc-recovery python3 scripts/test-ipc-recovery-gui.py \
  --gui "$CARGO_TARGET_DIR/debug/flowmux" \
  --cli "$CARGO_TARGET_DIR/debug/flowmuxctl"
# Keep probe evidence outside the CI temporary tree, which is deleted on exit.
TMPDIR=/tmp run_test polling-delay python3 scripts/test-polling-delay-gui.py \
  --gui "$CARGO_TARGET_DIR/debug/flowmux" \
  --cli "$CARGO_TARGET_DIR/debug/flowmuxctl"
if [[ $suite == extended ]]; then
    run_test idle-stability-unit python3 scripts/test-idle-stability-gui.py --self-check
    TMPDIR=/tmp run_test idle-stability python3 scripts/test-idle-stability-gui.py \
      --gui "$CARGO_TARGET_DIR/debug/flowmux" \
      --cli "$CARGO_TARGET_DIR/debug/flowmuxctl"
fi
TMPDIR=/tmp run_test editor-io python3 scripts/test-editor-io-gui.py \
  --gui "$CARGO_TARGET_DIR/debug/flowmux" \
  --cli "$CARGO_TARGET_DIR/debug/flowmuxctl"
TMPDIR=/tmp run_test editor-renderer python3 scripts/test-editor-renderer-gui.py \
  --gui "$CARGO_TARGET_DIR/debug/flowmux" \
  --cli "$CARGO_TARGET_DIR/debug/flowmuxctl"
run_test lazy-restore python3 scripts/test-lazy-restore-gui.py \
  --gui "$CARGO_TARGET_DIR/debug/flowmux" \
  --cli "$CARGO_TARGET_DIR/debug/flowmuxctl"
run_test ssh-workspace python3 scripts/ssh-workspace-fixture.py \
  --gui "$CARGO_TARGET_DIR/debug/flowmux" \
  --cli "$CARGO_TARGET_DIR/debug/flowmuxctl"
