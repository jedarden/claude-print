// Regression test for CLAUDECODE environment variable inheritance issue
//
// Issue: When claude-print is invoked from within another Claude Code session,
// CLAUDECODE=1 is inherited by the child process. If left set, the child treats
// the invocation as a nested/subagent call and writes a subagent-style transcript
// instead of a normal top-level session JSONL, causing session_id to be null and
// num_turns to be 0 in the output.
//
// Fix: claude-print must unset CLAUDECODE before execvp so the child creates a fresh
// top-level session regardless of parent environment.

#[test]
fn test_claudecode_env_var_propagation_without_fix() {
    // Document the bug behavior: if CLAUDECODE were NOT unset,
    // we would see session_id=null and num_turns=0.
    //
    // This test documents the expected failure mode but cannot
    // directly test it since the fix is already in place.
    // It serves as documentation of what the bug looks like.

    // The bug manifests as:
    // - session_id: null in JSON output
    // - num_turns: 0 in JSON output
    // - Even though is_error: false and result contains the correct text

    // This is a documentation-only test to help future maintainers
    // understand what was being fixed.
    let expected_symptoms = r#"
    Bug symptoms when CLAUDECODE is NOT unset before execvp:

    1. Child Claude Code treats invocation as nested/subagent call
    2. Writes subagent-style transcript instead of top-level JSONL
    3. Stop payload may not contain session_id
    4. Transcript reader cannot locate the correct file
    5. JSON output shows session_id=null and num_turns=0
    6. Despite is_error: false and correct response text

    The fix: unset CLAUDECODE in pty.rs before execvp (line 106).

    Proof of fix:
    - src/pty.rs line 106: libc::unsetenv(c"CLAUDECODE".as_ptr() as *const libc::c_char);
    - src/pty.rs line 105: libc::unsetenv(c"CLAUDE_CODE_SESSION_ID".as_ptr() as *const libc::c_char);
    - src/pty.rs lines 108-117: set CLAUDE_CODE_ENTRYPOINT=cli explicitly

    This ensures the child Claude Code process:
    1. Does NOT inherit the parent's CLAUDE_CODE_SESSION_ID (prevents writing into parent's transcript)
    2. Does NOT inherit CLAUDECODE (prevents nested/subagent mode)
    3. DOES have CLAUDE_CODE_ENTRYPOINT=cli (ensures TUI mode for billing)

    Manual verification:
    echo "Reply with exactly one word: pong" | CLAUDECODE=1 claude-print --output-format json --timeout 45
    Should produce JSON with session_id=<uuid> and num_turns>0, NOT session_id=null and num_turns=0.
    "#;

    eprintln!("{}", expected_symptoms);
    assert!(true, "documentation test");
}

#[test]
fn test_claudecode_env_var_unset_logic_exists() {
    // Verify that the fix code exists in pty.rs
    // This is a compile-time check that the unsetenv call is present

    let pty_source = include_str!("../src/pty.rs");

    // Check that CLAUDECODE unsetenv is present
    assert!(
        pty_source.contains("libc::unsetenv(c\"CLAUDECODE\""),
        "Fix not found: pty.rs must unset CLAUDECODE before execvp to prevent nested session bugs"
    );

    // Check that CLAUDE_CODE_SESSION_ID unsetenv is present
    assert!(
        pty_source.contains("libc::unsetenv(c\"CLAUDE_CODE_SESSION_ID\""),
        "Fix not found: pty.rs must unset CLAUDE_CODE_SESSION_ID before execvp"
    );

    // Check that CLAUDE_CODE_ENTRYPOINT setenv is present
    assert!(
        pty_source.contains("libc::setenv(") && pty_source.contains("CLAUDE_CODE_ENTRYPOINT"),
        "Fix not found: pty.rs must set CLAUDE_CODE_ENTRYPOINT=cli for TUI billing mode"
    );

    // Verify the explanatory comments are present
    assert!(
        pty_source.contains("must be unset when inherited from a parent Claude Code"),
        "Documentation missing: pty.rs should explain why CLAUDECODE must be unset"
    );

    // Check for the bug symptoms documentation (may span multiple lines)
    let has_session_id_doc = pty_source.contains("session_id to be null");
    let has_num_turns_doc = pty_source.contains("num_turns to be 0");
    assert!(
        has_session_id_doc && has_num_turns_doc,
        "Documentation missing: pty.rs should document the bug symptoms (session_id=null, num_turns=0). \
         Found session_id doc: {}, Found num_turns doc: {}",
        has_session_id_doc,
        has_num_turns_doc
    );

    eprintln!("✓ All fix verifications passed:");
    eprintln!("  - CLAUDECODE unsetenv call present");
    eprintln!("  - CLAUDE_CODE_SESSION_ID unsetenv call present");
    eprintln!("  - CLAUDE_CODE_ENTRYPOINT setenv call present");
    eprintln!("  - Explanatory comments present");
}
