#!/bin/sh
# install.sh — install claude-print from GitHub Releases
# Usage: sh install.sh
# Env vars:
#   SKIP_MOCK_CLAUDE=1  skip mock_claude installation
#   CLAUDE_PRINT_RELEASE_URL  base URL release assets are downloaded from
#                      (default: latest GitHub release of jedarden/claude-print;
#                      overridden by tests and mirrors to point elsewhere)
#
# Every artifact is verified against the release's published sha256sums.txt
# manifest before it is installed or executed. A missing manifest, a missing
# checksum entry, or a digest mismatch aborts the install with nothing placed.
set -e

REPO="jedarden/claude-print"
INSTALL_DIR="${HOME}/.local/bin"
NEEDLE_AGENTS_DIR="${HOME}/.needle/agents"
RELEASE_URL="${CLAUDE_PRINT_RELEASE_URL:-https://github.com/${REPO}/releases/latest/download}"
CHECKSUMS_ASSET="sha256sums.txt"

# Detect OS and architecture
OS=$(uname -s)
ARCH=$(uname -m)
case "${OS}-${ARCH}" in
  Linux-x86_64)  TARGET="x86_64-linux" ;;
  Linux-aarch64) TARGET="aarch64-linux" ;;
  *)
    echo "Unsupported platform: ${OS}-${ARCH}" >&2
    exit 1
    ;;
esac

BINARY_ASSET="claude-print-${TARGET}"
MOCK_ASSET="mock_claude-${TARGET}"

# Integrity verification requires sha256sum
if ! command -v sha256sum >/dev/null 2>&1; then
  echo "Error: sha256sum not found — cannot verify release artifacts" >&2
  exit 1
fi

# Print the checksum-file entry for an asset name, or nothing when the asset
# has no entry. Tolerates both sha256sum text formats ("hash  name" and
# "hash *name") and a leading "./" on the filename.
checksum_entry_for() {
  awk -v asset="$1" '
    {
      name = $2
      sub(/^\*/, "", name)
      sub(/^\.\//, "", name)
      if (name == asset) { print $1; exit }
    }
  ' "$2"
}

# verify_artifact FILE ASSET CHECKSUMS_FILE — print the digest and return 0
# only when ASSET has a checksum entry and FILE matches it; otherwise report
# and return 1 so the caller (under set -e) fails the install.
verify_artifact() {
  expected=$(checksum_entry_for "$2" "$3")
  if [ -z "${expected}" ]; then
    echo "Error: ${2} has no entry in ${CHECKSUMS_ASSET} — refusing to install unverified artifact" >&2
    return 1
  fi
  actual=$(sha256sum "$1" | awk '{print $1}')
  if [ "${actual}" != "${expected}" ]; then
    echo "Error: sha256 mismatch for ${2} (expected ${expected}, got ${actual}) — refusing to install" >&2
    return 1
  fi
  echo "Verified ${2} (sha256 ${actual})"
}

# Verify claude is on PATH
if ! command -v claude >/dev/null 2>&1; then
  echo "Error: 'claude' not found in PATH. Install Claude Code first." >&2
  exit 1
fi

# Create install dir
mkdir -p "${INSTALL_DIR}"

# Scratch files for the download + verification window; the trap removes them
# on every exit path, including a verification failure.
TMP_BIN=$(mktemp)
TMP_MOCK=$(mktemp)
TMP_CHECKSUMS=$(mktemp)
trap 'rm -f "${TMP_BIN}" "${TMP_MOCK}" "${TMP_CHECKSUMS}"' EXIT

# Download the release's checksum manifest first: without it nothing can be
# verified, so refuse to install anything at all.
echo "Downloading ${CHECKSUMS_ASSET}..."
if ! curl -fsSL "${RELEASE_URL}/${CHECKSUMS_ASSET}" -o "${TMP_CHECKSUMS}"; then
  echo "Error: ${CHECKSUMS_ASSET} is not available for this release — refusing to install unverified artifacts" >&2
  exit 1
fi

# Download claude-print binary and verify it before it is placed anywhere
BINARY_URL="${RELEASE_URL}/${BINARY_ASSET}"
echo "Downloading ${BINARY_ASSET}..."
if ! curl -fsSL "${BINARY_URL}" -o "${TMP_BIN}"; then
  echo "Error: ${BINARY_ASSET} could not be downloaded" >&2
  exit 1
fi
verify_artifact "${TMP_BIN}" "${BINARY_ASSET}" "${TMP_CHECKSUMS}"

# Backup existing binary (enables one-step rollback)
if [ -f "${INSTALL_DIR}/claude-print" ]; then
  echo "Backing up existing binary to ${INSTALL_DIR}/claude-print.prev"
  mv "${INSTALL_DIR}/claude-print" "${INSTALL_DIR}/claude-print.prev"
fi

install -m 755 "${TMP_BIN}" "${INSTALL_DIR}/claude-print"
echo "Installed ${INSTALL_DIR}/claude-print"

# Install mock_claude (unless SKIP_MOCK_CLAUDE=1). The asset is optional:
# releases that predate it carry no checksum entry, which stays a skip — but
# an entry that cannot be downloaded, or a downloaded mismatch, is fatal.
if [ "${SKIP_MOCK_CLAUDE:-0}" != "1" ]; then
  if [ -z "$(checksum_entry_for "${MOCK_ASSET}" "${TMP_CHECKSUMS}")" ]; then
    echo "Note: ${MOCK_ASSET} not found in this release — skipping mock_claude"
  else
    MOCK_URL="${RELEASE_URL}/${MOCK_ASSET}"
    echo "Downloading ${MOCK_ASSET}..."
    if ! curl -fsSL "${MOCK_URL}" -o "${TMP_MOCK}"; then
      echo "Error: ${MOCK_ASSET} is listed in ${CHECKSUMS_ASSET} but could not be downloaded — refusing to install" >&2
      exit 1
    fi
    verify_artifact "${TMP_MOCK}" "${MOCK_ASSET}" "${TMP_CHECKSUMS}"
    install -m 755 "${TMP_MOCK}" "${INSTALL_DIR}/mock_claude"
    echo "Installed ${INSTALL_DIR}/mock_claude"
  fi
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
