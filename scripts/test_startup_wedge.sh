#!/bin/bash
# Minimal repro for child claude startup hang investigation.
# Tests different trigger scenarios:
# 1. Fresh untrusted directory
# 2. Trusted directory
# 3. With/without --dangerously-skip-permissions
# 4. Input file vs inline prompt
# 5. With/without global settings inheritance

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
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

# Create test directories
TEST_BASE=$(mktemp -d)
log_info "Test base directory: $TEST_BASE"

TRUSTED_DIR="$TEST_BASE/trusted"
UNTRUSTED_DIR="$TEST_BASE/untrusted"
mkdir -p "$TRUSTED_DIR" "$UNTRUSTED_DIR"

# Create a simple test prompt
TEST_PROMPT="What is 2+2?"
TEST_PROMPT_FILE="$TEST_BASE/prompt.txt"
echo "$TEST_PROMPT" > "$TEST_PROMPT_FILE"

cleanup() {
    log_info "Cleaning up test directories"
    rm -rf "$TEST_BASE"
}
trap cleanup EXIT

# Test 1: Trusted directory with inline prompt
test_trusted_inline() {
    log_info "Test 1: Trusted directory with inline prompt"
    cd "$TRUSTED_DIR"

    # First, trust the directory by running claude once interactively
    # (This is a manual step - we'll skip and assume it's untrusted for now)

    timeout 10s "$CLAUDE_BIN" --dangerously-skip-permissions -p "$TEST_PROMPT" 2>&1 | head -20 || {
        log_error "Test 1 failed or timed out"
        return 1
    }
}

# Test 2: Untrusted directory with inline prompt
test_untrusted_inline() {
    log_info "Test 2: Untrusted directory with inline prompt"
    cd "$UNTRUSTED_DIR"

    timeout 10s "$CLAUDE_BIN" -p "$TEST_PROMPT" 2>&1 | head -20 || {
        log_error "Test 2 failed or timed out"
        return 1
    }
}

# Test 3: Untrusted directory with --dangerously-skip-permissions
test_untrusted_skip_perms() {
    log_info "Test 3: Untrusted directory with --dangerously-skip-permissions"
    cd "$UNTRUSTED_DIR"

    timeout 10s "$CLAUDE_BIN" --dangerously-skip-permissions -p "$TEST_PROMPT" 2>&1 | head -20 || {
        log_error "Test 3 failed or timed out"
        return 1
    }
}

# Test 4: Input file in untrusted directory
test_untrusted_input_file() {
    log_info "Test 4: Input file in untrusted directory"
    cd "$UNTRUSTED_DIR"

    timeout 10s "$CLAUDE_BIN" --dangerously-skip-permissions -f "$TEST_PROMPT_FILE" 2>&1 | head -20 || {
        log_error "Test 4 failed or timed out"
        return 1
    }
}

# Test 5: With custom settings (no Stop hook, simulating inheritance)
test_custom_settings() {
    log_info "Test 5: With custom settings file"
    cd "$UNTRUSTED_DIR"

    SETTINGS_DIR="$TEST_BASE/settings_only"
    mkdir -p "$SETTINGS_DIR"
    SETTINGS_FILE="$SETTINGS_DIR/settings.json"

    # Create minimal settings.json (empty, simulating no Stop hook)
    echo '{}' > "$SETTINGS_FILE"

    timeout 10s "$CLAUDE_BIN" --dangerously-skip-permissions --settings="$SETTINGS_FILE" -p "$TEST_PROMPT" 2>&1 | head -20 || {
        log_error "Test 5 failed or timed out"
        return 1
    }
}

# Test 6: Check global settings inheritance
test_global_settings_inheritance() {
    log_info "Test 6: Check global settings inheritance"

    # Check if there are MCP servers configured globally
    GLOBAL_SETTINGS="$HOME/.claude/settings.json"
    if [ -f "$GLOBAL_SETTINGS" ]; then
        log_info "Found global settings at $GLOBAL_SETTINGS"
        log_info "Checking for MCP servers..."
        if grep -q "mcpServers" "$GLOBAL_SETTINGS" 2>/dev/null; then
            log_warn "MCP servers configured in global settings:"
            grep -A 10 "mcpServers" "$GLOBAL_SETTINGS" 2>/dev/null || true
        else
            log_info "No MCP servers found in global settings"
        fi

        # Check for hooks
        if grep -q "hooks" "$GLOBAL_SETTINGS" 2>/dev/null; then
            log_warn "Hooks configured in global settings:"
            grep -A 10 "hooks" "$GLOBAL_SETTINGS" 2>/dev/null || true
        else
            log_info "No hooks found in global settings"
        fi
    else
        log_info "No global settings file found"
    fi
}

# Test 7: Simulate claude-print's temp settings with Stop hook
test_stop_hook_only() {
    log_info "Test 7: Simulate claude-print's temp settings (Stop hook only)"
    cd "$UNTRUSTED_DIR"

    SETTINGS_DIR="$TEST_BASE/settings_stop_hook"
    mkdir -p "$SETTINGS_DIR"
    SETTINGS_FILE="$SETTINGS_DIR/settings.json"
    HOOK_FILE="$SETTINGS_DIR/hook.sh"
    FIFO_FILE="$SETTINGS_DIR/stop.fifo"

    # Create settings.json with Stop hook only (like claude-print does)
    cat > "$SETTINGS_FILE" << 'EOF'
{
  "hooks": {
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "/bin/echo",
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
echo "Stop hook fired"
EOF
    chmod +x "$HOOK_FILE"

    # Create FIFO
    mkfifo "$FIFO_FILE" 2>/dev/null || true

    timeout 10s "$CLAUDE_BIN" --dangerously-skip-permissions --settings="$SETTINGS_FILE" -p "$TEST_PROMPT" 2>&1 | head -20 || {
        log_error "Test 7 failed or timed out"
        return 1
    }
}

# Test 8: Check for folder trust prompt
test_folder_trust_prompt() {
    log_info "Test 8: Check for folder trust prompt behavior"
    cd "$UNTRUSTED_DIR"

    # Run with very short timeout to catch the prompt
    log_info "Running with 3s timeout to catch trust prompt..."
    timeout 3s "$CLAUDE_BIN" -p "$TEST_PROMPT" 2>&1 || {
        EXIT_CODE=$?
        if [ $EXIT_CODE -eq 124 ]; then
            log_warn "Test timed out - likely waiting for trust prompt input"
        else
            log_error "Test failed with exit code $EXIT_CODE"
        fi
    }
}

# Run all tests
main() {
    log_info "Starting startup wedge investigation..."

    test_global_settings_inheritance

    echo ""
    log_info "Running test scenarios..."
    echo ""

    # Run tests in order
    test_untrusted_skip_perms
    test_untrusted_inline
    test_trusted_inline
    test_untrusted_input_file
    test_custom_settings
    test_stop_hook_only
    test_folder_trust_prompt

    log_info "Investigation complete"
}

main "$@"
