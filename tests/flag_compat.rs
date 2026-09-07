//! Child-argv compatibility with the installed `claude` binary.
//!
//! `claude-print` builds an argv for the child `claude` process. When Claude
//! Code drops or renames an option, that argv silently becomes invalid and the
//! child dies at argument parsing — before the prompt is ever injected — so
//! every claude-print invocation fails regardless of model or output format.
//!
//! That is not hypothetical: `--timeout` (claude-print's own watchdog flag) was
//! forwarded to a binary that has no such option, which broke claude-print
//! completely against claude 2.1.263.

// ── Child argv compatibility with the installed claude binary ────────────────

/// Every flag `claude-print` forwards to the child must still be accepted by
/// the installed `claude`. This is the check that was missing when `--timeout`
/// was forwarded to a binary that has no such option: claude-print built a
/// child argv that died at parsing with `error: unknown option '--timeout'`
/// before the prompt was injected, breaking every invocation.
///
/// The probe is credential-free and costs no tokens: an unknown *option* is
/// rejected by the child's argument parser, which runs before any model
/// request, so pairing the real flags with a deliberately invalid `--model`
/// separates the two failures. A parse error names the offending flag; a
/// model error proves the flags themselves parsed.
///
/// Skipped (pass) when `claude` is not on PATH.
#[test]
fn forwarded_flags_are_accepted_by_installed_claude() {
    // The flags main.rs puts on the child argv, with representative values.
    let forwarded: &[&[&str]] = &[
        &["--max-turns", "1"],
        &["--dangerously-skip-permissions"],
        &["--allowedTools", "Read"],
        &["--disallowedTools", "Bash"],
    ];

    for flags in forwarded {
        let mut cmd = std::process::Command::new("claude");
        cmd.arg("--print");
        cmd.args(*flags);
        // Invalid on purpose: fails at request time, long after argv parsing.
        cmd.args(["--model", "__claude_print_flag_probe__", "x"]);
        cmd.stdin(std::process::Stdio::null());

        let out = match cmd.output() {
            Ok(o) => o,
            // claude not on PATH — skip rather than fail.
            Err(_) => return,
        };

        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );

        assert!(
            !combined.contains("unknown option"),
            "installed claude rejects a flag claude-print forwards ({flags:?}): {}",
            combined.lines().next().unwrap_or("").trim()
        );
    }
}

/// The inverse assertion, so the probe above cannot silently stop detecting
/// anything: a flag the child genuinely does not have must be reported as an
/// unknown option. `--timeout` is claude-print's own watchdog flag and is the
/// exact one that regressed, so it doubles as the canary.
#[test]
fn probe_detects_a_flag_the_child_does_not_have() {
    let out = match std::process::Command::new("claude")
        .args(["--print", "--timeout", "5"])
        .args(["--model", "__claude_print_flag_probe__", "x"])
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(o) => o,
        Err(_) => return, // claude not on PATH — skip.
    };

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        combined.contains("unknown option"),
        "probe failed to flag --timeout as unknown; it can no longer detect \
         a forwarded flag the child does not accept. Output: {combined}"
    );
}
