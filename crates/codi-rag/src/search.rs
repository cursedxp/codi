//! BM25 full-text search (+ optional RRF fusion with embeddings from embed.rs).

use anyhow::{Context, Result};
use rusqlite::Connection;

/// A retrieved chunk with its relevance score.
#[derive(Debug, Clone)]
pub struct Chunk {
    pub id: i64,
    pub file_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub content: String,
    /// Relevance score (higher = more relevant; scale depends on search method).
    pub score: f64,
}

/// Search the FTS5 index with BM25 ranking. Returns up to `k` chunks.
pub fn bm25_search(conn: &Connection, query: &str, k: usize) -> Result<Vec<Chunk>> {
    if query.trim().is_empty() {
        return Ok(vec![]);
    }

    // Escape FTS5 special characters in the query.
    let safe_query = escape_fts5(query);

    let mut stmt = conn
        .prepare(
            "SELECT c.id, c.file_path, c.start_line, c.end_line, c.content,
                    -bm25(chunks_fts) AS score
             FROM chunks_fts
             JOIN chunks c ON c.id = chunks_fts.rowid
             WHERE chunks_fts MATCH ?
             ORDER BY score DESC
             LIMIT ?",
        )
        .context("preparing BM25 query")?;

    let chunks = stmt
        .query_map(rusqlite::params![safe_query, k as i64], |row| {
            Ok(Chunk {
                id: row.get(0)?,
                file_path: row.get(1)?,
                start_line: row.get(2)?,
                end_line: row.get(3)?,
                content: row.get(4)?,
                score: row.get(5)?,
            })
        })
        .context("executing BM25 query")?
        .collect::<Result<Vec<_>, _>>()
        .context("collecting BM25 results")?;

    Ok(chunks)
}

/// Fuse BM25 results and (optional) vector results using Reciprocal Rank Fusion.
///
/// RRF score = Σ 1/(k + rank_i) where k=60 is the standard constant.
pub fn rrf_fuse(bm25: Vec<Chunk>, vector: Vec<Chunk>, k: usize) -> Vec<Chunk> {
    use std::collections::HashMap;

    const RRF_K: f64 = 60.0;

    let mut scores: HashMap<i64, (Chunk, f64)> = HashMap::new();

    for (rank, chunk) in bm25.iter().enumerate() {
        let rrf = 1.0 / (RRF_K + (rank + 1) as f64);
        scores
            .entry(chunk.id)
            .and_modify(|(_, s)| *s += rrf)
            .or_insert_with(|| (chunk.clone(), rrf));
    }
    for (rank, chunk) in vector.iter().enumerate() {
        let rrf = 1.0 / (RRF_K + (rank + 1) as f64);
        scores
            .entry(chunk.id)
            .and_modify(|(_, s)| *s += rrf)
            .or_insert_with(|| (chunk.clone(), rrf));
    }

    let mut fused: Vec<Chunk> = scores
        .into_values()
        .map(|(mut c, s)| {
            c.score = s;
            c
        })
        .collect();
    fused.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    fused.truncate(k);
    fused
}

/// Format top chunks as a context block for injection into the model prompt.
pub fn format_context(chunks: &[Chunk]) -> String {
    if chunks.is_empty() {
        return String::new();
    }
    let mut out = String::from("# Repository context (retrieved)\n");
    for c in chunks {
        out.push_str(&format!(
            "\n## {} (lines {}-{})\n```\n{}\n```\n",
            c.file_path, c.start_line, c.end_line, c.content
        ));
    }
    out
}

/// Escape FTS5 query: wrap in quotes if it contains special characters.
fn escape_fts5(q: &str) -> String {
    // Simple approach: tokenize on whitespace, prefix-match each token.
    let tokens: Vec<String> = q
        .split_whitespace()
        .map(|t| {
            let safe: String = t.chars().filter(|c| c.is_alphanumeric() || *c == '_').collect();
            if safe.is_empty() { String::new() } else { format!("{safe}*") }
        })
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        q.to_string()
    } else {
        tokens.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use tempfile::tempdir;

    fn setup() -> (Connection, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let conn = db::open(&dir.path().join("test.sqlite")).unwrap();
        (conn, dir)
    }

    fn insert_chunk(conn: &Connection, path: &str, content: &str) {
        conn.execute(
            "INSERT INTO chunks (file_path, start_line, end_line, content, content_hash) VALUES (?, 1, 5, ?, 'abc')",
            rusqlite::params![path, content],
        )
        .unwrap();
    }

    #[test]
    fn bm25_finds_inserted_content() {
        let (conn, _dir) = setup();
        insert_chunk(&conn, "src/lib.rs", "pub fn hello_world() { println!(\"Hello\"); }");

        let results = bm25_search(&conn, "hello world", 5).unwrap();
        assert!(!results.is_empty());
        assert!(results[0].content.contains("hello_world"));
    }

    #[test]
    fn empty_query_returns_empty() {
        let (conn, _dir) = setup();
        let results = bm25_search(&conn, "", 5).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn rrf_fuse_deduplicates() {
        let make = |id: i64, score: f64| Chunk {
            id,
            file_path: "f".into(),
            start_line: 1,
            end_line: 2,
            content: "x".into(),
            score,
        };
        let bm25 = vec![make(1, 1.0), make(2, 0.8)];
        let vec_res = vec![make(1, 0.9), make(3, 0.7)];
        let fused = rrf_fuse(bm25, vec_res, 10);
        // IDs should be unique
        let ids: std::collections::HashSet<_> = fused.iter().map(|c| c.id).collect();
        assert_eq!(ids.len(), fused.len());
        // chunk 1 appears in both lists, should rank first
        assert_eq!(fused[0].id, 1);
    }
}
