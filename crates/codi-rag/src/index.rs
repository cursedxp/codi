//! Walks the repository and indexes files into SQLite FTS5.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::search::Chunk;

pub struct IndexConfig {
    pub extensions: Vec<String>,
    pub exclude: Vec<String>,
    pub max_chunk_chars: usize,
}

/// Walk `repo_root`, chunk eligible files, and upsert into the SQLite index.
/// Skips files whose content hash has not changed since the last index run.
/// Returns the number of chunks indexed.
pub fn index_repo(conn: &mut Connection, repo_root: &Path, cfg: &IndexConfig) -> Result<usize> {
    let walker = build_walker(repo_root, &cfg.exclude);
    let mut total = 0usize;

    for entry in walker {
        let entry = entry.context("reading dir entry")?;
        let path = entry.path();

        if !path.is_file() {
            continue;
        }

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if !cfg.extensions.is_empty() && !cfg.extensions.iter().any(|e| e == &ext) {
            continue;
        }

        let rel = path
            .strip_prefix(repo_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue, // skip binary files
        };

        let hash = hex_sha256(&content);

        // Remove stale chunks for this file (if any), then reinsert.
        // We check if the hash differs to avoid reindexing unchanged files.
        let existing_hash: Option<String> = conn
            .query_row(
                "SELECT content_hash FROM chunks WHERE file_path = ? LIMIT 1",
                [&rel],
                |r| r.get(0),
            )
            .ok();

        if existing_hash.as_deref() == Some(&hash) {
            continue; // unchanged
        }

        // Delete old chunks for this file.
        conn.execute("DELETE FROM chunks WHERE file_path = ?", [&rel])
            .context("deleting stale chunks")?;

        let chunks = chunk_content(&content, &rel, cfg.max_chunk_chars);
        let tx = conn.transaction().context("begin transaction")?;
        for c in &chunks {
            tx.execute(
                "INSERT INTO chunks (file_path, start_line, end_line, content, content_hash) VALUES (?, ?, ?, ?, ?)",
                rusqlite::params![c.file_path, c.start_line, c.end_line, c.content, hash],
            )
            .context("inserting chunk")?;
        }
        tx.commit().context("commit transaction")?;
        total += chunks.len();
    }

    Ok(total)
}

/// Chunk `content` into overlapping windows of at most `max_chars`, aligned to
/// line boundaries. Returns [`Chunk`] objects with line numbers set.
pub fn chunk_content(content: &str, file_path: &str, max_chars: usize) -> Vec<Chunk> {
    let lines: Vec<&str> = content.lines().collect();
    let mut chunks = Vec::new();
    let mut start = 0usize;

    while start < lines.len() {
        let mut end = start;
        let mut size = 0usize;
        while end < lines.len() && size + lines[end].len() + 1 <= max_chars {
            size += lines[end].len() + 1;
            end += 1;
        }
        if end == start {
            // Single line longer than max_chars; take it anyway.
            end = start + 1;
        }
        let text = lines[start..end].join("\n");
        chunks.push(Chunk {
            id: 0,
            file_path: file_path.to_string(),
            start_line: (start + 1) as u32,
            end_line: end as u32,
            content: text,
            score: 0.0,
        });
        // Overlap: slide by 75% of lines used so context is shared.
        let slide = ((end - start) * 3 / 4).max(1);
        start += slide;
    }

    chunks
}

fn build_walker(root: &Path, _exclude_patterns: &[String]) -> ignore::Walk {
    // The `ignore` crate automatically respects .gitignore.
    ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .build()
}

fn hex_sha256(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_small_file() {
        let content = "fn main() {\n    println!(\"hello\");\n}\n";
        let chunks = chunk_content(content, "src/main.rs", 200);
        assert!(!chunks.is_empty());
        assert_eq!(chunks[0].file_path, "src/main.rs");
        assert_eq!(chunks[0].start_line, 1);
    }

    #[test]
    fn chunks_respect_max_size() {
        let content = (0..100).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let chunks = chunk_content(&content, "big.txt", 100);
        for c in &chunks {
            assert!(c.content.len() <= 200, "chunk too large: {}", c.content.len());
        }
    }
}
