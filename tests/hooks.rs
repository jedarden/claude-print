/// Hook inheritance tests (Phase 10).
///
/// Verifies the hook installer creates the correct artifacts and that the temp
/// dir lifecycle works correctly.  These tests correspond to the "Hook
/// Inheritance Tests" section of the plan.
use claude_print::hook::HookInstaller;
use std::os::unix::fs::PermissionsExt;

// ── settings.json structure ───────────────────────────────────────────────────

#[test]
fn settings_json_double_nested_hooks_structure() {
    let installer = HookInstaller::new().unwrap();
    let content = std::fs::read_to_string(&installer.settings_path).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    // Schema: { "hooks": { "Stop": [{ "hooks": [{"type":"command", ...}] }] } }
    let stop = &val["hooks"]["Stop"];
    assert!(stop.is_array(), "Stop must be an array");
    let outer = &stop[0];
    assert!(outer.is_object(), "first Stop entry must be an object");
    let inner = &outer["hooks"];
    assert!(inner.is_array(), "hooks inside Stop entry must be an array");
    assert!(
        !inner.as_array().unwrap().is_empty(),
        "inner hooks must be non-empty"
    );
}

#[test]
fn settings_json_hook_type_is_command() {
    let installer = HookInstaller::new().unwrap();
    let content = std::fs::read_to_string(&installer.settings_path).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let hook = &val["hooks"]["Stop"][0]["hooks"][0];
    assert_eq!(hook["type"], "command", "hook type must be 'command'");
}

#[test]
fn settings_json_stop_hook_timeout_is_10() {
    let installer = HookInstaller::new().unwrap();
    let content = std::fs::read_to_string(&installer.settings_path).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let timeout = &val["hooks"]["Stop"][0]["hooks"][0]["timeout"];
    assert_eq!(timeout, 10, "relay hook timeout must be 10 seconds");
}

#[test]
fn settings_json_command_references_hook_sh() {
    let installer = HookInstaller::new().unwrap();
    let content = std::fs::read_to_string(&installer.settings_path).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let cmd = val["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap_or("");
    assert!(
        cmd.contains("hook.sh"),
        "command must reference hook.sh, got: {cmd:?}"
    );
    // Must reference the hook.sh within the temp dir
    assert!(
        cmd.starts_with(installer.dir_path().to_str().unwrap()),
        "command must be within the temp dir"
    );
}

// ── hook.sh format ────────────────────────────────────────────────────────────

#[test]
fn hook_sh_shebang_is_sh() {
    let installer = HookInstaller::new().unwrap();
    let content = std::fs::read_to_string(&installer.hook_path).unwrap();
    assert!(
        content.starts_with("#!/bin/sh"),
        "hook.sh must start with #!/bin/sh, got: {content:?}"
    );
}

#[test]
fn hook_sh_uses_cat_to_write_stdin_to_fifo() {
    let installer = HookInstaller::new().unwrap();
    let content = std::fs::read_to_string(&installer.hook_path).unwrap();
    assert!(
        content.contains("cat >"),
        "hook.sh must use cat > to pipe stdin to FIFO"
    );
    assert!(
        content.contains("stop.fifo"),
        "hook.sh must reference stop.fifo"
    );
}

#[test]
fn hook_sh_has_fire_and_forget_pattern() {
    let installer = HookInstaller::new().unwrap();
    let content = std::fs::read_to_string(&installer.hook_path).unwrap();
    // "|| true" ensures hook.sh exits 0 even if the FIFO write fails (T-4 mitigation)
    assert!(
        content.contains("|| true"),
        "hook.sh must use '|| true' for fire-and-forget; got: {content:?}"
    );
}

#[test]
fn hook_sh_fifo_path_is_single_quoted() {
    let installer = HookInstaller::new().unwrap();
    let content = std::fs::read_to_string(&installer.hook_path).unwrap();
    // T-4: fifo path embedded with single quotes prevents shell expansion.
    // The cat command should be: cat > '<path>'
    assert!(
        content.contains("cat > '"),
        "hook.sh must use single-quoted FIFO path (T-4 mitigation); got: {content:?}"
    );
}

// ── Temp dir lifecycle ────────────────────────────────────────────────────────

#[test]
fn hook_sh_and_fifo_paths_are_within_temp_dir() {
    let installer = HookInstaller::new().unwrap();
    let temp_dir = installer.dir_path().to_path_buf();
    assert!(
        installer.hook_path.starts_with(&temp_dir),
        "hook.sh must be inside the temp dir"
    );
    assert!(
        installer.fifo_path.starts_with(&temp_dir),
        "stop.fifo must be inside the temp dir"
    );
    assert!(
        installer.settings_path.starts_with(&temp_dir),
        "settings.json must be inside the temp dir"
    );
}

#[test]
fn hook_sh_is_executable() {
    let installer = HookInstaller::new().unwrap();
    let meta = std::fs::metadata(&installer.hook_path).unwrap();
    let mode = meta.permissions().mode();
    // Owner execute bit (0o100) must be set
    assert!(
        mode & 0o100 != 0,
        "hook.sh must be executable by owner; mode = {:#o}",
        mode
    );
}

#[test]
fn temp_dir_prefix_contains_claude_print() {
    let installer = HookInstaller::new().unwrap();
    let dir_name = installer
        .dir_path()
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    assert!(
        dir_name.contains("claude-print"),
        "temp dir name must contain 'claude-print'; got: {dir_name:?}"
    );
}

// ── Settings JSON is valid ────────────────────────────────────────────────────

#[test]
fn settings_json_is_valid_json() {
    let installer = HookInstaller::new().unwrap();
    let content = std::fs::read_to_string(&installer.settings_path).unwrap();
    let result: Result<serde_json::Value, _> = serde_json::from_str(&content);
    assert!(result.is_ok(), "settings.json must be valid JSON");
}

// ── No-inherit-hooks mode: settings path independent of user settings ─────────

#[test]
fn hook_installer_temp_dir_is_independent_of_home() {
    let installer = HookInstaller::new().unwrap();
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let temp_path = installer.dir_path().to_str().unwrap_or("");
    assert!(
        !temp_path.starts_with(&home) || temp_path.contains("tmp"),
        "temp dir should not be inside HOME/.claude (would break isolation)"
    );
}
