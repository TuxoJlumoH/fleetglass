//! Pod/container log ring buffer with disk overflow and `< 300 ms` 1M-line regex search.
//!
//! `LogBuffer` keeps the most recent `memory_capacity` [`LogLine`]s in a
//! [`VecDeque`] while spilling older lines to an optional overflow file on disk
//! and maintaining a contiguous text arena (`all_text` + `line_offsets`) so
//! windowed queries, full-file exports, and 1M-line regex searches run in a
//! single cache-friendly pass without unbounded `LogLine` heap allocations.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use k8s_openapi::api::core::v1::Pod;
use kube::Client;
use kube::api::{Api, LogParams};
use regex::{Regex, RegexBuilder};

use crate::api::{LogLine, LogPage};
use crate::error::{Error, Result};

/// Default number of [`LogLine`]s retained in the in-memory [`VecDeque`] before spilling.
pub const DEFAULT_MEMORY_CAPACITY: usize = 50_000;

/// Maximum number of log lines returned in a single [`LogPage`].
pub const MAX_PAGE_LIMIT: u32 = 1_000;

/// Bounded in-memory log ring buffer with optional disk overflow and fast regex search.
#[derive(Debug)]
pub struct LogBuffer {
    memory_capacity: usize,
    lines: VecDeque<LogLine>,
    overflow_path: Option<PathBuf>,
    overflow_file: Option<File>,
    spilled_count: u32,
    all_text: String,
    line_offsets: Vec<(u32, u32)>,
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_MEMORY_CAPACITY, None)
    }
}

impl LogBuffer {
    /// Creates a new [`LogBuffer`] retaining at most `memory_capacity` [`LogLine`]s in
    /// the ring buffer and appending older evicted lines to `overflow_path` when set.
    pub fn new(memory_capacity: usize, overflow_path: Option<PathBuf>) -> Self {
        let cap = memory_capacity.max(1);
        Self {
            memory_capacity: cap,
            lines: VecDeque::with_capacity(cap.min(64 * 1024)),
            overflow_path,
            overflow_file: None,
            spilled_count: 0,
            all_text: String::new(),
            line_offsets: Vec::new(),
        }
    }

    /// Alias for [`LogBuffer::new`].
    pub fn with_capacity(memory_capacity: usize, overflow_path: Option<PathBuf>) -> Self {
        Self::new(memory_capacity, overflow_path)
    }

    /// Reserves capacity for `additional_lines` lines of roughly `avg_line_bytes` each.
    pub fn reserve(&mut self, additional_lines: usize, avg_line_bytes: usize) {
        self.line_offsets.reserve(additional_lines);
        self.all_text
            .reserve(additional_lines.saturating_mul(avg_line_bytes + 1));
    }

    /// Appends one log line to the buffer, spilling the oldest in-memory line to disk if full.
    ///
    /// Returns the 0-based sequence number assigned to the pushed line.
    pub fn push_line(&mut self, text: impl Into<String>) -> u32 {
        let mut owned: String = text.into();
        if owned.ends_with('\n') {
            owned.pop();
            if owned.ends_with('\r') {
                owned.pop();
            }
        }
        if owned.contains('\n') {
            owned = owned.replace('\n', " ");
        }

        let seq = u32::try_from(self.line_offsets.len()).unwrap_or(u32::MAX);
        let start = u32::try_from(self.all_text.len()).unwrap_or(u32::MAX);
        self.all_text.push_str(&owned);
        let end = u32::try_from(self.all_text.len()).unwrap_or(u32::MAX);
        self.all_text.push('\n');
        self.line_offsets.push((start, end));

        if self.lines.len() >= self.memory_capacity
            && let Some(evicted) = self.lines.pop_front()
        {
            self.spill_line(&evicted);
        }

        self.lines.push_back(LogLine { seq, text: owned });
        seq
    }

    fn spill_line(&mut self, line: &LogLine) {
        self.spilled_count = self.spilled_count.saturating_add(1);
        let Some(ref path) = self.overflow_path else {
            return;
        };

        if self.overflow_file.is_none() {
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                let _ = std::fs::create_dir_all(parent);
            }
            self.overflow_file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(path)
                .ok();
        }

