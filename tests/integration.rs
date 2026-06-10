/// Integration test suite for claude-print (Phase 10).
///
/// These tests compose multiple library modules to verify end-to-end behaviors
/// at the library level — without invoking the compiled binary directly.
#[path = "integration/scenarios.rs"]
mod scenarios;
