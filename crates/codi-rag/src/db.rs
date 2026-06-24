//! SQLite + FTS5 database layer for the RAG index.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;

/// Open (or create) the SQLite database at `path`. Applies the schema if new.
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating db directory {}", parent.display()))?;
    }
    let conn = Connection::open(path)
        .with_context(|| format!("opening SQLite at {}", path.display()))?;

    apply_schema(&conn)?;
    Ok(conn)
}

fn apply_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        PRAGMA journal_mode=WAL;
        PRAGMA foreign_keys=ON;

        CREATE TABLE IF NOT EXISTS chunks (
            id          INTEGER PRIMARY KEY,
            file_path   TEXT    NOT NULL,
            start_line  INTEGER NOT NULL,
            end_line    INTEGER NOT NULL,
            content     TEXT    NOT NULL,
            content_hash TEXT   NOT NULL,
            indexed_at  INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE INDEX IF NOT EXISTS idx_chunks_file ON chunks(file_path);

        -- FTS5 virtual table for BM25 full-text search.
        CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts
            USING fts5(content, content='chunks', content_rowid='id', tokenize='porter unicode61');

        -- Keep FTS in sync.
        CREATE TRIGGER IF NOT EXISTS chunks_ai AFTER INSERT ON chunks BEGIN
            INSERT INTO chunks_fts(rowid, content) VALUES (new.id, new.content);
        END;
        CREATE TRIGGER IF NOT EXISTS chunks_ad AFTER DELETE ON chunks BEGIN
            INSERT INTO chunks_fts(chunks_fts, rowid, content) VALUES ('delete', old.id, old.content);
        END;
        CREATE TRIGGER IF NOT EXISTS chunks_au AFTER UPDATE ON chunks BEGIN
            INSERT INTO chunks_fts(chunks_fts, rowid, content) VALUES ('delete', old.id, old.content);
            INSERT INTO chunks_fts(rowid, content) VALUES (new.id, new.content);
        END;

        -- Optional: vector embeddings table (populated by embed.rs if enabled).
        CREATE TABLE IF NOT EXISTS embeddings (
            chunk_id    INTEGER PRIMARY KEY REFERENCES chunks(id) ON DELETE CASCADE,
            vector      BLOB NOT NULL   -- f32 little-endian array
        );
        ",
    )
    .context("applying SQLite schema")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn creates_schema() {
        let dir = tempdir().unwrap();
        let db = open(&dir.path().join("test.sqlite")).unwrap();
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
