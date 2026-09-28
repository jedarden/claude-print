//! Forgejo source-of-truth release-workflow pin (bead claudepr-c7ca74a6).
//!
//! The README's [Repository & contributions] section and
//! `docs/notes/release-runbook.md` state the provenance split — Forgejo is
//! the canonical repository and the sole push target; the GitHub repo is a
//! read-only push mirror; GitHub Releases is the artifact host. The
//! existing pins bind the *paper* to the *machinery*:
//! `tests/release_runbook_docs.rs` pins the runbook to the WorkflowTemplate
//! (publication order included), `tests/install_sh_release_source.rs` pins
//! the publisher slug to the installer's default, and
//! `tests/platform_matrix_docs.rs` pins the asset matrix. What nothing
//! pinned are the four invariants that keep GitHub a downstream-only
//! surface — the ones that fail not as doc drift but as *direction* drift:
//!
//! - **No `.github/` CI surface in the tree.** GitHub Actions are disabled
//!   org-wide and CI runs on the Argo trio (WorkflowTemplate + Sensor +
//!   EventSource stanza). The org-rule guard keeps *agents on one host*
//!   from writing `.github/workflows/*`; nothing kept the *repository*
//!   from carrying one, and a committed workflow file is exactly how the
//!   "GitHub is downstream" story starts accreting exceptions.
//! - **The WorkflowTemplate never moves refs to GitHub.** Every `git push`
//!   and every `git clone` in it must name Forgejo; its only GitHub writes
//!   are the `gh release` asset calls plus their credential. A template
//!   that starts pushing refs to the mirror would reintroduce the
//!   mirror-prune failure mode from the wrong side — refs born on GitHub
//!   are pruned at the next sync.
//! - **The CI trigger reaches Argo only through the read-only mirror's
//!   webhook.** The Sensor's subscription rides the `github-webhooks`
//!   EventSource (pinned by `tests/contract_maintenance.rs`); what that
//!   implies and nothing else states: the mirror repo must carry the push
//!   webhook, so the trigger is itself mirror traffic — one more reason
//!   nothing may publish to GitHub directly.
//! - **The live-host half is owned by a repo script.** The checkout's own
//!   remote configuration, the Forgejo-side mirror direction, and the tag
//!   namespaces of both hosts exist only at runtime — no hermetic test can
//!   see them. `scripts/check-release-provenance.sh` verifies all four
//!   areas against the real hosts (origin, mirror, tags, publishing) and
//!   the runbook's §"Provenance verification" wires it into the operator
//!   procedure; this suite pins the script's structure and its doc wiring
//!   so the verifier cannot quietly rot.
//!
//! Every leg is mutation-checked by always-on negative meta-tests (the
//! committed `tests/docs_build_commands.rs` pattern): each guarded input is
//! mutated in memory on every run and the owning check must fail naming the
//! drift.
//!
//! Library-level: reads the tree, the WorkflowTemplate, the Sensor, the
//! EventSource stanza, the runbook, and the script; spawns nothing.

use std::fs;
use std::path::{Path, PathBuf};

/// Repo-root probes: a directory holding both is a claude-print checkout.
const ROOT_PROBES: [&str; 2] = ["AGENTS.md", "Cargo.toml"];

/// The WorkflowTemplate — the only sanctioned CI surface and the one place
/// that names both hosts for what they are.
const TEMPLATE: &str = "claude-print-ci-workflowtemplate.yml";

/// The push-trigger Sensor and its EventSource stanza fragment.
const SENSOR: &str = "claude-print-ci-sensor.yml";
const STANZA: &str = "claude-print-eventsource-stanza.yml";

/// The runbook whose §"Provenance verification" wires the live verifier in.
const RUNBOOK: &str = "docs/notes/release-runbook.md";

/// The live provenance verifier this suite pins.
const SCRIPT: &str = "scripts/check-release-provenance.sh";

