#!/usr/bin/env bash
# One-command install for the codescratch static binary.
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/dzulfiqarzaky19/codescratch/main/install.sh | bash
set -euo pipefail

REPO="${CODESCRATCH_REPO:-dzulfiqarzaky19/codescratch}"
PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="${CODESCRATCH_BIN:-$PREFIX/bin}"

os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"
case "$arch" in
  x86_64|amd64) arch=x86_64 ;;
  aarch64|arm64) arch=aarch64 ;;
  *) echo "unsupported arch: $arch" >&2; exit 1 ;;
esac
case "$os" in
  linux)  target="${arch}-unknown-linux-musl" ;;
  darwin) target="${arch}-apple-darwin" ;;
  mingw*|msys*|cygwin*) target="${arch}-pc-windows-msvc" ;;
  *) echo "unsupported os: $os" >&2; exit 1 ;;
esac

asset="codescratch-${target}"
if [[ "$os" == mingw* || "$os" == msys* || "$os" == cygwin* ]]; then
  asset="${asset}.exe"
fi

url="https://github.com/${REPO}/releases/latest/download/${asset}"
tmp="$(mktemp)"
echo "downloading $url"
if ! curl -fsSL "$url" -o "$tmp"; then
  echo "release asset missing; building from source (needs rustup + cc)" >&2
  rm -f "$tmp"
  if ! command -v cargo >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
  fi
  git clone --depth 1 "https://github.com/${REPO}.git" /tmp/codescratch-src
  cargo build --release --manifest-path /tmp/codescratch-src/Cargo.toml
  mkdir -p "$BIN_DIR"
  install -m 0755 /tmp/codescratch-src/target/release/codescratch "$BIN_DIR/codescratch"
  rm -rf /tmp/codescratch-src
else
  mkdir -p "$BIN_DIR"
  chmod +x "$tmp"
  install -m 0755 "$tmp" "$BIN_DIR/codescratch"
  rm -f "$tmp"
fi

echo "installed $BIN_DIR/codescratch"
echo "add to PATH if needed:  export PATH=\"$BIN_DIR:\$PATH\""
echo "then:  codescratch setup && codescratch init"
