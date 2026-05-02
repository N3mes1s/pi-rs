//! Round-trip serialisation and framing tests for pi-sandbox-protocol.

use pi_sandbox_protocol::{
    framing::{
        read_request, read_request_with_max, read_response, read_response_with_max, write_request,
        write_response, DEFAULT_MAX_LINE_BYTES,
    },
    ProtocolError, ToolRequest, ToolResponse, CURRENT_PROTOCOL_VERSION,
};
use tokio::io::{duplex, BufReader};

fn sample_request() -> ToolRequest {
    ToolRequest {
        proto_version: CURRENT_PROTOCOL_VERSION,
        call_id: "call-abc-123".to_string(),
        tool_name: "bash".to_string(),
        tool_input: serde_json::json!({"command": "ls -la"}),
        max_output_bytes: 65536,
        timeout_ms: 30000,
    }
}

fn sample_response() -> ToolResponse {
    ToolResponse {
        call_id: "call-abc-123".to_string(),
        stdout: "total 8\ndrwxr-xr-x 2 root root 4096 Jan 1 00:00 .\n".to_string(),
        stderr: String::new(),
        exit_status: 0,
        guest_duration_ms: 42,
        is_error: false,
    }
}

// --- Serde round-trip tests (no framing) ---

#[test]
fn tool_request_serde_round_trip() {
    let req = sample_request();
    let json = serde_json::to_string(&req).expect("serialise");
    let decoded: ToolRequest = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(req, decoded);
}

#[test]
fn tool_response_serde_round_trip() {
    let resp = sample_response();
    let json = serde_json::to_string(&resp).expect("serialise");
    let decoded: ToolResponse = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(resp, decoded);
}

// --- deny_unknown_fields tests ---

#[test]
fn tool_request_rejects_extra_fields() {
    let bad_json = r#"{
        "proto_version": 1,
        "call_id": "x",
        "tool_name": "read",
        "tool_input": {},
        "max_output_bytes": 1024,
        "timeout_ms": 5000,
        "unexpected_field": "oops"
    }"#;
    let result: Result<ToolRequest, _> = serde_json::from_str(bad_json);
    assert!(result.is_err(), "expected error for unknown field in ToolRequest");
}

#[test]
fn tool_response_rejects_extra_fields() {
    let bad_json = r#"{
        "call_id": "x",
        "stdout": "hi",
        "stderr": "",
        "exit_status": 0,
        "guest_duration_ms": 1,
        "is_error": false,
        "bonus": "surprise"
    }"#;
    let result: Result<ToolResponse, _> = serde_json::from_str(bad_json);
    assert!(result.is_err(), "expected error for unknown field in ToolResponse");
}

// --- Framing round-trip tests ---

#[tokio::test]
async fn write_request_read_request_duplex_round_trip() {
    let (client, server) = duplex(4096);
    let (server_read, _server_write) = tokio::io::split(server);
    let (_client_read, mut client_write) = tokio::io::split(client);

    let req = sample_request();
    let req_clone = req.clone();

    let write_handle = tokio::spawn(async move {
        write_request(&mut client_write, &req_clone)
            .await
            .expect("write_request");
    });

    let mut buf_reader = BufReader::new(server_read);
    let read_handle = tokio::spawn(async move {
        read_request(&mut buf_reader).await.expect("read_request")
    });

    write_handle.await.expect("write task");
    let decoded = read_handle.await.expect("read task");
    assert_eq!(req, decoded);
}

#[tokio::test]
async fn write_response_read_response_duplex_round_trip() {
    let (client, server) = duplex(4096);
    let (server_read, _server_write) = tokio::io::split(server);
    let (_client_read, mut client_write) = tokio::io::split(client);

    let resp = sample_response();
    let resp_clone = resp.clone();

    let write_handle = tokio::spawn(async move {
        write_response(&mut client_write, &resp_clone)
            .await
            .expect("write_response");
    });

    let mut buf_reader = BufReader::new(server_read);
    let read_handle = tokio::spawn(async move {
        read_response(&mut buf_reader).await.expect("read_response")
    });

    write_handle.await.expect("write task");
    let decoded = read_handle.await.expect("read task");
    assert_eq!(resp, decoded);
}

// --- Version mismatch test ---

