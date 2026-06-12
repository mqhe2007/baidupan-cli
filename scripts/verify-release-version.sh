#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

TAG="${1:-${GITHUB_REF_NAME:-}}"

usage() {
  cat <<'EOF'
Usage: scripts/verify-release-version.sh <tag>

Ensure the git tag (e.g. v0.3.1) matches [package].version in Cargo.toml.
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ -z "$TAG" ]]; then
  usage >&2
  exit 1
fi

if [[ "$TAG" != v* ]]; then
  echo "error: tag must start with 'v', got: $TAG" >&2
  exit 1
fi

VERSION="${TAG#v}"
CARGO_VERSION="$(grep -m1 '^version = ' Cargo.toml | sed -E 's/^version = "(.*)"/\1/')"

if [[ -z "$CARGO_VERSION" ]]; then
  echo "error: could not read version from Cargo.toml" >&2
  exit 1
fi

if [[ "$VERSION" != "$CARGO_VERSION" ]]; then
  echo "error: tag version ($VERSION) does not match Cargo.toml ($CARGO_VERSION)" >&2
  exit 1
fi

echo "release version ok: $VERSION"
