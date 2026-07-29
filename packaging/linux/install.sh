#!/bin/sh
# Installs hideGit's binary, desktop entry and icons.
#
# Not a package. AppImage and Flatpak are M6; this is the manual path until
# then, and it is what a distribution packager would otherwise hand-write.
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

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
binary="$root/target/release/hidegit"
desktop="com.youhide.hidegit.desktop"
icon_sizes="16 24 32 48 64 128 256 512"

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

if [ ! -x "$binary" ]; then
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
install_file 644 "$root/packaging/linux/$desktop" "$appdir/$desktop"

for size in $icon_sizes; do
	install_file 644 \
		"$root/assets/generated/hicolor/${size}x${size}/apps/com.youhide.hidegit.png" \
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
