#!/usr/bin/env bash
set -euo pipefail

fail() {
  printf 'package-linux: %s\n' "$*" >&2
  exit 2
}

[ "$(uname -s)" = "Linux" ] || fail "run this script on Linux"
command -v cargo >/dev/null 2>&1 || fail "cargo is required"
command -v rustc >/dev/null 2>&1 || fail "rustc is required"
command -v git >/dev/null 2>&1 || fail "git is required"
command -v install >/dev/null 2>&1 || fail "install is required"
command -v sha256sum >/dev/null 2>&1 || fail "sha256sum is required"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_ROOT"

TARGET="$(rustc -vV | awk '/^host:/ { print $2 }')"
case "$TARGET" in
  x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu) ;;
  *) fail "unsupported native target '$TARGET'; supported targets are x86_64-unknown-linux-gnu and aarch64-unknown-linux-gnu" ;;
esac

if [ -n "$(git status --porcelain --untracked-files=all)" ]; then
  fail "commit or remove working-tree changes before packaging"
fi

VERSION="$(awk -F '"' '$1 ~ /^version = / { print $2; exit }' Cargo.toml)"
[ -n "$VERSION" ] || fail "cannot read package version from Cargo.toml"
COMMIT="$(git rev-parse --short=12 HEAD)"
SOURCE_DATE_EPOCH="$(git log -1 --format=%ct HEAD)"
RUSTC_VERSION="$(rustc --version)"
CARGO_VERSION="$(cargo --version)"
LOCKFILE_SHA256="$(sha256sum Cargo.lock | awk '{ print $1 }')"

export CARGO_INCREMENTAL=0
export SOURCE_DATE_EPOCH
cargo build --locked --release --target "$TARGET"

ARTIFACT_DIR="dist/talos-v${VERSION}-${TARGET}-${COMMIT}"
mkdir -p "$ARTIFACT_DIR"
install -m 0755 "target/$TARGET/release/talos" "$ARTIFACT_DIR/talos"
cp README.md "$ARTIFACT_DIR/README.md"
cat >"$ARTIFACT_DIR/BUILD-INFO.txt" <<EOF
package=talos
version=$VERSION
target=$TARGET
git_commit=$COMMIT
source_date_epoch=$SOURCE_DATE_EPOCH
rustc=$RUSTC_VERSION
cargo=$CARGO_VERSION
cargo_lock_sha256=$LOCKFILE_SHA256
EOF
(
  cd "$ARTIFACT_DIR"
  sha256sum talos README.md BUILD-INFO.txt >SHA256SUMS
)

printf 'Created %s\n' "$ARTIFACT_DIR"
