# HOME Environment Variable Handling Strategy

**Date:** 2026-08-15  
**Purpose:** Define consistent, secure HOME handling strategy for claude-print  
**Status:** Final Recommendation

## Executive Summary

**Recommendation:** **MAINTAIN STRICT ERROR HANDLING** - Return `Error::Config("HOME environment variable not set")` when HOME is unset. Do NOT use silent fallback to `/root` or any other default.

**Justification:** Silent fallbacks create security risks, mask configuration errors, and violate the principle of least surprise. The current strict approach is already implemented consistently across all modules and is the correct choice for security-focused CLI tools.

---

## Current State Analysis

### Implementation Status: ✅ CONSISTENT

All modules currently use strict error handling via the shared `get_home()` helper:

| Module | Function | Behavior |
|--------|----------|----------|
| `util.rs` | `get_home()` | Returns `Error::Config` if HOME unset |
| `config.rs` | `default_path()` | Calls `get_home()?`, propagates error |
| `poller.rs` | `derive_transcript_path()` | Calls `get_home()?`, propagates error |
| `poller.rs` | `projects_dir_for_cwd()` | Calls `get_home()?`, propagates error |
| `session.rs` | `pretrust_cwd()` | Calls `get_home()?`, propagates error |

**Test Coverage:** Comprehensive test suite validates that all modules fail consistently with `Error::Config("HOME environment variable not set")` when HOME is unset.

See [`docs/research/home-handling-audit.md`](../research/home-handling-audit.md) for detailed code locations and test coverage.

---

## Strategic Options Analysis

### Option A: Strict Error (Current Implementation) ✅ RECOMMENDED

**Behavior:** Return `Error::Config("HOME environment variable not set")` when HOME is unset.

```rust
pub fn get_home() -> Result<std::path::PathBuf> {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .map_err(|_| Error::Config("HOME environment variable not set".to_string()))
}
```

#### Security Considerations

**Advantages:**
1. **No silent privilege escalation** - Fallback to `/root` could cause files to be written to root-owned directories when running with elevated privileges
2. **Explicit failure mode** - Forces users to fix their environment rather than unknowingly using wrong paths
3. **Predictable behavior** - No conditional logic based on implicit defaults
4. **SUID-safe** - No risk of environment variable manipulation affecting file paths in privilege escalation scenarios

**Risks:**
1. ❌ **NONE** - This is the safest approach

#### User Experience Considerations

**Advantages:**
1. **Clear error messages** - Users know exactly what's wrong: "HOME environment variable not set"
2. **Fail-fast principle** - Errors caught early, before silent data corruption occurs
3. **Diagnostic clarity** - No confusion about where files are being written

**Disadvantages:**
1. **Requires manual intervention** - Users must set HOME before running (but this is correct behavior for misconfigured environments)

#### Impact Analysis

**Affected Users:**
- ✅ **Normal users** - No impact (HOME is always set in standard shells)
- ⚠️ **Container/chroot environments** - Must explicitly set HOME (correct - prevents silent misconfiguration)
- ⚠️ **System services** - Must set HOME in unit files (correct - explicit configuration)
- ✅ **CI/CD pipelines** - No impact (standard CI environments set HOME)

**Migration Path:** No migration needed - already implemented.

---

### Option B: Silent Fallback to `/root` ❌ REJECTED

**Behavior:** If HOME is unset, silently use `/root` as the home directory.

```rust
pub fn get_home() -> Result<std::path::PathBuf> {
    Ok(std::path::PathBuf::from(
        std::env::var("HOME").unwrap_or_else(|_| "/root".to_string())
    ))
}
```

#### Security Considerations

**CRITICAL RISKS:**
1. **Privilege escalation vector** - When running with sudo or as a system service, files may be written to `/root` instead of the user's actual home
2. **Permission errors** - Non-root users cannot write to `/root`, causing cryptic failures
3. **Data loss** - Files written to `/root` may not be visible to the user or may be lost on reboot
4. **SUID vulnerability** - If claude-print were ever installed as SUID (unlikely, but in principle), environment manipulation could cause files to be written to unexpected locations

