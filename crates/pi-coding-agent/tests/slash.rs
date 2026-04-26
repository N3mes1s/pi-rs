use pi_coding_agent::prompts::{PromptRegistry, PromptTemplate};
use pi_coding_agent::slash::{parse, render_template, SlashKind, SlashRegistry};
use std::collections::HashMap;
use std::path::PathBuf;

#[test]
fn parse_recognises_name_and_args() {
    let (name, args) = parse("/foo bar baz").unwrap();
    assert_eq!(name, "foo");
    assert_eq!(args, "bar baz");

    let (name, args) = parse("  /single").unwrap();
    assert_eq!(name, "single");
    assert_eq!(args, "");

    assert!(parse("not a slash").is_none());
    assert!(parse("/").is_none());
}

#[test]
fn render_template_substitutes_args_lower_and_upper() {
    let body = "do {{args}} and also {{ARGS}}";
    let s = render_template(body, "things");
    assert_eq!(s, "do things and also things");
}

#[test]
fn slash_registry_new_contains_builtins() {
    let reg = SlashRegistry::new();
    let names = reg.names();
    for needed in ["login", "logout", "model", "compact", "help", "quit"] {
        assert!(names.contains(&needed.to_string()), "missing builtin {needed}");
    }
    let help = reg.get("help").expect("help builtin");
    assert!(matches!(help.kind, SlashKind::Builtin));
}

#[test]
fn register_templates_adds_only_non_conflicting_names() {
    let mut prompts = PromptRegistry::new();
    prompts.add(PromptTemplate {
        name: "deploy".into(),
        body: "Deploy {{args}} now".into(),
        path: PathBuf::from("deploy.md"),
    });
    // A prompt that collides with a built-in must NOT replace the builtin.
    prompts.add(PromptTemplate {
        name: "help".into(),
        body: "Help override".into(),
        path: PathBuf::from("help.md"),
    });
    let mut reg = SlashRegistry::new();
    reg.register_templates(&prompts);
    let deploy = reg.get("deploy").expect("template should be added");
    match &deploy.kind {
        SlashKind::Template { body } => assert!(body.contains("Deploy")),
        _ => panic!("expected Template kind"),
    }
    // help stays a builtin.
    let help = reg.get("help").expect("help still present");
    assert!(matches!(help.kind, SlashKind::Builtin));
}

#[test]
fn prompt_template_render_fills_vars() {
    let t = PromptTemplate {
        name: "x".into(),
        body: "hi {{name}}, age {{age}}".into(),
        path: PathBuf::from("x.md"),
    };
    let mut vars = HashMap::new();
    vars.insert("name".to_string(), "Ada".to_string());
    vars.insert("age".to_string(), "37".to_string());
    let out = t.render(&vars);
    assert_eq!(out, "hi Ada, age 37");
}
