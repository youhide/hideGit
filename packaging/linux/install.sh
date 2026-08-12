#!/bin/sh
# Installs hideGit's binary, desktop entry and icons.
#
# Not a package. AppImage and Flatpak wait on a signing certificate; this is
# what ships in the release tarball, and what a distribution packager would
# otherwise hand-write.
#
# From a release tarball, where the binary sits beside this script:
#
#   PREFIX=~/.local ./install.sh
#
# From a source checkout, against target/release/hidegit:
#
#   cargo build --release
#   sudo ./packaging/linux/install.sh
#
#   PREFIX=~/.local ./packaging/linux/install.sh   # no root needed
#   DESTDIR=/tmp/stage ./packaging/linux/install.sh  # staged, for packaging
#
# Pass `uninstall` as the first argument to remove everything it installed.

set -eu

PREFIX="${PREFIX:-/usr/local}"
DESTDIR="${DESTDIR:-}"

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
desktop="com.youhide.hidegit.desktop"
icon_sizes="16 24 32 48 64 128 256 512"

# The release tarball is flat — binary, desktop entry and icons all beside this
# script — while a checkout keeps them in three different places. Detecting the
# layout rather than shipping two scripts keeps one of them from going stale.
if [ -f "$here/hidegit" ]; then
	binary="$here/hidegit"
	desktop_file="$here/$desktop"
	icons="$here/hicolor"
else
	root=$(CDPATH= cd -- "$here/../.." && pwd)
	binary="$root/target/release/hidegit"
	desktop_file="$root/packaging/linux/$desktop"
	icons="$root/assets/generated/hicolor"
fi

bindir="$DESTDIR$PREFIX/bin"
appdir="$DESTDIR$PREFIX/share/applications"
icondir="$DESTDIR$PREFIX/share/icons/hicolor"

if [ "${1:-install}" = "uninstall" ]; then
	rm -f "$bindir/hidegit" "$appdir/$desktop"
	for size in $icon_sizes; do
		rm -f "$icondir/${size}x${size}/apps/com.youhide.hidegit.png"
	done
	echo "removed hideGit from $PREFIX"
	exit 0
fi

# -f rather than -x: `install` sets the mode itself, and an archive unpacked
# without the executable bit is still perfectly installable.
if [ ! -f "$binary" ]; then
	echo "$binary does not exist — run \`cargo build --release\` first" >&2
	exit 1
fi

# `mkdir -p` then `install`, rather than `install -D`: the -D flag is GNU-only,
# and this way the script also runs on a BSD userland, which is where it gets
# smoke-tested.
install_file() {
	mkdir -p "$(dirname -- "$3")"
	install -m "$1" "$2" "$3"
}

install_file 755 "$binary" "$bindir/hidegit"
install_file 644 "$desktop_file" "$appdir/$desktop"

for size in $icon_sizes; do
	install_file 644 \
		"$icons/${size}x${size}/apps/com.youhide.hidegit.png" \
		"$icondir/${size}x${size}/apps/com.youhide.hidegit.png"
done

echo "installed hideGit to $PREFIX"

# Skipped when staging into a DESTDIR: the caches to refresh are the target
# system's, and the packaging tooling triggers them at install time instead.
if [ -z "$DESTDIR" ]; then
	if command -v gtk-update-icon-cache >/dev/null 2>&1; then
		gtk-update-icon-cache -qtf "$PREFIX/share/icons/hicolor" || true
	fi
	if command -v update-desktop-database >/dev/null 2>&1; then
		update-desktop-database -q "$PREFIX/share/applications" || true
	fi
fi
