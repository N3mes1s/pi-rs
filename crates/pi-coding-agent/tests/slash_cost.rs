use pi_coding_agent::slash::{parse, SlashKind, SlashRegistry};
use pi_coding_agent::slash_cost::format_cost_report;
use pi_stats::aggregate::FolderStats;
use std::path::PathBuf;

#[test]
fn cost_is_a_registered_builtin() {
    let reg = SlashRegistry::new();
    let cmd = reg.get("cost").expect("cost builtin missing");
    assert!(matches!(cmd.kind, SlashKind::Builtin));
    assert!(cmd.description.to_lowercase().contains("cost"));
}

#[test]
fn parse_recognises_bare_cost() {
    let (name, args) = parse("/cost").unwrap();
    assert_eq!(name, "cost");
    assert_eq!(args, "");
}

#[test]
fn formatter_matches_cwd_and_renders_dollars() {
    let cwd = PathBuf::from("/repo/foo");
    let folders = vec![
        FolderStats {
            folder: "/repo/foo".into(),
            requests: 4,
            cost: 1.2345,
            input_tokens: 999,
            output_tokens: 42,
        },
        FolderStats {
            folder: "/repo/bar".into(),
            requests: 1,
            cost: 0.01,
            input_tokens: 1,
            output_tokens: 1,
        },
    ];
    let out = format_cost_report(&cwd, &folders);
    assert!(out.contains("$1.2345"), "{out}");
    assert!(out.contains("4 request(s)"), "{out}");
    assert!(!out.contains("/repo/bar"), "{out}");
}

#[test]
fn formatter_reports_no_usage_when_folder_absent() {
    let cwd = PathBuf::from("/elsewhere");
    let out = format_cost_report(&cwd, &[]);
    assert!(out.contains("no recorded usage"), "{out}");
}