/// The canonical Forgejo URL — the same URL `tests/install_sh_release_source.rs`
/// pins as the workflow's clone source and the installer's slug derivation.
/// Two readers carrying it independently is the pin.
const FORGEJO_URL: &str = "https://git.ardenone.com/jedarden/claude-print.git";

/// The GitHub slug — the artifact host the workflow publishes assets to and
/// the mirror the trigger rides.
const GH_SLUG: &str = "jedarden/claude-print";

/// The webhook path the mirror's push webhook points at — the trigger's
/// only ingress into Argo.
const MIRROR_WEBHOOK_URL: &str = "https://webhooks-ci.ardenone.com/claude-print";

/// This suite's own name as the runbook's Hermetic-coverage table must
/// cite it.
const THIS_SUITE: &str = "tests/release_provenance.rs";

/// Where repo-relative files are read from, resolved at *runtime* — never
/// the bare compile-time `env!("CARGO_MANIFEST_DIR")`, which bakes the
/// building checkout's path into the test binary. The local cargo wrapper
/// maps `.git`-less `git archive` extractions onto one shared target dir,
/// so an extraction of unchanged content instant-reuses a cached test
/// binary compiled in an extraction that has since been deleted; a
/// baked-only root then fails every later run of that binary with
/// file-NotFound panics that have nothing to do with drift (the same chain
/// as `tests/release_runbook_docs.rs`). Candidates, most authoritative
/// first, each probe-verified before use:
///
/// 1. `$CLAUDE_PRINT_TEST_REPO` — explicit override for direct binary runs.
/// 2. the runtime `CARGO_MANIFEST_DIR` cargo sets in the test process.
/// 3. the compile-time `CARGO_MANIFEST_DIR` — last resort.
fn repo_root() -> PathBuf {
    resolve_repo_root(
        std::env::var("CLAUDE_PRINT_TEST_REPO").ok().as_deref(),
        std::env::var("CARGO_MANIFEST_DIR").ok().as_deref(),
        env!("CARGO_MANIFEST_DIR"),
    )
    .unwrap_or_else(|e| panic!("locating the repo root to read repo files from: {e}"))
}

/// [`repo_root`]'s candidate chain as a pure function.
fn resolve_repo_root(
    env_override: Option<&str>,
    runtime_manifest: Option<&str>,
    baked_manifest: &str,
) -> Result<PathBuf, String> {
    if let Some(override_root) = env_override {
        if is_repo_root(Path::new(override_root)) {
            return Ok(PathBuf::from(override_root));
        }
        return Err(format!(
            "$CLAUDE_PRINT_TEST_REPO={override_root:?} is set but not a claude-print \
             checkout (probe: {:?} + {:?})",
            ROOT_PROBES[0], ROOT_PROBES[1]
        ));
    }
    let mut chain: Vec<(&str, &str)> = vec![("compile-time", baked_manifest)];
    if let Some(runtime) = runtime_manifest {
        if runtime != baked_manifest {
            chain.insert(0, ("runtime", runtime));
        }
    }
    let mut rejected = Vec::new();
    for (origin, candidate) in chain {
        let path = Path::new(candidate);
        if is_repo_root(path) {
            return Ok(path.to_path_buf());
        }
        rejected.push(format!("{origin} CARGO_MANIFEST_DIR={}", path.display()));
    }
    Err(format!(
        "no candidate repo root is a claude-print checkout (probe: {:?} + {:?}): {}",
        ROOT_PROBES[0],
        ROOT_PROBES[1],
        rejected.join("; ")
    ))
}

/// Whether `p` holds this suite's root probes.
fn is_repo_root(p: &Path) -> bool {
    ROOT_PROBES.iter().all(|f| p.join(f).is_file())
}

/// Read a repo file from the checkout under test.
fn repo_file(relative: &str) -> String {
    fs::read_to_string(repo_root().join(relative))
        .unwrap_or_else(|e| panic!("read {relative} from the checkout under test: {e}"))
}

