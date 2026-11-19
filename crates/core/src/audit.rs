//! Local append-only JSONL audit log store with file rotation and windowed queries (P3-01).

use std::fs::OpenOptions;
use std::io::{Result, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::api::{AuditEntry, AuditLogPage};

/// Current UTC time as an RFC 3339 string with second precision (`2026-11-01T12:00:00Z`).
pub fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    rfc3339_from_unix(secs)
}

/// Formats `unix_seconds` as RFC 3339 UTC; falls back to the raw number if out of range.
pub fn rfc3339_from_unix(unix_seconds: i64) -> String {
    k8s_openapi::jiff::Timestamp::from_second(unix_seconds)
        .map(|ts| ts.to_string())
        .unwrap_or_else(|_| unix_seconds.to_string())
}

/// Default maximum size for an audit log file before rotation (10 MB).
const DEFAULT_MAX_BYTES: u64 = 10 * 1024 * 1024;
/// Default number of rotated backup files to retain.
const DEFAULT_MAX_BACKUPS: usize = 3;
/// Maximum page limit for audit log queries.
const MAX_QUERY_LIMIT: u32 = 1000;

/// Local append-only JSONL audit log store with file rotation and windowed queries.
#[derive(Debug)]
pub struct AuditLogStore {
    path: PathBuf,
    max_bytes: u64,
    max_backups: usize,
    /// Serialises file access. Holds the next [`AuditEntry::id`] to assign; `None` until
    /// the first append seeds it from the ids already on disk.
    lock: Mutex<Option<u32>>,
}

/// Only the `id` of a stored line, so seeding tolerates entries from older schemas.
#[derive(Deserialize)]
struct StoredId {
    id: u32,
}

/// Returns the next audit id after `id`. Wraps `u32::MAX` to `1` so ids never repeat `u32::MAX`.
fn next_id_after(id: u32) -> u32 {
    if id == u32::MAX {
        1
    } else {
        id.saturating_add(1)
    }
}

/// Advisory lock backed by a sidecar lock file (`<path>.lock`).
///
/// Ensures concurrent writers (such as the GUI and `fleetglass mcp`) do not
/// corrupt JSON lines, duplicate audit IDs, or race during file rotation.
struct SidecarLock {
    path: PathBuf,
}

