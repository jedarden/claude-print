//! Executable pin for the release artifacts' platform and static-linkage
//! gates (bead claudepr-40878fa4).
//!
//! The README limits support to Linux x86_64 and calls the prebuilt asset
//! a "static musl binary"; AGENTS.md's build fence carries the matching
//! musl release command; the runbook documents `verify_static` and the
//! size cap as the gates behind that promise. What nothing verified:
//!
//! - **The gate's decision logic.** `tests/platform_matrix_docs.rs` and
//!   `tests/release_runbook_docs.rs` both assert the *string*
//!   `verify_static` exists in the WorkflowTemplate — the gate could be
//!   gutted (grep weakened to match anything, the failing `exit 1`
//!   dropped, an invocation commented out) with every existing test
//!   green, shipping a dynamically linked binary under a "static musl"
//!   label. The template itself documents why the logic is subtle:
//!   `ldd` exits 0 for static AND dynamic binaries, so only its
//!   stdout/stderr markers can decide — an invariant no test held.
//! - **That CI builds the documented target at all on a plain push.**
//!   The musl builds and both artifact gates ran only in release mode
//!   (`tag` set), so a commit that broke the musl build — a dependency
//!   that will not cross-compile, a target-specific cfg mistake — was
//!   green in verify-only CI on every push and surfaced only at release
//!   time.
//!
//! Two halves, both against the vendored `claude-print-ci`
//! WorkflowTemplate:
//!
//! 1. **Executable** — the `verify_static` body is extracted *verbatim
//!    from the template* and executed under `bash -e` (the same `set -e`
//!    regime CI runs) with a stubbed `ldd` (the `tests/install_sh_arch.rs`
//!    PATH-stub pattern): both static markers pass, dynamic output and
//!    empty output fail with the HR-1 error, and ldd's exit status never
//!    decides — a marker arriving on stderr under a failing exit still
//!    passes, a clean exit with a dynamic dependency list still fails.
//!    Because the body is extracted rather than reimplemented, the test
//!    proves the *shipped* logic, and a template edit that changes the
//!    gate's behavior changes what runs here in the same breath.
//! 2. **Structural** — the musl builds, both `verify_static` invocations,
//!    and the size cap sit between the quality gates and the verify-only
//!    exit (so they run in BOTH modes), the release-only prelude (tag
//!    push + draft/publish idempotency) stays guarded by `[ -n "$TAG" ]`
//!    and ahead of the build (a re-run of a published release still
//!    exits before rebuilding), and the README/runbook claims about all
//!    of this stay aligned.
//!
//! Deliberately not pinned here: the toolchain triple and asset-name
//! derivation (`tests/platform_matrix_docs.rs`), the publication order
//! and manifest coverage (`tests/release_runbook_docs.rs`), and the
//! AGENTS.md musl-command row (`tests/docs_build_layout.rs`) — this
//! suite owns the gate's *logic* and its *both-modes placement* only.
//!
//! Hermetic: no network, no real `ldd`, no compiled binaries; a stub-bin
//! PATH of one stub (`ldd`) plus runtime-resolved coreutils symlinks
//! (`bash`, `grep`, `cat`), so nothing depends on the host layout.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const CI_TEMPLATE: &str = "claude-print-ci-workflowtemplate.yml";

/// The gate invocation lines, exactly as the template must carry them —
/// one per published binary, so commenting one out loses an asset's
/// static-linkage proof.
const VERIFY_MAIN: &str = "verify_static \"./${CLAUDE_PRINT_ASSET}\"";
const VERIFY_MOCK: &str = "verify_static \"./${MOCK_ASSET}\"";

/// The verify-only green exit's landmark echo — the same line
/// `tests/contract_maintenance.rs` anchors its gate-ordering pins on, so
/// the two suites cannot fork over where the exit sits.
const VERIFY_ONLY_EXIT: &str = "Verify-only mode: all quality gates passed";

/// Repo-root probes: a directory holding both is a claude-print checkout.
const ROOT_PROBES: [&str; 2] = ["AGENTS.md", "Cargo.toml"];

