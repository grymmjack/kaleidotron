#!/usr/bin/env bash
# install-deps.sh — install kaleidotron's build + runtime dependencies.
#
# kaleidotron itself is a single Rust binary, but several *plugins* shell out to external
# tools at runtime (the same lean approach as the rest of the app — no bundled libraries):
#
#   ffmpeg / ffprobe   Video plugin  — thumbnails, the in-app player, PNG/audio export,
#                                      lossless trim + join, YouTube playback
#   yt-dlp             YouTube        — search + download-in-place (needs a CURRENT version;
#                                      distro packages are often too old for today's YouTube)
#   poppler (pdftoppm) PDF plugin     — first-page render (metadata + placeholder without it)
#   blender            .blend tiles   — optional; renders a .blend's frame 1 on demand
#
# Build-time deps (Linux): a C toolchain + the X/Wayland/ALSA/SSL headers eframe & rodio need.
#
# Usage:  ./install-deps.sh            # install everything it can for your OS
#         ./install-deps.sh --no-yt    # skip yt-dlp (if you don't want the YouTube browser)
#
# It auto-detects apt / dnf / pacman / zypper / Homebrew and is safe to re-run.
set -euo pipefail

WANT_YT=1
for a in "$@"; do [ "$a" = "--no-yt" ] && WANT_YT=0; done

say()  { printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[!]\033[0m %s\n' "$*"; }
have() { command -v "$1" >/dev/null 2>&1; }

OS="$(uname -s)"

install_yt_dlp() {
  # Prefer pipx / pip so we get the LATEST yt-dlp — distro packages lag and today's YouTube
  # (SABR + rotating signatures) breaks old versions ("Requested format is not available").
  if have pipx; then
    say "Installing/upgrading yt-dlp via pipx (recommended)"
    pipx install yt-dlp 2>/dev/null || pipx upgrade yt-dlp || true
  elif have python3; then
    say "Installing yt-dlp into a venv at ~/.venvs/ytdlp + linking to ~/.local/bin"
    python3 -m venv "$HOME/.venvs/ytdlp"
    "$HOME/.venvs/ytdlp/bin/pip" install -q -U pip yt-dlp
    mkdir -p "$HOME/.local/bin"
    ln -sf "$HOME/.venvs/ytdlp/bin/yt-dlp" "$HOME/.local/bin/yt-dlp"
    case ":$PATH:" in
      *":$HOME/.local/bin:"*) : ;;
      *) warn "Add ~/.local/bin to your PATH (ahead of /usr/bin) so this yt-dlp is used." ;;
    esac
  else
    warn "No pipx/python3 found — install a current yt-dlp yourself for the YouTube browser."
  fi
}

case "$OS" in
  Linux)
    if have apt-get; then
      say "Debian/Ubuntu (apt)"
      sudo apt-get update
      sudo apt-get install -y build-essential pkg-config \
        libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
        libxkbcommon-dev libssl-dev libasound2-dev \
        ffmpeg poppler-utils
    elif have dnf; then
      say "Fedora (dnf)"
      sudo dnf install -y gcc pkgconf-pkg-config \
        libxcb-devel libxkbcommon-devel openssl-devel alsa-lib-devel \
        ffmpeg poppler-utils
    elif have pacman; then
      say "Arch (pacman)"
      sudo pacman -S --needed --noconfirm base-devel pkgconf \
        libxcb libxkbcommon openssl alsa-lib ffmpeg poppler
    elif have zypper; then
      say "openSUSE (zypper)"
      sudo zypper install -y gcc pkg-config libxcb-devel libxkbcommon-devel \
        libopenssl-devel alsa-devel ffmpeg poppler-tools
    else
      warn "Unknown Linux package manager — install: a C toolchain, pkg-config, the xcb/"
      warn "xkbcommon/openssl/alsa -dev headers, ffmpeg, and poppler-utils yourself."
    fi
    [ "$WANT_YT" = 1 ] && install_yt_dlp
    ;;
  Darwin)
    if have brew; then
      say "macOS (Homebrew)"
      brew install ffmpeg poppler
      [ "$WANT_YT" = 1 ] && brew install yt-dlp
    else
      warn "Install Homebrew (https://brew.sh), then: brew install ffmpeg poppler yt-dlp"
    fi
    ;;
  *)
    warn "Unsupported OS '$OS'. Install ffmpeg, poppler, and (for YouTube) a current yt-dlp."
    ;;
esac

say "Checking what's available now:"
for t in ffmpeg ffprobe yt-dlp pdftoppm blender; do
  if have "$t"; then
    printf '  \033[1;32m✓\033[0m %-9s %s\n' "$t" "$("$t" --version 2>/dev/null | head -1 | cut -c1-48)"
  else
    printf '  \033[1;33m–\033[0m %-9s (not installed%s)\n' "$t" \
      "$([ "$t" = blender ] && echo ', optional')"
  fi
done
say "Done. Build kaleidotron with:  cargo run --release"