impl SidecarLock {
    fn acquire(lock_path: &Path) -> Result<Self> {
        if let Some(parent) = lock_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(10);
        let mut attempts: u32 = 0;

        loop {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(lock_path)
            {
                Ok(mut file) => {
                    let _ = writeln!(file, "{}\n{}", std::process::id(), now_rfc3339());
                    let _ = file.sync_all();
                    return Ok(Self {
                        path: lock_path.to_path_buf(),
                    });
                }
                // On Windows, creating a file whose previous holder is still being deleted
                // (delete pending) fails with ERROR_ACCESS_DENIED instead of AlreadyExists.
                Err(err)
                    if err.kind() == std::io::ErrorKind::AlreadyExists
                        || (cfg!(windows)
                            && err.kind() == std::io::ErrorKind::PermissionDenied
                            && start.elapsed() < timeout) =>
                {
                    attempts = attempts.saturating_add(1);
                    // Check for stale lock after several failed attempts.
                    if attempts.is_multiple_of(10)
                        && let Ok(meta) = std::fs::metadata(lock_path)
                        && let Ok(modified) = meta.modified()
                        && let Ok(elapsed) = modified.elapsed()
                        && elapsed > std::time::Duration::from_secs(5)
                    {
                        tracing::warn!("breaking stale audit lock file {}", lock_path.display());
                        let _ = std::fs::remove_file(lock_path);
                        continue;
                    }

                    if start.elapsed() >= timeout {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            format!(
                                "timed out waiting for audit lock file {}",
                                lock_path.display()
                            ),
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(err) => return Err(err),
            }
        }
    }
}

impl Drop for SidecarLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl AuditLogStore {
    /// Creates a store with default rotation limits (10 MB per file, 3 backups).
    pub fn new(path: PathBuf) -> Self {
        Self::with_options(path, DEFAULT_MAX_BYTES, DEFAULT_MAX_BACKUPS)
    }

    /// Creates a store with custom rotation limits.
    pub fn with_options(path: PathBuf, max_bytes: u64, max_backups: usize) -> Self {
        Self {
            path,
            max_bytes,
            max_backups,
            lock: Mutex::new(None),
        }
    }

    /// Path to the active audit log file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Constructs the path for the `index`-th backup file (1-based: `audit.1.jsonl`, `audit.2.jsonl`, ...).
    fn backup_path(&self, index: usize) -> PathBuf {
        let file_stem = self
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("audit");
        let ext = self.path.extension().and_then(|s| s.to_str());
        let filename = match ext {
            Some(ext) => format!("{file_stem}.{index}.{ext}"),
            None => format!("{file_stem}.{index}"),
        };
        match self.path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.join(filename),
            _ => PathBuf::from(filename),
        }
    }

    /// Rotates backup files: `audit.N.jsonl` dropped, `audit.N-1` -> `audit.N`, ..., `audit.jsonl` -> `audit.1`.
    fn rotate(&self) -> Result<()> {
        if self.max_backups == 0 {
            if self.path.exists() {
                std::fs::remove_file(&self.path)?;
            }
            return Ok(());
        }

        for i in (1..=self.max_backups).rev() {
            let dst = self.backup_path(i);
            let src = if i == 1 {
                self.path.clone()
            } else {
                self.backup_path(i - 1)
            };

            if dst.exists() {
                std::fs::remove_file(&dst)?;
            }
            if src.exists() {
                std::fs::rename(&src, &dst)?;
            }
        }

        Ok(())
    }

    /// Active file followed by the rotated backups, newest first.
    fn all_files(&self) -> Vec<PathBuf> {
        let mut files = Vec::with_capacity(1 + self.max_backups);
        files.push(self.path.clone());
        for i in 1..=self.max_backups {
            files.push(self.backup_path(i));
        }
        files
    }

    /// Sidecar lock file path used for coordinating concurrent processes.
    fn lock_path(&self) -> PathBuf {
        let file_name = self
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("audit.jsonl");
        let lock_name = format!("{file_name}.lock");
        match self.path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.join(lock_name),
            _ => PathBuf::from(lock_name),
        }
    }

    /// Largest `id` in the active file and `audit.1` (0 if none).
    ///
    /// Malformed lines, non-UTF-8 bytes, and torn/truncated lines are tolerated
    /// gracefully with a warning instead of failing subsequent appends for the session.
    /// Only scans the active file and `audit.1`.
    fn seed_max_id(&self) -> u32 {
        let active = self.file_tail_id(&self.path);
        let backup1 = if self.max_backups > 0 {
            self.file_tail_id(&self.backup_path(1))
        } else {
            None
        };
        match (active, backup1) {
            (Some(a), Some(b)) => a.max(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => self.scan_max_id(),
        }
    }

    /// Reads up to the last 8 KB of `file_path` and finds the last valid [`StoredId`].
    fn file_tail_id(&self, file_path: &Path) -> Option<u32> {
        let mut file = std::fs::File::open(file_path).ok()?;
        let len = file.metadata().ok()?.len();
        if len == 0 {
            return None;
        }

        use std::io::{Read, Seek, SeekFrom};
        const TAIL_BYTES: u64 = 8192;
        let read_len = len.min(TAIL_BYTES);
        let seek_pos = len.saturating_sub(read_len);

        file.seek(SeekFrom::Start(seek_pos)).ok()?;
        let mut buf = Vec::with_capacity(read_len as usize);
        file.read_to_end(&mut buf).ok()?;

        let lossy = String::from_utf8_lossy(&buf);
        for line in lossy.lines().rev() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(stored) = serde_json::from_str::<StoredId>(trimmed) {
                return Some(stored.id);
            }
        }
        None
    }

