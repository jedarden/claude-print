/// Integration test suite for claude-print (Phase 10).
///
/// These tests compose multiple library modules to verify end-to-end behaviors
/// at the library level — without invoking the compiled binary directly.

// Pull in the config error helpers at the integration test crate level
mod config_error_helpers;

#[path = "integration/scenarios.rs"]
mod scenarios;