/// The document with every whitespace run collapsed to one space, so prose
/// assertions survive line re-wrapping.
fn normalized(doc: &str) -> String {
    doc.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ── The `.github/` surface ──────────────────────────────────────────────────

/// Every path under `root`, as `/`-separated strings relative to it. Build
/// output (`target/`) and the live checkout's `.git/` are skipped: the
/// invariant is about the *committed* tree, which is exactly what a
/// `git archive` extraction (the CI/close-gate shape, no `target/`, no
/// `.git/`) carries.
fn tree_paths(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => panic!("walk {} under the checkout under test: {e}", dir.display()),
        };
        for entry in entries {
            let entry = entry
                .unwrap_or_else(|e| panic!("read a directory entry under {}: {e}", dir.display()));
            let name = entry.file_name();
            let name = name.to_string_lossy().into_owned();
            if name == ".git" || name == "target" {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or_else(|e| panic!("relativize {}: {e}", path.display()))
                    .to_string_lossy()
                    .into_owned();
                out.push(rel);
            }
        }
    }
    out.sort();
    out
}

/// The `.github/`-surface check over an in-memory path list — split from
/// the walk so the negative meta-test can plant a workflow file without
/// touching the filesystem.
fn assert_no_github_surface(paths: &[String]) {
    let offenders: Vec<String> = paths
        .iter()
        .filter(|p| {
            Path::new(p)
                .components()
                .any(|c| c.as_os_str() == ".github")
        })
        .cloned()
        .collect();
    assert!(
        offenders.is_empty(),
        "the tree carries a `.github/` surface ({}) — GitHub Actions are \
         disabled org-wide and CI is the Argo trio (WorkflowTemplate + \
         Sensor + EventSource stanza); a committed GitHub workflow is a \
         direct-publishing surface the Forgejo source-of-truth workflow \
         forbids",
        offenders.join(", ")
    );
}

#[test]
fn no_github_ci_surface_exists_in_the_tree() {
    let root = repo_root();
    let paths = tree_paths(&root);
    assert!(
        !paths.is_empty(),
        "the checkout under test walked to zero files — wrong root?"
    );
    assert_no_github_surface(&paths);
}

// ── The WorkflowTemplate's GitHub-write surface ─────────────────────────────

/// The line indices (inclusive) of the release-notes string passed to
/// `gh release create` — the one place a GitHub URL is legitimate prose
/// (the install one-liner users paste), because it ships to users rather
/// than executes in CI.
fn notes_fence_bounds(template: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = template.lines().collect();
    let start = lines.iter().position(|l| l.contains("--notes \""))?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.trim() == "\" \\")
        .map(|offset| start + 1 + offset)?;
    Some((start, end))
}

/// Every line of the WorkflowTemplate that mentions GitHub outside a
/// sanctioned context. Sanctioned: comments (prose, and a commented-out
/// invocation is still caught by [`template_ref_moves_off_forgejo`], which
/// deliberately does *not* exempt comments), the gh CLI apt bootstrap, the
/// release-secret references, `gh release` calls (the sanctioned asset
/// channel, in whatever spelling including a full GitHub URL), and the
/// release-notes fence.
fn unsanctioned_github_lines(template: &str) -> Vec<String> {
    let fence = notes_fence_bounds(template).unwrap_or_else(|| {
        panic!(
            "the release-notes fence (--notes \" … \" \\) could not be located \
             in the WorkflowTemplate — the publication step changed shape; \
             update notes_fence_bounds with it before trusting this scan"
        )
    });
    let mut out = Vec::new();
    for (i, line) in template.lines().enumerate() {
        if fence.0 <= i && i <= fence.1 {
            continue;
        }
        let trimmed = line.trim();
        if !trimmed.to_ascii_lowercase().contains("github") {
            continue;
        }
        let sanctioned = trimmed.starts_with('#')
            || trimmed.contains("githubcli-archive-keyring.gpg")
            || trimmed.contains("github-cli.list")
            || trimmed.contains("github-webhook-secret")
            || trimmed.contains("gh release")
            || trimmed.contains(&format!("--repo {GH_SLUG}"))
            || trimmed.contains("GH_TOKEN");
        if !sanctioned {
            out.push(format!("line {}: {trimmed}", i + 1));
        }
    }
    out
}

