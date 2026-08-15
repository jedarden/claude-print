# HOME Environment Variable Handling Audit

**Date:** 2026-08-15  
**Purpose:** Document current HOME handling across all modules before making changes  
**Scope:** `src/config.rs`, `src/poller.rs`, `src/session.rs`

## Executive Summary

**Finding:** All three modules use **strict error handling** - they return `Error::Config("HOME environment variable not set")` when HOME is unset. There is **NO inconsistency** with silent fallback to `/root` in the current codebase.

**Note:** This document captures the current state as requested. If there was historical inconsistency with `/root` fallback, it has already been corrected to the strict approach across all modules.

## Module-by-Module Analysis

### 1. util.rs - `get_home()` helper (Lines 3-14)

**Location:** `src/util.rs:10-14`

```rust
pub fn get_home() -> Result<std::path::PathBuf> {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .map_err(|_| Error::Config("HOME environment variable not set".to_string()))
}
```

**Behavior:** STRICT - Returns `Error::Config` if HOME is unset

**Rationale (from docstring):**
> This is intentionally strict — a missing HOME indicates a misconfigured environment, and we want to fail explicitly rather than silently guess.

---

### 2. config.rs - Config path resolution (Lines 147-158)

**Location:** `src/config.rs:147-158`

```rust
pub fn default_path() -> Result<PathBuf> {
    // Try XDG_CONFIG_HOME first
    if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg_config)
            .join(CONFIG_DIR)
            .join(CONFIG_FILENAME));
    }

    // Fall back to ~/.config
    let home = get_home()?;
    Ok(home.join(".config").join(CONFIG_DIR).join(CONFIG_FILENAME))
}
```

**Behavior:** STRICT - Calls `get_home()?`, propagates `Error::Config` if HOME is unset

**Test coverage:** `src/config.rs:1031-1050` - `default_path_fails_when_home_not_set()`

---

### 3. poller.rs - Transcript path derivation (Lines 85-93)

**Location:** `src/poller.rs:85-93`

```rust
pub fn derive_transcript_path(session_id: &str, cwd: &str) -> Result<PathBuf> {
    let slug = cwd_to_slug(cwd)?;
    let home = get_home()?;
    Ok(home
        .join(".claude")
        .join("projects")
        .join(&slug)
        .join(format!("{session_id}.jsonl")))
}
```

**Behavior:** STRICT - Calls `get_home()?`, propagates `Error::Config` if HOME is unset

**Test coverage:** `src/poller.rs:536-554` - `derive_transcript_path_fails_when_home_not_set()`

---

### 4. poller.rs - Projects directory resolution (Lines 169-174)

**Location:** `src/poller.rs:169-174`

```rust
pub fn projects_dir_for_cwd() -> Result<PathBuf> {
    let cwd = std::env::current_dir().map_err(Error::Io)?;
    let slug = cwd_to_slug(&cwd.to_string_lossy())?;
    let home = get_home()?;
    Ok(home.join(".claude").join("projects").join(slug))
}
```

**Behavior:** STRICT - Calls `get_home()?`, propagates `Error::Config` if HOME is unset

**Test coverage:** `src/poller.rs:557-575` - `projects_dir_for_cwd_fails_when_home_not_set()`

---

### 5. session.rs - CWD pretrust (Lines 908-914)

**Location:** `src/session.rs:908-914`

```rust
fn pretrust_cwd() -> Result<()> {
    let cwd = std::env::current_dir()
        .map_err(|e| Error::Internal(anyhow::anyhow!("pretrust cwd: {e}")))?;
    let home = get_home()?;
    let claude_json = home.join(".claude.json");
    pretrust_cwd_at(&claude_json, cwd.to_string_lossy().as_ref())
}
```

**Behavior:** STRICT - Calls `get_home()?`, propagates `Error::Config` if HOME is unset

