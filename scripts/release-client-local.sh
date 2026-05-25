#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

usage() {
  cat <<'EOF'
Usage: scripts/release-client-local.sh [version]

Build and package baidupan-cli and baidupan-auth-server release artifacts for mainstream desktop targets.

Targets:
  - x86_64-apple-darwin
  - aarch64-apple-darwin
  - x86_64-unknown-linux-gnu
  - aarch64-unknown-linux-gnu
  - x86_64-pc-windows-gnu

Environment:
  - Reads .env automatically when present
  - Maps BAIDUPAN_APP_KEY, BAIDUPAN_APP_SECRET, BAIDUPAN_APP_NAME,
    BAIDUPAN_AUTH_SERVER, BAIDUPAN_CRYPTO_PASSPHRASE to compile-time defaults
    unless BAIDUPAN_DEFAULT_* is already set
  - BAIDUPAN_AUTH_SERVER is optional for local toolkit builds; when omitted, the
    packaged client has no built-in auth server default

Prerequisites for cross-platform builds:
  - rustup target add for the listed targets
  - cargo-zigbuild installed: cargo install cargo-zigbuild
  - zig installed and available in PATH
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

VERSION="${1:-local}"
DIST_DIR="$ROOT_DIR/dist/$VERSION"
CLIENT_BIN="baidupan-cli"
AUTH_SERVER_BIN="baidupan-auth-server"

if [[ -f "$ROOT_DIR/.env" ]]; then
  set -a
  # shellcheck disable=SC1091
  source "$ROOT_DIR/.env"
  set +a
fi

export BAIDUPAN_DEFAULT_APP_NAME="${BAIDUPAN_DEFAULT_APP_NAME:-${BAIDUPAN_APP_NAME:-}}"
export BAIDUPAN_DEFAULT_AUTH_SERVER="${BAIDUPAN_DEFAULT_AUTH_SERVER:-${BAIDUPAN_AUTH_SERVER:-}}"
export BAIDUPAN_DEFAULT_CRYPTO_PASSPHRASE="${BAIDUPAN_DEFAULT_CRYPTO_PASSPHRASE:-${BAIDUPAN_CRYPTO_PASSPHRASE:-}}"
export BAIDUPAN_DEFAULT_APP_KEY="${BAIDUPAN_DEFAULT_APP_KEY:-${BAIDUPAN_APP_KEY:-}}"
export BAIDUPAN_DEFAULT_APP_SECRET="${BAIDUPAN_DEFAULT_APP_SECRET:-${BAIDUPAN_APP_SECRET:-}}"

if [[ -z "$BAIDUPAN_DEFAULT_APP_NAME" ]]; then
  echo "error: BAIDUPAN_APP_NAME or BAIDUPAN_DEFAULT_APP_NAME is required" >&2
  exit 1
fi

mkdir -p "$DIST_DIR"

need_cmd() {
  local cmd="$1"
  local hint="$2"
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "error: missing command '$cmd' ($hint)" >&2
    exit 1
  fi
}

build_native() {
  local target="$1"
  local bin_name="$2"
  cargo build --locked --release --bin "$bin_name" --target "$target"
}

build_cross() {
  local target="$1"
  local bin_name="$2"
  cargo zigbuild --locked --release --bin "$bin_name" --target "$target"
}

package_unix() {
  local target="$1"
  local label="$2"
  local asset="$DIST_DIR/baidupan-toolkit-${VERSION}-${label}.tar.gz"
  tar -czf "$asset" -C "$ROOT_DIR/target/$target/release" "$CLIENT_BIN" "$AUTH_SERVER_BIN"
  echo "packaged $asset"
}

package_windows() {
  local target="$1"
  local label="$2"
  local asset="$DIST_DIR/baidupan-toolkit-${VERSION}-${label}.zip"
  (
    cd "$ROOT_DIR/target/$target/release"
    rm -f "$asset"
    zip -q "$asset" "${CLIENT_BIN}.exe" "${AUTH_SERVER_BIN}.exe"
  )
  echo "packaged $asset"
}

need_cmd rustup "install Rust targets"
need_cmd cargo "build Rust binaries"
need_cmd zip "package Windows release archives"
need_cmd zig "cross-link Linux/Windows targets"
if ! cargo zigbuild --help >/dev/null 2>&1; then
  echo "error: cargo-zigbuild is required (install with: cargo install cargo-zigbuild)" >&2
  exit 1
fi

ALL_TARGETS=(
  x86_64-apple-darwin
  aarch64-apple-darwin
  x86_64-unknown-linux-gnu
  aarch64-unknown-linux-gnu
  x86_64-pc-windows-gnu
)

echo "installing Rust targets..."
rustup target add "${ALL_TARGETS[@]}"

echo "building macOS targets..."
build_native x86_64-apple-darwin "$CLIENT_BIN"
build_native x86_64-apple-darwin "$AUTH_SERVER_BIN"
package_unix x86_64-apple-darwin macos-x86_64

build_native aarch64-apple-darwin "$CLIENT_BIN"
build_native aarch64-apple-darwin "$AUTH_SERVER_BIN"
package_unix aarch64-apple-darwin macos-aarch64

echo "building Linux targets..."
build_cross x86_64-unknown-linux-gnu "$CLIENT_BIN"
build_cross x86_64-unknown-linux-gnu "$AUTH_SERVER_BIN"
package_unix x86_64-unknown-linux-gnu linux-x86_64

build_cross aarch64-unknown-linux-gnu "$CLIENT_BIN"
build_cross aarch64-unknown-linux-gnu "$AUTH_SERVER_BIN"
package_unix aarch64-unknown-linux-gnu linux-aarch64

echo "building Windows target..."
build_cross x86_64-pc-windows-gnu "$CLIENT_BIN"
build_cross x86_64-pc-windows-gnu "$AUTH_SERVER_BIN"
package_windows x86_64-pc-windows-gnu windows-x86_64

echo "release artifacts written to $DIST_DIR"
