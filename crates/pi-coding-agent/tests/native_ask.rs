//! B3: Native `ask` tool.

use pi_coding_agent::native::ask::{AskInput, AskTool};
use pi_tools::{Tool, ToolContext};
use serde_json::json;

fn ctx() -> ToolContext {
    ToolContext::default()
}

#[test]
fn parse_minimal_input() {
    let v = json!({
        "question": "Pick one",
        "options": ["a", "b"]
    });
    let p = AskInput::parse(&v).unwrap();
    assert_eq!(p.question, "Pick one");
    assert_eq!(p.options, vec!["a".to_string(), "b".to_string()]);
    assert!(!p.allow_multi);
    assert!(p.descriptions.is_none());
}

#[test]
fn parse_full_input() {
    let v = json!({
        "question": "Pick many",
        "options": ["x", "y", "z"],
        "allow_multi": true,
        "descriptions": ["x desc", "y desc", "z desc"]
    });
    let p = AskInput::parse(&v).unwrap();
    assert!(p.allow_multi);
    assert_eq!(p.descriptions.unwrap().len(), 3);
}

#[test]
fn parse_rejects_missing_question() {
    let v = json!({"options": ["a"]});
    assert!(AskInput::parse(&v).is_err());
}

#[test]
fn parse_rejects_empty_options() {
    let v = json!({"question": "?", "options": []});
    let err = AskInput::parse(&v).unwrap_err();
    assert!(format!("{err}").contains("at least one"));
}

#[test]
fn parse_rejects_non_string_option() {
    let v = json!({"question": "?", "options": [1, 2]});
    assert!(AskInput::parse(&v).is_err());
}

#[tokio::test]
async fn invoke_returns_is_error_in_non_interactive_with_ask_payload() {
    let tool = AskTool;
    let r = tool
        .invoke(
            &ctx(),
            "call_1",
            json!({
                "question": "Continue?",
                "options": ["yes", "no"]
            }),
        )
        .await
        .unwrap();
    assert!(r.is_error);
    assert_eq!(r.model_output, "ASK requires interactive mode");
    let display = r.display.expect("display");
    assert_eq!(display["kind"], "ask");
    assert_eq!(display["question"], "Continue?");
    assert_eq!(display["options"][0], "yes");
}

#[tokio::test]
async fn invoke_propagates_parse_errors() {
    let tool = AskTool;
    let err = tool
        .invoke(&ctx(), "x", json!({"options": ["a"]}))
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("question"));
}

#[test]
fn spec_advertises_required_fields() {
    let s = AskTool.spec();
    assert_eq!(s.name, "ask");
    let req = s.input_schema.get("required").unwrap().as_array().unwrap();
    let names: Vec<&str> = req.iter().map(|v| v.as_str().unwrap()).collect();
    assert!(names.contains(&"question"));
    assert!(names.contains(&"options"));
}
