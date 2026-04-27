//! Round-trip serde + on-disk persistence for the trajectory-recording
//! [`SessionEntryKind`] variants added by G1: `ContextLoad`, `Outcome`,
//! `EvolveMarker`. These power the AGENTS.md evolution daemon (G2-G9).

use pi_agent_core::{OutcomeSource, SessionEntry, SessionEntryKind, SessionManager};

fn make(kind: SessionEntryKind) -> SessionEntry {
    SessionEntry {
        id: "e1".into(),
        parent_id: None,
        timestamp: 0,
        kind,
    }
}

#[test]
fn context_load_round_trips_through_json() {
    let entry = make(SessionEntryKind::ContextLoad {
        source: "AGENTS.md".into(),
        bytes: 4321,
        tokens: Some(1080),
    });
    let json = serde_json::to_string(&entry).unwrap();
    assert!(json.contains("\"kind\":\"context_load\""));
    let back: SessionEntry = serde_json::from_str(&json).unwrap();
    match back.kind {
        SessionEntryKind::ContextLoad { source, bytes, tokens } => {
            assert_eq!(source, "AGENTS.md");
            assert_eq!(bytes, 4321);
            assert_eq!(tokens, Some(1080));
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn outcome_round_trips_with_each_source() {
    for src in [
        OutcomeSource::Explicit,
        OutcomeSource::Heuristic,
        OutcomeSource::LlmJudge,
        OutcomeSource::Replay,
    ] {
        let entry = make(SessionEntryKind::Outcome {
            success: true,
            source: src,
            score: Some(0.87),
            notes: Some("tests passed".into()),
        });
        let json = serde_json::to_string(&entry).unwrap();
        let back: SessionEntry = serde_json::from_str(&json).unwrap();
        match back.kind {
            SessionEntryKind::Outcome { source, score, success, .. } => {
                assert_eq!(source, src);
                assert_eq!(score, Some(0.87));
                assert!(success);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }
}

#[test]
fn outcome_source_serializes_snake_case() {
    let json = serde_json::to_string(&OutcomeSource::LlmJudge).unwrap();
    assert_eq!(json, "\"llm_judge\"");
    let back: OutcomeSource = serde_json::from_str("\"heuristic\"").unwrap();
    assert_eq!(back, OutcomeSource::Heuristic);
}

#[test]
fn evolve_marker_round_trips() {
    let entry = make(SessionEntryKind::EvolveMarker {
        agents_md_hash: "deadbeef".into(),
        generation: 7,
        lineage: vec!["aaa".into(), "bbb".into()],
    });
    let json = serde_json::to_string(&entry).unwrap();
    assert!(json.contains("\"kind\":\"evolve_marker\""));
    let back: SessionEntry = serde_json::from_str(&json).unwrap();
    match back.kind {
        SessionEntryKind::EvolveMarker { agents_md_hash, generation, lineage } => {
            assert_eq!(agents_md_hash, "deadbeef");
            assert_eq!(generation, 7);
            assert_eq!(lineage, vec!["aaa", "bbb"]);
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn new_variants_persist_to_jsonl_and_reload() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let mgr =
        SessionManager::on_disk(dir.path().to_path_buf(), cwd.path().to_path_buf()).unwrap();
    let meta = mgr.create("anthropic", "sonnet").unwrap();

    mgr.append(
        &meta.id,
        SessionEntryKind::ContextLoad {
            source: "AGENTS.md".into(),
            bytes: 1024,
            tokens: Some(256),
        },
    )
    .unwrap();
    mgr.append(
        &meta.id,
        SessionEntryKind::EvolveMarker {
            agents_md_hash: "h0".into(),
            generation: 0,
            lineage: vec![],
        },
    )
    .unwrap();
    mgr.append(
        &meta.id,
        SessionEntryKind::Outcome {
            success: false,
            source: OutcomeSource::Heuristic,
            score: Some(0.2),
            notes: Some("agent looped on file".into()),
        },
    )
    .unwrap();

    // Reload from disk into a fresh manager and verify the kinds survived.
    let mgr2 =
        SessionManager::on_disk(dir.path().to_path_buf(), cwd.path().to_path_buf()).unwrap();
    let reopened = mgr2.open_existing(&meta.id).unwrap();
    let branch = mgr2.current_branch(&reopened.id);
    let kinds: Vec<&str> = branch
        .iter()
        .map(|e| match &e.kind {
            SessionEntryKind::Meta { .. } => "meta",
            SessionEntryKind::ContextLoad { .. } => "context_load",
            SessionEntryKind::EvolveMarker { .. } => "evolve_marker",
            SessionEntryKind::Outcome { .. } => "outcome",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, vec!["meta", "context_load", "evolve_marker", "outcome"]);
}
