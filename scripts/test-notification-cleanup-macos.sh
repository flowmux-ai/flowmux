#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
set -euo pipefail
cd "$(dirname "$0")/.."
config=$(mktemp /tmp/fm-notify-bus.XXXXXX)
trap 'rm -f "$config"' EXIT
cat > "$config" <<'XML'
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <policy context="default">
    <allow send_destination="*"/>
    <allow receive_sender="*"/>
    <allow own="*"/>
  </policy>
</busconfig>
XML
dbus-run-session --config-file="$config" -- env \
  FLOWMUX_NOTIFICATION_CLEANUP_SMOKE_ONLY=1 GSK_RENDERER=cairo \
  cargo test -p flowmux --test macos_native --features native-smoke --locked
