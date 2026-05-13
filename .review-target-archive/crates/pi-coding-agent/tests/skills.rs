use pi_coding_agent::skills::SkillRegistry;

#[test]
fn load_dir_reads_skill_md_in_subdirs_and_bare_md_files() {
    let dir = tempfile::tempdir().unwrap();

    // Subdir with SKILL.md
    let sub = dir.path().join("deployer");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(
        sub.join("SKILL.md"),
        "# Deployer\n\nShip code to prod.\n\nMore body.\n",
    )
    .unwrap();

    // Bare top-level .md file
    std::fs::write(
        dir.path().join("greeter.md"),
        "\n# Greeter\nSay hi to people.\n",
    )
    .unwrap();

    // Non-md should be ignored.
    std::fs::write(dir.path().join("ignore.txt"), "nope").unwrap();

    let mut reg = SkillRegistry::new();
    reg.load_dir(dir.path());

    let names = reg.names();
    // Subdir skills are keyed by file_stem of SKILL.md → "SKILL".
    // Bare files are keyed by their stem → "greeter".
    assert!(names.contains(&"greeter".to_string()), "names: {:?}", names);
    assert!(
        names.iter().any(|n| n == "SKILL" || n == "deployer"),
        "expected the subdir skill, got: {:?}",
        names
    );

    let greeter = reg.get("greeter").expect("greeter loaded");
    // First non-blank, non-heading line.
    assert_eq!(greeter.description, "Say hi to people.");

    let sub_skill = reg
        .get("SKILL")
        .or_else(|| reg.get("deployer"))
        .expect("subdir skill loaded");
    assert_eq!(sub_skill.description, "Ship code to prod.");
}

#[test]
fn skill_description_skips_blank_and_heading_lines() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("only.md"),
        "\n\n# Heading\n\n## Sub\n\nThe real description here.\nThen more.\n",
    )
    .unwrap();
    let mut reg = SkillRegistry::new();
    reg.load_dir(dir.path());
    let s = reg.get("only").expect("loaded");
    assert_eq!(s.description, "The real description here.");
}
