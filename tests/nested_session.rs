// Regression test for CLAUDECODE environment variable inheritance issue
//
// Issue: When claude-print is invoked from within another Claude Code session,
// CLAUDECODE=1 is inherited by the child process. If left set, the child treats
// the invocation as a nested/subagent call and writes a subagent-style transcript
// instead of a normal top-level session JSONL, causing session_id to be null and
// num_turns to be 0 in the output.
//
// Fix: claude-print must scrub CLAUDECODE (and the other session markers) from
// the child's environment before exec so the child creates a fresh top-level
// session regardless of parent environment.
//
// Since e75e810 (claudepr-26e7a0b6) the scrub is done by building the child's
// environment in the PARENT — `build_child_env`/`scrub_env` in src/pty.rs, which
// drops every var named in `SCRUBBED_ENV` and appends `FORCED_ENV` — and passing
// it to `execvpe`. The earlier mechanism (libc::unsetenv/libc::setenv between
// fork() and exec()) was removed because neither call is async-signal-safe and
// pool mode is multithreaded; it must not come back.

#[test]
fn test_claudecode_env_var_propagation_without_fix() {
    // Document the bug behavior: if CLAUDECODE were NOT scrubbed,
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
    Bug symptoms when CLAUDECODE is NOT scrubbed from the child environment:

    1. Child Claude Code treats invocation as nested/subagent call
    2. Writes subagent-style transcript instead of top-level JSONL
    3. Stop payload may not contain session_id
    4. Transcript reader cannot locate the correct file
    5. JSON output shows session_id=null and num_turns=0
    6. Despite is_error: false and correct response text

    The fix (src/pty.rs, since e75e810): build the child environment in the
    parent via build_child_env/scrub_env and pass it to execvpe.

    Proof of fix:
    - src/pty.rs SCRUBBED_ENV: drops CLAUDE_CODE_SESSION_ID, CLAUDECODE,
      CLAUDE_CODE_CHILD_SESSION, CLAUDE_CODE_SKIP_PROMPT_HISTORY
      (CLAUDE_CODE_CHILD_SESSION is the transcript-persistence gate of
      claude 2.1.263 — inherited, the TUI never writes the transcript,
      claudepr-26e7a0b6)
    - src/pty.rs FORCED_ENV: sets CLAUDE_CODE_ENTRYPOINT=cli explicitly
      (billing invariant) and CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1
    - The built environment is handed to execvpe; no setenv/unsetenv runs
      between fork() and exec() (async-signal-safety)

    This ensures the child Claude Code process:
    1. Does NOT inherit the parent's CLAUDE_CODE_SESSION_ID (prevents writing into parent's transcript)
    2. Does NOT inherit CLAUDECODE (prevents nested/subagent mode)
    3. Does NOT inherit CLAUDE_CODE_CHILD_SESSION (transcript persistence stays on)
    4. DOES have CLAUDE_CODE_ENTRYPOINT=cli (ensures TUI mode for billing)

    Manual verification:
    echo "Reply with exactly one word: pong" | CLAUDECODE=1 claude-print --output-format json --timeout 45
    Should produce JSON with session_id=<uuid> and num_turns>0, NOT session_id=null and num_turns=0.
    "#;

    eprintln!("{}", expected_symptoms);
}

