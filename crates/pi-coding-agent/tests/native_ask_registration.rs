//! Validate-bug #4: AskTool must only be registered in interactive mode.
//!
//! Headless modes (print / json / rpc) can't pop a TUI picker, so the
//! tool would always return `is_error: true` ("ASK requires interactive
//! mode") if the agent ever called it. We mirror how `approve` / `judge`
//! are wired by mode: the tool is simply absent from the registry
//! outside interactive runs.

use clap::Parser;
use pi_coding_agent::cli::Cli;
use pi_coding_agent::startup::assemble;

fn isolated_env() -> tempfile::TempDir {
    let td = tempfile::tempdir().expect("tempdir");
    // Point every env-rooted resolver at the empty tempdir so the test
    // is hermetic — no inheriting the developer's ~/.pi/agent.
    std::env::set_var("PI_CODING_AGENT_DIR", td.path().join("agent"));
    std::env::set_var("PI_PACKAGE_DIR", td.path().join("packages"));
    std::env::set_var("PI_WORKTREE_ROOT", td.path().join("wt"));
    td
}

#[tokio::test]
async fn ask_is_registered_in_interactive_mode() {
    let _td = isolated_env();
    let cli = Cli::try_parse_from(["pi", "--no-context-files", "--no-extensions"])
        .expect("parse interactive cli");
    let startup = assemble(cli).await.expect("assemble");
    let names = startup.runtime_config.tools.names();
    assert!(
        names.iter().any(|n| n == "ask"),
        "expected `ask` tool in interactive registry; got {names:?}"
    );
}

#[tokio::test]
async fn ask_is_absent_in_print_mode() {
    let _td = isolated_env();
    let cli = Cli::try_parse_from([
        "pi",
        "--print",
        "--no-context-files",
        "--no-extensions",
    ])
    .expect("parse print cli");
    let startup = assemble(cli).await.expect("assemble");
    let names = startup.runtime_config.tools.names();
    assert!(
        !names.iter().any(|n| n == "ask"),
        "expected `ask` tool to be absent in print/headless registry; got {names:?}"
    );
}