    /// Full scan of active file and `audit.1`, tolerating non-UTF-8 and torn lines.
    fn scan_max_id(&self) -> u32 {
        let mut max = 0;
        let mut files = Vec::with_capacity(2);
        files.push(self.path.clone());
        if self.max_backups > 0 {
            files.push(self.backup_path(1));
        }

        for file_path in files {
            let bytes = match std::fs::read(&file_path) {
                Ok(b) => b,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    tracing::warn!(
                        "skipping audit log file {} during ID seeding: {err}",
                        file_path.display()
                    );
                    continue;
                }
            };
            let content = String::from_utf8_lossy(&bytes);
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<StoredId>(trimmed) {
                    Ok(stored) => {
                        max = max.max(stored.id);
                    }
                    Err(err) => {
                        tracing::warn!(
                            "skipping malformed or torn audit log line in {}: {err}",
                            file_path.display()
                        );
                    }
                }
            }
        }
        max
    }

    /// Appends `entry` as one JSONL line and returns the id it was stored with.
    ///
    /// The store assigns [`AuditEntry::id`] itself (the caller's value is ignored) so ids
    /// stay unique across writers and restarts: under the sidecar advisory file lock,
    /// the counter is seeded from the largest id in the active file and its `audit.1` backup.
    /// Rotates backups when the file would exceed `max_bytes`, and syncs the file after writing.
    pub fn append(&self, entry: &AuditEntry) -> Result<u32> {
        let mut next_id = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let _file_lock = SidecarLock::acquire(&self.lock_path())?;

        let disk_max = self.seed_max_id();
        let id = match *next_id {
            Some(cached) if disk_max == 0 => next_id_after(cached),
            _ => next_id_after(disk_max),
        };

        let stored = AuditEntry {
            id,
            ..entry.clone()
        };
        let mut line = serde_json::to_string(&stored)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        line.push('\n');
        let line_len = line.len() as u64;

        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        let current_size = match std::fs::metadata(&self.path) {
            Ok(meta) => meta.len(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => 0,
            Err(err) => return Err(err),
        };

        if current_size > 0 && current_size + line_len > self.max_bytes {
            self.rotate()?;
        }

        let mut file = OpenOptions::new()
            .read(true)
            .create(true)
            .append(true)
            .open(&self.path)?;

        // If the file ends with a torn line lacking a trailing newline, ensure a newline
        // precedes the new line so the new entry remains a distinct, valid JSONL line.
        if current_size > 0 {
            use std::io::{Read, Seek, SeekFrom};
            if file.seek(SeekFrom::End(-1)).is_ok() {
                let mut last_byte = [0u8; 1];
                if file.read_exact(&mut last_byte).is_ok() && last_byte[0] != b'\n' {
                    file.write_all(b"\n")?;
                }
            }
        }

        file.write_all(line.as_bytes())?;
        file.sync_all()?;

        // Only advance once the line is written, so a failed append doesn't burn an id.
        *next_id = Some(id);
        Ok(id)
    }

    /// [`Self::append`], logging a failure with `tracing::warn!` instead of returning it.
    ///
    /// For callers whose own result must not change when auditing fails (mutating actions).
    pub fn append_or_warn(&self, entry: &AuditEntry) {
        if let Err(err) = self.append(entry) {
            tracing::warn!(
                "failed to append audit entry ({}) to {}: {err}",
                entry.kubectl,
                self.path.display()
            );
        }
    }

    /// Queries audit log entries in reverse chronological order (newest first).
    ///
    /// Malformed lines are skipped gracefully (warning logged).
    /// Limit is clamped to at most 1000.
    pub fn query(
        &self,
        offset: u32,
        limit: u32,
        cluster: Option<&str>,
        action: Option<&str>,
    ) -> Result<AuditLogPage> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());

        let clamped_limit = limit.min(MAX_QUERY_LIMIT);

        let mut matching = Vec::new();

        for file_path in self.all_files() {
            let bytes = match std::fs::read(&file_path) {
                Ok(b) => b,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err),
            };
            let content = String::from_utf8_lossy(&bytes);

            for line in content.lines().rev() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                match serde_json::from_str::<AuditEntry>(trimmed) {
                    Ok(entry) => {
                        if matches_filter(&entry, cluster, action) {
                            matching.push(entry);
                        }
                    }
                    Err(err) => {
                        tracing::warn!(
                            "skipping malformed audit log line in {}: {err}",
                            file_path.display()
                        );
                    }
                }
            }
        }

        let total = matching.len() as u32;
        let entries = matching
            .into_iter()
            .skip(offset as usize)
            .take(clamped_limit as usize)
            .collect();

        Ok(AuditLogPage {
            total,
            offset,
            entries,
        })
    }
}

