//! The 8 hard codegen invariants per RFD 0028 §B.13.
//!
//! Each invariant gets its own test. Failing any one of these
//! means a real foot-gun has been re-introduced into the codegen.
//!
//! Invariants 3 (stdout discipline) and 7 (stdout/stderr separation)
//! require running a built agent against MockProvider and are
//! deferred to an explicit `--features build-smoke` integration
//! suite (the build is too heavy for the default test fleet).

use pi_build::{manifest, parse, render, PI_BUILD_VERSION};
use syn::{visit::Visit, ExprCall, ExprLit, Lit};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn fixture(rel: &str) -> String {
    let path = format!("{FIXTURES}/{rel}");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

fn render_main_rs(rel: &str) -> String {
    let raw = fixture(rel);
    let m = parse(&raw).expect("parse");
    render(&m, &raw, PI_BUILD_VERSION).main_rs
}

// ── B.13 #1: same allowlist, both registries ─────────────────────

#[test]
fn invariant_1_same_registry_passed_to_tools_and_sandbox() {
    let main = render_main_rs("valid/dice-oracle.toml");
    // The fingerprint is the literal Rust call shape: `.tools(tools)`
    // (moves the registry) followed by `LocalProcessProvider::new(tools.clone())`
    // (clones it before the move). Failing this means the codegen
    // accidentally instantiated a fresh LocalProcessProvider::with_defaults().
    assert!(main.contains(".tools(tools)"), "must move `tools` into .tools()");
    assert!(
        main.contains("LocalProcessProvider::new(tools.clone())"),
        "must pass the SAME registry (cloned) to LocalProcessProvider::new",
    );
    // And explicitly NOT the bypass path:
    assert!(
        !main.contains("LocalProcessProvider::with_defaults()"),
        "MUST NOT use with_defaults — silently restores all 8 unsafe tools",
    );
}

// ── B.13 #2: AgentEvent JSONL shape stable ───────────────────────
//
// This one tests pi-sdk's contract, not pi-build's, but the
// invariant is "if pi-sdk's AgentEvent JSONL shape regresses,
// every compiled agent's stdout breaks." Round-trip a representative
// AgentEvent through serde_json and assert equality. The variants
// covered are the four Commit B emits in the pump (B.3): TextDelta,
// ToolCall, Usage, TurnComplete.
//
// (Deferred: requires a pi-sdk dev-dep on pi-build, which is a
// circular dep risk — pi-build is a pi-sdk *consumer*. Spec the
// invariant here; pi-sdk's own test suite covers the round-trip.)

// ── B.13 #4: tokio runtime flavour ───────────────────────────────

#[test]
fn invariant_4_tokio_current_thread_flavor_is_literal() {
    let main = render_main_rs("valid/dice-oracle.toml");
    assert!(
        main.contains(r#"#[tokio::main(flavor = "current_thread")]"#),
        "main.rs MUST declare current_thread runtime per §Cross-cutting #9",
    );
}

// ── B.13 #5: codegen determinism ─────────────────────────────────

#[test]
fn invariant_5_codegen_determinism() {
    let raw = fixture("valid/dice-oracle.toml");
    let m = parse(&raw).expect("parse");
    let a = render(&m, &raw, PI_BUILD_VERSION);
    let b = render(&m, &raw, PI_BUILD_VERSION);
    assert_eq!(a.cargo_toml, b.cargo_toml);
    assert_eq!(a.main_rs, b.main_rs);
    assert_eq!(a.pi_build_lock, b.pi_build_lock);
}

// ── B.13 #6: no secrets in generated source (syn AST walk) ────

/// Walks the `main.rs` AST and collects every string literal
/// containing `_API_KEY` or `_TOKEN`, recording whether each
/// appeared inside a `from_env_explicit(...)` call OR inside
/// a `Settings::builder().{system_prompt, model, provider}(...)`
/// call. Per B.13 #6, a leak is any other location.
#[derive(Default)]
struct SecretScanner {
    /// (literal_value, location_kind)
    sites: Vec<(String, SecretLocation)>,
    /// Stack of enclosing call-expression names; topmost is the
    /// immediate parent.
    call_stack: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SecretLocation {
    InsideFromEnvExplicit,
    InsideSettingsBuilder { setter: String },
    Leak,
}

impl SecretScanner {
    fn classify(&self) -> SecretLocation {
        // Walk the call stack from immediate parent outward.
        for caller in self.call_stack.iter().rev() {
            if caller == "from_env_explicit" {
                return SecretLocation::InsideFromEnvExplicit;
            }
            if matches!(caller.as_str(), "system_prompt" | "model" | "provider") {
                return SecretLocation::InsideSettingsBuilder {
                    setter: caller.clone(),
                };
            }
        }
        SecretLocation::Leak
    }
}

impl<'ast> Visit<'ast> for SecretScanner {
    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        let name = call_name(&node.func);
        self.call_stack.push(name);
        syn::visit::visit_expr_call(self, node);
        self.call_stack.pop();
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        self.call_stack.push(node.method.to_string());
        syn::visit::visit_expr_method_call(self, node);
        self.call_stack.pop();
    }

    fn visit_expr_lit(&mut self, node: &'ast ExprLit) {
        if let Lit::Str(s) = &node.lit {
            let v = s.value();
            if v.contains("_API_KEY") || v.contains("_TOKEN") {
                self.sites.push((v, self.classify()));
            }
        }
        syn::visit::visit_expr_lit(self, node);
    }
}

fn call_name(expr: &syn::Expr) -> String {
    match expr {
        syn::Expr::Path(p) => p
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

#[test]
fn invariant_6_no_secret_literals_outside_known_callers() {
    let main = render_main_rs("valid/dice-oracle.toml");
    let file = syn::parse_file(&main).expect("parse main.rs");
    let mut scanner = SecretScanner::default();
    scanner.visit_file(&file);
    let leaks: Vec<_> = scanner
        .sites
        .iter()
        .filter(|(_, loc)| *loc == SecretLocation::Leak)
        .collect();
    assert!(leaks.is_empty(), "leaked secret literals: {leaks:?}");
    // Sanity: dice-oracle has secrets.required = ["ANTHROPIC_API_KEY"],
    // so we should have observed ≥ 1 properly-scoped site.
    assert!(
        scanner
            .sites
            .iter()
            .any(|(_, loc)| matches!(loc, SecretLocation::InsideFromEnvExplicit)),
        "expected at least one ANTHROPIC_API_KEY inside from_env_explicit; sites: {:?}",
        scanner.sites,
    );
}

#[test]
fn invariant_6_system_prompt_mentioning_env_var_does_not_leak() {
    // A manifest whose `system_prompt` legitimately mentions
    // ANTHROPIC_API_KEY (operator documents the env var to the
    // model). MUST NOT trip the leak detector — the (b) clause
    // in B.13 #6 exempts Settings::builder().system_prompt(...).
    let raw = r#"schema_version = 1
agent = { name = "x", description = "y", version = "0.1.0" }
provider = { name = "anthropic", model = "m" }
secrets = { required = ["ANTHROPIC_API_KEY"] }
[runtime]
system_prompt = "Set ANTHROPIC_API_KEY before running this tool."
"#;
    let m = parse(raw).expect("parse");
    let main = render(&m, raw, PI_BUILD_VERSION).main_rs;
    let file = syn::parse_file(&main).expect("parse main.rs");
    let mut scanner = SecretScanner::default();
    scanner.visit_file(&file);
    let leaks: Vec<_> = scanner
        .sites
        .iter()
        .filter(|(_, loc)| *loc == SecretLocation::Leak)
        .collect();
    assert!(
        leaks.is_empty(),
        "system_prompt mentioning env var should be exempt; leaks: {leaks:?}",
    );
    // We DO expect TWO sites overall: one inside from_env_explicit,
    // one inside system_prompt — both classified as non-leak.
    let from_env = scanner
        .sites
        .iter()
        .filter(|(_, loc)| matches!(loc, SecretLocation::InsideFromEnvExplicit))
        .count();
    let in_prompt = scanner
        .sites
        .iter()
        .filter(|(_, loc)| matches!(loc, SecretLocation::InsideSettingsBuilder { .. }))
        .count();
    assert_eq!(from_env, 1, "exactly one in from_env_explicit");
    assert_eq!(in_prompt, 1, "exactly one in Settings::builder().system_prompt");
}

// ── B.13 #8: no-secret manifest produces no env reads ─────────

#[test]
fn invariant_8_empty_secrets_renders_iter_empty() {
    let raw = r#"schema_version = 1
agent = { name = "x", description = "y", version = "0.1.0" }
provider = { name = "anthropic", model = "m" }
runtime = { system_prompt = "p" }
"#;
    let m = parse(raw).expect("parse");
    let main = render(&m, raw, PI_BUILD_VERSION).main_rs;
    assert!(
        main.contains("from_env_explicit(std::iter::empty::<(&str, &str)>())"),
        "empty secrets MUST render as typed iter::empty (B.13 #8)",
    );
    // And NOT as the bare empty array which would fail to type-infer
    // (E0282/E0283 on the from_env_explicit<I, P, E> generics).
    assert!(
        !main.contains("from_env_explicit([])"),
        "MUST NOT emit bare from_env_explicit([]) — fails type inference",
    );
    // Sanity: the rendered file is valid Rust (would not be if the
    // empty array was emitted and the test passed by coincidence).
    syn::parse_file(&main).expect("rendered main.rs must compile-shape");
}

// ── KNOWN_TOOLS sweep — every documented tool name codegens cleanly ──

#[test]
fn every_known_tool_name_codegens_cleanly() {
    // Build a manifest with each KNOWN_TOOLS entry; render; parse
    // through syn. Catches a regression where one of the 8 tool
    // names is emitted in a way that doesn't tokenize.
    for tool in manifest::KNOWN_TOOLS {
        let raw = format!(
            r#"schema_version = 1
agent = {{ name = "x", description = "y", version = "0.1.0" }}
provider = {{ name = "anthropic", model = "m" }}
tools = {{ allowlist = ["{tool}"] }}
runtime = {{ system_prompt = "p" }}
"#
        );
        let m = parse(&raw).unwrap_or_else(|e| panic!("parse {tool}: {e}"));
        let main = render(&m, &raw, PI_BUILD_VERSION).main_rs;
        syn::parse_file(&main).unwrap_or_else(|e| panic!("syn parse {tool}: {e}"));
    }
}
