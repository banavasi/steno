#!/bin/sh
# steno installer — detects OS/arch and installs the latest prebuilt
# `steno` binary from GitHub Releases into ~/.local/bin.
#
#   curl -fsSL https://raw.githubusercontent.com/banavasi/steno/main/install.sh | sh
#
# Override the destination with STENO_INSTALL_DIR.
set -eu

REPO="banavasi/steno"
BIN_DIR="${STENO_INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -s)" in
  Linux)  os="linux" ;;
  Darwin) os="macos" ;;
  *) echo "unsupported OS: $(uname -s) — on Windows use install.ps1, or: cargo install --git https://github.com/$REPO" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64|amd64)  arch="x86_64" ;;
  arm64|aarch64) arch="aarch64" ;;
  *) echo "unsupported arch: $(uname -m) — build from source: cargo install --git https://github.com/$REPO" >&2; exit 1 ;;
esac
asset="steno-${arch}-${os}"
url="https://github.com/$REPO/releases/latest/download/$asset"

mkdir -p "$BIN_DIR"
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

echo "→ downloading $asset (latest release)"
if curl -fSL --progress-bar -o "$tmp" "$url"; then
  :
elif command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
  # fallback for forks/private mirrors
  gh release download --repo "$REPO" --pattern "$asset" --output "$tmp" --clobber
else
  echo "download failed: $url" >&2
  echo "build from source instead: cargo install --git https://github.com/$REPO" >&2
  exit 1
fi

install -m 755 "$tmp" "$BIN_DIR/steno"
echo "✓ installed: $BIN_DIR/steno ($("$BIN_DIR/steno" --version 2>/dev/null || echo '?'))"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "⚠ $BIN_DIR is not on your PATH — add it to your shell profile" ;;
esac
# warn if another install (e.g. an old `cargo install` copy) shadows this one
resolved="$(command -v steno 2>/dev/null || true)"
if [ -n "$resolved" ] && [ "$resolved" != "$BIN_DIR/steno" ]; then
  echo "⚠ 'steno' currently resolves to $resolved — remove it or fix PATH order, or updates here won't take effect"
fi
echo "next: run \`steno\` — the first start offers the STT model download (~650 MB),"
echo "and \`steno doctor\` walks the remaining setup (claude CLI, loopback device)."