#[tokio::test]
async fn read_request_rejects_wrong_proto_version() {
    // Build a request with proto_version = 0 (wrong).
    let bad_req = ToolRequest {
        proto_version: 0,
        call_id: "x".to_string(),
        tool_name: "ls".to_string(),
        tool_input: serde_json::Value::Null,
        max_output_bytes: 1024,
        timeout_ms: 1000,
    };
    let mut line = serde_json::to_vec(&bad_req).expect("serialise");
    line.push(b'\n');

    let mut buf_reader = BufReader::new(line.as_slice());
    let result = read_request(&mut buf_reader).await;

    match result {
        Err(ProtocolError::VersionMismatch { expected, found }) => {
            assert_eq!(expected, CURRENT_PROTOCOL_VERSION);
            assert_eq!(found, 0);
        }
        other => panic!("expected VersionMismatch, got {:?}", other),
    }
}

// --- EOF test ---

#[tokio::test]
async fn read_request_returns_eof_on_closed_stream() {
    // Empty reader simulates a closed stream.
    let empty: &[u8] = &[];
    let mut buf_reader = BufReader::new(empty);
    let result = read_request(&mut buf_reader).await;

    match result {
        Err(ProtocolError::Eof) => {}
        other => panic!("expected Eof, got {:?}", other),
    }
}

// --- EOF without newline tests ---

#[tokio::test]
async fn read_request_returns_eof_on_partial_frame_without_newline() {
    // A valid JSON object but NO trailing '\n' before EOF.
    let req = sample_request();
    let bytes = serde_json::to_vec(&req).expect("serialise");
    // Deliberately omit the '\n'.
    let mut buf_reader = BufReader::new(bytes.as_slice());
    let result = read_request(&mut buf_reader).await;

    match result {
        Err(ProtocolError::Eof) => {}
        other => panic!("expected Eof for frame with no trailing newline, got {:?}", other),
    }
}

#[tokio::test]
async fn read_response_returns_eof_on_partial_frame_without_newline() {
    // A valid JSON response but NO trailing '\n' before EOF.
    let resp = sample_response();
    let bytes = serde_json::to_vec(&resp).expect("serialise");
    let mut buf_reader = BufReader::new(bytes.as_slice());
    let result = read_response(&mut buf_reader).await;

    match result {
        Err(ProtocolError::Eof) => {}
        other => panic!("expected Eof for response with no trailing newline, got {:?}", other),
    }
}

// --- Frame-too-large regression test ---

#[tokio::test]
async fn read_request_rejects_oversized_frame() {
    // Build a valid request, then pad tool_name to exceed the tiny limit.
    let req = ToolRequest {
        proto_version: CURRENT_PROTOCOL_VERSION,
        call_id: "x".to_string(),
        // tool_name alone is fine, but the full JSON line will be big
        tool_name: "a".repeat(256),
        tool_input: serde_json::Value::Null,
        max_output_bytes: 1024,
        timeout_ms: 1000,
    };
    let mut line = serde_json::to_vec(&req).expect("serialise");
    line.push(b'\n');

    // The serialised line is > 256 bytes; cap at 64 bytes to force rejection.
    let max_bytes = 64;
    assert!(
        line.len() > max_bytes,
        "test precondition: line ({} bytes) must exceed cap ({} bytes)",
        line.len(),
        max_bytes
    );

    let mut buf_reader = BufReader::new(line.as_slice());
    let result = read_request_with_max(&mut buf_reader, max_bytes).await;

    match result {
        Err(ProtocolError::FrameTooLarge { size, limit }) => {
            assert!(size > max_bytes, "reported size ({size}) should exceed limit");
            assert_eq!(limit, max_bytes);
        }
        other => panic!("expected FrameTooLarge, got {:?}", other),
    }
}

// --- Invalid UTF-8 rejection test ---

#[tokio::test]
async fn read_request_rejects_invalid_utf8_frame() {
    // Construct a frame that looks like valid JSON structure but contains
    // an invalid UTF-8 byte (0xFF) in the tool_name field value.
    let prefix = br#"{"proto_version":1,"call_id":"x","tool_name":"ba"#;
    let suffix = br#"sh","tool_input":null,"max_output_bytes":1024,"timeout_ms":1000}"#;
    let invalid_byte: &[u8] = &[0xFF]; // not valid UTF-8
    let mut frame: Vec<u8> = Vec::new();
    frame.extend_from_slice(prefix);
    frame.extend_from_slice(invalid_byte);
    frame.extend_from_slice(suffix);
    frame.push(b'\n');

    let mut buf_reader = BufReader::new(frame.as_slice());
    let result = read_request(&mut buf_reader).await;

    // Must be an error — not Ok with a mutated tool_name containing U+FFFD.
    assert!(
        result.is_err(),
        "expected error for invalid UTF-8 frame, got Ok({:?})",
        result.ok()
    );
}

