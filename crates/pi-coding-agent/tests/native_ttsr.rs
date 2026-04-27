//! B5: TTSR — rule loader, matcher, and a mock-provider injection test.

use pi_coding_agent::native::ttsr::{
    rule::{parse_rule, render_reminder, Rule},
    MatchResult, Matcher, RuleSet,
};
use std::path::PathBuf;
use tempfile::tempdir;

// ── parse_rule ──────────────────────────────────────────────────────────────

#[test]
fn parse_rule_minimal_quoted_trigger() {
    let raw = "---\nttsrTrigger: '\\bplan\\b'\n---\n\nStop and plan.\n".to_string();
    let r = parse_rule("planner".into(), raw, &PathBuf::from("planner.md")).expect("parsed");
    assert_eq!(r.name, "planner");
    assert_eq!(r.trigger_pattern, "\\bplan\\b");
    assert_eq!(r.body, "Stop and plan.");
}

#[test]
fn parse_rule_double_quoted_trigger() {
    let raw = "---\nttsrTrigger: \"foo|bar\"\n---\nbody\n".to_string();
    let r = parse_rule("x".into(), raw, &PathBuf::from("x.md")).unwrap();
    assert_eq!(r.trigger_pattern, "foo|bar");
}

#[test]
fn parse_rule_unquoted_trigger() {
    let raw = "---\nttsrTrigger: foo\n---\nbody\n".to_string();
    let r = parse_rule("x".into(), raw, &PathBuf::from("x.md")).unwrap();
    assert_eq!(r.trigger_pattern, "foo");
}

#[test]
fn parse_rule_no_frontmatter_returns_none() {
    let raw = "no leading dashes\nttsrTrigger: foo\n".to_string();
    assert!(parse_rule("x".into(), raw, &PathBuf::from("x.md")).is_none());
}

#[test]
fn parse_rule_no_trigger_returns_none() {
    let raw = "---\nfoo: bar\n---\nbody\n".to_string();
    assert!(parse_rule("x".into(), raw, &PathBuf::from("x.md")).is_none());
}

// ── RuleSet::load_dir ───────────────────────────────────────────────────────

#[test]
fn load_dir_picks_up_md_files_and_skips_garbage() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("good.md"),
        "---\nttsrTrigger: 'foo'\n---\nbody\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("bad.md"),
        "this is not frontmatter\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("not-md.txt"),
        "---\nttsrTrigger: 'foo'\n---\nignored\n",
    )
    .unwrap();
    let rs = RuleSet::load_dir(dir.path());
    assert_eq!(rs.len(), 1);
    assert_eq!(rs.rules()[0].0.name, "good");
}

#[test]
fn load_dir_returns_empty_for_missing_path() {
    let rs = RuleSet::load_dir(std::path::Path::new("/no/such/dir"));
    assert!(rs.is_empty());
}

#[test]
fn load_dir_skips_invalid_regex() {
    let dir = tempdir().unwrap();
    // `[` is an unterminated character class — invalid regex.
    std::fs::write(
        dir.path().join("bad.md"),
        "---\nttsrTrigger: '['\n---\nbody\n",
    )
    .unwrap();
    let rs = RuleSet::load_dir(dir.path());
    assert!(rs.is_empty());
}

// ── Matcher ─────────────────────────────────────────────────────────────────

fn ruleset_with(triggers: &[(&str, &str, &str)]) -> RuleSet {
    let mut rs = RuleSet::new();
    for (name, pat, body) in triggers {
        rs.push(Rule {
            name: (*name).into(),
            trigger_pattern: (*pat).into(),
            body: (*body).into(),
            path: PathBuf::from(format!("{name}.md")),
        })
        .expect("compiles");
    }
    rs
}

#[test]
fn matcher_fires_on_first_matching_rule() {
    let rs = ruleset_with(&[("a", "alpha", "do A"), ("b", "beta", "do B")]);
    let mut m = Matcher::new(&rs);
    assert_eq!(m.feed("hello "), MatchResult::None);
    assert_eq!(m.feed("alpha "), MatchResult::Fired { rule_index: 0 });
}