/// Whether the (comment-stripped) line opens with the `git <sub>`
/// invocation — word-boundary checked, so prose like "git pushed tags are
/// pruned" does not read as an invocation.
fn opens_invocation(line: &str, sub: &str) -> bool {
    let mut words = line.split_whitespace();
    words.next() == Some("git") && words.next() == Some(sub)
}

/// Every `git push` / `git clone` in the WorkflowTemplate that moves refs
/// to or from anywhere but Forgejo. Comments are deliberately NOT exempt:
/// a commented-out GitHub push satisfies nothing (the
/// `tests/docs_build_commands.rs` posture), so a leading `#` is stripped
/// before the invocation check.
fn template_ref_moves_off_forgejo(template: &str) -> Vec<String> {
    let lines: Vec<&str> = template.lines().collect();
    let mut violations = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let effective = trimmed
            .strip_prefix('#')
            .map(str::trim_start)
            .unwrap_or(trimmed);
        if opens_invocation(effective, "push")
            && (!line.contains("git.ardenone.com") || line.to_ascii_lowercase().contains("github"))
        {
            violations.push(format!("line {}: {trimmed}", i + 1));
        }
        if opens_invocation(effective, "clone") {
            if line.to_ascii_lowercase().contains("github") {
                violations.push(format!("line {}: {trimmed}", i + 1));
            }
            // The clone URL sits on the wrapped continuation line — but a
            // commented-out clone's continuation is prose, not code, so the
            // continuation check applies only to live invocations.
            if !trimmed.starts_with('#') {
                if let Some(next) = lines.get(i + 1) {
                    if next.to_ascii_lowercase().contains("github") {
                        violations.push(format!("line {}: {next}", i + 2));
                    } else if !next.contains("git.ardenone.com") {
                        violations.push(format!(
                            "line {}: clone URL is not the Forgejo source of truth: {}",
                            i + 2,
                            next.trim()
                        ));
                    }
                }
            }
        }
    }
    violations
}

/// The owning check for the template legs, as one panicking unit so the
/// meta-tests can drive it over mutated input.
fn assert_template_publishes_nothing_but_release_assets_to_github(template: &str) {
    let ref_violations = template_ref_moves_off_forgejo(template);
    assert!(
        ref_violations.is_empty(),
        "the WorkflowTemplate moves refs off the Forgejo source of truth — \
         every git push and git clone must target {FORGEJO_URL} (GitHub is \
         a downstream read-only mirror; refs born there are pruned at the \
         next sync): {}",
        ref_violations.join(" | ")
    );
    let github_violations = unsanctioned_github_lines(template);
    assert!(
        github_violations.is_empty(),
        "the WorkflowTemplate touches GitHub outside the sanctioned set \
         (comments, the gh CLI apt bootstrap, the release secret, `gh \
         release` asset calls, the release-notes fence): {}",
        github_violations.join(" | ")
    );
}

#[test]
fn the_workflow_template_publishes_nothing_but_release_assets_to_github() {
    let template = repo_file(TEMPLATE);
    assert_template_publishes_nothing_but_release_assets_to_github(&template);

    // Non-vacuity: the pinned legs must actually be present, not merely
    // unviolated — the template really does push the tag to Forgejo and
    // really does clone from it.
    assert!(
        template.contains("git push"),
        "the WorkflowTemplate carries no git push at all — the tag-before-\
         release invariant's Forgejo push vanished"
    );
    assert!(
        template.contains("git clone"),
        "the WorkflowTemplate carries no git clone at all — the build has no \
         pinned source"
    );
}

// ── The trigger rides the read-only mirror ──────────────────────────────────

/// The document as one line of prose with comment markers dissolved, so a
/// sentence may wrap across `#`-prefixed comment lines (the Sensor's
/// header) and still read contiguously.
fn normalized_prose(doc: &str) -> String {
    normalized(&doc.replace('#', " "))
}