fn repo_root() -> PathBuf {
    let baked = env!("CARGO_MANIFEST_DIR").to_string();
    let candidates: [(&str, Option<String>); 3] = [
        (
            "$CLAUDE_PRINT_TEST_REPO",
            std::env::var("CLAUDE_PRINT_TEST_REPO").ok(),
        ),
        (
            "runtime CARGO_MANIFEST_DIR",
            std::env::var("CARGO_MANIFEST_DIR").ok(),
        ),
        ("compile-time CARGO_MANIFEST_DIR", Some(baked)),
    ];
    let mut rejected = Vec::new();
    for (origin, candidate) in &candidates {
        if let Some(path) = candidate {
            let path = Path::new(path);
            if ROOT_PROBES.iter().all(|probe| path.join(probe).is_file()) {
                return path.to_path_buf();
            }
            rejected.push(format!("{origin}={}", path.display()));
        }
    }
    panic!(
        "no candidate repo root is a claude-print checkout (probe: {:?} + {:?}): {} — \
         run via `cargo test` from a checkout, or set $CLAUDE_PRINT_TEST_REPO to one",
        ROOT_PROBES[0],
        ROOT_PROBES[1],
        rejected.join("; ")
    )
}

fn repo_file(relative: &str) -> String {
    fs::read_to_string(repo_root().join(relative))
        .unwrap_or_else(|e| panic!("read {relative} from the checkout under test: {e}"))
}

/// Whitespace-normalized text — runbook/README claims are wrapped across
/// lines, so pins match the words, not the wrapping.
fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Byte position of the first line carrying `needle` that is not a
/// comment line (`#`-prefixed after trimming) — template landmarks must
/// live in executed code, and the template's own comments quote several
/// of them.
fn position_uncommented(document: &str, needle: &str) -> Option<usize> {
    let mut offset = 0usize;
    for line in document.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('#') && trimmed.contains(needle) {
            return Some(offset);
        }
        offset += line.len() + 1;
    }
    None
}

/// The `verify_static() { … }` body, extracted verbatim from the
/// template so the executable half below runs the gate CI actually
/// ships, not a reimplementation.
fn verify_static_body(template: &str) -> String {
    let start = template
        .find("verify_static() {")
        .expect("the WorkflowTemplate must define the verify_static gate");
    let mut body = String::new();
    for line in template[start..].lines() {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(line);
        if line.trim() == "}" {
            return body;
        }
    }
    panic!("verify_static's definition has no closing brace");
}

/// The nesting-aware segment a `if …; then` guard at `guard_start`
/// covers, through its matching `fi` — nested `if`/`fi` pairs (the
/// IS_DRAFT checks inside the prelude) tracked by depth.
fn guarded_segment(template: &str, guard_start: usize) -> Option<String> {
    let script = &template[guard_start..];
    let mut depth = 0i32;
    let mut offset = 0usize;
    for line in script.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('#') {
            if trimmed.starts_with("if ") || trimmed.ends_with("; then") {
                depth += 1;
            }
            if trimmed == "fi" {
                depth -= 1;
                if depth == 0 {
                    return Some(script[..offset + line.len()].to_string());
                }
            }
        }
        offset += line.len() + 1;
    }
    None
}

