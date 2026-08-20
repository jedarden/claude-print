# HOME Environment Variable Unset - Test Coverage Documentation

## Summary

This document describes the comprehensive test coverage for the HOME environment variable unset scenario across all entry points in claude-print.

## Error Behavior

When the `HOME` environment variable is not set:

- **Error Type**: `Error::Config("HOME environment variable not set")`
- **Exit Code**: 2 (Setup error)
- **Error Message**: "HOME environment variable not set"

## Implementation Details

The `get_home()` function in `src/util.rs` provides centralized HOME resolution:

```rust
pub fn get_home() -> Result<std::path::PathBuf> {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .map_err(|_| Error::Config("HOME environment variable not set".to_string()))
}
```

This strict approach ensures that missing HOME fails explicitly rather than silently guessing paths.

## Modules Using get_home()

The following modules depend on `get_home()` and will fail with `Error::Config` when HOME is unset:

1. **src/config.rs** - Config path resolution (when XDG_CONFIG_HOME is not set)
2. **src/session.rs** - Transcript path resolution for temp directory
3. **src/poller.rs** - Transcript path derivation for Stop hook processing

## Entry Points Tested

### Early Exit Commands (No HOME Required)

These commands exit before config loading and work without HOME:

1. **`--help`** - Shows usage information
   - Test: `test_help_command_works_when_home_unset`
   - Expected: Exit code 0, shows help text
   - Status: ✅ PASS

2. **`--version`** - Shows version information  
   - Test: `test_version_command_works_when_home_unset`
   - Expected: Exit code 0, shows version
   - Status: ✅ PASS

3. **`--check`** - Runs installation self-test
   - Test: `test_check_command_works_when_home_unset`
   - Expected: Exit code 0, shows check results
   - Status: ✅ PASS

### Session Commands (Require HOME)

These commands require HOME and fail consistently:

4. **Text format execution** - Default output format
   - Test: `test_text_format_fails_when_home_unset`
   - Command: `claude-print "test prompt"`
   - Expected: Exit code 2, error contains "HOME environment variable not set"
   - Status: ✅ PASS

5. **JSON format execution** - Structured JSON output
   - Test: `test_json_format_fails_when_home_unset`
   - Command: `claude-print --output-format json "test prompt"`
   - Expected: Exit code 2, JSON error with HOME message
   - Status: ✅ PASS

6. **Stream-JSON format execution** - Streaming JSON output
   - Test: `test_stream_json_format_fails_when_home_unset`
   - Command: `claude-print --output-format stream-json "test prompt"`
   - Expected: Exit code 2, JSON error with HOME message
   - Status: ✅ PASS

### Other CLI Flags (Require HOME)

7. **`--model` flag** - Model selection
   - Test: `test_with_model_flag_fails_when_home_unset`
   - Command: `claude-print --model claude-opus-4 "test"`
   - Expected: Exit code 2, error contains HOME message
   - Status: ✅ PASS

### Edge Cases

8. **Input file with HOME unset** - Different error precedence
   - Test: `test_with_input_file_fails_when_home_unset`
   - Command: `claude-print --input-file /dev/null`
   - Expected: Exit code 4 (file check happens before HOME check)
   - Status: ✅ PASS
   - Note: File validation occurs before config loading

9. **XDG_CONFIG_HOME set, HOME unset** - Edge case
   - Test: `test_with_xdg_config_home_set_fails_when_home_unset`
   - Command: `env -u HOME XDG_CONFIG_HOME=/tmp/test-xdg claude-print --help`
   - Expected: Exit code 0 or 2 (documents current behavior)
   - Status: ✅ PASS
   - Note: XDG_CONFIG_HOME takes precedence for config loading

## Consistency Tests

10. **Consistent error type** - All session commands fail with same exit code
    - Test: `test_consistent_error_type_across_commands`
    - Commands tested: text, json, stream-json formats
    - Expected: All fail with exit code 2
    - Status: ✅ PASS

11. **Consistent error message** - All session commands show same error text
    - Test: `test_consistent_error_message_across_commands`
    - Commands tested: text, json, stream-json formats
    - Expected: All output "HOME environment variable not set"
    - Status: ✅ PASS

## Test File

All tests are located in: `tests/home_unset.rs`

Test count: 11 tests
All tests passing: ✅

## Running the Tests

```bash
# Run all HOME unset tests
cargo test --test home_unset

# Run specific test
cargo test --test home_unset test_text_format_fails_when_home_unset

# Run with verbose output
cargo test --test home_unset -- --nocapture
```

## Manual Verification

```bash
# Build debug binary (has correct HOME checking)
cargo build

# Test with HOME unset - should fail with exit code 2
env -u HOME -u XDG_CONFIG_HOME ./target/debug/claude-print "test"
echo "Exit code: $?"

# Should see: "error: invalid config: HOME environment variable not set"
# Exit code: 2
```

## Acceptance Criteria Status

- ✅ Test: `env -u HOME claude-print --help` works (exit 0) - documented that early exit commands don't require HOME
- ✅ Test: `env -u HOME claude-print ...` (session commands) fail consistently with exit code 2
- ✅ Verify all modules fail with same error type (`Error::Config`)
- ✅ Error message clearly states "HOME environment variable not set"
- ✅ All existing tests that unset HOME still pass (no regressions)
- ✅ Document test coverage: this file lists all entry points tested

## Code Coverage

### Functions Tested

- `src/util.rs::get_home()` - Core HOME resolution (indirectly tested via all modules)
- `src/config.rs::default_path()` - Config path resolution
- `src/session.rs::*` - Session creation with transcript paths
- `src/poller.rs::derive_transcript_path()` - Transcript path derivation

### Error Conversion Flow

```
Error::Config("HOME environment variable not set")
  ↓ (converted via From<Error> for ClaudeudePrintError)
ClaudePrintError::Setup("HOME environment variable not set")
  ↓ (exit code)
2
```

## Notes

1. **Debug vs Release Binaries**: Always test with the debug binary (`cargo build`) as it includes the current code. Release builds may be cached.

2. **XDG_CONFIG_HOME Precedence**: Config loading checks `XDG_CONFIG_HOME` first, falling back to `HOME/.config` only if XDG is not set.

3. **File Validation Order**: Input file validation happens before config loading, so `--input-file` errors (exit code 4) take precedence over HOME errors.

4. **Early Exit Optimization**: Commands like `--help`, `--version`, and `--check` exit before any filesystem operations that require HOME.

## Future Considerations

If new entry points are added to claude-print, they should be added to this test suite if they:
- Load configuration files
- Resolve paths relative to HOME
- Create files/directories in user's home directory
- Depend on any module that calls `get_home()`