#[test]
fn the_ci_trigger_rides_the_readonly_mirror() {
    let sensor = normalized_prose(&repo_file(SENSOR));
    let stanza = repo_file(STANZA);

    // The Sensor's own header states the sibling requirement this suite
    // holds it to: the trigger fires from the *mirror's* webhook, so the
    // GitHub side is a trigger source — never a push target.
    let lead = "the GitHub mirror repo must carry a push webhook pointing at";
    assert!(
        sensor.contains(lead),
        "the Sensor lost the mirror-webhook sibling requirement ({lead:?}) — \
         the trigger reaches Argo only through the read-only mirror"
    );
    assert!(
        sensor.contains(MIRROR_WEBHOOK_URL),
        "the Sensor lost the mirror webhook URL ({MIRROR_WEBHOOK_URL})"
    );

    // The stanza fragment carries the mirror side of that webhook.
    assert!(
        stanza.contains("endpoint: /claude-print"),
        "the EventSource stanza lost its /claude-print webhook endpoint"
    );
    assert!(
        stanza.contains("url: https://webhooks-ci.ardenone.com"),
        "the EventSource stanza lost its webhook ingress URL"
    );
}

// ── The live verifier's doc wiring ──────────────────────────────────────────

/// The runbook wiring check, split from its `#[test]` so the negative
/// meta-test can strip the wiring in memory and require the failure.
fn check_runbook_carries_the_live_verifier(runbook: &str) {
    let runbook_norm = normalized(runbook);
    assert!(
        runbook.contains("## Provenance verification"),
        "the release runbook lost its `## Provenance verification` section — \
         the live half of the release workflow has no documented verifier"
    );
    assert!(
        runbook_norm.contains(SCRIPT),
        "the runbook's provenance section must cite {SCRIPT}"
    );
    for check in ["origin", "mirror", "tags", "publishing"] {
        assert!(
            runbook_norm.contains(check),
            "the runbook's provenance section must name the `{check}` check"
        );
    }
    assert!(
        runbook_norm.contains("--skip"),
        "the runbook's provenance section must document --skip (hosts without \
         a Forgejo credential)"
    );
    assert!(
        runbook_norm.contains(THIS_SUITE),
        "the runbook's Hermetic-coverage table must name {THIS_SUITE}"
    );
}

#[test]
fn the_runbook_carries_the_live_provenance_verifier() {
    let runbook = repo_file(RUNBOOK);
    check_runbook_carries_the_live_verifier(&runbook);
}

// ── The live verifier itself ────────────────────────────────────────────────

/// The script's git-inventory check, split for the same reason. Every `git`
/// invocation outside a comment must be one of the read-only subcommands
/// the verifier declares. Shell syntax is dissolved before tokenizing so
/// `VAR="$(git …)"` still yields the `git` token, `-C` consumes its
/// argument, and `for tool in git curl …` word lists (a line that cannot
/// invoke anything) are skipped wholesale.
fn assert_script_git_inventory_is_read_only(script: &str) {
    const ALLOWED: [&str; 4] = ["remote", "ls-remote", "credential", "ls-files"];
    for (i, line) in script.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() || trimmed.starts_with("for ") {
            continue;
        }
        let mut flattened = String::with_capacity(trimmed.len());
        let mut in_single_quote = false;
        for c in trimmed.chars() {
            if in_single_quote {
                // Single-quoted spans are inert literals in bash (grep
                // patterns, sed expressions) — never invocations; drop
                // their content.
                if c == '\'' {
                    in_single_quote = false;
                }
                flattened.push(' ');
            } else {
                match c {
                    '\'' => {
                        in_single_quote = true;
                        flattened.push(' ');
                    }
                    '(' | ')' | '"' | '=' | ';' | '|' | '&' | '`' => flattened.push(' '),
                    other => flattened.push(other),
                }
            }
        }
        let mut tokens = flattened.split_whitespace();
        while let Some(token) = tokens.next() {
            if token != "git" {
                continue;
            }
            // Skip option arguments (`-C "$REPO_ROOT"`) and expansions to
            // reach the subcommand.
            let sub = tokens
                .by_ref()
                .find(|t| !t.starts_with('-') && !t.starts_with('$'))
                .unwrap_or("");
            assert!(
                ALLOWED.contains(&sub),
                "line {}: scripts/check-release-provenance.sh invokes `git \
                 {sub}` — the verifier is strictly read-only (allowed: \
                 {ALLOWED:?})",
                i + 1
            );
        }
    }
}

