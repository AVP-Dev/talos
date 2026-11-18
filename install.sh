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
    VERSION="v0.1.0"
  fi
fi

# Strip leading 'v' if present for tarball matching
CLEAN_VERSION="${VERSION#v}"
TARBALL_NAME="talos-v${CLEAN_VERSION}-${TARGET}.tar.gz"
DOWNLOAD_URL="https://github.com/${GITHUB_REPO}/releases/download/${VERSION}/${TARBALL_NAME}"

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/talos-install.XXXXXX")"
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

mkdir -p "$INSTALL_DIR"

log_info "Downloading ${TARBALL_NAME}..."
if curl --fail --location --progress-bar "$DOWNLOAD_URL" -o "$TMP_DIR/$TARBALL_NAME" 2>/dev/null; then
  tar -xzf "$TMP_DIR/$TARBALL_NAME" -C "$TMP_DIR"
  if [ -f "$TMP_DIR/talos" ]; then
    BIN_SRC="$TMP_DIR/talos"
  elif [ -f "$TMP_DIR/talos-v${CLEAN_VERSION}-${TARGET}/talos" ]; then
    BIN_SRC="$TMP_DIR/talos-v${CLEAN_VERSION}-${TARGET}/talos"
  else
    log_error "Could not locate 'talos' executable inside the downloaded archive."
    exit 1
  fi
  cp -f "$BIN_SRC" "$INSTALL_DIR/talos"
  chmod 0755 "$INSTALL_DIR/talos"
else
  # Fallback to local build if repo checkout is detected or release not published yet
  if command -v cargo >/dev/null 2>&1 && [ -f "Cargo.toml" ] && grep -q 'name = "talos"' Cargo.toml 2>/dev/null; then
    log_warn "Release archive not found online. Building from local source via cargo..."
    cargo build --release
    cp -f "target/release/talos" "$INSTALL_DIR/talos"
    chmod 0755 "$INSTALL_DIR/talos"
  else
    log_error "Failed to download $DOWNLOAD_URL."
    log_error "Please verify the release exists at https://github.com/${GITHUB_REPO}/releases"
    exit 1
  fi
fi

# Remove macOS quarantine attribute so Gatekeeper never prompts
if [ "$OS" = "Darwin" ]; then
  xattr -d com.apple.quarantine "$INSTALL_DIR/talos" 2>/dev/null || true
fi

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

printf "\nInstalled version: "
"$INSTALL_DIR/talos" --version

printf "\nOpen a terminal in your Git project and run ${BOLD}talos${RESET} to get started.\n"
