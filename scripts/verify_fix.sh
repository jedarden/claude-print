#!/bin/bash
# Verification script for the startup hang fix.
# Demonstrates that --setting-sources= prevents the hang.

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Find claude binary
CLAUDE_BIN=$(which claude) || {
    log_error "claude binary not found in PATH"
    exit 1
}

log_info "Using claude binary: $CLAUDE_BIN"

# Create temp directory with settings.json containing only a Stop hook
TEMP_DIR=$(mktemp -d)
SETTINGS_FILE="$TEMP_DIR/settings.json"
HOOK_FILE="$TEMP_DIR/hook.sh"
FIFO_FILE="$TEMP_DIR/stop.fifo"

log_info "Test directory: $TEMP_DIR"

# Create settings.json with Stop hook only (like claude-print does)
cat > "$SETTINGS_FILE" << 'EOF'
{
  "hooks": {
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "/bin/echo 'Stop hook fired'",
            "timeout": 10
          }
        ]
      }
    ]
  }
}
EOF

# Create the hook script
cat > "$HOOK_FILE" << 'EOF'
#!/bin/sh
echo "Stop hook fired" >&2
EOF
chmod +x "$HOOK_FILE"

# Create FIFO
mkfifo "$FIFO_FILE" 2>/dev/null || true

cleanup() {
    log_info "Cleaning up test directory"
    rm -rf "$TEMP_DIR"
}
trap cleanup EXIT

# Test 1: WITHOUT --setting-sources= (should hang)
log_info "Test 1: WITHOUT --setting-sources= (expected to hang)"
cd /tmp
timeout 5s "$CLAUDE_BIN" --dangerously-skip-permissions --settings="$SETTINGS_FILE" -p "What is 2+2?" 2>&1 | head -5 || {
    EXIT_CODE=$?
    if [ $EXIT_CODE -eq 124 ]; then
        log_error "Test 1 TIMED OUT (as expected - this is the bug)"
    else
        log_error "Test 1 failed with exit code $EXIT_CODE"
    fi
}

echo ""
log_info "Test 2: WITH --setting-sources= (should work)"
timeout 10s "$CLAUDE_BIN" --dangerously-skip-permissions --settings="$SETTINGS_FILE" --setting-sources= -p "What is 2+2?" 2>&1 || {
    EXIT_CODE=$?
    if [ $EXIT_CODE -eq 124 ]; then
        log_error "Test 2 TIMED OUT (fix didn't work)"
    else
        log_error "Test 2 failed with exit code $EXIT_CODE"
    fi
}

log_info "Verification complete"