/// The script's credential-hygiene check: the Forgejo token reaches curl
/// through a stdin config, never an argument, never a log line.
fn assert_script_credential_hygiene(script: &str) {
    assert!(
        script.contains("git credential fill"),
        "the verifier must read the Forgejo credential with `git credential fill`"
    );
    assert!(
        script.contains("-K -"),
        "the verifier must hand curl its config (and the token) through \
         stdin (`curl -K -`), never the argument vector"
    );
    for (i, line) in script.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.contains("Authorization") {
            assert!(
                trimmed.contains("printf"),
                "line {}: the Authorization header may only be built inside \
                 the printf'd curl stdin config",
                i + 1
            );
        }
        if trimmed.contains("TOKEN") && !trimmed.starts_with('#') {
            assert!(
                !trimmed.contains("echo") && !trimmed.contains("log_"),
                "line {}: the token must never reach an echo or a log line",
                i + 1
            );
        }
    }
}

#[test]
fn the_provenance_script_is_wired_and_safe() {
    let path = repo_root().join(SCRIPT);
    let meta = fs::metadata(&path).unwrap_or_else(|e| panic!("read metadata for {SCRIPT}: {e}"));
    assert!(
        meta.is_file(),
        "{SCRIPT} is missing — the live half of the \
        release-workflow verification has no executable owner"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert!(
            meta.permissions().mode() & 0o111 != 0,
            "{SCRIPT} is not executable"
        );
    }

    let script = repo_file(SCRIPT);
    assert!(
        script.starts_with("#!/usr/bin/env bash"),
        "{SCRIPT} must carry the `env bash` shebang this NixOS host requires"
    );
    assert!(
        script.contains("set -euo pipefail"),
        "{SCRIPT} must fail closed on unset variables and pipeline errors"
    );

    // The four checks the bead names, each individually skippable.
    for flag in ["SKIP_ORIGIN", "SKIP_MIRROR", "SKIP_TAGS", "SKIP_PUBLISHING"] {
        assert!(
            script.contains(flag),
            "{SCRIPT} lost the {flag} check — the four-area verification is \
             not optional in parts"
        );
    }
    assert!(
        script.contains("--skip"),
        "{SCRIPT} lost the --skip flag (hosts without a Forgejo credential \
         cannot run the mirror check)"
    );

    // Exit-code contract: pass / drift / indeterminate.
    assert!(
        script.contains("exit 1") && script.contains("exit 2"),
        "{SCRIPT} must implement the 0/1/2 exit-code convention (1 = drift, \
         2 = cannot determine — fail closed)"
    );

    assert_script_git_inventory_is_read_only(&script);
    assert_script_credential_hygiene(&script);

    // GET-only HTTP: no method overrides, no request bodies, no uploads.
    for forbidden in [" -X ", "--data", " -T ", "--upload", "--request"] {
        assert!(
            !script.contains(forbidden),
            "{SCRIPT} must be GET-only; found {forbidden:?}"
        );
    }
}

// ── Negative meta-tests: the pins must FAIL when their inputs rot ───────────

