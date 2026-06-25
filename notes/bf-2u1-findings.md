# Startup Wedge Investigation Findings

## Root Cause Identified

**The child claude hangs at startup when global settings have hooks that are NOT overridden by the temp settings.json**

## Evidence

### Global Settings State
The global settings at `~/.claude/settings.json` contain many hooks:
- SessionStart (2 hooks)
- SessionEnd (2 hooks)  
- Stop (4 hooks)
- UserPromptSubmit (3 hooks)
- PreToolUse
- PermissionRequest (2 hooks)
- Notification

### Test 7: The Smoking Gun
When running with a temp settings.json that contains **only a Stop hook** (simulating claude-print's behavior):
- Command: `claude --dangerously-skip-permissions --settings=<temp_dir> -p "test"`
- Result: **TIMED OUT** (no output produced)
- This exactly matches the reported wedge: child never produces output, Stop hook never fires

### Why This Happens
When `--settings=<path>` is passed, Claude Code:
1. Loads the settings from the specified path
2. **Merges** with global settings (not a complete override)
3. Some global hooks (SessionStart, etc.) are still active
4. The child may hang waiting for these hooks to complete or for user input

### Test 8: Confirmation
Without `--dangerously-skip-permissions`, child prompts for folder trust (expected).
With `--dangerously-skip-permissions` but no `--settings`, child works fine.
With `--dangerously-skip-permissions` AND `--settings=<temp_path>`, child hangs.

## The Wedge Mechanism

1. claude-print creates a temp settings.json with ONLY a Stop hook
2. It passes `--settings=<temp_path>` to child claude
3. Child loads temp settings and merges with global settings
4. Global SessionStart hooks (or other hooks) fire
5. One of these hooks hangs or requires interaction
6. Child never produces output
7. claude-print's first-output timeout fires (90s default)
8. Child is SIGTERM'd
9. Stop hook never fires (because child never reached a state where it would fire)

## Minimal Rep

```bash
#!/bin/bash
# Create temp directory with settings.json containing only a Stop hook
TEMP_DIR=$(mktemp -d)
SETTINGS_FILE="$TEMP_DIR/settings.json"
HOOK_FILE="$TEMP_DIR/hook.sh"
FIFO_FILE="$TEMP_DIR/stop.fifo"

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

# Run in untrusted directory with temp settings
cd /tmp
timeout 10s claude --dangerously-skip-permissions --settings="$SETTINGS_FILE" -p "What is 2+2?"
```

Expected result: **Hangs/timeout with no output**

## Solution

Pass `--setting-sources=` (empty string) to child claude to prevent global settings inheritance. This is already supported by the codebase (see `main.rs` line 109 for the `--no-inherit-hooks` flag).

Current code has:
```rust
if cli.no_inherit_hooks {
    claude_args.push("--setting-sources=".into());
}
```

The fix is to **ALWAYS pass `--setting-sources=`** when launching with a custom settings.json, not just when `--no-inherit-hooks` is set.
