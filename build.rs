use std::env;
use std::path::Path;
/// Build script to compile mock-claude test fixture.
///
/// This ensures that integration tests (specifically tests/watchdog.rs) have
/// the mock-claude binary available without manual building.
use std::process::Command;

fn main() {
    // Only build mock-claude if we're not in a doctest context
    // (doctests don't need the fixture and would slow down the build)
    if env::var("CARGO_PRIMARY_PACKAGE").is_ok() {
        println!("cargo:rerun-if-changed=test-fixtures/mock-claude/Cargo.toml");
        println!("cargo:rerun-if-changed=test-fixtures/mock-claude/src/main.rs");

        let mock_claude_dir = Path::new("test-fixtures/mock-claude");
        let status = Command::new("cargo")
            .arg("build")
            .arg("--manifest-path")
            .arg(mock_claude_dir.join("Cargo.toml"))
            .status()
            .expect("Failed to build mock-claude. Is cargo installed?");

        if !status.success() {
            panic!("Failed to build mock-claude binary");
        }
    }
}