/// Run `check` and require it to panic with every fragment of `expected` in
/// the message — the failure must be the planted drift, not an incidental
/// one.
fn require_panic(check: impl FnOnce() + std::panic::UnwindSafe, expected: &[&str]) {
    match std::panic::catch_unwind(check) {
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("(non-string panic payload)");
            for fragment in expected {
                assert!(
                    message.contains(fragment),
                    "the check failed, but its message must name \
                     {fragment:?}; got: {message}"
                );
            }
        }
        Ok(()) => panic!(
            "expected the check to fail naming {expected:?} — it passed, so \
             the pin is vacuous"
        ),
    }
}

#[test]
fn meta_github_surface_check_fails_on_a_planted_workflow_file() {
    let entries = vec![
        "README.md".to_string(),
        "Cargo.toml".to_string(),
        ".github/workflows/ci.yml".to_string(),
    ];
    require_panic(|| assert_no_github_surface(&entries), &[".github", "Argo"]);
}

#[test]
fn meta_template_scan_rejects_a_github_push() {
    let mut template = repo_file(TEMPLATE);
    template
        .push_str("\n            git push \"https://github.com/jedarden/claude-print.git\" main\n");
    require_panic(
        || assert_template_publishes_nothing_but_release_assets_to_github(&template),
        &["git push", "github.com"],
    );
}

#[test]
fn meta_template_scan_rejects_a_commented_out_github_push() {
    let mut template = repo_file(TEMPLATE);
    template.push_str(
        "\n            # git push \"https://github.com/jedarden/claude-print.git\" main\n",
    );
    require_panic(
        || assert_template_publishes_nothing_but_release_assets_to_github(&template),
        &["git push"],
    );
}

#[test]
fn meta_template_scan_rejects_a_github_clone() {
    let mut template = repo_file(TEMPLATE);
    template.push_str(
        "\n            git clone --depth 1 \\\n              \
         \"https://github.com/jedarden/claude-print.git\" /workspace\n",
    );
    require_panic(
        || assert_template_publishes_nothing_but_release_assets_to_github(&template),
        &["clone", "github.com"],
    );
}

#[test]
fn meta_github_line_scan_is_not_fence_blind() {
    // The fence exempts the release-notes *prose*; the same URL pasted as
    // executable template code outside the fence must be flagged.
    let mut template = repo_file(TEMPLATE);
    template.push_str(
        "\n            curl -fsSL https://raw.githubusercontent.com/jedarden/\
         claude-print/main/install.sh -o install.sh\n",
    );
    require_panic(
        || assert_template_publishes_nothing_but_release_assets_to_github(&template),
        &["raw.githubusercontent.com"],
    );
}

#[test]
fn meta_missing_notes_fence_fails_loudly() {
    let template = repo_file(TEMPLATE).replace("--notes \"Release v${VERSION}", "NOTES:");
    require_panic(
        || assert_template_publishes_nothing_but_release_assets_to_github(&template),
        &["fence"],
    );
}

#[test]
fn meta_runbook_wiring_fails_when_the_script_citation_is_stripped() {
    let runbook = repo_file(RUNBOOK).replace(SCRIPT, "scripts/redacted.sh");
    require_panic(
        || check_runbook_carries_the_live_verifier(&runbook),
        &[SCRIPT],
    );
}

#[test]
fn meta_runbook_wiring_fails_when_the_coverage_row_is_stripped() {
    let runbook = repo_file(RUNBOOK).replace(THIS_SUITE, "tests/redacted.rs");
    require_panic(
        || check_runbook_carries_the_live_verifier(&runbook),
        &[THIS_SUITE],
    );
}

#[test]
fn meta_script_inventory_rejects_a_mutating_git_subcommand() {
    let script = repo_file(SCRIPT).replace("ls-remote --tags origin", "push origin main");
    require_panic(
        || assert_script_git_inventory_is_read_only(&script),
        &["git push"],
    );
}

#[test]
fn meta_credential_hygiene_rejects_a_token_echo() {
    let script = repo_file(SCRIPT).replace(
        "if [ -z \"$TOKEN\" ]; then",
        "if [ -z \"$TOKEN\" ]; then echo \"$TOKEN\"; then",
    );
    require_panic(|| assert_script_credential_hygiene(&script), &["echo"]);
}
