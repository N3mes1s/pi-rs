use pi_ai::message::{Attachment, AttachmentKind, ContentBlock};
use pi_ai::provider::anthropic::content_blocks_to_anthropic;
use serde_json::json;

#[test]
fn text_block_serialises() {
    let blocks = vec![ContentBlock::Text { text: "hi".into() }];
    let v = content_blocks_to_anthropic(&blocks);
    assert_eq!(v, json!([{"type": "text", "text": "hi"}]));
}

#[test]
fn thinking_block_serialises_with_signature() {
    let blocks = vec![ContentBlock::Thinking {
        text: "step 1".into(),
        signature: Some("sig".into()),
    }];
    let v = content_blocks_to_anthropic(&blocks);
    assert_eq!(
        v,
        json!([{
            "type": "thinking",
            "thinking": "step 1",
            "signature": "sig",
        }])
    );
}

#[test]
fn thinking_block_serialises_without_signature_as_null() {
    let blocks = vec![ContentBlock::Thinking {
        text: "raw".into(),
        signature: None,
    }];
    let v = content_blocks_to_anthropic(&blocks);
    assert_eq!(
        v,
        json!([{
            "type": "thinking",
            "thinking": "raw",
            "signature": serde_json::Value::Null,
        }])
    );
}

#[test]
fn tool_use_block_serialises() {
    let blocks = vec![ContentBlock::ToolUse {
        id: "tu_1".into(),
        name: "fs_read".into(),
        input: json!({"path": "/tmp/x"}),
    }];
    let v = content_blocks_to_anthropic(&blocks);
    assert_eq!(
        v,
        json!([{
            "type": "tool_use",
            "id": "tu_1",
            "name": "fs_read",
            "input": {"path": "/tmp/x"},
        }])
    );
}

#[test]
fn tool_result_block_serialises() {
    let blocks = vec![ContentBlock::ToolResult {
        tool_use_id: "tu_1".into(),
        content: "file contents".into(),
        is_error: true,
    }];
    let v = content_blocks_to_anthropic(&blocks);
    assert_eq!(
        v,
        json!([{
            "type": "tool_result",
            "tool_use_id": "tu_1",
            "content": "file contents",
            "is_error": true,
        }])
    );
}

#[test]
fn attachment_image_serialises_as_base64_image() {
    let blocks = vec![ContentBlock::Attachment {
        attachment: Attachment {
            kind: AttachmentKind::Image {
                mime: "image/png".into(),
                base64: "AAAA".into(),
            },
        },
    }];
    let v = content_blocks_to_anthropic(&blocks);
    assert_eq!(
        v,
        json!([{
            "type": "image",
            "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"},
        }])
    );
}

#[test]
fn attachment_file_serialises_as_document() {
    let blocks = vec![ContentBlock::Attachment {
        attachment: Attachment {
            kind: AttachmentKind::File {
                mime: "application/pdf".into(),
                base64: "JVBER".into(),
                name: "report.pdf".into(),
            },
        },
    }];
    let v = content_blocks_to_anthropic(&blocks);
    assert_eq!(
        v,
        json!([{
            "type": "document",
            "source": {"type": "base64", "media_type": "application/pdf", "data": "JVBER"},
            "name": "report.pdf",
        }])
    );
}

#[test]
fn empty_returns_empty_array() {
    let blocks: Vec<ContentBlock> = vec![];
    let v = content_blocks_to_anthropic(&blocks);
    assert_eq!(v, json!([]));
}

#[test]
fn multiple_blocks_preserve_order() {
    let blocks = vec![
        ContentBlock::Text { text: "a".into() },
        ContentBlock::Text { text: "b".into() },
    ];
    let v = content_blocks_to_anthropic(&blocks);
    assert_eq!(
        v,
        json!([
            {"type": "text", "text": "a"},
            {"type": "text", "text": "b"},
        ])
    );
}
