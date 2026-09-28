//! Documentation-drift guard for the required formatting and lint gates.
//!
//! AGENTS.md's build fence is the copy-paste contract for contributors, while
//! the vendored WorkflowTemplate is the command surface CI actually executes.
//! Keep the two aligned on the exact rustfmt and Clippy commands, the explicit
//! component installation, and the wrapper's important split: only `cargo
//! test` is offloaded to `rust-verify`; the quality gates run locally through
//! the resource-limited Cargo wrapper (claudepr-373e38da).
//!
//! This is library-level and hermetic: it reads repository files only and
//! spawns nothing. The negative meta-tests mutate strings in memory so a
//! missing command, commented-out CI step, or rewritten wrapper expectation
//! cannot make the guard pass vacuously.

use std::fs;
use std::path::{Path, PathBuf};

const BUILD_COMMANDS_HEADING: &str = "## Build commands";
const FMT_COMMAND: &str = "cargo fmt --check";
const CLIPPY_COMMAND: &str = "cargo clippy --all-targets -- -D warnings";
const CI_WORKFLOW_FILE: &str = "claude-print-ci-workflowtemplate.yml";
const RUST_COMPONENTS_COMMAND: &str = "rustup component add rustfmt clippy";
const WRAPPER_SCOPE: &str =
    "The Cargo wrapper applies the shared target-directory and cgroup limits to";
const WRAPPER_REMOTE_SPLIT: &str = "but only `cargo test` is submitted to the `rust-verify`";
const LOCAL_GATE_EXPECTATION: &str = "`cargo fmt --check` and `cargo clippy --all-targets -- -D";
const LOCAL_GATE_CONTINUATION: &str = "warnings` therefore run locally";
const CI_EXPECTATION: &str =
    "CI installs the\n`rustfmt` and `clippy` components and runs these exact commands";

const ROOT_PROBES: [&str; 2] = ["AGENTS.md", "Cargo.toml"];

fn repo_root() -> PathBuf {
    let override_root = std::env::var("CLAUDE_PRINT_TEST_REPO").ok();
    let runtime_manifest = std::env::var("CARGO_MANIFEST_DIR").ok();
    let baked_manifest = env!("CARGO_MANIFEST_DIR");

    if let Some(root) = override_root.as_deref() {
        if is_repo_root(Path::new(root)) {
            return PathBuf::from(root);
        }
        panic!(
            "$CLAUDE_PRINT_TEST_REPO={root:?} is not a claude-print checkout; +             expected {:?} and {:?}",
            ROOT_PROBES[0],
            ROOT_PROBES[1]
        );
    }

    for candidate in [runtime_manifest.as_deref(), Some(baked_manifest)]
        .into_iter()
        .flatten()
    {
        let path = Path::new(candidate);
        if is_repo_root(path) {
            return path.to_path_buf();
        }
    }

    panic!(
        "no candidate repo root is a claude-print checkout; expected {:?} and {:?}",
        ROOT_PROBES[0], ROOT_PROBES[1]
    );
}

fn is_repo_root(path: &Path) -> bool {
    ROOT_PROBES.iter().all(|probe| path.join(probe).is_file())
}

fn repo_file(relative: &str) -> String {
    fs::read_to_string(repo_root().join(relative))
        .unwrap_or_else(|error| panic!("reading {relative} from the checkout: {error}"))
}

/// Return the markdown section beginning at `heading`, stopping at the next
/// heading outside a fenced block. The build section contains a bash fence,
/// so heading detection must not mistake shell comments for markdown.
fn section_after_heading(document: &str, heading: &str) -> String {
    let lines: Vec<&str> = document.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.trim() == heading)
        .unwrap_or_else(|| panic!("AGENTS.md must keep the {heading:?} heading"));

    let mut in_fence = false;
    let mut end = lines.len();
    for (index, line) in lines.iter().enumerate().skip(start + 1) {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        let hash_count = line.len() - line.trim_start_matches('#').len();
        if !in_fence
            && (1..=6).contains(&hash_count)
            && (line[hash_count..].is_empty() || line[hash_count..].starts_with([' ', '\t']))
        {
            end = index;
            break;
        }
    }
    lines[start..end].join("\n")
}

fn fenced_lines(section: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut in_fence = false;
    for line in section.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        } else if in_fence {
            lines.push(line);
        }
    }
    lines
}

fn executed_lines(document: &str) -> Vec<&str> {
    document
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            (!trimmed.is_empty() && !trimmed.starts_with('#')).then_some(trimmed)
        })
        .collect()
}

