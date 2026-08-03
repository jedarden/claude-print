# Bead bf-2v7m: dangerously-skip-permissions flag verification

## Status
✓ **Already fixed** - No code changes needed

## What was verified

### 1. Fix already applied (commit 8bcc030)
The unconditional push of `--dangerously-skip-permissions` was removed from `src/session.rs`:
- Line 357 no longer contains `args.push(CString::new("--dangerously-skip-permissions").unwrap());`
- Capacity reduced from `claude_args.len() + 4` to `claude_args.len() + 3`

### 2. Current state (as of this verification)
- `src/session.rs`: No occurrences of "dangerously-skip-permissions" ✓
- `src/main.rs:223`: Conditional push only (gated on `cli.dangerously_skip_permissions`) ✓

### 3. Tests already in place (tests/binary_e2e.rs)
Two comprehensive E2E tests verify the behavior:

**Test 1: `dangerously_skip_permissions_flag_absent_when_not_passed`**
- Runs claude-print WITHOUT the flag
- Captures child argv via MOCK_RECORD_ARGS
- Asserts `--dangerously-skip-permissions` does NOT appear

**Test 2: `dangerously_skip_permissions_flag_appears_exactly_once_when_passed`**
- Runs claude-print WITH the flag
- Captures child argv via MOCK_RECORD_ARGS
- Asserts the flag appears EXACTLY ONCE (not duplicated)

### 4. All acceptance criteria met
- ✓ Tests construct/observe child argv via mock-claude's MOCK_RECORD_ARGS seam
- ✓ Tests assert flag absent when not passed
- ✓ Tests assert flag appears exactly once when passed
- ✓ `grep -n 'dangerously-skip-permissions' src/session.rs` returns nothing
- ✓ cargo fmt clean
- ✓ cargo clippy clean
- ✓ Tests pass (2 passed in 2.12s)

## Original issue
The bug described in bf-2v7m:
1. The CLI flag was inert (permissions skipped regardless of whether flag was passed)
2. When flag WAS passed, it appeared twice in child argv

Both issues were resolved by removing the unconditional push from session.rs.
