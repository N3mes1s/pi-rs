use pi_agent_core::{
    default_embedding_model_path, fetch_default_embeddings, validate_embedding_model,
};

#[tokio::test]
async fn fetched_embedding_model_is_valid_onnx() {
    let path = default_embedding_model_path();
    if !path.exists() {
        fetch_default_embeddings().await.unwrap();
    }
    validate_embedding_model(&path).unwrap();
}