#[test]
fn matcher_fires_each_rule_at_most_once_per_session() {
    let rs = ruleset_with(&[("a", "alpha", "x")]);
    let mut m = Matcher::new(&rs);
    assert_eq!(m.feed("alpha"), MatchResult::Fired { rule_index: 0 });
    // Second occurrence does NOT re-fire.
    m.turn_reset();
    assert_eq!(m.feed("alpha alpha"), MatchResult::None);
}

#[test]
fn matcher_handles_split_delta_boundary() {
    let rs = ruleset_with(&[("p", "\\bplan\\b", "do plan")]);
    let mut m = Matcher::new(&rs);
    assert_eq!(m.feed("we should pl"), MatchResult::None);
    // Split across two deltas — buffer holds the joined text.
    assert_eq!(m.feed("an now"), MatchResult::Fired { rule_index: 0 });
}

#[test]
fn matcher_clear_fired_lets_rule_fire_again() {
    let rs = ruleset_with(&[("a", "alpha", "x")]);
    let mut m = Matcher::new(&rs);
    let _ = m.feed("alpha");
    m.clear_fired();
    m.turn_reset();
    assert_eq!(m.feed("alpha"), MatchResult::Fired { rule_index: 0 });
}

#[test]
fn render_reminder_wraps_body_in_system_reminder() {
    let r = Rule {
        name: "planner".into(),
        trigger_pattern: "x".into(),
        body: "stop and plan".into(),
        path: PathBuf::from("planner.md"),
    };
    let s = render_reminder(&r);
    assert!(s.contains("<system_reminder name=\"planner\">"));
    assert!(s.contains("stop and plan"));
    assert!(s.ends_with("</system_reminder>"));
}

// ── injection mechanic with a mock provider ─────────────────────────────────
//
// This test simulates the full injection loop without depending on
// pi-agent-core: a hand-rolled "provider" emits text deltas, the matcher
// observes them, and on `Fired` we collect the rendered reminder into the
// outgoing message list. After the simulated abort + restart, the
// reminder is the last user-side message — that's the contract.

#[derive(Default)]
struct MockTurn {
    deltas: Vec<&'static str>,
}

#[test]
fn injection_mechanic_aborts_and_appends_reminder() {
    let rules = ruleset_with(&[("planner", "\\bplan\\b", "STOP AND PLAN FIRST")]);
    let mut matcher = Matcher::new(&rules);

    let mut messages: Vec<String> = vec!["user: do the thing".to_string()];

    let turn1 = MockTurn {
        deltas: vec!["sure, let me ", "plan ", "the change"],
    };

    let mut consumed = String::new();
    let mut aborted = false;
    let mut fired_rule_idx = None;
    for d in &turn1.deltas {
        consumed.push_str(d);
        match matcher.feed(d) {
            MatchResult::None => continue,
            MatchResult::Fired { rule_index } => {
                aborted = true;
                fired_rule_idx = Some(rule_index);
                break;
            }
        }
    }
    assert!(aborted, "expected the planner rule to fire mid-stream");
    let rule_idx = fired_rule_idx.unwrap();
    let (rule, _) = &rules.rules()[rule_idx];
    messages.push(format!("user: {}", render_reminder(rule)));

    // After abort, only "sure, let me plan " (or up to where the trigger
    // matched) was streamed; the assistant's full reply was discarded.
    assert!(consumed.starts_with("sure, let me "));

    // The message stack now ends with the system reminder so the next
    // turn sees it.
    assert!(messages.last().unwrap().contains("STOP AND PLAN FIRST"));
    assert_eq!(messages.len(), 2);

    // Restart turn — the rule cannot fire again (one-shot per session).
    matcher.turn_reset();
    let turn2 = MockTurn {
        deltas: vec!["okay, here's the plan: ", "step one"],
    };
    let mut second_aborted = false;
    for d in &turn2.deltas {
        if matches!(matcher.feed(d), MatchResult::Fired { .. }) {
            second_aborted = true;
            break;
        }
    }
    assert!(!second_aborted, "rule must be one-shot per session");
}
