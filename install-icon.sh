#!/usr/bin/env bash
# Install kaleidotron's desktop entry + icon so KDE/Wayland shows a real task icon
# (Wayland keys the task-switcher icon off app_id -> a matching .desktop file).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
ICON_DIR="$DATA/icons/hicolor/256x256/apps"
APP_DIR="$DATA/applications"

mkdir -p "$ICON_DIR" "$APP_DIR"
install -m644 "$HERE/assets/kaleidotron.png" "$ICON_DIR/kaleidotron.png"
install -m644 "$HERE/kaleidotron.desktop" "$APP_DIR/kaleidotron.desktop"

# Point Exec at the built binary if kaleidotron isn't already on PATH.
if ! command -v kaleidotron >/dev/null 2>&1; then
    BIN="$HERE/target/release/kaleidotron"
    if [ -x "$BIN" ]; then
        sed -i "s|^Exec=kaleidotron|Exec=$BIN|" "$APP_DIR/kaleidotron.desktop"
    fi
fi

# A short `kt` alias on PATH, alongside the full name. Same binary; the long name stays for
# discoverability, the short one is what you actually type.
BIN_DIR="$HOME/.local/bin"
BIN="$HERE/target/release/kaleidotron"
if [ -x "$BIN" ]; then
    mkdir -p "$BIN_DIR"
    ln -sf "$BIN" "$BIN_DIR/kaleidotron"
    ln -sf "$BIN" "$BIN_DIR/kt"
    echo "  $BIN_DIR/kaleidotron  ->  $BIN"
    echo "  $BIN_DIR/kt          ->  $BIN"
    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *) echo "NOTE: $BIN_DIR is not on your PATH — add it to use 'kt'." ;;
    esac
fi

command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APP_DIR" || true
command -v gtk-update-icon-cache  >/dev/null 2>&1 && gtk-update-icon-cache -f "$DATA/icons/hicolor" 2>/dev/null || true

echo "Installed:"
echo "  $APP_DIR/kaleidotron.desktop"
echo "  $ICON_DIR/kaleidotron.png"
echo "Log out/in (or restart plasmashell/kwin) if the task icon doesn't refresh immediately."
