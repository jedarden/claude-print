# bf-2u1: Startup Wedge Investigation Report

## Executive Summary

**Root Cause:** Child claude hangs at startup when global settings containing hooks (SessionStart, SessionEnd, etc.) are inherited despite claude-print creating a temp settings.json with only a Stop hook.

**Solution:** Always pass `--setting-sources=` to child claude to prevent global settings inheritance.

**Status:** Fix implemented in src/session.rs (lines 127-129) but NOT yet committed.

---

## Problem Description

### Symptoms
- Per-invocation `.tmp/claude-print-<pid>/` directories contained:
  - `hook.sh` + `settings.json` + orphaned `stop.fifo`
  - claude-print blocked in `do_sys_poll` on FIFO fds
  - Child claude idle (never produced output, never reached Stop event)

### Why This Happens

1. **claude-print creates temp settings.json with ONLY a Stop hook:**
   ```json
   {
     "hooks": {
       "Stop": [{"hooks": [{"type": "command", "command": "<hook.sh>", "timeout": 10}]}]
     }
   }
   ```

2. **Passes `--settings=<temp_path>` to child claude**

3. **Claude Code merges temp settings with GLOBAL settings** (not a complete override)

4. **Global hooks still fire:**
   - SessionStart hooks (2 hooks in global settings)
   - SessionEnd hooks (2 hooks)
   - UserPromptSubmit hooks (3 hooks)
   - PreToolUse, PermissionRequest, Notification hooks

5. **One of these global hooks hangs or requires interaction**

6. **Child never produces output → first-output timeout fires (90s) → SIGTERM**

7. **Stop hook never fires** (child never reached state where it would fire)

---

## Evidence

### Test Results (from notes/bf-2u1-findings.md)

**Test 7: Smoking Gun**
```bash
# Create temp settings.json with only Stop hook
TEMP_DIR=$(mktemp -d)
cat > "$TEMP_DIR/settings.json" << 'EOF'
{
  "hooks": {
    "Stop": [{"hooks": [{"type": "command", "command": "/bin/echo", "timeout": 10}]}]
  }
}
EOF

# Run with temp settings
timeout 10s claude --dangerously-skip-permissions --settings="$TEMP_DIR/settings.json" -p "What is 2+2?"
# Result: TIMED OUT (no output produced)
```

**Test 8: Confirmation**
- Without `--dangerously-skip-permissions`: prompts for folder trust (expected)
- With `--dangerously-skip-permissions` but NO `--settings`: works fine
- With `--dangerously-skip-permissions` AND `--settings=<temp_path>`: **HANGS**

### Global Settings State

Global `~/.claude/settings.json` contains multiple hooks:
- SessionStart: 2 hooks
- SessionEnd: 2 hooks
- Stop: 4 hooks
- UserPromptSubmit: 3 hooks
- PreToolUse: 1 hook
- PermissionRequest: 2 hooks
- Notification: 1 hook

Any of these can hang when inherited.

---

## The Fix

### Implementation (src/session.rs, lines 122-129)

```rust
// Build child argv
let mut args: Vec<CString> = Vec::with_capacity(claude_args.len() + 3);
args.push(CString::new("--dangerously-skip-permissions").unwrap());
args.push(
    CString::new(format!("--settings={}", installer.settings_path.to_string_lossy()))
        .map_err(|e| Error::Internal(anyhow::anyhow!("settings path invalid: {e}")))?,
);
// Prevent global settings inheritance - the temp settings.json contains only the Stop hook
// and inheriting global hooks (SessionStart, etc.) can cause the child to hang at startup.
args.push(CString::new("--setting-sources=").unwrap());
```

### Why This Works

The `--setting-sources=` flag (empty string) tells Claude Code to **ONLY load settings from the explicitly specified path** and NOT merge with global settings from:
- `~/.claude/settings.json`
- `.claude/settings.json`
- Environment variables
- Default settings

With this flag:
- Child loads ONLY the temp settings.json
- ONLY the Stop hook is active
- No global hooks can cause hangs
- Child produces output normally
- Stop hook fires as expected

---

## Verification

### Minimal Rep (Pre-Fix)

```bash
#!/bin/bash
set -euo pipefail

# Create temp directory with settings.json containing only a Stop hook
TEMP_DIR=$(mktemp -d)
SETTINGS_FILE="$TEMP_DIR/settings.json"

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

# Run in untrusted directory with temp settings (SIMULATES OLD BEHAVIOR)
cd /tmp
echo "Testing WITHOUT fix (should hang)..."
timeout 5s claude --dangerously-skip-permissions --settings="$SETTINGS_FILE" -p "What is 2+2?" || echo "TIMED OUT (as expected)"
```

**Expected result:** Times out with no output

### Test Post-Fix

```bash
# Test WITH fix (--setting-sources=)
echo "Testing WITH fix (should work)..."
timeout 5s claude --dangerously-skip-permissions --settings="$SETTINGS_FILE" --setting-sources= -p "What is 2+2?" || echo "Command completed"
```

**Expected result:** Output produced, Stop hook fires

### Integration Test

After fix is committed, claude-print should work correctly:

```bash
echo "What is 2+2?" | ./target/release/claude-print
# Expected: normal output, Stop hook fires, clean exit
```

---

## Impact

### Before Fix
- claude-print would hang indefinitely when global settings had hooks
- Orphaned temp directories with stop.fifo files
- Users forced to SIGKILL the process
- No way to use claude-print with global hooks configured

### After Fix
- claude-print works regardless of global settings
- Clean temp directory cleanup
- Reliable Stop hook behavior
- No hangs or orphaned FIFOs

---

## Related Code

### Files Modified
1. **src/session.rs** (lines 122-129): Added `--setting-sources=` flag
2. **src/main.rs** (lines 108-110): Already had `--no-inherit-hooks` option

### Design Decision
The fix is in `session.rs` (internal) rather than `main.rs` (CLI flag) because:
- This is NOT a user-facing option
- It's an implementation detail required for correct operation
- The temp settings.json is created internally by the HookInstaller
- Users should NOT need to know about this workaround

---

## Commit Status

**Current state:** Fix implemented but NOT committed
- Modified file: `src/session.rs`
- Lines 127-129 added with explanatory comment
- Git shows: `modified:   src/session.rs`

**Next steps:**
1. Verify fix compiles: `cargo build --release`
2. Test with global hooks present
3. Commit with message explaining the fix
4. Push to origin
5. Close bead bf-2u1

---

## References

- Original findings: `notes/bf-2u1-findings.md`
- Related beads:
  - `bf-2w7`: temp dir and FIFO cleanup
  - `bf-3ag`: session implementation
  - `bf-4aw`: main.rs execution path

- Claude Code flags documentation:
  - `--setting-sources`: Controls settings file inheritance
  - `--settings`: Explicit settings file path
  - `--dangerously-skip-permissions`: Bypass permission prompts
