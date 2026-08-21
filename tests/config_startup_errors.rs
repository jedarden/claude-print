//! End-to-end coverage for configuration failures during CLI startup.

mod config_error_helpers;

use config_error_helpers::{
    assert_exits_with_code, assert_stderr_contains, assert_stdout_empty,
    run_with_config_and_format, ConfigFixture, Outcome,
};

fn assert_structured_config_error(outcome: &Outcome) {
    assert_exits_with_code(outcome, 2);
    assert_stdout_empty(outcome);
    let error = outcome.parse_json_stderr();
    assert_eq!(error["type"], "result");
    assert_eq!(error["is_error"], true);
    assert_eq!(error["subtype"], "internal_error");
    let message = error["error_message"]
        .as_str()
        .expect("config error response must contain a string error_message");
    assert!(
        message.contains("config") && message.contains("invalid"),
        "error_message should identify the invalid config, got: {message:?}"
    );
}

#[test]
fn malformed_config_is_visible_in_text_mode() {
    let fixture = ConfigFixture::new();
    fixture.write_config("[defaults\nmodel = \"claude-sonnet-4-6\"\n");

    let outcome = run_with_config_and_format(fixture.path(), "text", "test prompt");

    assert_exits_with_code(&outcome, 2);
    assert_stdout_empty(&outcome);
    assert_stderr_contains(&outcome, "invalid config");
}

#[test]
fn malformed_config_is_structured_in_json_mode() {
    let fixture = ConfigFixture::new();
    fixture.write_config("[defaults\nmodel = \"claude-sonnet-4-6\"\n");

    let outcome = run_with_config_and_format(fixture.path(), "json", "test prompt");

    assert_structured_config_error(&outcome);
}

#[test]
fn malformed_config_json_error_is_structured_on_stderr() {
    let malformed_configs = [
        ("unclosed bracket", "[["),
        ("assignment without key", "= \"value\""),
        ("array instead of table", "defaults = []"),
    ];

    for (case, contents) in malformed_configs {
        let fixture = ConfigFixture::new();
        fixture.write_config(contents);

        let outcome = run_with_config_and_format(fixture.path(), "json", "test prompt");

        assert_structured_config_error(&outcome);
        assert_ne!(
            outcome.code,
            Some(0),
            "{case}: malformed config must not silently fall back to defaults"
        );
    }
}

#[test]
fn wrong_config_type_is_structured_in_stream_json_mode() {
    let fixture = ConfigFixture::new();
    fixture.write_config("[defaults]\nmax_turns = \"thirty\"\n");

    let outcome = run_with_config_and_format(fixture.path(), "stream-json", "test prompt");

    assert_structured_config_error(&outcome);
}
