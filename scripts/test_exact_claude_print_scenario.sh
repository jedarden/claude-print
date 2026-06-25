#!/bin/bash
# Exact simulation of claude-print's scenario
# Creates temp dir with settings.json (Stop hook only), hook.sh, and FIFO
# Then runs child claude with --dangerously-skip-permissions and --settings=<temp>

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
    echo "$(date '+%Y-%m-%d %H:%M:%S') [INFO] $1" >&2
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
    echo "$(date '+%Y-%m-%d %H:%M:%S') [ERROR] $1" >&2
}

CLAUDE_BIN=$(which claude)
log_info "Using claude binary: $CLAUDE_BIN"

# Create temp dir exactly like claude-print does
TEMP_DIR=$(mktemp -d)
SETTINGS_FILE="$TEMP_DIR/settings.json"
HOOK_FILE="$TEMP_DIR/hook.sh"
FIFO_FILE="$TEMP_DIR/stop.fifo"

log_info "Created temp dir: $TEMP_DIR"

# Create hook.sh that writes to FIFO (exactly like claude-print)
cat > "$HOOK_FILE" << EOF
#!/bin/sh
cat > '$FIFO_FILE' 2>/dev/null || true
EOF
chmod +x "$HOOK_FILE"

# Create settings.json with Stop hook only (exactly like claude-print)
cat > "$SETTINGS_FILE" << EOF
{
  "hooks": {
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "$HOOK_FILE",
            "timeout": 10
          }
        ]
      }
    ]
  }
}
EOF

# Create FIFO
mkfifo "$FIFO_FILE" 2>/dev/null || true

log_info "Created artifacts:"
log_info "  - settings.json: $SETTINGS_FILE"
log_info "  - hook.sh: $HOOK_FILE"
log_info "  - stop.fifo: $FIFO_FILE"

cleanup() {
    log_info "Cleaning up temp dir: $TEMP_DIR"
    rm -rf "$TEMP_DIR"
}
trap cleanup EXIT

# Test in untrusted directory
cd /tmp

log_info "Test 1: WITH --settings (should hang based on original evidence)"
log_info "Running: $CLAUDE_BIN --dangerously-skip-permissions --settings=$SETTINGS_FILE -p 'test'"

# Run with timeout and capture ALL output
timeout 10s "$CLAUDE_BIN" --dangerously-skip-permissions --settings="$SETTINGS_FILE" -p "test" 2>&1 | tee /tmp/test1_output.txt || {
    EXIT_CODE=$?
    log_error "Test 1 exited with code: $EXIT_CODE"
    if [ $EXIT_CODE -eq 124 ]; then
        log_error "TIMEOUT - child produced no output within 10s"
    fi
}

# Check if we got any output
if [ -s /tmp/test1_output.txt ]; then
    log_info "Test 1 produced output:"
    head -3 /tmp/test1_output.txt
else
    log_error "Test 1 produced NO output (this is the bug!)"
fi

echo ""
log_info "Test 2: WITH --settings AND --setting-sources= (should work)"
log_info "Running: $CLAUDE_BIN --dangerously-skip-permissions --settings=$SETTINGS_FILE --setting-sources= -p 'test'"

timeout 10s "$CLAUDE_BIN" --dangerously-skip-permissions --settings="$SETTINGS_FILE" --setting-sources= -p "test" 2>&1 | tee /tmp/test2_output.txt || {
    EXIT_CODE=$?
    log_error "Test 2 exited with code: $EXIT_CODE"
    if [ $EXIT_CODE -eq 124 ]; then
        log_error "TIMEOUT - even with --setting-sources= it hung"
    fi
}

# Check if we got any output
if [ -s /tmp/test2_output.txt ]; then
    log_info "Test 2 produced output:"
    head -3 /tmp/test2_output.txt
else
    log_error "Test 2 produced NO output"
fi

# Summary
echo ""
log_info "=== SUMMARY ==="
if [ -s /tmp/test1_output.txt ]; then
    log_info "Test 1 (with --settings): WORKED (produced output)"
else
    log_error "Test 1 (with --settings): FAILED (no output - child hung)"
fi

if [ -s /tmp/test2_output.txt ]; then
    log_info "Test 2 (with --settings --setting-sources=): WORKED (produced output)"
else
    log_error "Test 2 (with --settings --setting-sources=): FAILED (no output)"
fi

log_info "Test complete"
