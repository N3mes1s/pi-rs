//! Integration tests for the public POD types in `pi-tool-types`.
//!
//! Covers serialisation round-trips and field defaulting for `ToolSpec` and
//! `ToolResult`, the `ToolDispatch` variant semantics (including `Default`),
//! and the `Display` messages emitted by each `ToolError` variant.

use pi_tool_types::{ToolDispatch, ToolError, ToolResult, ToolSpec};

// ── ToolSpec ────────────────────────────────────────────────────────────────

/// A `ToolSpec` serialised to JSON and back must preserve all fields,
/// including a nested JSON Schema stored as `serde_json::Value`.
#[test]
fn tool_spec_serde_round_trip() {
    let spec = ToolSpec {
        name: "bash".to_string(),
        description: "Run a shell command and return stdout/stderr.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string" }
            },
            "required": ["command"]
        }),
    };

    let json = serde_json::to_string(&spec).expect("serialise ToolSpec");
    let decoded: ToolSpec = serde_json::from_str(&json).expect("deserialise ToolSpec");

    assert_eq!(decoded.name, spec.name);
    assert_eq!(decoded.description, spec.description);
    assert_eq!(decoded.input_schema, spec.input_schema);
}

/// Deserialising a `ToolSpec` whose `input_schema` is JSON `null` must
/// succeed — the field type is `serde_json::Value`, which accepts `null`.
#[test]
fn tool_spec_accepts_null_input_schema() {
    let json = r#"{"name":"noop","description":"does nothing","input_schema":null}"#;
    let spec: ToolSpec = serde_json::from_str(json).expect("deserialise with null schema");
    assert_eq!(spec.name, "noop");
    assert!(spec.input_schema.is_null());
}

// ── ToolResult ───────────────────────────────────────────────────────────────

/// `ToolResult` must round-trip cleanly, including the optional `display`
/// field and the `is_error` flag.
#[test]
fn tool_result_serde_round_trip_success() {
    let result = ToolResult {
        tool_use_id: "toolu_01XYZ".to_string(),
        model_output: "total 4\ndrwxr-xr-x 2 root root 4096 Jan 1 00:00 .".to_string(),
        display: Some(serde_json::json!({"kind": "file-list", "count": 1})),
        is_error: false,
    };

    let json = serde_json::to_string(&result).expect("serialise ToolResult");
    let decoded: ToolResult = serde_json::from_str(&json).expect("deserialise ToolResult");

    assert_eq!(decoded.tool_use_id, result.tool_use_id);
    assert_eq!(decoded.model_output, result.model_output);
    assert_eq!(decoded.display, result.display);
    assert!(!decoded.is_error);
}

/// When `display` and `is_error` are absent from JSON, `#[serde(default)]`
/// must supply `None` and `false` respectively — these fields must never
/// be required at the wire level.
#[test]
fn tool_result_defaults_optional_fields_when_absent() {
    let json = r#"{"tool_use_id":"toolu_02","model_output":"ok"}"#;
    let result: ToolResult = serde_json::from_str(json)
        .expect("deserialise ToolResult with only required fields");

    assert_eq!(result.tool_use_id, "toolu_02");
    assert_eq!(result.model_output, "ok");
    assert!(result.display.is_none(), "display should default to None");
    assert!(!result.is_error, "is_error should default to false");
}

/// An error result (`is_error = true`, no `display` payload) must round-trip
/// cleanly, preserving the error flag exactly.
#[test]
fn tool_result_serde_round_trip_error() {
    let result = ToolResult {
        tool_use_id: "toolu_03ERR".to_string(),
        model_output: "file not found: /etc/missing".to_string(),
        display: None,
        is_error: true,
    };

    let json = serde_json::to_string(&result).expect("serialise error ToolResult");
    let decoded: ToolResult = serde_json::from_str(&json).expect("deserialise error ToolResult");

    assert!(decoded.is_error, "is_error flag must survive round-trip");
    assert_eq!(decoded.model_output, result.model_output);
    assert!(decoded.display.is_none());
}

// ── ToolDispatch ─────────────────────────────────────────────────────────────

/// The default variant must be `Guest`, as documented: the vast majority of
/// tools run inside the sandbox execution environment.
#[test]
fn tool_dispatch_default_is_guest() {
    assert_eq!(ToolDispatch::default(), ToolDispatch::Guest);
}

/// Two `Unavailable` variants are equal iff their `reason` strings are
/// identical — different reasons must compare unequal.
#[test]
fn tool_dispatch_unavailable_equality_is_reason_sensitive() {
    let a = ToolDispatch::Unavailable { reason: "lsp: host-process state" };
    let b = ToolDispatch::Unavailable { reason: "lsp: host-process state" };
    let c = ToolDispatch::Unavailable { reason: "monitor: streaming not supported" };

    assert_eq!(a, b, "same reason must compare equal");
    assert_ne!(a, c, "different reasons must compare unequal");
}

/// `Guest` and `Unavailable` are never equal to each other, regardless of
/// the reason string.
#[test]
fn tool_dispatch_guest_ne_unavailable() {
    let guest = ToolDispatch::Guest;
    let unavail = ToolDispatch::Unavailable { reason: "any reason" };
    assert_ne!(guest, unavail);
}

// ── ToolError display messages ───────────────────────────────────────────────

/// Each `ToolError` variant must produce the documented `Display` string so
/// that operators can identify the failure class from logs without inspecting
/// the enum variant directly.
#[test]
fn tool_error_display_messages_match_documented_format() {
    let not_found = ToolError::NotFound("web_search".to_string());
    assert_eq!(not_found.to_string(), "tool not found: web_search");

    let invalid = ToolError::InvalidInput("missing required field: command".to_string());
    assert_eq!(invalid.to_string(), "invalid input: missing required field: command");

    let other = ToolError::Other("upstream returned 429".to_string());
    assert_eq!(other.to_string(), "upstream returned 429");
}

/// `ToolError::Io` must wrap a `std::io::Error` via the `#[from]` impl and
/// include the original IO message in its `Display` output.
#[test]
fn tool_error_io_wraps_std_io_error() {
    let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file or directory");
    let tool_err = ToolError::from(io_err);
    let msg = tool_err.to_string();
    // The documented format is "io error: <original message>".
    assert!(
        msg.starts_with("io error:"),
        "ToolError::Io display must start with 'io error:'; got: {msg:?}"
    );
    assert!(
        msg.contains("no such file or directory"),
        "ToolError::Io display must contain the original IO message; got: {msg:?}"
    );
}