#[test]
fn test_claudecode_env_scrub_logic_exists() {
    // Verify that the scrub code exists in pty.rs
    // This is a compile-time check that the env construction is present

    let pty_source = include_str!("../src/pty.rs");

    // Check that every session marker is listed in SCRUBBED_ENV
    for marker in [
        "CLAUDECODE",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_CODE_CHILD_SESSION",
        "CLAUDE_CODE_SKIP_PROMPT_HISTORY",
    ] {
        assert!(
            pty_source.contains(&format!("\"{marker}\",")),
            "Fix not found: pty.rs SCRUBBED_ENV must list {marker} so it cannot \
             reach the child and trigger nested-session behavior"
        );
    }

    // Check that the forced variables are present
    assert!(
        pty_source.contains("(\"CLAUDE_CODE_ENTRYPOINT\", \"cli\")"),
        "Fix not found: pty.rs FORCED_ENV must set CLAUDE_CODE_ENTRYPOINT=cli for TUI billing mode"
    );
    assert!(
        pty_source.contains("(\"CLAUDE_CODE_FORCE_SESSION_PERSISTENCE\", \"1\")"),
        "Fix not found: pty.rs FORCED_ENV must force CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1 \
         (claudepr-26e7a0b6: claude 2.1.263 gates transcript persistence on the scrubbed markers)"
    );

    // Check that the built environment is what the child actually execs with
    assert!(
        pty_source.contains("build_child_env()") && pty_source.contains("&child_env"),
        "Fix not found: pty.rs must build the child environment via build_child_env \
         and pass it to execvpe — a scrub that never reaches the exec call is a no-op"
    );

    // e75e810 invariant: no setenv/unsetenv may run between fork() and exec().
    // Neither is async-signal-safe and both may allocate; pool mode is
    // multithreaded, so a fork racing a thread that holds the allocator lock
    // would deadlock the child before it reached exec.
    assert!(
        !pty_source.contains("libc::unsetenv(") && !pty_source.contains("libc::setenv("),
        "Regression: pty.rs must not call libc::setenv/libc::unsetenv — build the \
         child environment in the parent and pass it to execvpe instead"
    );

    eprintln!("✓ All fix verifications passed:");
    eprintln!("  - SCRUBBED_ENV lists all four session markers");
    eprintln!("  - FORCED_ENV sets ENTRYPOINT=cli and FORCE_SESSION_PERSISTENCE=1");
    eprintln!("  - child_env is built in the parent and passed to execvpe");
    eprintln!("  - no post-fork setenv/unsetenv (async-signal-safety)");
}

#[test]
fn test_pty_spawner_scrubs_markers_and_forces_persistence_in_child_env() {
    // Behavioral counterpart of the source check above: spawn a real child
    // through the public PtySpawner API and inspect the environment it
    // actually receives. `env` prints its own environment to the PTY.
    //
    // This mutates no process environment, so it cannot race the parallel
    // test threads in this binary (see the scrub_env doc comment in pty.rs
    // for why env mutation in tests is avoided).
    use claude_print::pty::PtySpawner;
    use std::ffi::CString;
    use std::os::unix::io::AsRawFd;

    let cmd = CString::new("env").unwrap();
    let spawner = PtySpawner::spawn(&cmd, &[]).expect("PtySpawner::spawn should succeed");

    let master_fd = spawner.master.as_raw_fd();
    let mut output = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        // SAFETY: master_fd is a valid PTY master fd owned by `spawner`.
        let n = unsafe { libc::read(master_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break; // EOF, or EIO once the child exits and closes the slave side
        }
        output.extend_from_slice(&buf[..n as usize]);
    }
    let _ = nix::sys::wait::waitpid(spawner.child_pid, None);

    let text = String::from_utf8_lossy(&output);

    // The forced variables must reach the child.
    assert!(
        text.contains("CLAUDE_CODE_ENTRYPOINT=cli"),
        "billing invariant: child env must force CLAUDE_CODE_ENTRYPOINT=cli, got: {text:?}"
    );
    assert!(
        text.contains("CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1"),
        "child env must force CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1, got: {text:?}"
    );

    // The scrubbed markers must not reach the child. Trivially true when the
    // test runner itself has none set (e.g. plain `cargo test`); a real guard
    // under agent/NEEDLE dispatch, where the runner inherits them and this
    // bug actually bit.
    for marker in [
        "CLAUDECODE=",
        "CLAUDE_CODE_SESSION_ID=",
        "CLAUDE_CODE_CHILD_SESSION=",
        "CLAUDE_CODE_SKIP_PROMPT_HISTORY=",
    ] {
        assert!(
            !text.contains(marker),
            "{marker} must be scrubbed from the child env, got: {text:?}"
        );
    }

    // An inherited sdk-cli entrypoint must not survive the forced override.
    assert!(
        !text.contains("CLAUDE_CODE_ENTRYPOINT=sdk-cli"),
        "inherited sdk-cli entrypoint must be overridden by cli, got: {text:?}"
    );
}
