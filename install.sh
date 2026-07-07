#!/bin/sh
# voice-mentor installer — detects OS/arch, fetches the latest prebuilt `mentor`
# binary from GitHub Releases into ~/.local/bin (override: MENTOR_INSTALL_DIR).
#
#   gh api repos/banavasi/voice-mentor/contents/install.sh \
#     -H "Accept: application/vnd.github.raw" | sh
#
# The repo is private, so downloads authenticate via the `gh` CLI (preferred)
# or a GITHUB_TOKEN env var. Falls back to `cargo install --git` instructions.
set -eu

REPO="banavasi/voice-mentor"
BIN_DIR="${MENTOR_INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -s)" in
  Linux)  os="linux" ;;
  Darwin) os="macos" ;;
  *) echo "unsupported OS: $(uname -s) — on Windows run install.ps1, or: cargo install --git https://github.com/$REPO" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64|amd64)  arch="x86_64" ;;
  arm64|aarch64) arch="aarch64" ;;
  *) echo "unsupported arch: $(uname -m) — build from source: cargo install --git https://github.com/$REPO" >&2; exit 1 ;;
esac
asset="mentor-${arch}-${os}"

mkdir -p "$BIN_DIR"
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

echo "→ installing $asset (latest release) to $BIN_DIR/mentor"
if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
  gh release download --repo "$REPO" --pattern "$asset" --output "$tmp" --clobber
elif [ -n "${GITHUB_TOKEN:-}" ]; then
  # private repo via raw API: find the asset id, then download it
  asset_id="$(curl -fsSL -H "Authorization: Bearer $GITHUB_TOKEN" \
      "https://api.github.com/repos/$REPO/releases/latest" \
    | grep -B3 "\"name\": \"$asset\"" | grep -m1 '"id":' | grep -o '[0-9]*')"
  [ -n "$asset_id" ] || { echo "asset $asset not found in the latest release" >&2; exit 1; }
  curl -fsSL -H "Authorization: Bearer $GITHUB_TOKEN" \
    -H "Accept: application/octet-stream" \
    -o "$tmp" "https://api.github.com/repos/$REPO/releases/assets/$asset_id"
else
  echo "this repo is private: install and auth the gh CLI (https://cli.github.com)," >&2
  echo "or set GITHUB_TOKEN, or build from source:" >&2
  echo "  cargo install --git https://github.com/$REPO" >&2
  exit 1
fi

install -m 755 "$tmp" "$BIN_DIR/mentor"
echo "✓ installed: $BIN_DIR/mentor"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "⚠ $BIN_DIR is not on your PATH — add it to your shell profile" ;;
esac
echo "next: run \`mentor\` — the first start offers the STT model download (~650 MB),"
echo "and \`mentor doctor\` walks the remaining setup (claude CLI, gcli, loopback)."