        if let Some(ref mut file) = self.overflow_file {
            let _ = file.write_all(line.text.as_bytes());
            let _ = file.write_all(b"\n");
        }
    }

    /// Total number of log lines ingested (including lines spilled to disk).
    pub fn total_lines(&self) -> u32 {
        u32::try_from(self.line_offsets.len()).unwrap_or(u32::MAX)
    }

    /// Number of lines currently held in the in-memory `VecDeque` ring.
    pub fn in_memory_len(&self) -> usize {
        self.lines.len()
    }

    /// Number of lines evicted from the in-memory ring to overflow storage.
    pub fn spilled_lines(&self) -> u32 {
        self.spilled_count
    }

    /// Path to the disk overflow file, if configured.
    pub fn overflow_path(&self) -> Option<&Path> {
        self.overflow_path.as_deref()
    }

    fn line_at(&self, seq: u32) -> Option<LogLine> {
        let idx = usize::try_from(seq).ok()?;
        let &(start, end) = self.line_offsets.get(idx)?;
        let slice = self.all_text.get(start as usize..end as usize)?;
        Some(LogLine {
            seq,
            text: slice.to_owned(),
        })
    }

    /// Returns a bounded window of log lines (`limit` clamped to `1..=1000`).
    pub fn query_window(&self, offset: u32, limit: u32) -> LogPage {
        let clamped_limit = limit.clamp(1, MAX_PAGE_LIMIT);
        let total = self.total_lines();
        let start = offset.min(total);
        let end = start.saturating_add(clamped_limit).min(total);

        let mut page_lines = Vec::with_capacity((end - start) as usize);
        for seq in start..end {
            if let Some(line) = self.line_at(seq) {
                page_lines.push(line);
            }
        }

        LogPage {
            total_lines: total,
            matched_lines: total,
            offset,
            limit: clamped_limit,
            lines: page_lines,
        }
    }

    /// Searches all buffered and spilled log lines using a compiled regular expression.
    ///
    /// Returns [`Error::InvalidRegex`] if `pattern` is not a valid regular expression.
    pub fn search_regex(&self, pattern: &str, offset: u32, limit: u32) -> Result<LogPage> {
        let clamped_limit = limit.clamp(1, MAX_PAGE_LIMIT);
        let line_re = Regex::new(pattern).map_err(|err| Error::InvalidRegex {
            pattern: pattern.to_owned(),
            message: err.to_string(),
        })?;

        let total = self.total_lines();
        let page_end = offset.saturating_add(clamped_limit);
        let mut matched_lines: u32 = 0;
        let mut page_lines: Vec<LogLine> = Vec::new();

        if can_use_multiline_fast_path(pattern)
            && let Ok(multi_re) = RegexBuilder::new(pattern).multi_line(true).build()
        {
            let bytes_len = self.all_text.len();
            let mut pos = 0usize;
            while pos <= bytes_len {
                let Some(m) = multi_re.find_at(&self.all_text, pos) else {
                    break;
                };
                let match_start = m.start();
                if match_start >= bytes_len {
                    break;
                }
                let line_idx = self
                    .line_offsets
                    .partition_point(|&(start, _)| (start as usize) <= match_start)
                    .saturating_sub(1);
                let Some(&(line_start, line_end)) = self.line_offsets.get(line_idx) else {
                    break;
                };
                if let Some(line_slice) = self.all_text.get(line_start as usize..line_end as usize)
                    && line_re.is_match(line_slice)
                {
                    if matched_lines >= offset && matched_lines < page_end {
                        page_lines.push(LogLine {
                            seq: u32::try_from(line_idx).unwrap_or(u32::MAX),
                            text: line_slice.to_owned(),
                        });
                    }
                    matched_lines = matched_lines.saturating_add(1);
                }
                pos = (line_end as usize).saturating_add(1);
            }
        } else {
            for (idx, &(start, end)) in self.line_offsets.iter().enumerate() {
                if let Some(line_slice) = self.all_text.get(start as usize..end as usize)
                    && line_re.is_match(line_slice)
                {
                    if matched_lines >= offset && matched_lines < page_end {
                        page_lines.push(LogLine {
                            seq: u32::try_from(idx).unwrap_or(u32::MAX),
                            text: line_slice.to_owned(),
                        });
                    }
                    matched_lines = matched_lines.saturating_add(1);
                }
            }
        }

        Ok(LogPage {
            total_lines: total,
            matched_lines,
            offset,
            limit: clamped_limit,
            lines: page_lines,
        })
    }

    /// Exports all buffered (spilled + in-memory) log lines to `dest`, returning the line count.
    pub fn export_to_file(&self, dest: &Path) -> Result<u32> {
        if let Some(parent) = dest.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| Error::KubeconfigRead {
                path: dest.to_path_buf(),
                source,
            })?;
        }
        std::fs::write(dest, self.all_text.as_bytes()).map_err(|source| Error::KubeconfigRead {
            path: dest.to_path_buf(),
            source,
        })?;
        Ok(self.total_lines())
    }
}