/// The release-only prelude — the `[ -n "$TAG" ]`-guarded segment holding
/// the tag push and the draft/publish idempotency check — as
/// `(guard_open_position, segment_text)`. The template carries a second
/// `[ -n "$TAG" ]` guard (the clone arm near the top), so the prelude is
/// identified by its contents, not by being the only guard: exactly one
/// guarded segment must contain the tag push.
fn release_only_prelude(template: &str) -> (usize, String) {
    let guard = "if [ -n \"$TAG\" ]; then";
    let tag_push =
        "git push \"https://x-token:${FORGEJO_TOKEN}@git.ardenone.com/jedarden/claude-print.git\" \"refs/tags/v${VERSION}\"";
    let mut prelude: Option<(usize, String)> = None;
    let mut offset = 0usize;
    for line in template.lines() {
        if line.trim() == guard {
            if let Some(segment) = guarded_segment(template, offset) {
                if segment.contains(tag_push) {
                    assert!(
                        prelude.is_none(),
                        "the WorkflowTemplate must carry exactly one {guard:?} guard \
                         around the release-only prelude — a second guard pushing \
                         the tag would double-push on a release run"
                    );
                    prelude = Some((offset, segment));
                }
            } else {
                panic!(
                    "the release-only prelude guard at byte {offset} is never \
                     closed — the whole script after it would be release-only"
                );
            }
        }
        offset += line.len() + 1;
    }
    prelude.unwrap_or_else(|| {
        panic!(
            "the tag push must live inside a {guard:?} guard — the \
             release-only prelude — or a verify-only run pushes a tag and \
             queries GitHub"
        )
    })
}

/// The structural half: the template builds the documented target and
/// gates it in BOTH modes, with the release-only prelude guarded and
/// still ahead of the build.
fn check_template_gates(template: &str) {
    // --- The gate body: the load-bearing lines. ---
    let body = verify_static_body(template);
    assert!(
        body.contains("out=\"$(ldd \"$1\" 2>&1 || true)\""),
        "verify_static must capture ldd's stdout AND stderr (2>&1) while \
         swallowing its exit status (|| true) — ldd exits 0 for dynamic \
         binaries too, and under CI's set -e a failing ldd would abort the \
         script before the grep could judge the markers (the template's own NOTE)"
    );
    assert!(
        body.contains("grep -qE 'statically linked|not a dynamic executable'"),
        "verify_static must accept BOTH static markers — glibc ldd says \
         \"not a dynamic executable\" for static binaries, musl ldd says \
         \"statically linked\"; dropping either arm rejects a genuinely static \
         binary on one family of hosts"
    );
    assert!(
        body.contains("exit 1"),
        "verify_static's failure branch must exit 1 — without it the gate is a \
         warning and a dynamically linked binary ships"
    );
    assert!(
        body.contains(">&2"),
        "verify_static's failure must land on stderr — CI operators read the \
         error where failures are expected"
    );

    // --- The gate runs on both published binaries. ---
    for invocation in [VERIFY_MAIN, VERIFY_MOCK] {
        assert!(
            position_uncommented(template, invocation).is_some(),
            "the WorkflowTemplate must invoke {invocation} — an asset built but \
             never verified is exactly the gap this suite exists for"
        );
    }

    // --- Both-modes placement: toolchain → builds → gates → verify-only
    // exit, so a plain push builds the documented target and proves its
    // static linkage before anything could ship. ---
    let chain: [(&str, &str); 8] = [
        ("rustup target add", "the musl toolchain install"),
        (
            "cargo build --release --target \"${MUSL_TARGET}\" --bin claude-print",
            "the main binary's musl build",
        ),
        (
            "cargo build --release --target \"${MUSL_TARGET}\" --manifest-path test-fixtures/mock-claude/Cargo.toml",
            "the mock-claude fixture's musl build",
        ),
        (VERIFY_MAIN, "the main binary's static-linkage gate"),
        (VERIFY_MOCK, "the fixture's static-linkage gate"),
        ("MAX_SIZE_BYTES=$((10 * 1024 * 1024))", "the 10 MiB size cap"),
        (VERIFY_ONLY_EXIT, "the verify-only green exit"),
        (
            "sha256sum \"${CLAUDE_PRINT_ASSET}\" \"${MOCK_ASSET}\" last-claude-version.txt > sha256sums.txt",
            "the release manifest",
        ),
    ];
    let mut cursor = 0usize;
    for (needle, what) in chain {
        let at = position_uncommented(&template[cursor..], needle)
            .map(|i| i + cursor)
            .unwrap_or_else(|| {
                panic!(
                    "the WorkflowTemplate must carry {what} ({needle:?}) after the \
                     previous stage — the artifact gates only protect a push if \
                     they run BEFORE the verify-only exit"
                )
            });
        cursor = at + needle.len();
    }

    // --- The release-only prelude: guarded, and idempotency before build
    // time (the runbook's ordering — a re-run of an already-published
    // release exits without rebuilding). ---
    let (guard_at, prelude) = release_only_prelude(template);
    assert!(
        prelude.contains("gh release view \"v${VERSION}\""),
        "the draft/publish idempotency check must live inside the release-only \
         guard"
    );
    let build_at = position_uncommented(
        template,
        "cargo build --release --target \"${MUSL_TARGET}\" --bin claude-print",
    )
    .expect("the main musl build");
    let view_at = guard_at + prelude.find("gh release view").expect("idempotency check");
    assert!(
        view_at < build_at,
        "the idempotency check must precede the musl build — a re-run of an \
         already-published release exits before spending build time"
    );
    assert!(
        guard_at + prelude.len() < build_at,
        "the release-only guard must close before the artifact build — the build \
         is the shared part that verify-only runs too"
    );

    // --- And nothing publication-shaped may follow the verify-only exit
    // on the shared path: the exit precedes the manifest and the upload. ---
    let exit_at =
        position_uncommented(template, VERIFY_ONLY_EXIT).expect("the verify-only green exit");
    for publication in [
        "gh release create \"v${VERSION}\"",
        "sha256sum \"${CLAUDE_PRINT_ASSET}\" \"${MOCK_ASSET}\" last-claude-version.txt > sha256sums.txt",
    ] {
        let at = position_uncommented(template, publication)
            .unwrap_or_else(|| panic!("the WorkflowTemplate must carry {publication:?}"));
        assert!(
            exit_at < at,
            "the verify-only exit must precede {publication:?} — a plain push \
             uploads nothing"
        );
    }
}

