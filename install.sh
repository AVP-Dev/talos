#!/usr/bin/env bash
# TALOS Installer Script — https://talos.avpdev.com
# Usage: curl -fsSL https://talos.avpdev.com/install.sh | bash

set -euo pipefail

BOLD="$(printf '\033[1m')"
GREEN="$(printf '\033[0;32m')"
BLUE="$(printf '\033[0;34m')"
YELLOW="$(printf '\033[0;33m')"
RED="$(printf '\033[0;31m')"
RESET="$(printf '\033[0m')"

log_info() {
  printf "${BLUE}==>${RESET} ${BOLD}%s${RESET}\n" "$1"
}

log_success() {
  printf "${GREEN}==>${RESET} ${BOLD}%s${RESET}\n" "$1"
}

log_warn() {
  printf "${YELLOW}warning:${RESET} %s\n" "$1"
}

log_error() {
  printf "${RED}error:${RESET} %s\n" "$1" >&2
}

GITHUB_REPO="${TALOS_REPO:-AVP-Dev/talos}"
INSTALL_DIR="${TALOS_INSTALL_DIR:-$HOME/.local/bin}"

# Detect OS and Architecture
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Darwin)
    case "$ARCH" in
      arm64|aarch64)
        TARGET="aarch64-apple-darwin"
        ;;
      x86_64)
        TARGET="x86_64-apple-darwin"
        ;;
      *)
        log_error "Unsupported macOS architecture: $ARCH"
        exit 1
        ;;
    esac
    ;;
  Linux)
    case "$ARCH" in
      x86_64)
        TARGET="x86_64-unknown-linux-gnu"
        ;;
      aarch64|arm64)
        TARGET="aarch64-unknown-linux-gnu"
        ;;
      *)
        log_error "Unsupported Linux architecture: $ARCH"
        exit 1
        ;;
    esac
    ;;
  *)
    log_error "Unsupported operating system: $OS"
    exit 1
    ;;
esac

log_info "Installing TALOS for ${BOLD}${TARGET}${RESET}..."

# Resolve latest release version if not explicitly set
VERSION="${TALOS_VERSION:-latest}"
if [ "$VERSION" = "latest" ]; then
  LATEST_RELEASE_URL="https://api.github.com/repos/${GITHUB_REPO}/releases/latest"
  VERSION="$(curl -fsSL "$LATEST_RELEASE_URL" 2>/dev/null | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/' || true)"
  if [ -z "$VERSION" ]; then
    log_error "Could not find the latest TALOS release. Check your connection to GitHub and try again."
    log_error "To install a specific published release, set TALOS_VERSION to its Git tag."
    exit 1
  fi
fi

