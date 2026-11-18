#!/usr/bin/env bash
set -euo pipefail

tag=${1:?Usage: render-homebrew-formula.sh vX.Y.Z archive-directory}
archives=${2:?Missing directory containing the release archives}
if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  printf 'Expected a stable release tag in the form vX.Y.Z.\n' >&2
  exit 1
fi
version=${tag#v}
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
targets=(aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu)
checksums=()
for target in "${targets[@]}"; do
  archive="$archives/talos-$tag-$target.tar.gz"
  actual="$(shasum -a 256 "$archive")"
  read -r expected _ < "$archive.sha256"
  if [[ ! "$expected" =~ ^[0-9a-f]{64}$ ]] || [ "${actual%% *}" != "$expected" ]; then
    printf 'Checksum does not match %s\n' "$archive" >&2
    exit 1
  fi
  checksums+=("$expected")
done

awk -v version="$version" -v tag="$tag" \
  -v mac_arm="${checksums[0]}" -v mac_intel="${checksums[1]}" \
  -v linux_arm="${checksums[2]}" -v linux_intel="${checksums[3]}" '
  BEGIN {
    checksums["aarch64-apple-darwin"] = mac_arm
    checksums["x86_64-apple-darwin"] = mac_intel
    checksums["aarch64-unknown-linux-gnu"] = linux_arm
    checksums["x86_64-unknown-linux-gnu"] = linux_intel
  }
  /^  version "/ { $0 = "  version \"" version "\"" }
  /    url "/ {
    for (target in checksums) {
      if (index($0, "-" target ".tar.gz")) {
        $0 = "      url \"https://github.com/AVP-Dev/talos/releases/download/" tag "/talos-" tag "-" target ".tar.gz\""
        checksum = checksums[target]
        seen[target]++
        break
      }
    }
  }
  /    sha256 "/ {
    if (!checksum) exit 1
    $0 = "      sha256 \"" checksum "\""
    checksum = ""
  }
  /assert_match "talos / { sub(/talos [0-9.]+/, "talos " version) }
  { print }
  END { for (target in checksums) if (seen[target] != 1) exit 1 }
' "$root/Formula/talos.rb"
