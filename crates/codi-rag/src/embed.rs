//! Optional embedding client for hybrid retrieval.
//!
//! When `rag.embeddings = true` in codi.toml, this module calls the local
//! `/v1/embeddings` endpoint and stores vectors in the `embeddings` table.
//! On any error (model not available, bad config) it logs a warning and
//! degrades gracefully to BM25-only.

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// Configuration for the embedding endpoint.
#[derive(Debug, Clone)]
pub struct EmbedConfig {
    pub base_url: String,
    pub model: String,
    pub api_key: String,
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: Vec<&'a str>,
}

#[derive(Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedData>,
}

#[derive(Deserialize)]
struct EmbedData {
    embedding: Vec<f32>,
}

/// Compute embeddings for `texts` and store them in the `embeddings` table
/// for the corresponding `chunk_ids`. Silently returns `Ok(0)` if the
/// endpoint is unavailable.
pub async fn embed_chunks(
    conn: &Connection,
    cfg: &EmbedConfig,
    chunk_ids: &[i64],
    texts: &[String],
) -> Result<usize> {
    if chunk_ids.is_empty() {
        return Ok(0);
    }

    let url = format!("{}/embeddings", cfg.base_url.trim_end_matches('/'));
    let client = reqwest::Client::new();

    let body = EmbedRequest {
        model: &cfg.model,
        input: texts.iter().map(|s| s.as_str()).collect(),
    };

    let resp = match client
        .post(&url)
        .bearer_auth(&cfg.api_key)
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("embedding request failed (BM25 fallback): {e}");
            return Ok(0);
        }
    };

    let embed_resp: EmbedResponse = resp
        .json()
        .await
        .context("parsing embedding response")?;

    let mut count = 0usize;
    for (chunk_id, data) in chunk_ids.iter().zip(embed_resp.data.iter()) {
        let bytes = f32_slice_to_bytes(&data.embedding);
        conn.execute(
            "INSERT OR REPLACE INTO embeddings (chunk_id, vector) VALUES (?, ?)",
            rusqlite::params![chunk_id, bytes],
        )
        .context("storing embedding")?;
        count += 1;
    }

    Ok(count)
}

/// Cosine-similarity search over all stored embeddings for `query`.
/// Returns a sorted list of (chunk_id, similarity) pairs.
pub async fn vector_search(
    conn: &Connection,
    cfg: &EmbedConfig,
    query: &str,
    k: usize,
) -> Result<Vec<(i64, f64)>> {
    let query_vec = embed_one(cfg, query).await?;

    let mut stmt = conn
        .prepare("SELECT chunk_id, vector FROM embeddings")
        .context("preparing vector scan")?;

    let mut scored: Vec<(i64, f64)> = stmt
        .query_map([], |row| {
            let chunk_id: i64 = row.get(0)?;
            let bytes: Vec<u8> = row.get(1)?;
            Ok((chunk_id, bytes))
        })
        .context("querying embeddings")?
        .filter_map(|r| r.ok())
        .map(|(id, bytes)| {
            let vec = bytes_to_f32_slice(&bytes);
            let sim = cosine_similarity(&query_vec, &vec);
            (id, sim)
        })
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(k);
    Ok(scored)
}

async fn embed_one(cfg: &EmbedConfig, text: &str) -> Result<Vec<f32>> {
    let url = format!("{}/embeddings", cfg.base_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let body = EmbedRequest {
        model: &cfg.model,
        input: vec![text],
    };
    let resp: EmbedResponse = client
        .post(&url)
        .bearer_auth(&cfg.api_key)
        .json(&body)
        .send()
        .await
        .context("embed_one request")?
        .json()
        .await
        .context("embed_one response")?;
    resp.data
        .into_iter()
        .next()
        .map(|d| d.embedding)
        .context("empty embedding response")
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f64 = a.iter().zip(b.iter()).map(|(x, y)| (*x as f64) * (*y as f64)).sum();
    let mag_a: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let mag_b: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    if mag_a == 0.0 || mag_b == 0.0 {
        return 0.0;
    }
    dot / (mag_a * mag_b)
}

fn f32_slice_to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter()
        .flat_map(|f| f.to_le_bytes())
        .collect()
}

fn bytes_to_f32_slice(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identical() {
        let v = vec![1.0_f32, 0.0, 0.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal() {
        let a = vec![1.0_f32, 0.0];
        let b = vec![0.0_f32, 1.0];
        assert!((cosine_similarity(&a, &b)).abs() < 1e-6);
    }

    #[test]
    fn roundtrip_bytes() {
        let v = vec![1.5_f32, -0.25, 3.0];
        let b = f32_slice_to_bytes(&v);
        let back = bytes_to_f32_slice(&b);
        assert_eq!(v, back);
    }
}
