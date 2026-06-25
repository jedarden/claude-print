#!/bin/bash
# Test to specifically check if SessionStart hook causes the hang

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

CLAUDE_BIN=$(which claude)
log_info "Using claude binary: $CLAUDE_BIN"

# Create a SessionStart hook that hangs
TEMP_DIR=$(mktemp -d)
HOOK_FILE="$TEMP_DIR/sessionstart-hang.sh"

cat > "$HOOK_FILE" << 'EOF'
#!/bin/bash
# This hook hangs forever (simulating a slow/hanging hook)
sleep 1000
EOF
chmod +x "$HOOK_FILE"

log_info "Created hanging SessionStart hook: $HOOK_FILE"

# Temp settings to test
SETTINGS_DIR="$TEMP_DIR/settings"
mkdir -p "$SETTINGS_DIR"
SETTINGS_FILE="$SETTINGS_DIR/settings.json"

cleanup() {
    log_info "Cleaning up"
    rm -rf "$TEMP_DIR"
}
trap cleanup EXIT

# Test 1: Temp settings with Stop hook only
log_info "Test 1: Temp settings with Stop hook only (should inherit global SessionStart)"
cat > "$SETTINGS_FILE" << 'EOF'
{
  "hooks": {
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "/bin/echo 'Stop hook'",
            "timeout": 10
          }
        ]
      }
    ]
  }
}
EOF

cd /tmp
timeout 5s "$CLAUDE_BIN" --dangerously-skip-permissions --settings="$SETTINGS_FILE" -p "test" 2>&1 | head -3 || {
    EXIT_CODE=$?
    if [ $EXIT_CODE -eq 124 ]; then
        log_warn "Test 1 TIMED OUT - suggests global SessionStart hooks fire"
    else
        log_warn "Test 1 exited with code $EXIT_CODE"
    fi
}

# Test 2: Same but with --setting-sources=
log_info "Test 2: Same with --setting-sources= (should NOT inherit)"
timeout 5s "$CLAUDE_BIN" --dangerously-skip-permissions --settings="$SETTINGS_FILE" --setting-sources= -p "test" 2>&1 | head -3 || {
    EXIT_CODE=$?
    if [ $EXIT_CODE -eq 124 ]; then
        log_warn "Test 2 TIMED OUT - unexpected"
    else
        log_warn "Test 2 exited with code $EXIT_CODE"
    fi
}

log_info "Check your global SessionStart hooks:"
cat "$HOME/.claude/settings.json" 2>/dev/null | grep -A 20 "SessionStart" || echo "No SessionStart hooks found"