/// Checks whether `entry` matches optional `cluster` and `action` filters.
fn matches_filter(entry: &AuditEntry, cluster: Option<&str>, action: Option<&str>) -> bool {
    if let Some(c) = cluster {
        let trimmed = c.trim();
        if !trimmed.is_empty()
            && !entry
                .cluster
                .to_ascii_lowercase()
                .contains(&trimmed.to_ascii_lowercase())
        {
            return false;
        }
    }

    if let Some(a) = action {
        let trimmed = a.trim();
        if !trimmed.is_empty()
            && !entry
                .action
                .to_ascii_lowercase()
                .contains(&trimmed.to_ascii_lowercase())
        {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir(PathBuf);

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn unique_test_dir(label: &str) -> TestDir {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "fleetglass-audit-test-{}-{label}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create test dir");
        TestDir(dir)
    }

    fn sample_entry(id: u32, cluster: &str, action: &str) -> AuditEntry {
        AuditEntry {
            id,
            timestamp: "2026-03-31T12:00:00Z".to_string(),
            cluster: cluster.to_string(),
            user: Some("alice@example.com".to_string()),
            action: action.to_string(),
            kind: "Pod".to_string(),
            namespace: Some("default".to_string()),
            name: format!("pod-{id}"),
            kubectl: format!("kubectl delete pod pod-{id}"),
            success: true,
            error: None,
        }
    }

    #[test]
    fn rfc3339_formats_utc_seconds() {
        assert_eq!(rfc3339_from_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_from_unix(1_798_761_600), "2027-01-01T00:00:00Z");

        let now = now_rfc3339();
        assert_eq!(now.len(), "2026-11-01T12:00:00Z".len(), "got {now}");
        assert!(now.ends_with('Z'), "got {now}");
        assert!(now.parse::<k8s_openapi::jiff::Timestamp>().is_ok());
    }

    #[test]
    fn append_assigns_ids_ignoring_caller_value() {
        let test_dir = unique_test_dir("assign");
        let store = AuditLogStore::new(test_dir.0.join("audit.jsonl"));

        assert_eq!(
            store.append(&sample_entry(42, "prod", "delete")).unwrap(),
            1
        );
        assert_eq!(
            store.append(&sample_entry(42, "prod", "delete")).unwrap(),
            2
        );
        let ids: Vec<u32> = store
            .query(0, 10, None, None)
            .unwrap()
            .entries
            .iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(ids, vec![2, 1]);
    }

    #[test]
    fn reopened_store_continues_after_max_id_including_audit_1() {
        let test_dir = unique_test_dir("reopen");
        let log_path = test_dir.0.join("audit.jsonl");
        // Small files so the first entries rotate into `audit.1.jsonl`.
        let first = AuditLogStore::with_options(log_path.clone(), 450, 3);
        for _ in 0..5 {
            first.append(&sample_entry(0, "prod", "delete")).unwrap();
        }
        assert!(test_dir.0.join("audit.1.jsonl").exists());

        // audit.1 holding the highest id and junk lines.
        {
            let mut backup1 = OpenOptions::new()
                .create(true)
                .append(true)
                .open(test_dir.0.join("audit.1.jsonl"))
                .unwrap();
            let mut high = serde_json::to_string(&sample_entry(77, "prod", "scale")).unwrap();
            high.push('\n');
            backup1.write_all(high.as_bytes()).unwrap();
            backup1.write_all(b"{not-valid-json\n\n").unwrap();
        }

        // audit.2 holding an even higher id which should be IGNORED by seeding (only active + audit.1 are scanned).
        {
            let mut backup2 = OpenOptions::new()
                .create(true)
                .append(true)
                .open(test_dir.0.join("audit.2.jsonl"))
                .unwrap();
            let mut high2 = serde_json::to_string(&sample_entry(999, "prod", "scale")).unwrap();
            high2.push('\n');
            backup2.write_all(high2.as_bytes()).unwrap();
        }

        // Process restart: a fresh store on the same files.
        let reopened = AuditLogStore::with_options(log_path.clone(), 450, 3);
        let id = reopened
            .append(&sample_entry(1, "prod", "restart"))
            .unwrap();
        assert_eq!(
            id, 78,
            "seeding must use active + audit.1 and ignore audit.2"
        );
        assert_eq!(
            reopened
                .append(&sample_entry(1, "prod", "restart"))
                .unwrap(),
            79
        );

        let page = reopened.query(0, 100, None, None).unwrap();
        let mut ids: Vec<u32> = page.entries.iter().map(|e| e.id).collect();
        let total = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), total, "duplicate ids on disk: {ids:?}");
    }

    #[test]
    fn concurrent_appends_get_unique_ids() {
        let test_dir = unique_test_dir("concurrent");
        let store = std::sync::Arc::new(AuditLogStore::new(test_dir.0.join("audit.jsonl")));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let store = std::sync::Arc::clone(&store);
                std::thread::spawn(move || {
                    (0..25)
                        .map(|_| store.append(&sample_entry(0, "prod", "delete")).unwrap())
                        .collect::<Vec<u32>>()
                })
            })
            .collect();
        let mut returned: Vec<u32> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        returned.sort_unstable();
        assert_eq!(returned, (1..=200).collect::<Vec<u32>>());

        let mut stored: Vec<u32> = store
            .query(0, 1000, None, None)
            .unwrap()
            .entries
            .iter()
            .map(|e| e.id)
            .collect();
        stored.sort_unstable();
        assert_eq!(stored, returned);
    }

    #[test]
    fn append_and_query_newest_first() {
        let test_dir = unique_test_dir("newest-first");
        let log_path = test_dir.0.join("audit.jsonl");
        let store = AuditLogStore::new(log_path);

        store.append(&sample_entry(1, "prod", "delete")).unwrap();
        store.append(&sample_entry(2, "prod", "scale")).unwrap();
        store
            .append(&sample_entry(3, "staging", "restart"))
            .unwrap();

        let page = store.query(0, 10, None, None).unwrap();
        assert_eq!(page.total, 3);
        assert_eq!(page.offset, 0);
        assert_eq!(page.entries.len(), 3);
        assert_eq!(page.entries[0].id, 3);
        assert_eq!(page.entries[1].id, 2);
        assert_eq!(page.entries[2].id, 1);
    }

    #[test]
    fn pagination_offset_limit_and_clamping() {
        let test_dir = unique_test_dir("pagination");
        let log_path = test_dir.0.join("audit.jsonl");
        let store = AuditLogStore::new(log_path);

        for id in 1..=15 {
            store.append(&sample_entry(id, "prod", "delete")).unwrap();
        }

        // Page 1: first 5 items (ids 15 down to 11)
        let page1 = store.query(0, 5, None, None).unwrap();
        assert_eq!(page1.total, 15);
        assert_eq!(page1.offset, 0);
        assert_eq!(page1.entries.len(), 5);
        let ids1: Vec<u32> = page1.entries.iter().map(|e| e.id).collect();
        assert_eq!(ids1, vec![15, 14, 13, 12, 11]);

        // Page 2: next 5 items (ids 10 down to 6)
        let page2 = store.query(5, 5, None, None).unwrap();
        assert_eq!(page2.total, 15);
        assert_eq!(page2.offset, 5);
        assert_eq!(page2.entries.len(), 5);
        let ids2: Vec<u32> = page2.entries.iter().map(|e| e.id).collect();
        assert_eq!(ids2, vec![10, 9, 8, 7, 6]);

        // Page 3: offset 12, limit 5 (ids 3, 2, 1)
        let page3 = store.query(12, 5, None, None).unwrap();
        assert_eq!(page3.total, 15);
        assert_eq!(page3.offset, 12);
        assert_eq!(page3.entries.len(), 3);
        let ids3: Vec<u32> = page3.entries.iter().map(|e| e.id).collect();
        assert_eq!(ids3, vec![3, 2, 1]);

        // Beyond total: offset 20
        let page_empty = store.query(20, 5, None, None).unwrap();
        assert_eq!(page_empty.total, 15);
        assert_eq!(page_empty.offset, 20);
        assert!(page_empty.entries.is_empty());

        // Clamping limit to 1000
        let page_clamped = store.query(0, 5000, None, None).unwrap();
        assert_eq!(page_clamped.total, 15);
        assert_eq!(page_clamped.entries.len(), 15);

        // Verify clamping with >1000 entries
        let test_dir_large = unique_test_dir("large-pagination");
        let large_store = AuditLogStore::new(test_dir_large.0.join("audit.jsonl"));
        for id in 1..=1005 {
            large_store
                .append(&sample_entry(id, "prod", "delete"))
                .unwrap();
        }
        let clamped = large_store.query(0, 5000, None, None).unwrap();
        assert_eq!(clamped.total, 1005);
        assert_eq!(clamped.entries.len(), 1000);
        assert_eq!(clamped.entries[0].id, 1005);
        assert_eq!(clamped.entries[999].id, 6);
    }

    #[test]
    fn filtering_by_cluster_and_action() {
        let test_dir = unique_test_dir("filtering");
        let log_path = test_dir.0.join("audit.jsonl");
        let store = AuditLogStore::new(log_path);

        store
            .append(&sample_entry(1, "prod-us-east-1", "delete"))
            .unwrap();
        store
            .append(&sample_entry(2, "prod-eu-central-1", "scale"))
            .unwrap();
        store
            .append(&sample_entry(3, "staging-us-west", "delete"))
            .unwrap();
        store
            .append(&sample_entry(4, "staging-us-west", "restart"))
            .unwrap();

        // Substring & case-insensitive cluster filter
        let prod_page = store.query(0, 10, Some("PROD"), None).unwrap();
        assert_eq!(prod_page.total, 2);
        assert_eq!(prod_page.entries.len(), 2);
        assert_eq!(prod_page.entries[0].id, 2);
        assert_eq!(prod_page.entries[1].id, 1);

        let staging_page = store.query(0, 10, Some("staging"), None).unwrap();
        assert_eq!(staging_page.total, 2);
        assert_eq!(staging_page.entries[0].id, 4);
        assert_eq!(staging_page.entries[1].id, 3);

        // Action filter (case-insensitive substring)
        let delete_page = store.query(0, 10, None, Some("DELETE")).unwrap();
        assert_eq!(delete_page.total, 2);
        assert_eq!(delete_page.entries[0].id, 3);
        assert_eq!(delete_page.entries[1].id, 1);

        let scale_page = store.query(0, 10, None, Some("scale")).unwrap();
        assert_eq!(scale_page.total, 1);
        assert_eq!(scale_page.entries[0].id, 2);

        // Combined filter
        let combined = store.query(0, 10, Some("prod"), Some("delete")).unwrap();
        assert_eq!(combined.total, 1);
        assert_eq!(combined.entries[0].id, 1);

        // No match filter
        let no_match = store.query(0, 10, Some("dev-cluster"), None).unwrap();
        assert_eq!(no_match.total, 0);
        assert!(no_match.entries.is_empty());
    }

    #[test]
    fn file_rotation_and_query_across_rotated_files() {
        let test_dir = unique_test_dir("rotation");
        let log_path = test_dir.0.join("audit.jsonl");

        // Use a small max_bytes (450 bytes) and max_backups = 3
        // Each serialized sample_entry is ~220 bytes.
        let store = AuditLogStore::with_options(log_path.clone(), 450, 3);

        // Entry 1: size ~220 bytes -> fits in audit.jsonl
        store.append(&sample_entry(1, "prod", "delete")).unwrap();
        // Entry 2: size 220 + 220 = 440 <= 450 -> fits in audit.jsonl
        store.append(&sample_entry(2, "prod", "delete")).unwrap();
        // Entry 3: 440 + 220 = 660 > 450 -> rotates! audit.jsonl becomes audit.1.jsonl
        store.append(&sample_entry(3, "prod", "delete")).unwrap();

        let backup_1 = test_dir.0.join("audit.1.jsonl");
        assert!(
            backup_1.exists(),
            "audit.1.jsonl should be created on first rotation"
        );

        // Entry 4: 220 + 220 = 440 <= 450 -> fits in audit.jsonl
        store.append(&sample_entry(4, "prod", "delete")).unwrap();
        // Entry 5: 440 + 220 = 660 > 450 -> rotates!
        // audit.1.jsonl -> audit.2.jsonl, audit.jsonl -> audit.1.jsonl
        store.append(&sample_entry(5, "prod", "delete")).unwrap();

        let backup_2 = test_dir.0.join("audit.2.jsonl");
        assert!(
            backup_2.exists(),
            "audit.2.jsonl should be created on second rotation"
        );

        // Entry 6: 220 + 220 = 440 <= 450
        store.append(&sample_entry(6, "prod", "delete")).unwrap();
        // Entry 7: rotates!
        // audit.2 -> audit.3, audit.1 -> audit.2, audit -> audit.1
        store.append(&sample_entry(7, "prod", "delete")).unwrap();

        let backup_3 = test_dir.0.join("audit.3.jsonl");
        assert!(
            backup_3.exists(),
            "audit.3.jsonl should be created on third rotation"
        );

        // Query across all rotated files (active audit.jsonl + backups 1, 2, 3)
        let page = store.query(0, 100, None, None).unwrap();
        assert_eq!(page.total, 7);
        assert_eq!(page.entries.len(), 7);
        let ids: Vec<u32> = page.entries.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![7, 6, 5, 4, 3, 2, 1]);

        // Entry 8: fits
        store.append(&sample_entry(8, "prod", "delete")).unwrap();
        // Entry 9: rotates! Since max_backups = 3, audit.3 is dropped and replaced with old audit.2.
        store.append(&sample_entry(9, "prod", "delete")).unwrap();

        let page_after_drop = store.query(0, 100, None, None).unwrap();
        // Entry 1 and 2 were in oldest backup that got dropped
        assert!(page_after_drop.total < 9);
        let ids_after: Vec<u32> = page_after_drop.entries.iter().map(|e| e.id).collect();
        assert_eq!(ids_after[0], 9);
        assert_eq!(ids_after[1], 8);
    }

    #[test]
    fn malformed_line_recovery() {
        let test_dir = unique_test_dir("malformed");
        let log_path = test_dir.0.join("audit.jsonl");
        let store = AuditLogStore::new(log_path.clone());

        store.append(&sample_entry(1, "prod", "delete")).unwrap();

        // Inject unparseable corrupted line directly into audit.jsonl
        {
            let mut file = OpenOptions::new().append(true).open(&log_path).unwrap();
            file.write_all(b"{not-valid-json: true\n").unwrap();
            file.write_all(b"\n").unwrap(); // blank line
            file.sync_all().unwrap();
        }

        store.append(&sample_entry(2, "prod", "scale")).unwrap();

        // Query should succeed without error, skipping the malformed line
        let page = store
            .query(0, 10, None, None)
            .expect("query must not fail on malformed line");
        assert_eq!(page.total, 2);
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.entries[0].id, 2);
        assert_eq!(page.entries[1].id, 1);
    }

    #[test]
    fn seeding_tolerates_non_utf8_and_torn_lines() {
        let test_dir = unique_test_dir("torn-seed");
        let log_path = test_dir.0.join("audit.jsonl");
        let store = AuditLogStore::new(log_path.clone());
        store.append(&sample_entry(0, "prod", "delete")).unwrap(); // id 1
        store.append(&sample_entry(0, "prod", "scale")).unwrap(); // id 2

        // Append non-UTF-8 bytes and a torn line directly to active file
        {
            let mut file = OpenOptions::new().append(true).open(&log_path).unwrap();
            file.write_all(&[0xFF, 0xFE, 0xFD, b'\n']).unwrap();
            file.write_all(b"{\"id\": 999, \"torn\": true").unwrap(); // no closing brace or newline
            file.sync_all().unwrap();
        }

        // Fresh store instance: seeding should not fail
        let fresh = AuditLogStore::new(log_path);
        let id3 = fresh.append(&sample_entry(0, "prod", "restart")).unwrap();
        assert_eq!(id3, 3);

        // Query should also succeed and skip torn line
        let page = fresh.query(0, 10, None, None).unwrap();
        assert_eq!(page.total, 3);
        let ids: Vec<u32> = page.entries.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![3, 2, 1]);
    }

    #[test]
    fn id_wraps_at_u32_max_and_does_not_repeat() {
        let test_dir = unique_test_dir("u32-max");
        let log_path = test_dir.0.join("audit.jsonl");
        // Write an entry with id = u32::MAX
        {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .unwrap();
            let entry = sample_entry(u32::MAX, "prod", "delete");
            let mut line = serde_json::to_string(&entry).unwrap();
            line.push('\n');
            file.write_all(line.as_bytes()).unwrap();
            file.sync_all().unwrap();
        }

        let store = AuditLogStore::new(log_path);
        let id1 = store.append(&sample_entry(0, "prod", "scale")).unwrap();
        assert_eq!(id1, 1, "next id after u32::MAX should wrap to 1");
        let id2 = store.append(&sample_entry(0, "prod", "scale")).unwrap();
        assert_eq!(id2, 2, "next id after 1 should be 2");
    }

    #[test]
    fn two_stores_concurrent_appends_on_same_file() {
        let test_dir = unique_test_dir("two-stores");
        let log_path = test_dir.0.join("audit.jsonl");
        let store1 = std::sync::Arc::new(AuditLogStore::new(log_path.clone()));
        let store2 = std::sync::Arc::new(AuditLogStore::new(log_path));

        let handles: Vec<_> = (0..8)
            .map(|i| {
                let store = if i % 2 == 0 {
                    std::sync::Arc::clone(&store1)
                } else {
                    std::sync::Arc::clone(&store2)
                };
                std::thread::spawn(move || {
                    (0..25)
                        .map(|_| store.append(&sample_entry(0, "prod", "delete")).unwrap())
                        .collect::<Vec<u32>>()
                })
            })
            .collect();

        let mut returned: Vec<u32> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        returned.sort_unstable();
        assert_eq!(returned, (1..=200).collect::<Vec<u32>>());

        let page = store1.query(0, 1000, None, None).unwrap();
        assert_eq!(page.total, 200);
        let mut stored: Vec<u32> = page.entries.iter().map(|e| e.id).collect();
        stored.sort_unstable();
        assert_eq!(stored, returned);
    }
}