fn check_agents_quality_gates(document: &str) {
    let section = section_after_heading(document, BUILD_COMMANDS_HEADING);
    let fence = fenced_lines(&section);
    assert!(
        fence.iter().any(|line| line.trim() == FMT_COMMAND),
        "the {BUILD_COMMANDS_HEADING:?} command fence must carry the exact +         `{FMT_COMMAND}` line"
    );
    assert!(
        fence.iter().any(|line| line.trim() == CLIPPY_COMMAND),
        "the {BUILD_COMMANDS_HEADING:?} command fence must carry the exact +         `{CLIPPY_COMMAND}` line"
    );
    assert!(
        section.contains(WRAPPER_SCOPE),
        "the build guidance must preserve the wrapper resource-limit +         expectation anchored by {WRAPPER_SCOPE:?}"
    );
    assert!(
        section.contains(WRAPPER_REMOTE_SPLIT),
        "the build guidance must say that only the test command is submitted +         remotely ({WRAPPER_REMOTE_SPLIT:?})"
    );
    assert!(
        section.contains(LOCAL_GATE_EXPECTATION)
            && section.contains(LOCAL_GATE_CONTINUATION),
        "the build guidance must say that both quality gates run locally, +         rather than promising remote offload"
    );
    assert!(
        section.contains(CI_EXPECTATION),
        "the build guidance must preserve the CI component-install and exact-command +         expectation ({CI_EXPECTATION:?})"
    );
}

fn check_ci_quality_gates(document: &str) {
    let lines = executed_lines(document);
    let component_index = lines
        .iter()
        .position(|line| *line == RUST_COMPONENTS_COMMAND)
        .unwrap_or_else(|| {
            panic!(
                "{CI_WORKFLOW_FILE} must execute `{RUST_COMPONENTS_COMMAND}` before +                 its quality gates"
            )
        });
    let fmt_index = lines
        .iter()
        .position(|line| *line == FMT_COMMAND)
        .unwrap_or_else(|| {
            panic!(
                "{CI_WORKFLOW_FILE} must execute the exact `{FMT_COMMAND}` gate; +                 an echo or comment is not a gate"
            )
        });
    let clippy_index = lines
        .iter()
        .position(|line| *line == CLIPPY_COMMAND)
        .unwrap_or_else(|| {
            panic!(
                "{CI_WORKFLOW_FILE} must execute the exact `{CLIPPY_COMMAND}` gate; +                 an echo or comment is not a gate"
            )
        });
    let tests_index = lines
        .iter()
        .position(|line| line.starts_with("cargo test --tests"))
        .unwrap_or_else(|| {
            panic!(
                "{CI_WORKFLOW_FILE} must keep its test legs after the fmt and +                 clippy quality gates"
            )
        });

    assert!(
        component_index < fmt_index,
        "{CI_WORKFLOW_FILE} must install rustfmt/clippy before cargo fmt"
    );
    assert!(
        fmt_index < clippy_index && clippy_index < tests_index,
        "{CI_WORKFLOW_FILE} quality-gate order must be fmt, clippy, then tests"
    );
}

#[test]
fn agents_documents_the_required_quality_gates_and_wrapper_split() {
    check_agents_quality_gates(&repo_file("AGENTS.md"));
}

#[test]
fn ci_runs_the_same_quality_gates_after_installing_components() {
    check_ci_quality_gates(&repo_file(CI_WORKFLOW_FILE));
}

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
            .map(|message| (*message).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string()),
    };
    for fragment in expected {
        assert!(
            message.contains(fragment),
            "the check failed, but not for the planted drift:\n{message}\n+             missing fragment: {fragment:?}"
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
fn negative_meta_tests_keep_the_quality_gate_guard_non_vacuous() {
    let agents = repo_file("AGENTS.md");
    let section = section_after_heading(&agents, BUILD_COMMANDS_HEADING);
    assert_drift(
        || {
            check_agents_quality_gates(&replaced_once(
                &agents,
                "\ncargo fmt --check\n",
                "\n# cargo fmt --check\n",
            ));
        },
        &["cargo fmt --check"],
    );
    assert_drift(
        || {
            check_agents_quality_gates(&replaced_once(
                &agents,
                "\ncargo clippy --all-targets -- -D warnings\n",
                "\n# cargo clippy --all-targets -- -D warnings\n",
            ));
        },
        &["cargo clippy --all-targets -- -D warnings"],
    );
    assert_drift(
        || {
            check_agents_quality_gates(&replaced_once(
                &agents,
                WRAPPER_REMOTE_SPLIT,
                "the test command is always submitted remotely",
            ));
        },
        &["only the test command is submitted"],
    );
    assert!(section.contains(FMT_COMMAND));

    let workflow = repo_file(CI_WORKFLOW_FILE);
    assert_drift(
        || {
            check_ci_quality_gates(&replaced_once(
                &workflow,
                "\n            cargo fmt --check\n",
                "\n            # cargo fmt --check\n",
            ));
        },
        &["must execute the exact `cargo fmt --check` gate"],
    );
    assert_drift(
        || {
            check_ci_quality_gates(&replaced_once(
                &workflow,
                "\n            cargo clippy --all-targets -- -D warnings\n",
                "\n            cargo clippy --all-targets -- -D\n",
            ));
        },
        &["must execute the exact `cargo clippy --all-targets -- -D warnings` gate"],
    );
    assert_drift(
        || {
            check_ci_quality_gates(&replaced_once(
                &workflow,
                "\n            rustup component add rustfmt clippy\n",
                "\n            # rustup component add rustfmt clippy\n",
            ));
        },
        &["must execute `rustup component add rustfmt clippy`"],
    );
}