/// The docs half: the runbook's mode/gate claims and the README's
/// supported-platform paragraph stay aligned with the both-modes gates.
fn check_docs_alignment(readme: &str, runbook: &str) {
    let runbook_norm = normalized(runbook);

    // The runbook's verify-only bullet: builds the artifacts, gates them,
    // uploads nothing.
    assert!(
        runbook_norm.contains("builds the release artifacts for the documented target"),
        "the runbook's verify-only bullet must say the artifacts are built on a \
         plain push — the behavior this suite pins"
    );
    assert!(
        runbook_norm.contains("*without* uploading anything"),
        "the runbook's verify-only bullet must state nothing is uploaded"
    );
    // The idempotency-preserved claim for the prelude reorder.
    assert!(
        runbook_norm.contains("still exits before rebuilding"),
        "the runbook must keep the idempotency-before-build claim the prelude \
         guard preserves"
    );
    // The gates run on every push, and the pin is discoverable.
    assert!(
        runbook_norm.contains("verify-only mode too"),
        "the runbook's build-gates paragraph must state the gates run in \
         verify-only mode too"
    );
    assert!(
        runbook_norm.contains("tests/musl_artifact_verification.rs"),
        "the runbook must name this suite as the pin for the gate's logic"
    );

    // The README's Supported platforms section: the artifact gates are
    // part of what the matrix promises, and the pin is named.
    let section_start = readme
        .find("### Supported platforms")
        .expect("README.md must carry a `### Supported platforms` heading");
    let rest = &readme[section_start..];
    let section_end = rest[1..].find("\n#").map(|i| i + 1).unwrap_or(rest.len());
    let section = normalized(&rest[..section_end]);
    assert!(
        section.contains("verify_static"),
        "the README's Supported platforms section must name the static-linkage gate"
    );
    assert!(
        section.contains("verify-only mode as well as release mode"),
        "the README's Supported platforms section must state the artifact gates \
         run on every push, not only at release time"
    );
    assert!(
        section.contains("tests/musl_artifact_verification.rs"),
        "the README's Supported platforms section must name this suite"
    );
}

