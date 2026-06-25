#!/bin/bash
# Verification script for startup wedge fix (bf-2u1)
#
# This script verifies that the fix for the startup wedge issue works correctly.
# The wedge occurred when child claude inherited global settings that contained
# hooks (SessionStart, etc.) which could cause hangs at startup.
#
# Fix: Always pass --setting-sources= when launching with custom settings.json

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TEMP_DIR=$(mktemp -d)
SETTINGS_FILE="$TEMP_DIR/settings.json"
HOOK_FILE="$TEMP_DIR/hook.sh"
FIFO_FILE="$TEMP_DIR/stop.fifo"

cleanup() {
    rm -rf "$TEMP_DIR"
}
trap cleanup EXIT

echo "=== Startup Wedge Fix Verification ==="
echo "Temp dir: $TEMP_DIR"
echo

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

echo "✓ Created settings.json with Stop hook only"

# Create the hook script
cat > "$HOOK_FILE" << 'EOF'
#!/bin/sh
echo "Stop hook fired"
EOF
chmod +x "$HOOK_FILE"
echo "✓ Created hook.sh"

# Create FIFO
mkfifo "$FIFO_FILE" 2>/dev/null || true
echo "✓ Created stop.fifo"

cd "$TEMP_DIR"

echo
echo "Test 1: WITH fix (--setting-sources=)"
echo "Running: timeout 5s claude --dangerously-skip-permissions --settings='$SETTINGS_FILE' --setting-sources= -p 'What is 2+2?'"
echo

if timeout 5s claude --dangerously-skip-permissions --settings="$SETTINGS_FILE" --setting-sources= -p "What is 2+2?" > /dev/null 2>&1; then
    echo "✓ PASS: Child completed successfully with --setting-sources="
    RESULT1="PASS"
else
    echo "✗ FAIL: Child failed or timed out even with --setting-sources="
    RESULT1="FAIL"
fi

echo
echo "Test 2: WITHOUT fix (no --setting-sources=)"
echo "Running: timeout 5s claude --dangerously-skip-permissions --settings='$SETTINGS_FILE' -p 'What is 2+2?'"
echo

if timeout 5s claude --dangerously-skip-permissions --settings="$SETTINGS_FILE" -p "What is 2+2?" > /dev/null 2>&1; then
    echo "✓ PASS: Child completed successfully without --setting-sources= (no global hooks conflict)"
    RESULT2="PASS"
else
    echo "✗ EXPECTED FAIL: Child failed or timed out (this demonstrates the wedge)"
    RESULT2="FAIL"
fi

echo
echo "=== Summary ==="
echo "Test 1 (WITH fix):    $RESULT1"
echo "Test 2 (WITHOUT fix): $RESULT2"
echo

if [ "$RESULT1" = "PASS" ]; then
    echo "✓ Fix verified: --setting-sources= prevents startup wedge"
    exit 0
else
    echo "✗ Fix verification failed"
    exit 1
fi