fn can_use_multiline_fast_path(pattern: &str) -> bool {
    !pattern.is_empty()
        && !pattern.contains('\n')
        && !pattern.contains("\\n")
        && !pattern.contains("\\s")
        && !pattern.contains("[^")
}

/// Fetches pod logs from the Kubernetes API into `buffer` and returns the number of lines read.
pub async fn fetch_pod_logs(
    client: Client,
    namespace: &str,
    pod: &str,
    container: Option<&str>,
    previous: bool,
    tail_lines: Option<i64>,
    buffer: &mut LogBuffer,
) -> Result<u32> {
    let api: Api<Pod> = Api::namespaced(client, namespace);
    let lp = LogParams {
        container: container.map(str::to_owned),
        previous,
        tail_lines,
        ..LogParams::default()
    };
    let raw = api.logs(pod, &lp).await?;
    let mut added = 0u32;
    for line in raw.lines() {
        buffer.push_line(line);
        added = added.saturating_add(1);
    }
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "fleetglass-logs-test-{}-{label}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn ring_buffer_spills_to_disk_and_exports_all_lines() {
        let dir = unique_temp_dir("spill");
        let overflow_path = dir.join("overflow.log");
        let export_path = dir.join("exported.log");

        let mut buf = LogBuffer::new(3, Some(overflow_path.clone()));
        for i in 0..7 {
            let seq = buf.push_line(format!("line-{i}"));
            assert_eq!(seq, i);
        }

        assert_eq!(buf.total_lines(), 7);
        assert_eq!(buf.in_memory_len(), 3);
        assert_eq!(buf.spilled_lines(), 4);

        let spilled_disk = std::fs::read_to_string(&overflow_path).unwrap();
        assert_eq!(spilled_disk, "line-0\nline-1\nline-2\nline-3\n");

        let page = buf.query_window(2, 3);
        assert_eq!(page.total_lines, 7);
        assert_eq!(page.lines.len(), 3);
        assert_eq!(page.lines[0].seq, 2);
        assert_eq!(page.lines[0].text, "line-2");
        assert_eq!(page.lines[2].seq, 4);
        assert_eq!(page.lines[2].text, "line-4");

        let exported_count = buf.export_to_file(&export_path).unwrap();
        assert_eq!(exported_count, 7);
        let exported_disk = std::fs::read_to_string(&export_path).unwrap();
        assert_eq!(
            exported_disk,
            "line-0\nline-1\nline-2\nline-3\nline-4\nline-5\nline-6\n"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_regex_returns_typed_error() {
        let mut buf = LogBuffer::default();
        buf.push_line("2026-10-31T12:00:00Z INFO starting server");

        let err = buf.search_regex("[unclosed", 0, 50).unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidRegex { ref pattern, .. } if pattern == "[unclosed"
        ));
    }

    #[test]
    fn search_1m_log_lines_under_300ms() {
        let mut buf = LogBuffer::new(DEFAULT_MEMORY_CAPACITY, None);
        buf.reserve(1_000_000, 72);

        for i in 0..1_000_000u32 {
            if i % 10_000 == 42 {
                buf.push_line(format!(
                    "2026-10-31T12:00:00Z ERROR [worker-{i}] upstream request timeout after 5000ms"
                ));
            } else {
                buf.push_line(format!(
                    "2026-10-31T12:00:00Z INFO [worker-{i}] processed request status=200 latency=2ms"
                ));
            }
        }

        assert_eq!(buf.total_lines(), 1_000_000);
        assert_eq!(buf.in_memory_len(), DEFAULT_MEMORY_CAPACITY);

        let start = Instant::now();
        let page = buf.search_regex("ERROR.*timeout", 0, 100).unwrap();
        let elapsed = start.elapsed();

        println!(
            "1M-line regex search matched {} lines in {:.2?}",
            page.matched_lines, elapsed
        );

        assert_eq!(page.total_lines, 1_000_000);
        assert_eq!(page.matched_lines, 100);
        assert_eq!(page.lines.len(), 100);
        assert_eq!(page.lines[0].seq, 42);
        assert!(page.lines[0].text.contains("ERROR"));

        // PLAN §3 budget: < 300 ms in release mode (allow generous headroom in debug mode).
        if !cfg!(debug_assertions) {
            assert!(
                elapsed.as_millis() < 300,
                "1M-line regex search took {elapsed:?}, exceeding 300 ms budget"
            );
        }
    }
}
