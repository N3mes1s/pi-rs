use pi_coding_agent::prompts::{resolve, PromptRegistry, PromptTemplate};
use std::path::PathBuf;

// ─── helpers ────────────────────────────────────────────────────────────────

fn registry_with(name: &str, body: &str) -> PromptRegistry {
    let mut reg = PromptRegistry::new();
    reg.add(PromptTemplate {
        name: name.to_string(),
        body: body.to_string(),
        path: PathBuf::from(format!("/fake/{name}.md")),
    });
    reg
}

// ─── @path tests ────────────────────────────────────────────────────────────

/// `@` prefix reads the file and substitutes `{{args}}`.
#[test]
fn at_path_reads_file_and_substitutes_args() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("tmpl.md");
    std::fs::write(&p, "review: {{args}}").unwrap();

    let reg = PromptRegistry::new();
    let spec = format!("@{}", p.display());
    let result = resolve(&spec, &reg, "my code").unwrap();
    assert_eq!(result, "review: my code");
}

/// `@` prefix also substitutes `{{ARGS}}` (upper-case alias).
#[test]
fn at_path_substitutes_args_upper_case() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("tmpl2.md");
    std::fs::write(&p, "UPPER: {{ARGS}}").unwrap();

    let reg = PromptRegistry::new();
    let spec = format!("@{}", p.display());
    let result = resolve(&spec, &reg, "hello").unwrap();
    assert_eq!(result, "UPPER: hello");
}

/// `@` prefix with both `{{args}}` and `{{ARGS}}` in the same body.
#[test]
fn at_path_substitutes_both_case_variants() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("both.md");
    std::fs::write(&p, "lo={{args}} hi={{ARGS}}").unwrap();

    let reg = PromptRegistry::new();
    let spec = format!("@{}", p.display());
    let result = resolve(&spec, &reg, "X").unwrap();
    assert_eq!(result, "lo=X hi=X");
}

/// `@` prefix pointing at a nonexistent path returns `Err`.
#[test]
fn at_path_missing_file_returns_err() {
    let reg = PromptRegistry::new();
    let result = resolve("@/nonexistent/path/that/does/not/exist.md", &reg, "args");
    assert!(result.is_err(), "expected Err for missing file");
}

// ─── registry lookup tests ───────────────────────────────────────────────────

/// Bare name found in registry renders correctly.
#[test]
fn bare_name_found_in_registry_renders() {
    let reg = registry_with("review", "please review: {{args}}");
    let result = resolve("review", &reg, "main.rs").unwrap();
    assert_eq!(result, "please review: main.rs");
}

/// Bare name found in registry with `{{ARGS}}` upper-case placeholder.
#[test]
fn bare_name_substitutes_args_upper_case() {
    let reg = registry_with("upper", "INPUT: {{ARGS}}");
    let result = resolve("upper", &reg, "foo").unwrap();
    assert_eq!(result, "INPUT: foo");
}

/// Both `{{args}}` and `{{ARGS}}` are substituted when both appear in body.
#[test]
fn bare_name_substitutes_both_case_variants() {
    let reg = registry_with("both", "a={{args}} A={{ARGS}}");
    let result = resolve("both", &reg, "val").unwrap();
    assert_eq!(result, "a=val A=val");
}

/// Bare name NOT found in registry returns `Err` with the spec name in the message.
#[test]
fn missing_template_returns_err_with_spec_name() {
    let reg = PromptRegistry::new();
    let result = resolve("no_such_template", &reg, "whatever");
    match result {
        Err(msg) => {
            assert!(
                msg.contains("no_such_template"),
                "error message should contain the spec name; got: {msg}"
            );
        }
        Ok(_) => panic!("expected Err, got Ok"),
    }
}

// ─── empty args ──────────────────────────────────────────────────────────────

/// When `args` is empty the placeholder is replaced with an empty string.
#[test]
fn empty_args_leaves_no_placeholder() {
    let reg = registry_with("greet", "Hello, {{args}}!");
    let result = resolve("greet", &reg, "").unwrap();
    assert_eq!(result, "Hello, !");
}

// ─── no placeholder in body ──────────────────────────────────────────────────

/// Body without any `{{args}}` placeholder is returned verbatim.
#[test]
fn body_without_placeholder_is_returned_verbatim() {
    let reg = registry_with("static", "just a static prompt");
    let result = resolve("static", &reg, "ignored args").unwrap();
    assert_eq!(result, "just a static prompt");
}
