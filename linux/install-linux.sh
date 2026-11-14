#!/bin/sh
# Installs Cartridge for your user so it shows up in the app menu.
# Usage:  ./install-linux.sh            (install)
#         ./install-linux.sh --uninstall
set -e
BIN="$HOME/.local/bin"
APPS="$HOME/.local/share/applications"
ICONS="$HOME/.local/share/icons/hicolor/256x256/apps"

if [ "$1" = "--uninstall" ]; then
  rm -f "$BIN/cartridge" "$APPS/cartridge.desktop" "$ICONS/cartridge.png"
  echo "Cartridge removed."
else
  HERE="$(cd "$(dirname "$0")" && pwd)"
  mkdir -p "$BIN" "$APPS" "$ICONS"
  install -m 755 "$HERE/cartridge" "$BIN/cartridge"
  install -m 644 "$HERE/cartridge.png" "$ICONS/cartridge.png"
  sed "s|^Exec=.*|Exec=$BIN/cartridge|" "$HERE/cartridge.desktop" > "$APPS/cartridge.desktop"
  chmod 644 "$APPS/cartridge.desktop"
  echo "Cartridge installed. Search for \"Cartridge\" in your app menu (you may need to log out and back in once)."
fi

command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APPS" >/dev/null 2>&1 || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$HOME/.local/share/icons/hicolor" >/dev/null 2>&1 || true