# Accept a numeric version or its release tag.
CLEAN_VERSION="${VERSION#v}"
if [[ ! "$CLEAN_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  log_error "Expected a release tag such as v0.1.2."
  exit 1
fi
VERSION="v$CLEAN_VERSION"
TARBALL_NAME="talos-v${CLEAN_VERSION}-${TARGET}.tar.gz"
DOWNLOAD_URL="https://github.com/${GITHUB_REPO}/releases/download/${VERSION}/${TARBALL_NAME}"

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/talos-install.XXXXXX")"
STAGED_BINARY=""
cleanup() {
  if [ -n "$STAGED_BINARY" ]; then
    rm -f "$STAGED_BINARY"
  fi
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

previous_binary="$(command -v talos || true)"
mkdir -p "$INSTALL_DIR"

log_info "Downloading ${TARBALL_NAME}..."
if curl --fail --location --progress-bar "$DOWNLOAD_URL" -o "$TMP_DIR/$TARBALL_NAME" 2>/dev/null; then
  if ! curl -fsSL "$DOWNLOAD_URL.sha256" -o "$TMP_DIR/$TARBALL_NAME.sha256"; then
    log_error "The release checksum is unavailable. Try again or choose another published release."
    exit 1
  fi
  read -r expected_checksum _ < "$TMP_DIR/$TARBALL_NAME.sha256"
  if command -v sha256sum >/dev/null 2>&1; then
    actual_checksum="$(sha256sum "$TMP_DIR/$TARBALL_NAME")"
  else
    actual_checksum="$(shasum -a 256 "$TMP_DIR/$TARBALL_NAME")"
  fi
  if [[ ! "$expected_checksum" =~ ^[0-9a-f]{64}$ ]] || [ "${actual_checksum%% *}" != "$expected_checksum" ]; then
    log_error "Archive checksum mismatch. Your current installation was not replaced."
    exit 1
  fi
  tar -xzf "$TMP_DIR/$TARBALL_NAME" -C "$TMP_DIR"
  if [ -f "$TMP_DIR/talos" ]; then
    BIN_SRC="$TMP_DIR/talos"
  elif [ -f "$TMP_DIR/talos-v${CLEAN_VERSION}-${TARGET}/talos" ]; then
    BIN_SRC="$TMP_DIR/talos-v${CLEAN_VERSION}-${TARGET}/talos"
  else
    log_error "Could not locate 'talos' executable inside the downloaded archive."
    exit 1
  fi
else
  # Fallback to local build if repo checkout is detected or release not published yet
  if command -v cargo >/dev/null 2>&1 && [ -f "Cargo.toml" ] && grep -q 'name = "talos"' Cargo.toml 2>/dev/null; then
    log_warn "Release archive not found online. Building from local source via cargo..."
    cargo build --release --locked
    BIN_SRC="target/release/talos"
  else
    log_error "Failed to download $DOWNLOAD_URL."
    log_error "Please verify the release exists at https://github.com/${GITHUB_REPO}/releases"
    exit 1
  fi
fi

STAGED_BINARY="$(mktemp "$INSTALL_DIR/.talos.XXXXXX")"
install -m 0755 "$BIN_SRC" "$STAGED_BINARY"
installed_version="$("$STAGED_BINARY" --version)"
mv -f "$STAGED_BINARY" "$INSTALL_DIR/talos"
STAGED_BINARY=""

# Ensure target directory is in PATH
SHELL_CONFIG=""
case "${SHELL:-}" in
  */zsh)
    SHELL_CONFIG="$HOME/.zshrc"
    ;;
  */bash)
    if [ -f "$HOME/.bash_profile" ]; then
      SHELL_CONFIG="$HOME/.bash_profile"
    else
      SHELL_CONFIG="$HOME/.bashrc"
    fi
    ;;
esac

PATH_UPDATED=0
if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
  if [ -n "$SHELL_CONFIG" ] && [ -w "$SHELL_CONFIG" ]; then
    if ! grep -q "$INSTALL_DIR" "$SHELL_CONFIG" 2>/dev/null; then
      printf '\n# TALOS CLI\nexport PATH="%s:$PATH"\n' "$INSTALL_DIR" >> "$SHELL_CONFIG"
      PATH_UPDATED=1
    fi
  fi
fi

export PATH="$INSTALL_DIR:$PATH"

log_success "TALOS successfully installed to ${BOLD}$INSTALL_DIR/talos${RESET}!"

if [ "$PATH_UPDATED" -eq 1 ]; then
  printf "\n${YELLOW}Note:${RESET} Added %s to ${BOLD}%s${RESET}.\n" "$INSTALL_DIR" "$SHELL_CONFIG"
  printf "Run ${BOLD}source %s${RESET} or open a new terminal window to refresh your PATH.\n\n" "$SHELL_CONFIG"
fi

if [ -n "$previous_binary" ] && [ "$previous_binary" != "$INSTALL_DIR/talos" ]; then
  log_warn "Another TALOS was found at $previous_binary. Run 'type -a talos' if the older copy still opens."
fi

printf '\nInstalled version: %s\n' "$installed_version"

printf "\nOpen a terminal in your Git project and run ${BOLD}talos${RESET} to get started.\n"