// ---------------------------------------------------------------------------
// The executable half: run the template's own gate body under bash with a
// stubbed ldd (the install_sh_arch PATH-stub pattern).
// ---------------------------------------------------------------------------

/// Symlink the real tools the gate body needs (resolved from the test
/// process's PATH at runtime — NixOS hosts have no /usr/bin) into the
/// stub bin dir, alongside the test's `ldd` stub. Single-entry PATH, so
/// the real host `ldd` is genuinely unreachable.
fn link_coreutils(bin: &Path) {
    fs::create_dir_all(bin).expect("create the stub bin dir");
    for tool in ["bash", "grep", "cat"] {
        let dest = bin.join(tool);
        if dest.symlink_metadata().is_ok() {
            continue;
        }
        let real = std::env::var_os("PATH")
            .and_then(|path| {
                std::env::split_paths(&path)
                    .map(|dir| dir.join(tool))
                    .find(|p| p.exists())
            })
            .unwrap_or_else(|| panic!("{tool} must be on PATH to build the stub bin dir"));
        std::os::unix::fs::symlink(&real, &dest).expect("symlink the real coreutil");
    }
}

/// Execute `function` (the extracted `verify_static` body, or a mutated
/// copy in the meta-tests) against a stubbed `ldd` that prints `payload`
/// and exits `ldd_exit`. With `stderr_marker` the payload is printed to
/// the stub's stderr instead of stdout — the shape the gate's `2>&1`
/// capture exists for. The harness runs under `set -e`, CI's own regime,
/// so an ungated failing command aborts exactly as it would in the pod.
/// A sentinel echo follows the invocation: a gate that passes runs on to
/// it, a gate that fails never reaches it.
fn run_gate(
    function: &str,
    payload: &str,
    ldd_exit: i32,
    stderr_marker: bool,
) -> (i32, String, String) {
    let dir = tempfile::tempdir().expect("tempdir for the gate run");
    let bin = dir.path().join("bin");
    link_coreutils(&bin);

    // The ldd stub: payload on stdout (or stderr with stderr_marker),
    // scripted exit status.
    let emit = if stderr_marker {
        "cat \"$LD_PAYLOAD\" >&2"
    } else {
        "cat \"$LD_PAYLOAD\""
    };
    let ldd = bin.join("ldd");
    fs::write(
        &ldd,
        format!("#!/usr/bin/env bash\n{emit}\nexit {ldd_exit}\n"),
    )
    .expect("write the ldd stub");
    fs::set_permissions(&ldd, fs::Permissions::from_mode(0o755)).expect("chmod the ldd stub");
    let payload_path = dir.path().join("ldd-output.txt");
    fs::write(&payload_path, payload).expect("write the ldd payload");

    // The binary under inspection — content irrelevant, ldd is stubbed.
    let fake = dir.path().join("fakebin");
    fs::write(&fake, "#!/usr/bin/env bash\ntrue\n").expect("write the fake binary");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).expect("chmod the fake binary");

    let script = dir.path().join("gate.sh");
    fs::write(
        &script,
        format!("set -e\n{function}\nverify_static ./fakebin\necho GATE-COMPLETED\n"),
    )
    .expect("write the gate script");

    let output = Command::new("bash")
        .arg(&script)
        .env("PATH", &bin)
        .env("LD_PAYLOAD", &payload_path)
        .current_dir(dir.path())
        .output()
        .expect("run the extracted verify_static gate");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// A realistic dynamic-linkage report — the output shape that must be
/// REJECTED even though ldd exits 0 printing it.
const DYNAMIC_LDD_OUTPUT: &str = "\
linux-vdso.so.1 (0x00007ffd13b5c000)
libc.so.6 => /lib/x86_64-linux-gnu/libc.so.6 (0x00007f2e4a9a0000)
/lib64/ld-linux-x86-64.so.2 (0x00007f2e4abc0000)
";

