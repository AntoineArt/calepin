#!/bin/bash
# Installs Calepin for the current user: binary, desktop entry, icon.
set -euo pipefail
cd "$(dirname "$0")"
PREFIX="${PREFIX:-$HOME/.local}"
APP=io.github.antoineart.Calepin
cargo install --path . --root "$PREFIX" --locked
install -Dm644 "data/$APP.desktop" "$PREFIX/share/applications/$APP.desktop"
install -Dm644 assets/icon.svg "$PREFIX/share/icons/hicolor/scalable/apps/$APP.svg"
# Former name.
cargo uninstall --root "$PREFIX" mywriter 2>/dev/null || true
rm -f "$PREFIX/share/applications/io.github.antoineart.MyWriter.desktop" \
  "$PREFIX/share/icons/hicolor/scalable/apps/io.github.antoineart.MyWriter.svg"
update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
gtk-update-icon-cache -q -t "$PREFIX/share/icons/hicolor" 2>/dev/null || true
echo "calepin installed in $PREFIX"
