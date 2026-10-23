#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Input DMG has already passed SHA-256 verification in the native updater.
set -eu

[ "$#" -eq 5 ] || { echo 'expected DMG, app, version, host PID and swap helper' >&2; exit 1; }
image=$1
destination=$2
version=$3
host_pid=$4
helper=$5
[ "$(uname -s)" = Darwin ] || exit 1
case "$host_pid" in ''|*[!0-9]*) exit 1 ;; esac
[ "$host_pid" -gt 1 ] || exit 1
name=$(basename "$destination")
[ "$(printf '%s' "$name" | tr '[:upper:]' '[:lower:]')" = flowmux.app ] || exit 1
[ -d "$destination/Contents/MacOS" ] && [ ! -L "$destination" ] || exit 1
[ -f "$helper" ] || exit 1
parent=$(dirname "$destination")
parent=$(cd "$parent" && pwd -P)
destination="$parent/$name"
for pending in "$parent/.$name.pending."*; do
    if [ -e "$pending" ]; then
        echo 'An update is already staged; quit all Flowmux instances to apply it.' >&2
        exit 1
    fi
done

work=$(mktemp -d /tmp/fm-dmg-update.XXXXXX)
mounted=0
staged=
detach_image() {
    # This is our private read-only mount; background scanners can keep it busy.
    hdiutil detach "$work/mount" -quiet || hdiutil detach -force "$work/mount" -quiet
}
cleanup() {
    if [ -n "$staged" ]; then rm -rf "$staged"; fi
    if [ "$mounted" -eq 1 ]; then detach_image || return; fi
    rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
mkdir "$work/mount"
hdiutil attach "$image" -readonly -nobrowse -mountpoint "$work/mount" -quiet
mounted=1
bundle="$work/mount/Flowmux.app"
[ -d "$bundle" ] && [ ! -L "$bundle" ] || { echo 'DMG is missing Flowmux.app' >&2; exit 1; }
plist="$bundle/Contents/Info.plist"
[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$plist")" = com.flowmux.App ] || exit 1
[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$plist")" = "$version" ] || {
    echo 'DMG version does not match the requested release' >&2; exit 1;
}
for binary in flowmux flowmuxctl flowmux-md-viewer; do
    executable="$bundle/Contents/MacOS/$binary"
    [ -f "$executable" ] && [ -x "$executable" ] && [ ! -L "$executable" ] || exit 1
    lipo "$executable" -verify_arch arm64
done
codesign --verify --deep --strict "$bundle"
staged="$parent/.$name.pending.$host_pid"
ditto "$bundle" "$staged"
codesign --verify --deep --strict "$staged"
detach_image
mounted=0

# Every current instance using this bundle must exit before the shared swap.
pids=$host_pid
ps -ww -axo pid=,comm= > "$work/processes"
while read -r pid command; do
    for binary in flowmux flowmuxctl flowmux-md-viewer; do
        # ps can report /tmp while the bundle path resolves through /private/tmp.
        if [ "$command" -ef "$destination/Contents/MacOS/$binary" ]; then
            pids="$pids $pid"
            break
        fi
    done
done < "$work/processes"
label="com.flowmux.update.$host_pid"
launchctl submit -l "$label" -o "$parent/.$name.update.log" -e "$parent/.$name.update.log" \
    -- /bin/sh "$helper" "$pids" "$staged" \
    "$destination" "$parent/.$name.previous.$host_pid"
staged=
echo '==> Update staged; quit all Flowmux instances, then open Flowmux again.'
