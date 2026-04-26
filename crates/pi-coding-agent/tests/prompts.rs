use pi_coding_agent::prompts::PromptRegistry;
use std::collections::HashMap;

#[test]
fn load_dir_picks_up_md_files_only() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hello.md"), "Greet {{name}}").unwrap();
    std::fs::write(dir.path().join("notes.txt"), "ignored").unwrap();
    std::fs::write(dir.path().join("README.md"), "doc body").unwrap();

    let mut reg = PromptRegistry::new();
    reg.load_dir(dir.path());
    let names = reg.names();
    assert!(names.contains(&"hello".to_string()));
    assert!(names.contains(&"README".to_string()));
    assert!(
        !names.contains(&"notes".to_string()),
        ".txt files must be ignored"
    );
}

#[test]
fn missing_dir_is_silent_noop() {
    let mut reg = PromptRegistry::new();
    reg.load_dir(std::path::Path::new("/this/does/not/exist/at/all"));
    assert!(reg.names().is_empty());
}

#[test]
fn render_fills_in_vars() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("greet.md"), "hi {{name}}, age {{age}}").unwrap();
    let mut reg = PromptRegistry::new();
    reg.load_dir(dir.path());
    let t = reg.get("greet").expect("greet should be loaded");
    let mut vars = HashMap::new();
    vars.insert("name".into(), "Ada".into());
    vars.insert("age".into(), "37".into());
    assert_eq!(t.render(&vars), "hi Ada, age 37");
}
