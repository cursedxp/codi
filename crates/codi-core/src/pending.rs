//! Pending improvement queue — JSON inbox.
//!
//! Only active items live here. Items are removed (not status-changed) on
//! approve or dismiss. History lives in `.codi/improvement_log.jsonl`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::risk::ImprovementCandidate;

pub struct PendingQueue {
    path: PathBuf,
    items: Vec<ImprovementCandidate>,
}

impl PendingQueue {
    /// Load the queue. Returns an empty queue when the file does not exist.
    pub fn load(path: &Path) -> Result<Self> {
        let items = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str::<Vec<ImprovementCandidate>>(&text)
                .with_context(|| format!("parsing {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(e) => {
                return Err(e).with_context(|| format!("reading {}", path.display()));
            }
        };
        Ok(PendingQueue { path: path.to_path_buf(), items })
    }

    pub fn items(&self) -> &[ImprovementCandidate] {
        &self.items
    }

    /// Push a candidate. Silently ignores duplicates (matching `id`).
    pub fn push(&mut self, candidate: ImprovementCandidate) -> Result<()> {
        if !self.items.iter().any(|c| c.id == candidate.id) {
            self.items.push(candidate);
        }
        Ok(())
    }

    /// Remove and return the candidate with `id`, or `None` if not found.
    pub fn remove(&mut self, id: &str) -> Option<ImprovementCandidate> {
        self.items.iter().position(|c| c.id == id).map(|i| self.items.remove(i))
    }

    /// Write current items to disk atomically (write-then-rename).
    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(&self.items)
            .context("serializing pending queue")?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &json)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("renaming to {}", self.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::risk::{ImprovementCandidate, RiskLevel};
    use tempfile::tempdir;

    fn candidate(id: &str, desc: &str) -> ImprovementCandidate {
        ImprovementCandidate {
            id: id.to_string(), description: desc.to_string(),
            risk: RiskLevel::High, risk_reason: "test".to_string(),
            source_signals: vec![], context: "src/lib.rs".to_string(),
            created_at: 0,
        }
    }

    #[test]
    fn load_missing_file_returns_empty_queue() {
        let dir = tempdir().unwrap();
        let q = PendingQueue::load(&dir.path().join("pending.json")).unwrap();
        assert!(q.items().is_empty());
    }

    #[test]
    fn push_save_and_reload_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("pending.json");
        let mut q = PendingQueue::load(&path).unwrap();
        q.push(candidate("abc", "fix something")).unwrap();
        q.save().unwrap();

        let q2 = PendingQueue::load(&path).unwrap();
        assert_eq!(q2.items().len(), 1);
        assert_eq!(q2.items()[0].id, "abc");
        assert_eq!(q2.items()[0].description, "fix something");
    }

    #[test]
    fn remove_returns_item_and_shrinks_queue() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("pending.json");
        let mut q = PendingQueue::load(&path).unwrap();
        q.push(candidate("id1", "task 1")).unwrap();
        q.push(candidate("id2", "task 2")).unwrap();

        let removed = q.remove("id1");
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().id, "id1");
        assert_eq!(q.items().len(), 1);
        assert_eq!(q.items()[0].id, "id2");
    }

    #[test]
    fn remove_unknown_id_returns_none() {
        let dir = tempdir().unwrap();
        let mut q = PendingQueue::load(&dir.path().join("p.json")).unwrap();
        assert!(q.remove("nope").is_none());
    }

    #[test]
    fn duplicate_id_is_silently_ignored() {
        let dir = tempdir().unwrap();
        let mut q = PendingQueue::load(&dir.path().join("p.json")).unwrap();
        q.push(candidate("dup", "first")).unwrap();
        q.push(candidate("dup", "second")).unwrap();
        assert_eq!(q.items().len(), 1);
        assert_eq!(q.items()[0].description, "first");
    }
}