#[test]
fn the_templates_verify_static_gate_decides_by_marker_not_exit_status() {
    let body = verify_static_body(&repo_file(CI_TEMPLATE));

    // Both static markers pass — on stdout — and the gate says so.
    for marker in ["statically linked", "not a dynamic executable"] {
        let (code, stdout, _) = run_gate(&body, &format!("\t{marker}\n"), 0, false);
        assert_eq!(
            code, 0,
            "a static binary whose ldd reports {marker:?} must pass the gate"
        );
        assert!(
            stdout.contains("OK: ./fakebin is statically linked"),
            "the pass must be visible on stdout: {stdout}"
        );
        assert!(
            stdout.contains("GATE-COMPLETED"),
            "a passing gate must let the script continue: {stdout}"
        );
    }

    // The marker may arrive on ldd's STDERR — the `2>&1` capture is what
    // keeps the gate working when it does.
    let (code, _, _) = run_gate(&body, "not a dynamic executable\n", 0, true);
    assert_eq!(
        code, 0,
        "a static marker on ldd's stderr must still pass — strip the 2>&1 and \
         this is the case that breaks"
    );

    // A dynamic dependency list fails — exit 1, HR-1 error on stderr,
    // nothing after the gate runs.
    let (code, stdout, stderr) = run_gate(&body, DYNAMIC_LDD_OUTPUT, 0, false);
    assert_eq!(
        code, 1,
        "a dynamically linked binary must fail the gate — ldd exiting 0 changes \
         nothing, the markers decide"
    );
    assert!(
        stderr.contains("NOT statically linked") && stderr.contains("HR-1"),
        "the failure must carry the HR-1 error on stderr: {stderr}"
    );
    assert!(
        !stdout.contains("GATE-COMPLETED"),
        "a failing gate must stop the script: {stdout}"
    );

    // Empty ldd output fails too — no marker, no proof.
    let (code, _, _) = run_gate(&body, "", 0, false);
    assert_eq!(code, 1, "empty ldd output must fail the gate");

    // The exit-status trap: a static marker arriving on a FAILING ldd
    // still passes — the `|| true` swallow is what keeps set -e from
    // aborting the script before the grep can judge the marker.
    let (code, _, _) = run_gate(&body, "not a dynamic executable\n", 3, false);
    assert_eq!(
        code, 0,
        "a static marker on a failing ldd must still pass — the gate trusts the \
         marker, never the exit status"
    );
}

#[test]
fn ci_builds_and_gates_the_documented_target_in_both_modes() {
    check_template_gates(&repo_file(CI_TEMPLATE));
}

#[test]
fn docs_keep_the_artifact_gate_claims_aligned() {
    check_docs_alignment(
        &repo_file("README.md"),
        &repo_file("docs/notes/release-runbook.md"),
    );
}

// ---------------------------------------------------------------------------
// Negative meta-tests (the claudepr-4d967120 pattern): each guarded input
// mutated in memory must fail the owning check naming the drift, proving
// the guard non-vacuous on every run.
// ---------------------------------------------------------------------------

fn assert_drift<F>(check: F, expected: &[&str])
where
    F: FnOnce() + std::panic::UnwindSafe,
{
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(check);
    std::panic::set_hook(previous_hook);
    let message = match result {
        Ok(()) => panic!("the mutated input passed; this drift check is vacuous"),
        Err(payload) => payload
            .downcast_ref::<&str>()
            .map(|m| (*m).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string()),
    };
    for fragment in expected {
        assert!(
            message.contains(fragment),
            "the check failed, but not for the planted drift:\n{message}\nmissing \
             fragment: {fragment:?}"
        );
    }
}

fn replaced_once(document: &str, from: &str, to: &str) -> String {
    assert_eq!(
        document.matches(from).count(),
        1,
        "the meta-test input {from:?} must occur exactly once"
    );
    document.replacen(from, to, 1)
}