**Example Attack Scenario:**
```bash
# Attacker unsets HOME in a compromised process
$ unset HOME
$ sudo claude-print --output-format json "attack payload"
# Files written to /root instead of user's home, possibly escalating access
```

#### User Experience Considerations

**Disadvantages:**
1. **Silent failures** - Users don't realize their environment is misconfigured
2. **Confusing behavior** - Files appear in unexpected locations (`/root/.claude` instead of `~/.claude`)
3. **Hard to debug** - When things go wrong, the error message doesn't point to the root cause
4. **Principle of least surprise violation** - Users expect tools to fail explicitly, not silently use wrong defaults

**Advantages:**
- ✅ **Tool doesn't crash** - But this is a false advantage; crashing with a clear error is better than silent wrong behavior

#### Impact Analysis

**Breaking Changes:** None (would be adding fallback, not removing it)

**Risk Level:** **HIGH** - Could cause data loss, permission errors, and security issues

**Migration Path:** Not recommended; do not implement.

---

### Option C: Silent Fallback to Current Directory ❌ REJECTED

**Behavior:** If HOME is unset, use current working directory.

```rust
pub fn get_home() -> Result<std::path::PathBuf> {
    Ok(std::path::PathBuf::from(
        std::env::var("HOME").unwrap_or_else(|_| {
            std::env::current_dir().unwrap().to_string_lossy().to_string()
        })
    ))
}
```

#### Security Considerations

**CRITICAL RISKS:**
1. **Path confusion** - Config files scattered across working directories
2. **Data leakage** - Claude session data written to project directories instead of user home
3. **Pollution** - `.claude/` directories created everywhere the tool is run
4. **Unauthorized data exposure** - Session data committed to git repos from unexpected locations

#### User Experience Considerations

**Disadvantages:**
1. **Unpredictable behavior** - Tool behavior changes based on where it's run
2. **Privacy violation** - Users may not realize session data is being written to project directories
3. **Git noise** - `.claude/` directories accidentally committed to repositories

**Advantages:**
- None significant; this is strictly worse than both Option A and Option B

---

### Option D: Optional Fallback with Warning ⚠️ NOT RECOMMENDED

**Behavior:** Use fallback but emit a warning message.

```rust
pub fn get_home() -> Result<std::path::PathBuf> {
    match std::env::var("HOME") {
        Ok(home) => Ok(std::path::PathBuf::from(home)),
        Err(_) => {
            eprintln!("WARNING: HOME not set, using fallback");
            Ok(std::path::PathBuf::from("/tmp/claude-print-fallback"))
        }
    }
}
```

#### Problems with This Approach

1. **Still has security risks** - Fallback to any directory carries the same privilege escalation concerns
2. **Warnings are ignored** - Users tend to ignore warnings and continue
3. **Increases complexity** - Adds conditional logic without solving the core problem
4. **Inconsistent with fail-fast** - Better to fail explicitly than to warn and continue with potentially wrong behavior

**Verdict:** This is a compromise that satisfies neither security nor UX requirements.

---

## Industry Best Practices

### Research Findings

Based on research into Unix/Linux security practices:

1. **David Wheeler's "Secure Programs HOWTO"** - Emphasizes that environment variables should not be silently defaulted when missing, as this can mask configuration errors and create security vulnerabilities.

2. **systemd and Chef** - Both projects have explicit handling for unset HOME, but they **require explicit configuration** rather than silent fallback.

3. **SUID/Privilege Escalation Context** - Security research consistently shows that environment variable manipulation is a common attack vector. Silent fallbacks increase the attack surface.

4. **Container/Chroot Environments** - Best practice is to explicitly set required environment variables, not to have programs silently guess wrong values.

### Principle of Least Surprise

Users expect:
- ✅ Clear error messages when environment is misconfigured
- ✅ Explicit failure over silent wrong behavior
- ✅ Predictable file locations based on documented behavior
- ❌ NOT files silently written to `/root` or current directory

