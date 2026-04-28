use clap::Parser;
use pi_coding_agent::cli::{Cli, Mode};
use std::path::PathBuf;

fn parse(args: &[&str]) -> Cli {
    let mut argv = vec!["pi"];
    argv.extend_from_slice(args);
    Cli::parse_from(argv)
}

#[test]
fn effective_mode_priority_rpc_over_json_print_interactive() {
    // rpc beats json + print
    let cli = parse(&["--rpc", "--json", "--print"]);
    assert_eq!(cli.effective_mode(), Mode::Rpc);

    let cli = parse(&["--json", "--print"]);
    assert_eq!(cli.effective_mode(), Mode::Json);

    let cli = parse(&["--print"]);
    assert_eq!(cli.effective_mode(), Mode::Print);

    let cli = parse(&[]);
    assert_eq!(cli.effective_mode(), Mode::Interactive);
}

#[test]
fn prompt_text_joins_positionals_skipping_at_files() {
    let cli = parse(&["hello", "world", "@notes.txt", "again"]);
    assert_eq!(cli.prompt_text().as_deref(), Some("hello world again"));
}

#[test]
fn prompt_text_returns_none_when_only_at_files() {
    let cli = parse(&["@a.txt", "@b.txt"]);
    assert_eq!(cli.prompt_text(), None);
}

#[test]
fn at_files_extracts_at_prefixed_paths() {
    let cli = parse(&["msg", "@a.txt", "more", "@nested/path.md"]);
    let files: Vec<PathBuf> = cli.at_files();
    assert_eq!(
        files,
        vec![PathBuf::from("a.txt"), PathBuf::from("nested/path.md")]
    );
}

#[test]
fn route_flag_defaults_to_static_and_accepts_off() {
    let cli = parse(&[]);
    assert_eq!(cli.route, "static");

    let cli = parse(&["--route", "off"]);
    assert_eq!(cli.route, "off");
}

#[test]
fn thinking_flag_rejects_non_allowed_values() {
    let res = Cli::try_parse_from(["pi", "--thinking", "ultra"]);
    assert!(res.is_err(), "clap should reject `ultra` thinking value");

    let ok = Cli::try_parse_from(["pi", "--thinking", "high"]);
    assert!(ok.is_ok(), "`high` is allowed");
    let ok = Cli::try_parse_from(["pi", "--thinking", "off"]);
    assert!(ok.is_ok(), "`off` is allowed");
}

#[test]
fn no_builtin_tools_and_no_tools_pass_through() {
    let cli = parse(&["--no-builtin-tools", "--no-tools"]);
    assert!(cli.no_builtin_tools);
    assert!(cli.no_tools);

    let cli = parse(&[]);
    assert!(!cli.no_builtin_tools);
    assert!(!cli.no_tools);
}

#[test]
fn tools_list_parses_into_vec_of_names() {
    let cli = parse(&["--tools", "read,bash"]);
    let raw = cli.tools.expect("tools should be set");
    let names: Vec<&str> = raw.split(',').map(|s| s.trim()).collect();
    assert_eq!(names, vec!["read", "bash"]);
}

#[test]
fn session_and_session_dir_coexist() {
    let cli = parse(&["--session", "abc123", "--session-dir", "/tmp/sessions"]);
    assert_eq!(cli.session.as_deref(), Some("abc123"));
    assert_eq!(
        cli.session_dir.as_deref(),
        Some(std::path::Path::new("/tmp/sessions"))
    );
}

#[test]
fn continue_recent_and_resume_are_independent_booleans() {
    let cli = parse(&["-c"]);
    assert!(cli.continue_recent);
    assert!(!cli.resume);

    let cli = parse(&["-r"]);
    assert!(cli.resume);
    assert!(!cli.continue_recent);

    let cli = parse(&["-c", "-r"]);
    assert!(cli.continue_recent);
    assert!(cli.resume);

    let cli = parse(&[]);
    assert!(!cli.continue_recent);
    assert!(!cli.resume);
}
