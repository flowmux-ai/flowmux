#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build and install a local macOS Flowmux.app bundle.
#
# Usage:
#   scripts/install-macos.sh [--check] [--fast|--debug|--release] [--launch]
#                            [--app-dir DIR] [--bin-dir DIR] [--bundle-only]
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_DIR="${FLOWMUX_MACOS_APP_DIR:-$HOME/Applications}"
BIN_DIR="${FLOWMUX_MACOS_BIN_DIR:-$HOME/.local/bin}"
PROFILE="${FLOWMUX_MACOS_PROFILE:-fast}"
CHECK_ONLY=false
BUNDLE_ONLY=false
LAUNCH=false

usage() {
    sed -n '3,9p' "$0" >&2
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --check)
            CHECK_ONLY=true
            shift
            ;;
        --fast)
            PROFILE=fast
            shift
            ;;
        --debug)
            PROFILE=debug
            shift
            ;;
        --release)
            PROFILE=release
            shift
            ;;
        --launch)
            LAUNCH=true
            shift
            ;;
        --bundle-only)
            BUNDLE_ONLY=true
            shift
            ;;
        --app-dir)
            APP_DIR="${2:?--app-dir requires a directory}"
            shift 2
            ;;
        --bin-dir)
            BIN_DIR="${2:?--bin-dir requires a directory}"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage
            exit 2
            ;;
    esac
done

if [ "$BUNDLE_ONLY" = true ] && [ "$LAUNCH" = true ]; then
    echo "error: --bundle-only cannot be combined with --launch" >&2
    exit 2
fi

confirm() {
    local answer
    if ! ( : </dev/tty ) 2>/dev/null; then
        echo "error: dependency installation needs a terminal; rerun ./install.sh in a terminal" >&2
        exit 1
    fi
    printf '%s [Y/n] ' "$1" >/dev/tty
    read -r answer </dev/tty || exit 1
    case "$answer" in
        ""|y|Y|yes|YES) ;;
        *) echo "==> installation cancelled"; exit 1 ;;
    esac
}

install_prerequisites() {
    local sdk_path missing_formulas=() module formula
    sdk_path="$(xcrun --sdk macosx --show-sdk-path 2>/dev/null || true)"
    if [ ! -d "$sdk_path/System/Library/Frameworks/WebKit.framework" ]; then
        confirm "Install Apple's command-line build tools?"
        xcode-select --install
        echo "Finish the Apple installer, then rerun ./install.sh." >&2
        exit 1
    fi
    command -v python3 >/dev/null 2>&1 || missing_formulas+=(python)
    if ! command -v pkg-config >/dev/null 2>&1; then
        missing_formulas+=(pkg-config gtk4 libadwaita vte3)
    else
        for module in gtk4 libadwaita-1 vte-2.91-gtk4; do
            case "$module" in
                gtk4) formula=gtk4 ;;
                libadwaita-1) formula=libadwaita ;;
                vte-2.91-gtk4) formula=vte3 ;;
            esac
            pkg-config --exists "$module" || missing_formulas+=("$formula")
        done
    fi
    if [ "${#missing_formulas[@]}" -gt 0 ]; then
        if ! command -v brew >/dev/null 2>&1; then
            confirm "Homebrew is required for the native libraries. Install it?"
            local bootstrap
            bootstrap=$(mktemp /tmp/flowmux-homebrew.XXXXXX)
            if ! curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh -o "$bootstrap" \
                || ! /bin/bash "$bootstrap" </dev/tty; then
                rm -f "$bootstrap"
                return 1
            fi
            rm -f "$bootstrap"
            configure_macos_path
        fi
        confirm "Install required Homebrew packages: ${missing_formulas[*]}?"
        brew install "${missing_formulas[@]}"
    fi
    if ! command -v cargo >/dev/null 2>&1; then
        confirm "Install Rust with rustup?"
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
            | sh -s -- -y --profile minimal
        configure_macos_path
    fi
}

preflight_macos() {
    if [ "$(uname -s)" != "Darwin" ]; then
        cat >&2 <<'EOF'
error: scripts/install-macos.sh is for macOS.
On Linux or WSLg, use ./install.sh instead.
EOF
        exit 1
    fi

    local missing_commands=()
    for command in cargo python3 pkg-config codesign iconutil install open plutil xattr xcrun; do
        if ! command -v "$command" >/dev/null 2>&1; then
            missing_commands+=("$command")
        fi
    done

    local missing_frameworks=()
    if command -v xcrun >/dev/null 2>&1; then
        local sdk_path
        sdk_path="$(xcrun --sdk macosx --show-sdk-path 2>/dev/null || true)"
        if [ -z "$sdk_path" ] || [ ! -d "$sdk_path/System/Library/Frameworks/WebKit.framework" ]; then
            missing_frameworks+=("WebKit.framework (macOS SDK)")
        fi
    fi

    local missing_modules=()
    if command -v pkg-config >/dev/null 2>&1; then
        for module in gtk4 libadwaita-1 vte-2.91-gtk4; do
            if ! pkg-config --exists "$module"; then
                missing_modules+=("$module")
            fi
        done
    fi

    if [ "${#missing_commands[@]}" -gt 0 ] || [ "${#missing_frameworks[@]}" -gt 0 ] || [ "${#missing_modules[@]}" -gt 0 ]; then
        if [ "${#missing_commands[@]}" -gt 0 ]; then
            echo "error: missing commands: ${missing_commands[*]}" >&2
        fi
        if [ "${#missing_frameworks[@]}" -gt 0 ]; then
            echo "error: missing Apple frameworks: ${missing_frameworks[*]}" >&2
        fi
        if [ "${#missing_modules[@]}" -gt 0 ]; then
            echo "error: missing pkg-config modules: ${missing_modules[*]}" >&2
        fi
        exit 1
    fi
}

