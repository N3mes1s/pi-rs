use clap::Parser;
use pi_agent_core::RouteMode;
use pi_coding_agent::{cli::Cli, startup::assemble};

fn isolated_env() -> tempfile::TempDir {
    let td = tempfile::tempdir().expect("tempdir");
    std::env::set_var("PI_CODING_AGENT_DIR", td.path().join("agent"));
    std::env::set_var("PI_PACKAGE_DIR", td.path().join("packages"));
    std::env::set_var("PI_WORKTREE_ROOT", td.path().join("wt"));
    td
}

#[tokio::test]
async fn omitted_route_flag_preserves_settings_route_but_cli_still_overrides() {
    let td = isolated_env();
    let agent_dir = td.path().join("agent");
    std::fs::create_dir_all(&agent_dir).expect("create agent dir");
    std::fs::write(agent_dir.join("settings.json"), r#"{"route":"auto"}"#)
        .expect("write settings.json");

    let cwd = td.path().join("cwd");
    std::fs::create_dir_all(&cwd).expect("create cwd");
    let previous_cwd = std::env::current_dir().expect("current dir");
    std::env::set_current_dir(&cwd).expect("switch cwd");

    let cli = Cli::try_parse_from(["pi", "--no-context-files", "--no-extensions"])
        .expect("parse cli without explicit route");
    let startup = assemble(cli)
        .await
        .expect("assemble without explicit route");
    assert_eq!(startup.settings.route, RouteMode::Auto);

    let cli = Cli::try_parse_from([
        "pi",
        "--route",
        "off",
        "--no-context-files",
        "--no-extensions",
    ])
    .expect("parse cli with explicit route override");
    let startup = assemble(cli)
        .await
        .expect("assemble with explicit route override");
    assert_eq!(startup.settings.route, RouteMode::Off);

    std::env::set_current_dir(previous_cwd).expect("restore cwd");
}