// --- Response-side frame cap test ---

#[tokio::test]
async fn read_response_with_max_rejects_oversized_response_frame() {
    // Build a response whose stdout is large enough to exceed a tiny frame cap.
    let resp = ToolResponse {
        call_id: "x".to_string(),
        stdout: "z".repeat(300),
        stderr: String::new(),
        exit_status: 0,
        guest_duration_ms: 1,
        is_error: false,
    };
    let mut line = serde_json::to_vec(&resp).expect("serialise");
    line.push(b'\n');

    let frame_max = 64;
    assert!(
        line.len() > frame_max,
        "test precondition: line ({} bytes) must exceed cap ({} bytes)",
        line.len(),
        frame_max
    );

    let mut buf_reader = tokio::io::BufReader::new(line.as_slice());
    // stdout_max_bytes = usize::MAX so only the frame cap fires.
    let result = read_response_with_max(&mut buf_reader, frame_max, usize::MAX).await;

    match result {
        Err(ProtocolError::FrameTooLarge { size, limit }) => {
            assert!(size > frame_max, "reported size ({size}) should exceed limit");
            assert_eq!(limit, frame_max);
        }
        other => panic!("expected FrameTooLarge for oversized response, got {:?}", other),
    }
}

/// Verify that the frame cap and the stdout cap are independent:
/// a response whose stdout exactly equals `max_output_bytes` is
/// accepted (the JSON frame is larger, but both checks pass when
/// the caller uses the correct limits).
#[tokio::test]
async fn read_response_with_max_separates_frame_cap_from_stdout_cap() {
    let stdout_content = "x".repeat(100);
    let resp = ToolResponse {
        call_id: "x".to_string(),
        stdout: stdout_content.clone(),
        stderr: String::new(),
        exit_status: 0,
        guest_duration_ms: 1,
        is_error: false,
    };
    let mut line = serde_json::to_vec(&resp).expect("serialise");
    line.push(b'\n');

    // The JSON frame is larger than 100 bytes because of the envelope.
    assert!(
        line.len() > 100,
        "precondition: frame ({} B) must exceed raw stdout size (100 B)",
        line.len()
    );

    // Use the true frame length as the frame cap, and the raw stdout length
    // as the stdout cap. Both should pass.
    let frame_cap = DEFAULT_MAX_LINE_BYTES;
    let stdout_cap = stdout_content.len(); // exactly 100

    let mut buf_reader = tokio::io::BufReader::new(line.as_slice());
    let decoded = read_response_with_max(&mut buf_reader, frame_cap, stdout_cap)
        .await
        .expect("response should be accepted when stdout fits within stdout_cap");
    assert_eq!(decoded, resp);
}

/// Verify that StdoutTooLarge fires when stdout exceeds the negotiated cap
/// even though the JSON frame itself would fit within DEFAULT_MAX_LINE_BYTES.
#[tokio::test]
async fn read_response_with_max_rejects_stdout_exceeding_negotiated_cap() {
    let stdout_content = "y".repeat(200);
    let resp = ToolResponse {
        call_id: "x".to_string(),
        stdout: stdout_content.clone(),
        stderr: String::new(),
        exit_status: 0,
        guest_duration_ms: 1,
        is_error: false,
    };
    let mut line = serde_json::to_vec(&resp).expect("serialise");
    line.push(b'\n');

    // Frame fits within DEFAULT_MAX_LINE_BYTES, but we negotiate a 50-byte
    // stdout cap — the guest violated it.
    let stdout_cap = 50;
    assert!(
        stdout_content.len() > stdout_cap,
        "precondition: stdout ({} B) must exceed stdout_cap ({} B)",
        stdout_content.len(),
        stdout_cap
    );

    let mut buf_reader = tokio::io::BufReader::new(line.as_slice());
    let result = read_response_with_max(&mut buf_reader, DEFAULT_MAX_LINE_BYTES, stdout_cap).await;

    match result {
        Err(ProtocolError::StdoutTooLarge { size, limit }) => {
            assert_eq!(size, stdout_content.len());
            assert_eq!(limit, stdout_cap);
        }
        other => panic!("expected StdoutTooLarge, got {:?}", other),
    }
}
