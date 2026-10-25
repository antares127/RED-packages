#!/bin/sh
set -eu

cd "$(dirname "$0")"

PREFIX=${PREFIX:-/usr/local}
BINDIR="$PREFIX/bin"
SHAREDIR="$PREFIX/share"
DESTDIR=${DESTDIR:-}

BIN="$DESTDIR$BINDIR/red"
APPS="$DESTDIR$SHAREDIR/applications"
MIME="$DESTDIR$SHAREDIR/mime"
ICONS="$DESTDIR$SHAREDIR/icons/hicolor/scalable/apps"
MENUS="$DESTDIR$SHAREDIR/kio/servicemenus"

if [ ! -x target/release/red ]; then
    echo "target/release/red not found; run: cargo build --release" >&2
    exit 1
fi

install -Dm755 target/release/red "$BIN"
install -Dm644 share/applications/red.desktop "$APPS/red.desktop"
install -Dm644 share/mime/packages/red.xml "$MIME/packages/red.xml"
install -Dm644 share/icons/hicolor/scalable/apps/red.svg "$ICONS/red.svg"
install -Dm755 share/kio/servicemenus/red-servicemenu.desktop "$MENUS/red-servicemenu.desktop"

if [ -z "$DESTDIR" ]; then
    command -v update-desktop-database >/dev/null 2>&1 \
        && update-desktop-database "$APPS" 2>/dev/null || true
    command -v update-mime-database >/dev/null 2>&1 \
        && update-mime-database "$MIME" 2>/dev/null || true
    command -v gtk-update-icon-cache >/dev/null 2>&1 \
        && gtk-update-icon-cache -f -t "$DESTDIR$SHAREDIR/icons/hicolor" 2>/dev/null || true
fi

echo "Installed red to $BIN"
echo "Desktop integration installed under $DESTDIR$SHAREDIR"
