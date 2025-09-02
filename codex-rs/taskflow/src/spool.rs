use std::collections::BTreeMap;
use std::path::PathBuf;
use tokio::sync::Mutex;

use crate::error::TaskError;

/// Routed message spool for future-wave agents.
/// - In-memory queue per target task id
/// - Persisted append-only file per target task id under `<base_run_dir>/spool/<task-id>.txt`
///
/// On delivery, we read the persisted file and then clear both memory and file.
pub struct RoutedSpool {
    base_run_dir: PathBuf,
    mem: Mutex<BTreeMap<String, Vec<String>>>,
    // Track how many in-memory entries have been persisted to disk to avoid duplication
    persisted_counts: Mutex<BTreeMap<String, usize>>,
}

impl RoutedSpool {
    pub fn new(base_run_dir: PathBuf) -> Self {
        Self {
            base_run_dir,
            mem: Mutex::new(BTreeMap::new()),
            persisted_counts: Mutex::new(BTreeMap::new()),
        }
    }

    fn spool_dir(&self) -> PathBuf {
        self.base_run_dir.join("spool")
    }

    fn file_for(&self, task_id: &str) -> PathBuf {
        self.spool_dir().join(format!("{task_id}.txt"))
    }

    /// Append a message destined for `to_task_id` to the in-memory buffer
    /// and persist it to the per-run spool file.
    pub async fn append(&self, to_task_id: &str, msg: &str) -> Result<(), TaskError> {
        {
            let mut m = self.mem.lock().await;
            m.entry(to_task_id.to_string())
                .or_default()
                .push(msg.to_string());
        }
        // Ensure directory exists and append to file
        tokio::fs::create_dir_all(self.spool_dir()).await?;
        let path = self.file_for(to_task_id);
        let mut f = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await?;
        tokio::io::AsyncWriteExt::write_all(&mut f, msg.as_bytes()).await?;
        tokio::io::AsyncWriteExt::write_all(&mut f, b"\n").await?;

        // Record that we've persisted one more entry for this task to avoid duplication on read
        let mut pc = self.persisted_counts.lock().await;
        let e = pc.entry(to_task_id.to_string()).or_insert(0);
        *e += 1;
        Ok(())
    }

    /// Take all queued messages for a task, merging persisted file content with any
    /// additional in-memory entries that have not yet been persisted (best-effort),
    /// then clear both memory and persisted file.
    pub async fn take_all(&self, to_task_id: &str) -> Result<Vec<String>, TaskError> {
        // Read persisted file first (authoritative order)
        let path = self.file_for(to_task_id);
        let mut out: Vec<String> = Vec::new();
        if let Ok(data) = tokio::fs::read_to_string(&path).await {
            for line in data.lines() {
                if !line.is_empty() {
                    out.push(line.to_string());
                }
            }
        }

        // Merge any in-memory entries that were not persisted yet
        let mut mem_guard = self.mem.lock().await;
        let mut pc_guard = self.persisted_counts.lock().await;
        if let Some(buf) = mem_guard.get_mut(to_task_id) {
            let persisted = *pc_guard.get(to_task_id).unwrap_or(&0);
            if buf.len() > persisted {
                // Only take the tail that wasn't persisted to disk
                out.extend(buf[persisted..].iter().cloned());
            }
            // Clear memory state for this task
            buf.clear();
        }
        pc_guard.insert(to_task_id.to_string(), 0);

        // Truncate the persisted file after reading
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            let _ = tokio::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&path)
                .await?;
        }

        Ok(out)
    }

    #[cfg(test)]
    pub async fn __test_append_mem_only(&self, to_task_id: &str, msg: &str) {
        let mut m = self.mem.lock().await;
        m.entry(to_task_id.to_string())
            .or_default()
            .push(msg.to_string());
        // Do not update persisted_counts here; simulates not-yet-persisted entries.
    }
}

#[cfg(test)]
mod tests {
    use super::RoutedSpool;
    use std::path::PathBuf;
    use uuid::Uuid;

    #[tokio::test]
    async fn test_spool_append_take_clear_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("codex_spool_{}", Uuid::new_v4().to_string()));
        let s = RoutedSpool::new(tmp.clone());

        s.append("agentB", "hello").await.unwrap();
        s.append("agentB", "world").await.unwrap();

        let msgs = s.take_all("agentB").await.unwrap();
        assert_eq!(msgs, vec!["hello", "world"]);

        // After take_all, file should exist but be truncated
        let path = tmp.join("spool").join("agentB.txt");
        let data = tokio::fs::read(&path).await.unwrap_or_default();
        assert!(data.is_empty());
    }

    #[tokio::test]
    async fn test_spool_merge_persisted_and_memory() {
        let tmp = std::env::temp_dir().join(format!("codex_spool_{}", Uuid::new_v4().to_string()));
        let s = RoutedSpool::new(tmp.clone());
        let spool_file = tmp.join("spool").join("futureA.txt");

        // Pre-seed persisted file with two lines (simulates prior run append)
        tokio::fs::create_dir_all(spool_file.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&spool_file, b"persist1\npersist2\n")
            .await
            .unwrap();

        // Now add a mem-only message that hasn't been persisted yet
        s.__test_append_mem_only("futureA", "mem-only").await;

        let msgs = s.take_all("futureA").await.unwrap();
        assert_eq!(msgs, vec!["persist1", "persist2", "mem-only"]);

        // Ensure file is truncated and memory cleared
        let data = tokio::fs::read(&spool_file).await.unwrap_or_default();
        assert!(data.is_empty());
        let round2 = s.take_all("futureA").await.unwrap();
        assert!(round2.is_empty());
    }
}
