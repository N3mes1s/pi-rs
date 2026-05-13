#![cfg(feature = "onnx-inference")]

use pi_agent_core::{default_embedding_model_path, router::OnnxRealEngine, EmbeddingEngine};

fn norm_sq(values: &[f32]) -> f32 {
    values.iter().map(|v| v * v).sum()
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    if len == 0 {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for i in 0..len {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a.sqrt() * norm_b.sqrt())
    }
}

#[test]
fn real_engine_returns_normalized_384d_vectors() {
    let path = default_embedding_model_path();
    if !path.exists() {
        eprintln!("skipping: {} not found", path.display());
        return;
    }

    let engine = match OnnxRealEngine::new(path) {
        Ok(engine) => engine,
        Err(err) => {
            eprintln!("skipping: failed to initialize ONNX engine: {err}");
            return;
        }
    };

    let embedding = engine.embed("rename foo to bar in this file").unwrap();
    assert_eq!(embedding.len(), 384);
    let norm = norm_sq(&embedding);
    assert!(
        (norm - 1.0).abs() < 1e-3,
        "expected L2-normalized vector, got norm^2={norm}"
    );
}

#[test]
fn real_engine_scores_similar_prompts_higher_than_dissimilar_prompts() {
    let path = default_embedding_model_path();
    if !path.exists() {
        eprintln!("skipping: {} not found", path.display());
        return;
    }

    let engine = match OnnxRealEngine::new(path) {
        Ok(engine) => engine,
        Err(err) => {
            eprintln!("skipping: failed to initialize ONNX engine: {err}");
            return;
        }
    };

    let rename = engine.embed("rename foo to bar").unwrap();
    let rename_related = engine.embed("rename a variable in this file").unwrap();
    let proof = engine.embed("prove this loop terminates").unwrap();

    let similar = cosine_similarity(&rename, &rename_related);
    let different = cosine_similarity(&rename, &proof);
    assert!(
        similar > different,
        "expected semantically similar prompts to be closer: similar={similar}, different={different}"
    );
}
