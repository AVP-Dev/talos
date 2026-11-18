#!/usr/bin/env bash
set -euo pipefail

fail() {
  printf 'package-macos: %s\n' "$*" >&2
  exit 2
}

[ "$(uname -s)" = "Darwin" ] || fail "run this script on macOS"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_ROOT"

TARGET="${1:-$(rustc -vV | awk '/^host:/ { print $2 }')}"
case "$TARGET" in
  aarch64-apple-darwin|x86_64-apple-darwin) ;;
  *) fail "target must be a macOS target triple (got '$TARGET')" ;;
esac

exec 3< <(git status --porcelain=v1 -z --untracked-files=all)
STATUS_PID=$!
if IFS= read -r -n 1 -u 3 STATUS_BYTE; then
  exec 3<&-
  kill "$STATUS_PID" 2>/dev/null || true
  wait "$STATUS_PID" 2>/dev/null || true
  fail "commit or remove working-tree changes before packaging"
fi
exec 3<&-
if ! wait "$STATUS_PID"; then
  fail "cannot inspect Git working-tree status"
fi

VERSION="$(awk -F '"' '$1 ~ /^version = / { print $2; exit }' Cargo.toml)"
[ -n "$VERSION" ] || fail "cannot read package version from Cargo.toml"
COMMIT="$(git rev-parse --short=12 HEAD)"
SOURCE_DATE_EPOCH="$(git log -1 --format=%ct HEAD)"
RUSTC_VERSION="$(rustc --version)"
LOCKFILE_SHA256="$(shasum -a 256 Cargo.lock | awk '{ print $1 }')"

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
cargo_lock_sha256=$LOCKFILE_SHA256
EOF
(
  cd "$ARTIFACT_DIR"
  shasum -a 256 talos README.md BUILD-INFO.txt >SHA256SUMS
)

printf 'Created %s\n' "$ARTIFACT_DIR"