configure_macos_path() {
    local dir
    for dir in \
        /usr/local/sbin \
        /usr/local/bin \
        /opt/homebrew/sbin \
        /opt/homebrew/bin \
        "$HOME/.cargo/bin"; do
        if [ -d "$dir" ] && [[ ":${PATH:-}:" != *":$dir:"* ]]; then
            PATH="$dir${PATH:+:$PATH}"
        fi
    done
    export PATH
}

workspace_version() {
    awk -F '"' '/^version = / { print $2; exit }' "$REPO_ROOT/Cargo.toml"
}

create_icon() {
    local resources="$1"
    local iconset="$REPO_ROOT/target/macos-bundle/Flowmux.iconset"

    rm -rf "$iconset"
    mkdir -p "$iconset"
    cp "$REPO_ROOT/resources/icons/flowmux-16.png" "$iconset/icon_16x16.png"
    cp "$REPO_ROOT/resources/icons/flowmux-32.png" "$iconset/icon_16x16@2x.png"
    cp "$REPO_ROOT/resources/icons/flowmux-32.png" "$iconset/icon_32x32.png"
    cp "$REPO_ROOT/resources/icons/flowmux-64.png" "$iconset/icon_32x32@2x.png"
    cp "$REPO_ROOT/resources/icons/flowmux-128.png" "$iconset/icon_128x128.png"
    cp "$REPO_ROOT/resources/icons/flowmux-256.png" "$iconset/icon_128x128@2x.png"
    cp "$REPO_ROOT/resources/icons/flowmux-256.png" "$iconset/icon_256x256.png"
    cp "$REPO_ROOT/resources/icons/flowmux-512.png" "$iconset/icon_256x256@2x.png"
    cp "$REPO_ROOT/resources/icons/flowmux-512.png" "$iconset/icon_512x512.png"
    cp "$REPO_ROOT/resources/icons/flowmux-1024.png" "$iconset/icon_512x512@2x.png"
    iconutil -c icns "$iconset" -o "$resources/flowmux.icns"
}

write_info_plist() {
    local path="$1"
    local version="$2"
    cat > "$path" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleDisplayName</key>
  <string>Flowmux</string>
  <key>CFBundleExecutable</key>
  <string>flowmux</string>
  <key>CFBundleIconFile</key>
  <string>flowmux</string>
  <key>CFBundleIdentifier</key>
  <string>com.flowmux.App</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>Flowmux</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>$version</string>
  <key>CFBundleVersion</key>
  <string>$version</string>
  <key>LSApplicationCategoryType</key>
  <string>public.app-category.developer-tools</string>
  <key>LSMinimumSystemVersion</key>
  <string>13.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSDocumentsFolderUsageDescription</key>
  <string>Flowmux needs access to Documents so terminal panes and file tools can use workspace files.</string>
  <key>NSDesktopFolderUsageDescription</key>
  <string>Flowmux needs access to Desktop so terminal panes and file tools can use workspace files.</string>
  <key>NSDownloadsFolderUsageDescription</key>
  <string>Flowmux needs access to Downloads so terminal panes and file tools can use workspace files.</string>
</dict>
</plist>
EOF
}

build_args=()
case "$PROFILE" in
    fast)
        build_args+=(--profile fast)
        ;;
    release)
        build_args+=(--release)
        ;;
    debug)
        ;;
    *)
        echo "error: unsupported profile: $PROFILE" >&2
        exit 2
        ;;
esac
if [ "$BUNDLE_ONLY" = true ]; then
    build_args+=(--locked)
fi

configure_macos_path
if [ "$CHECK_ONLY" = false ] && [ "$(uname -s)" = Darwin ]; then
    install_prerequisites
fi
preflight_macos
if [ "$CHECK_ONLY" = true ]; then
    echo "==> macOS native preflight checks passed"
    exit 0
fi
if [ "$BUNDLE_ONLY" = false ]; then
    bash "$REPO_ROOT/scripts/install-thorvg-macos.sh"
fi

cd "$REPO_ROOT"
echo "==> building flowmux ($PROFILE)"
build_log=$(mktemp /tmp/flowmux-build.XXXXXX)
trap 'rm -f "$build_log"' EXIT
if [ "${#build_args[@]}" -gt 0 ]; then
    cargo build "${build_args[@]}" -p flowmux -p flowmux-cli -p flowmux-md-viewer --message-format=json-render-diagnostics > "$build_log"
