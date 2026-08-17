#!/bin/sh
# Builds hideGit.AppImage: one file, downloadable, runs on any glibc at or above
# the one it was built against.
#
# **No certificate is involved.** An AppImage is not signed by anybody, which is
# exactly the same position the tarball is in — see docs/RELEASING.md for what
# that costs the person downloading. Nothing here waits on anything paid for.
#
# What it does *not* bundle is as important as what it does. `linuxdeploy`
# copies the libraries hideGit links against into the AppDir, minus an exclude
# list of the ones that must come from the host: libGL, libvulkan, libwayland's
# client protocols, glibc itself. Bundling a graphics driver is the classic way
# to build an AppImage that starts on the machine that made it and nowhere else.
#
#   cargo build --release
#   ./packaging/linux/appimage.sh              # -> dist/hideGit-x86_64.AppImage
#   VERSION=0.1.0 ./packaging/linux/appimage.sh  # -> dist/hideGit-0.1.0-x86_64.AppImage
#
# Linux x86_64 only: `linuxdeploy` runs where the binary it packages runs.

set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$here/../.." && pwd)

binary="${BINARY:-$root/target/release/hidegit}"
out="${OUT:-$root/dist}"
appdir="${APPDIR:-$root/target/appimage/AppDir}"
desktop="com.youhide.hidegit.desktop"

if [ ! -f "$binary" ]; then
	echo "$binary does not exist — run \`cargo build --release\` first" >&2
	exit 1
fi

# The tool, fetched rather than vendored: it is a 100 MB AppImage and it is not
# hideGit's to keep a copy of. Pinned to a release rather than `continuous`, so
# a build from a year-old tag produces the same package it did then.
tool_version="${LINUXDEPLOY_VERSION:-1-alpha-20250213-2}"
tool="${LINUXDEPLOY:-$root/target/appimage/linuxdeploy-x86_64.AppImage}"

if [ ! -x "$tool" ]; then
	mkdir -p "$(dirname -- "$tool")"
	echo "fetching linuxdeploy $tool_version"
	curl --fail --location --silent --show-error \
		--output "$tool" \
		"https://github.com/linuxdeploy/linuxdeploy/releases/download/$tool_version/linuxdeploy-x86_64.AppImage"
	chmod +x "$tool"
fi

# A container without FUSE cannot mount an AppImage, and CI runners are such a
# container. This makes the tool unpack itself instead of mounting, which is
# slower and always works.
export APPIMAGE_EXTRACT_AND_RUN=1

rm -rf "$appdir"
mkdir -p "$appdir" "$out"

# Every size, not just the one linuxdeploy needs. A desktop that picks 128px off
# a 512px source gets a blurry launcher, and the sizes already exist.
icons=""
for size in 16 24 32 48 64 128 256 512; do
	icon="$root/assets/generated/hicolor/${size}x${size}/apps/com.youhide.hidegit.png"
	target="$appdir/usr/share/icons/hicolor/${size}x${size}/apps"
	mkdir -p "$target"
	cp "$icon" "$target/"
	icons="$icons --icon-file=$icon"
done

version_suffix=""
if [ -n "${VERSION:-}" ]; then
	version_suffix="-$VERSION"
fi

# `-e` installs the binary and is what linuxdeploy reads the library list from;
# `-d` is the desktop entry, whose `Icon=` key has to match an icon it was
# given, or it refuses to build rather than shipping a launcher with no icon.
OUTPUT="$out/hideGit${version_suffix}-x86_64.AppImage" \
	"$tool" \
	--appdir "$appdir" \
	--executable "$binary" \
	--desktop-file "$here/$desktop" \
	$icons \
	--output appimage

echo "built $out/hideGit${version_suffix}-x86_64.AppImage"