#[test]
fn negative_meta_tests_keep_the_artifact_gate_guard_non_vacuous() {
    let template = repo_file(CI_TEMPLATE);

    // Gut the marker alternation → the body pin fails.
    assert_drift(
        || {
            check_template_gates(&replaced_once(
                &template,
                "grep -qE 'statically linked|not a dynamic executable'",
                "grep -qE 'statically linked'",
            ));
        },
        &["BOTH static markers"],
    );

    // Trust ldd's exit status (drop the swallow) → the body pin fails.
    assert_drift(
        || {
            check_template_gates(&replaced_once(
                &template,
                "out=\"$(ldd \"$1\" 2>&1 || true)\"",
                "out=\"$(ldd \"$1\" 2>&1)\"",
            ));
        },
        &["swallowing its exit status"],
    );

    // Drop the failing exit → the gate becomes advisory.
    assert_drift(
        || {
            check_template_gates(&replaced_once(
                &template,
                "echo \"ERROR: $1 is NOT statically linked — violates HR-1\" >&2\n                exit 1\n",
                "echo \"ERROR: $1 is NOT statically linked — violates HR-1\" >&2\n",
            ));
        },
        &["exit 1"],
    );

    // Comment out the fixture's invocation → one asset ungated.
    assert_drift(
        || {
            check_template_gates(&replaced_once(
                &template,
                &format!("\n            {VERIFY_MOCK}\n"),
                &format!("\n            # {VERIFY_MOCK}\n"),
            ));
        },
        &[VERIFY_MOCK],
    );

    // Unguard the prelude → a verify-only run would push a tag. The
    // guard line also appears in the clone arm, so anchor the surgery on
    // the prelude comment directly above it.
    assert_drift(
        || {
            check_template_gates(&replaced_once(
                &template,
                "# exits without rebuilding).\n            if [ -n \"$TAG\" ]; then\n",
                "# exits without rebuilding).\n",
            ));
        },
        &["the tag push must live inside"],
    );

    // Regress to release-only gates: move the verify-only exit in front
    // of the artifact build (the pre-bead shape).
    let verify_exit_block = "\
            # In verify-only mode (TAG empty), skip release creation after
            # quality gates AND artifact gates: the documented target has
            # been built and statically verified, nothing is uploaded.
            if [ -z \"$TAG\" ]; then
              echo \"Verify-only mode: all quality gates passed, exiting without release\"
              exit 0
            fi

";
    let build_comment = "            # Build the documented release target on EVERY invocation —";
    let regressed = template.replacen(verify_exit_block, "", 1).replacen(
        build_comment,
        &format!("{verify_exit_block}{build_comment}"),
        1,
    );
    assert_ne!(
        regressed, template,
        "the regression mutation must change the template"
    );
    assert_drift(
        || check_template_gates(&regressed),
        &["verify-only green exit"],
    );

    // Docs drift: the runbook stops claiming the every-push build.
    let runbook = repo_file("docs/notes/release-runbook.md");
    assert_drift(
        || {
            check_docs_alignment(
                &repo_file("README.md"),
                &normalized(&runbook).replacen(
                    "builds the release artifacts for the documented target",
                    "runs every quality gate",
                    1,
                ),
            )
        },
        &["artifacts are built on a plain push"],
    );

    // Docs drift: the README drops the suite citation.
    assert_drift(
        || {
            check_docs_alignment(
                &replaced_once(
                    &repo_file("README.md"),
                    "tests/musl_artifact_verification.rs",
                    "the CI WorkflowTemplate",
                ),
                &runbook,
            )
        },
        &["Supported platforms section must name this suite"],
    );

    // Execution-level vacuity proof: with the grep gutted to match
    // anything, the dynamic case no longer fails — demonstrating the
    // dynamic payload genuinely flows through the real grep decision.
    let gutted = verify_static_body(&template).replacen(
        "grep -qE 'statically linked|not a dynamic executable'",
        "grep -qE '.*'",
        1,
    );
    let (code, _, _) = run_gate(&gutted, DYNAMIC_LDD_OUTPUT, 0, false);
    assert_eq!(
        code, 0,
        "the dynamic case must pass against a gutted gate — if it failed anyway, \
         the case would not be exercising the grep and its real-run failure \
         would prove nothing"
    );
}