else
    cargo build -p flowmux -p flowmux-cli -p flowmux-md-viewer --message-format=json-render-diagnostics > "$build_log"
fi

# Cargo reports the executable path including target-dir and target-triple settings.
built_executable() {
    python3 - "$build_log" "$1" <<'PY'
import json, sys
with open(sys.argv[1]) as source:
    paths = {entry["executable"] for line in source
             if (entry := json.loads(line)).get("reason") == "compiler-artifact"
             and entry["target"]["name"] == sys.argv[2] and entry.get("executable")}
if len(paths) != 1:
    sys.exit("Expected one built executable for " + sys.argv[2])
print(paths.pop())
PY
}
flowmux_binary=$(built_executable flowmux)
cli_binary=$(built_executable flowmuxctl)
markdown_binary=$(built_executable flowmux-md-viewer)

version="$(workspace_version)"
bundle_work="$REPO_ROOT/target/macos-bundle/Flowmux.app"
contents="$bundle_work/Contents"
macos="$contents/MacOS"
resources="$contents/Resources"
bundle_dest="$APP_DIR/Flowmux.app"
# Preserve an existing bundle's path on case-sensitive volumes. Its display
# name is refreshed by Info.plist; do not create a duplicate or rename a live app.
if [ ! -d "$bundle_dest" ]; then
    for existing_bundle in "$APP_DIR"/*.[aA][pP][pP]; do
        if [ -d "$existing_bundle" ] && \
            [ "$(basename "$existing_bundle" | tr '[:upper:]' '[:lower:]')" = "flowmux.app" ]; then
            bundle_dest="$existing_bundle"
            break
        fi
    done
fi

echo "==> creating $bundle_work"
rm -rf "$bundle_work"
mkdir -p "$macos" "$resources"
install -m755 "$flowmux_binary" "$macos/flowmux"
install -m755 "$cli_binary" "$macos/flowmuxctl"
install -m755 "$markdown_binary" "$macos/flowmux-md-viewer"
install -m644 \
    "$REPO_ROOT/LICENSE" \
    "$REPO_ROOT/NOTICE" \
    "$REPO_ROOT/docs/legal/THIRD_PARTY_LICENSES.md" \
    "$resources/"
create_icon "$resources"
write_info_plist "$contents/Info.plist" "$version"
plutil -lint "$contents/Info.plist" >/dev/null
xattr -cr "$bundle_work" "$macos/flowmux" "$macos/flowmuxctl" "$macos/flowmux-md-viewer"
codesign --force --sign - "$macos/flowmux" "$macos/flowmuxctl" "$macos/flowmux-md-viewer" >/dev/null
codesign --force --deep --sign - "$bundle_work" >/dev/null

if [ "$BUNDLE_ONLY" = true ]; then
    echo "==> built bundle: $bundle_work"
    exit 0
fi

mkdir -p "$APP_DIR" "$BIN_DIR"
bundle_pending=$(mktemp -d "$APP_DIR/.Flowmux.app.previous.XXXXXX")
echo "==> installing app to $bundle_dest"
cp -R "$bundle_work/Contents" "$bundle_pending/Contents"
codesign --verify --deep --strict "$bundle_pending"
installed_version="$("$bundle_pending/Contents/MacOS/flowmux" --version)"
if [ "$installed_version" != "flowmux $version" ]; then
    echo "error: built app reports '$installed_version'; expected flowmux $version" >&2
    rm -rf "$bundle_pending"
    exit 1
fi
# Keep the outer app directory for Dock bookmarks and retain old mapped files.
python3 - "$bundle_pending" "$bundle_dest" <<'PYTHON'
import ctypes
import os
import sys

pending, destination = sys.argv[1:]
if os.path.exists(destination):
    libc = ctypes.CDLL(None, use_errno=True)
    swap = libc.renamex_np
    swap.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
    swap.restype = ctypes.c_int
    if swap(os.fsencode(pending + "/Contents"),
            os.fsencode(destination + "/Contents"), 0x00000002) != 0:  # RENAME_SWAP
        raise OSError(ctypes.get_errno(), "could not atomically replace app contents")
    print("==> previous app retained for running sessions: " + pending)
else:
    os.rename(pending, destination)
PYTHON
install -m755 "$flowmux_binary" "$BIN_DIR/flowmux"
install -m755 "$cli_binary" "$BIN_DIR/flowmuxctl"
install -m755 "$markdown_binary" "$BIN_DIR/flowmux-md-viewer"

"$BIN_DIR/flowmuxctl" hooks refresh-shims
echo "==> existing agent wrappers refreshed; provider settings and hooks unchanged"
echo "    Run '$BIN_DIR/flowmuxctl fix' to enable or repair other integrations."

echo "==> installed Flowmux $version:"
echo "    $bundle_dest"
echo "    $BIN_DIR/flowmux"
echo "    $BIN_DIR/flowmuxctl"
echo "    $BIN_DIR/flowmux-md-viewer"
echo "$installed_version"
echo "==> running sessions keep their current version; the next app launch uses $version"
echo "==> launch with: open \"$bundle_dest\""

if [ "$LAUNCH" = true ]; then
    open "$bundle_dest"
fi