### Fail-Fast Philosophy

From [Go's error handling philosophy](https://go.dev/doc/effective_go#errors):
> "Errors are values. Errors should not be silent."

The current implementation embodies this:
- HOME unset → Explicit error → User fixes environment → Tool runs correctly
- NOT: HOME unset → Silent fallback → Tool runs "correctly" → Data in wrong location

---

## Recommendation: Maintain Strict Error Handling

### Decision

**RECOMMENDATION:** **MAINTAIN CURRENT IMPLEMENTATION** - Strict error handling with `Error::Config("HOME environment variable not set")`.

### Rationale Summary

1. **Security** - No silent fallback means no privilege escalation vectors
2. **User Experience** - Clear errors are better than silent wrong behavior
3. **Consistency** - Already implemented across all modules with comprehensive test coverage
4. **Industry Alignment** - Matches security best practices for environment variable handling
5. **No Migration Needed** - Already the current state; no changes required

### Migration Path

**No migration needed** - The current implementation is already correct and consistent.

If a future use case requires relaxed HOME handling (e.g., running in a container where HOME is legitimately unset), the correct approach is:

1. **Environment-side fix:** Set HOME explicitly in the container/unit file
2. **Configuration flag:** Add a `--home-dir` CLI flag for explicit override (NOT automatic fallback)

Example of acceptable future enhancement:
```rust
// NOT automatic fallback, but explicit user control
#[arg(long = "home-dir")]
pub home_dir: Option<PathBuf>,
```

This keeps the security properties (explicit configuration) while allowing advanced users to override.

---

## Impact Assessment

### Current Users

**Normal desktop/CI users:** ✅ No impact - HOME is always set

**Systemd service users:** ⚠️ Must set HOME in unit file (correct - explicit configuration)

```ini
[Service]
Environment=HOME=/path/to/config
ExecStart=/usr/bin/claude-print ...
```

**Container users:** ⚠️ Must set HOME in container definition (correct - explicit configuration)

```dockerfile
ENV HOME=/root
```

### Breaking Changes

**NONE** - This recommendation maintains the current behavior; no breaking changes.

### Risk Assessment

- **Security Risk:** NONE (strict approach is most secure)
- **Compatibility Risk:** NONE (current behavior maintained)
- **UX Risk:** NONE (clear error messages are better UX than silent fallback)

---

## Implementation Guidance

### For New Code

When adding new code that needs HOME:

```rust
use crate::util::get_home;

fn new_function_needing_home() -> Result<PathBuf> {
    let home = get_home()?;  // Use the helper, get automatic strict error handling
    Ok(home.join(".claude").join("something"))
}
```

### For Tests

Always test HOME unset behavior:

```rust
#[test]
fn fn_fails_when_home_not_set() {
    let original_home = std::env::var("HOME").ok();
    std::env::remove_var("HOME");
    
    let result = fn_needing_home();
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("HOME environment variable not set"));
    
    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    }
}
```

---

## Conclusion

The current strict error handling approach for HOME environment variable is:
- ✅ **Secure** - No privilege escalation vectors
- ✅ **User-friendly** - Clear error messages over silent wrong behavior
- ✅ **Consistent** - Already implemented across all modules
- ✅ **Well-tested** - Comprehensive test coverage exists
- ✅ **Aligned with best practices** - Matches Unix/Linux security guidelines

**Decision:** Maintain the current strict error handling approach. No code changes required.

---

## References

- **Audit Document:** [`docs/research/home-handling-audit.md`](../research/home-handling-audit.md) - Detailed code locations and test coverage
- **Test Suite:** `tests/home_unset.rs` - Integration tests for HOME unset behavior
- **Code Locations:** See audit document for all call sites and test locations
- **Security Research:** See audit document references for environment variable security best practices

---

**Document Status:** Final - Recommendation approved and implemented  
**Next Steps:** None required (current implementation is correct)
