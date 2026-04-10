#!/bin/sh
set -eu

REPO="devchris123/cloudsync"

# --- Detect OS ---
OS="$(uname -s)"
case "$OS" in
  Linux)  OS_TAG="linux" ;;
  Darwin) OS_TAG="darwin" ;;
  *)      echo "Error: unsupported OS: $OS"; exit 1 ;;
esac

# --- Detect Architecture ---
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64)         ARCH_TAG="x86_64" ;;
  aarch64|arm64)  ARCH_TAG="aarch64" ;;
  *)              echo "Error: unsupported architecture: $ARCH"; exit 1 ;;
esac

ASSET_NAME="cloudsync-${OS_TAG}-${ARCH_TAG}"

# --- Determine version ---
if [ -n "${CLOUDSYNC_VERSION:-}" ]; then
  TAG="$CLOUDSYNC_VERSION"
else
  TAG="latest"
fi

# --- Construct download URL ---
if [ "$TAG" = "latest" ]; then
  DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/${ASSET_NAME}"
else
  DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${TAG}/${ASSET_NAME}"
fi

# --- Determine install directory ---
if [ -w /usr/local/bin ]; then
  INSTALL_DIR="/usr/local/bin"
else
  INSTALL_DIR="${HOME}/.local/bin"
  mkdir -p "$INSTALL_DIR"
fi

TMPFILE="$(mktemp)"
trap 'rm -f "$TMPFILE"' EXIT

# --- Download ---
echo "Downloading ${ASSET_NAME} (${TAG})..."
if command -v curl > /dev/null 2>&1; then
  curl -fsSL -o "$TMPFILE" "$DOWNLOAD_URL"
elif command -v wget > /dev/null 2>&1; then
  wget -qO "$TMPFILE" "$DOWNLOAD_URL"
else
  echo "Error: neither curl nor wget found"; exit 1
fi

# --- Install ---
chmod +x "$TMPFILE"
mv "$TMPFILE" "${INSTALL_DIR}/cloudsync"

echo "Installed cloudsync to ${INSTALL_DIR}/cloudsync"

# --- PATH hint ---
case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    echo ""
    echo "NOTE: ${INSTALL_DIR} is not in your PATH."
    echo "Add it with:  export PATH=\"${INSTALL_DIR}:\$PATH\""
    ;;
esac