**Test coverage:** `src/session.rs:1743-1759` - `pretrust_cwd_fails_when_home_not_set()`

---

### 6. session.rs - Cross-module consistency test (Lines 1762-1813)

**Location:** `src/session.rs:1762-1813`

```rust
#[test]
fn home_unset_consistent_error_handling_across_all_modules() {
    // This test verifies that all modules consistently return Error::Config
    // when HOME is not set, rather than panicking or using silent fallbacks.
    // This is critical for predictable behavior in headless/chroot environments.

    use crate::config::Config;
    use crate::poller::{derive_transcript_path, projects_dir_for_cwd};

    // Save original HOME
    let original_home = std::env::var("HOME").ok();

    // Unset HOME
    std::env::remove_var("HOME");
    std::env::remove_var("XDG_CONFIG_HOME");

    // Test 1: Config::default_path() fails with clear error
    let config_result = Config::default_path();
    assert!(config_result.is_err());
    assert!(config_result
        .unwrap_err()
        .to_string()
        .contains("HOME environment variable not set"));

    // Test 2: derive_transcript_path fails with clear error
    let derive_result = derive_transcript_path("session-123", "/project/dir");
    assert!(derive_result.is_err());
    assert!(derive_result
        .unwrap_err()
        .to_string()
        .contains("HOME environment variable not set"));

    // Test 3: projects_dir_for_cwd fails with clear error
    let projects_result = projects_dir_for_cwd();
    assert!(projects_result.is_err());
    assert!(projects_result
        .unwrap_err()
        .to_string()
        .contains("HOME environment variable not set"));

    // Test 4: pretrust_cwd fails with clear error
    let pretrust_result = pretrust_cwd();
    assert!(pretrust_result.is_err());
    assert!(pretrust_result
        .unwrap_err()
        .to_string()
        .contains("HOME environment variable not set"));

    // Restore HOME
    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    }
}
```

**Purpose:** Validates consistency across ALL modules - proves no silent fallbacks exist

---

## Summary Table

| Module | Function | Line | HOME Handling | Test Coverage |
|--------|----------|------|---------------|---------------|
| util.rs | `get_home()` | 10-14 | STRICT: `Error::Config` | N/A (helper) |
| config.rs | `default_path()` | 147-158 | STRICT: via `get_home()?` | Lines 1031-1050 |
| poller.rs | `derive_transcript_path()` | 85-93 | STRICT: via `get_home()?` | Lines 536-554 |
| poller.rs | `projects_dir_for_cwd()` | 169-174 | STRICT: via `get_home()?` | Lines 557-575 |
| session.rs | `pretrust_cwd()` | 908-914 | STRICT: via `get_home()?` | Lines 1743-1759 |

---

## Conclusion

**Current State:** ✅ **CONSISTENT** across all modules

All three modules (config, poller, session) use the same strict approach via the shared `get_home()` helper:
- Return `Error::Config("HOME environment variable not set")` when HOME is unset
- No silent fallback to `/root` or any other default
- Comprehensive test coverage validates this behavior

**Recommendation:** No changes needed for consistency. The current strict approach is correct and well-tested.

---

## Appendix: Code Locations

For quick navigation:

```bash
# Helper function
src/util.rs:10-14              # get_home() definition

# Call sites (all strict)
src/config.rs:156              # Config::default_path()
src/poller.rs:87               # derive_transcript_path()
src/poller.rs:172              # projects_dir_for_cwd()
src/session.rs:911             # pretrust_cwd()

# Test coverage
src/config.rs:1031-1050        # default_path_fails_when_home_not_set
src/poller.rs:536-554          # derive_transcript_path_fails_when_home_not_set
src/poller.rs:557-575          # projects_dir_for_cwd_fails_when_home_not_set
src/session.rs:1743-1759       # pretrust_cwd_fails_when_home_not_set
src/session.rs:1762-1813       # home_unset_consistent_error_handling_across_all_modules
```
