#!/bin/sh
# install.sh — install claude-print from GitHub Releases
# Usage: sh install.sh
# Env vars:
#   SKIP_MOCK_CLAUDE=1  skip mock_claude installation
set -e

REPO="jedarden/claude-print"
INSTALL_DIR="${HOME}/.local/bin"
NEEDLE_AGENTS_DIR="${HOME}/.needle/agents"

# Detect architecture
ARCH=$(uname -m)
case "$ARCH" in
  x86_64)  TARGET="x86_64-unknown-linux-musl" ;;
  aarch64) TARGET="aarch64-unknown-linux-musl" ;;
  *)
    echo "Unsupported architecture: ${ARCH}" >&2
    exit 1
    ;;
esac

BINARY_ASSET="claude-print-${TARGET}"
MOCK_ASSET="mock_claude-${TARGET}"

# Verify claude is on PATH
if ! command -v claude >/dev/null 2>&1; then
  echo "Error: 'claude' not found in PATH. Install Claude Code first." >&2
  exit 1
fi

# Create install dir
mkdir -p "${INSTALL_DIR}"

# Download claude-print binary
BINARY_URL="https://github.com/${REPO}/releases/latest/download/${BINARY_ASSET}"
echo "Downloading ${BINARY_ASSET}..."
TMP_BIN=$(mktemp)
trap 'rm -f "${TMP_BIN}"' EXIT
curl -fsSL "${BINARY_URL}" -o "${TMP_BIN}"

# Backup existing binary (enables one-step rollback)
if [ -f "${INSTALL_DIR}/claude-print" ]; then
  echo "Backing up existing binary to ${INSTALL_DIR}/claude-print.prev"
  mv "${INSTALL_DIR}/claude-print" "${INSTALL_DIR}/claude-print.prev"
fi

install -m 755 "${TMP_BIN}" "${INSTALL_DIR}/claude-print"
echo "Installed ${INSTALL_DIR}/claude-print"

# Install mock_claude (unless SKIP_MOCK_CLAUDE=1)
if [ "${SKIP_MOCK_CLAUDE:-0}" != "1" ]; then
  MOCK_URL="https://github.com/${REPO}/releases/latest/download/${MOCK_ASSET}"
  TMP_MOCK=$(mktemp)
  trap 'rm -f "${TMP_BIN}" "${TMP_MOCK}"' EXIT
  if curl -fsSL "${MOCK_URL}" -o "${TMP_MOCK}" 2>/dev/null; then
    install -m 755 "${TMP_MOCK}" "${INSTALL_DIR}/mock_claude"
    echo "Installed ${INSTALL_DIR}/mock_claude"
  else
    echo "Note: ${MOCK_ASSET} not found in this release — skipping mock_claude"
  fi
  rm -f "${TMP_MOCK}"
fi

# Install NEEDLE agent config if NEEDLE is installed or agents dir exists
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
if command -v needle >/dev/null 2>&1 || [ -d "${NEEDLE_AGENTS_DIR}" ]; then
  mkdir -p "${NEEDLE_AGENTS_DIR}"
  if [ -f "${SCRIPT_DIR}/claude-print.yaml" ]; then
    install -m 644 "${SCRIPT_DIR}/claude-print.yaml" "${NEEDLE_AGENTS_DIR}/claude-print.yaml"
    echo "Installed ${NEEDLE_AGENTS_DIR}/claude-print.yaml"
  else
    echo "Note: claude-print.yaml not found alongside install.sh — skipping NEEDLE config"
  fi
fi

# Verify installation
echo ""
echo "Running claude-print --check..."
if ! "${INSTALL_DIR}/claude-print" --check; then
  echo "Error: claude-print --check failed" >&2
  exit 1
fi

echo ""
"${INSTALL_DIR}/claude-print" --version
echo ""
echo "Installation complete."
