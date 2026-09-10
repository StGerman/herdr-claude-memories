#!/bin/sh
# Put a herdr-claude-memories binary at bin/herdr-claude-memories.
#
# Prefers a published release artifact so installing the plugin needs no Rust
# toolchain. Falls back to building from source, which is the normal path while
# developing against a local checkout.

set -eu

REPO="StGerman/herdr-claude-memories"
OUT_DIR="bin"
OUT="$OUT_DIR/herdr-claude-memories"

mkdir -p "$OUT_DIR"

case "$(uname -s)" in
  Darwin) os="macos" ;;
  Linux)  os="linux" ;;
  *) echo "herdr-claude-memories: unsupported OS $(uname -s)" >&2; exit 1 ;;
esac

case "$(uname -m)" in
  arm64|aarch64) arch="aarch64" ;;
  x86_64|amd64)  arch="x86_64" ;;
  *) echo "herdr-claude-memories: unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac

ASSET="herdr-claude-memories-${os}-${arch}"
URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"

if command -v curl >/dev/null 2>&1 && curl -fsSL "$URL" -o "$OUT.download" 2>/dev/null; then
  chmod +x "$OUT.download"
  mv "$OUT.download" "$OUT"
  echo "herdr-claude-memories: installed released $ASSET"
  exit 0
fi

rm -f "$OUT.download"

if command -v cargo >/dev/null 2>&1; then
  echo "herdr-claude-memories: no release asset for $ASSET, building from source" >&2
  cargo build --release
  cp target/release/herdr-claude-memories "$OUT"
  chmod +x "$OUT"
  exit 0
fi

echo "herdr-claude-memories: no release asset for $ASSET and no cargo to build with" >&2
exit 1
