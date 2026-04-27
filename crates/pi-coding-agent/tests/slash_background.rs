//! B4: /background slash is registered.

use pi_coding_agent::slash::SlashRegistry;

#[test]
fn background_slash_is_registered_as_builtin() {
    let r = SlashRegistry::new();
    let cmd = r.get("background").expect("/background registered");
    assert_eq!(cmd.name, "background");
    assert!(cmd.description.to_lowercase().contains("background")
        || cmd.description.to_lowercase().contains("detach"));
}

#[test]
fn skill_slash_is_registered_as_builtin() {
    let r = SlashRegistry::new();
    assert!(r.get("skill").is_some(), "/skill registered");
}
