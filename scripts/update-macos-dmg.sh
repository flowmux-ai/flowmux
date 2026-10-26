#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Input DMG has already passed SHA-256 verification in the native updater.
# The app contents are replaced immediately, like install-macos.sh: running
# instances keep their mapped files in a retained copy; the next launch uses
# the new release.
set -eu

[ "$#" -eq 3 ] || { echo 'expected DMG, app and version' >&2; exit 1; }
image=$1
destination=$2
version=$3
[ "$(uname -s)" = Darwin ] || exit 1
name=$(basename "$destination")
[ "$(printf '%s' "$name" | tr '[:upper:]' '[:lower:]')" = flowmux.app ] || exit 1
[ -d "$destination/Contents/MacOS" ] && [ ! -L "$destination" ] || exit 1
parent=$(dirname "$destination")
parent=$(cd "$parent" && pwd -P)
destination="$parent/$name"

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
staged=$(mktemp -d "$parent/.$name.pending.XXXXXX")
ditto "$bundle/Contents" "$staged/Contents"
codesign --verify --deep --strict "$staged"
detach_image
mounted=0

# Older updaters staged a bundle here for a launchd job that replaced the app
# after every instance quit. Those jobs do not survive a logout; a leftover
# must never swap an old release over this one later.
for pending in "$parent/.$name.pending."*; do
    [ "$pending" = "$staged" ] || rm -rf "$pending"
done

echo "==> installing app to $destination"
retained=$(mktemp -d "$parent/.$name.previous.XXXXXX")
mv "$destination/Contents" "$retained/Contents"
if ! mv "$staged/Contents" "$destination/Contents"; then
    mv "$retained/Contents" "$destination/Contents"
    rmdir "$retained"
    echo 'could not replace app contents; existing installation unchanged' >&2
    exit 1
fi
rmdir "$staged"
staged=
echo "==> previous app retained for running sessions: $retained"

# Drop retained copies that no process maps any more.
for previous in "$parent/.$name.previous."*; do
    [ -d "$previous" ] && [ "$previous" != "$retained" ] || continue
    users=$(lsof -t -- "$previous/Contents/MacOS/flowmux" "$previous/Contents/MacOS/flowmuxctl" \
        "$previous/Contents/MacOS/flowmux-md-viewer" 2>/dev/null || true)
    [ -n "$users" ] || rm -rf "$previous"
done
echo "==> installed Flowmux $version; restart Flowmux to use it."
