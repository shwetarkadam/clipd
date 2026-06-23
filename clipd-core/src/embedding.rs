use crate::transform::TransformConfig;
use serde::{Deserialize, Serialize};

pub type Embedding = Vec<f32>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingResult {
    pub clip_id: i64,
    pub score: f32,
}

/// Generate an embedding vector for the given text using OpenAI-compatible API.
pub fn generate_embedding(text: &str, config: &TransformConfig) -> Result<Embedding, String> {
    let api_key = config
        .api_key
        .as_deref()
        .filter(|k| !k.is_empty())
        .ok_or("Embedding requires an API key in transform.json")?;

    let trimmed = truncate_for_embedding(text, 8000);

    let embed_url = embedding_url_from_config(config);

    let body = serde_json::json!({
        "model": "text-embedding-3-small",
        "input": trimmed,
    });

    let mut request = ureq::post(&embed_url).set("Content-Type", "application/json");
    if !api_key.is_empty() {
        request = request.set("Authorization", &format!("Bearer {}", api_key));
    }

    let response = request
        .send_json(body)
        .map_err(|e| format!("Embedding API error: {}", e))?;

    let resp: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("Failed to parse embedding response: {}", e))?;

    if let Some(err) = resp["error"]["message"].as_str() {
        return Err(format!("Embedding API error: {}", err));
    }

    let embedding = resp["data"][0]["embedding"]
        .as_array()
        .ok_or("Unexpected embedding response format")?
        .iter()
        .filter_map(|v| v.as_f64().map(|f| f as f32))
        .collect::<Vec<f32>>();

    if embedding.is_empty() {
        return Err("Empty embedding returned".into());
    }

    Ok(embedding)
}

/// Batch-generate embeddings for multiple texts in one API call.
pub fn generate_embeddings_batch(
    texts: &[&str],
    config: &TransformConfig,
) -> Result<Vec<Embedding>, String> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }

    let api_key = config
        .api_key
        .as_deref()
        .filter(|k| !k.is_empty())
        .ok_or("Embedding requires an API key")?;

    let trimmed: Vec<String> = texts
        .iter()
        .map(|t| truncate_for_embedding(t, 8000).into_owned())
        .collect();

    let embed_url = embedding_url_from_config(config);

    let body = serde_json::json!({
        "model": "text-embedding-3-small",
        "input": trimmed,
    });

    let mut request = ureq::post(&embed_url).set("Content-Type", "application/json");
    if !api_key.is_empty() {
        request = request.set("Authorization", &format!("Bearer {}", api_key));
    }

    let response = request
        .send_json(body)
        .map_err(|e| format!("Embedding API error: {}", e))?;

    let resp: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("Failed to parse embedding response: {}", e))?;

    if let Some(err) = resp["error"]["message"].as_str() {
        return Err(format!("API error: {}", err));
    }

    let data = resp["data"]
        .as_array()
        .ok_or("Unexpected batch embedding response")?;

    let mut results: Vec<(usize, Embedding)> = Vec::with_capacity(data.len());
    for item in data {
        let idx = item["index"].as_u64().unwrap_or(0) as usize;
        let vec: Embedding = item["embedding"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|v| v.as_f64().map(|f| f as f32))
            .collect();
        results.push((idx, vec));
    }
    results.sort_by_key(|(idx, _)| *idx);

    Ok(results.into_iter().map(|(_, v)| v).collect())
}

/// Cosine similarity between two vectors.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom < 1e-10 {
        0.0
    } else {
        dot / denom
    }
}

/// Search stored embeddings for the nearest matches to the query embedding.
pub fn search_embeddings(
    query: &Embedding,
    stored: &[(i64, Embedding)],
    max_results: usize,
    min_score: f32,
) -> Vec<EmbeddingResult> {
    let mut results: Vec<EmbeddingResult> = stored
        .iter()
        .map(|(clip_id, emb)| EmbeddingResult {
            clip_id: *clip_id,
            score: cosine_similarity(query, emb),
        })
        .filter(|r| r.score >= min_score)
        .collect();

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(max_results);
    results
}

/// Serialize an embedding to a compact binary blob (little-endian f32).
pub fn embedding_to_bytes(embedding: &[f32]) -> Vec<u8> {
    embedding.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Deserialize an embedding from a binary blob.
pub fn embedding_from_bytes(bytes: &[u8]) -> Embedding {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

fn truncate_for_embedding(text: &str, max_chars: usize) -> std::borrow::Cow<'_, str> {
    // Return Cow::Borrowed when no truncation needed to avoid an extra allocation.
    if text.chars().count() <= max_chars {
        std::borrow::Cow::Borrowed(text)
    } else {
        std::borrow::Cow::Owned(text.chars().take(max_chars).collect::<String>())
    }
}

fn embedding_url_from_config(config: &TransformConfig) -> String {
    let base = &config.api_url;
    if base.contains("/chat/completions") {
        base.replace("/chat/completions", "/embeddings")
    } else if base.ends_with("/v1") || base.ends_with("/v1/") {
        format!("{}/embeddings", base.trim_end_matches('/'))
    } else {
        format!(
            "{}/embeddings",
            base.trim_end_matches('/')
                .trim_end_matches("/chat/completions")
        )
    }
}

/// Check if embedding API is available (API key configured).
pub fn is_embedding_available(config: &TransformConfig) -> bool {
    config.api_key.as_deref().map_or(false, |k| !k.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity_identical() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 0.001);
    }

    #[test]
    fn test_embedding_roundtrip() {
        let original = vec![0.1, 0.2, -0.3, 0.45];
        let bytes = embedding_to_bytes(&original);
        let restored = embedding_from_bytes(&bytes);
        for (a, b) in original.iter().zip(restored.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn test_search_embeddings() {
        let query = vec![1.0, 0.0, 0.0];
        let stored = vec![
            (1, vec![0.9, 0.1, 0.0]),
            (2, vec![0.0, 1.0, 0.0]),
            (3, vec![0.8, 0.2, 0.1]),
        ];
        let results = search_embeddings(&query, &stored, 5, 0.5);
        assert_eq!(results[0].clip_id, 1);
    }

    #[test]
    fn test_url_derivation() {
        let config = TransformConfig {
            api_key: Some("test".into()),
            api_url: "https://api.openai.com/v1/chat/completions".into(),
            model: "gpt-4o-mini".into(),
        };
        let url = embedding_url_from_config(&config);
        assert_eq!(url, "https://api.openai.com/v1/embeddings");
    }
}
