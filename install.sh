#!/bin/bash
# Installs Calepin for the current user: binary, desktop entry, icon.
set -euo pipefail
cd "$(dirname "$0")"
PREFIX="${PREFIX:-$HOME/.local}"
APP=io.github.antoineart.Calepin
cargo install --path . --root "$PREFIX" --locked
install -Dm644 "data/$APP.desktop" "$PREFIX/share/applications/$APP.desktop"
install -Dm644 assets/icon.png "$PREFIX/share/icons/hicolor/512x512/apps/$APP.png"
# Earlier releases shipped an SVG icon, which would take precedence.
rm -f "$PREFIX/share/icons/hicolor/scalable/apps/$APP.svg"
update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
gtk-update-icon-cache -q -t "$PREFIX/share/icons/hicolor" 2>/dev/null || true
echo "calepin installed in $PREFIX"
