//! Workload logs that follow pods across rollouts (`P2-03`).
//!
//! The logic lives in two pure parts:
//! - [`PodSetTracker`] turns successive snapshots of the matching pods into the
//!   `(pod, container)` streams to start and stop;
//! - [`LogMux`] assigns a global `seq`, tags lines with their source, detects
//!   JSON and batches lines into [`MuxLogEvent::Lines`].
//!
//! [`spawn_workload_log_follower`] only wires a pod watcher and per-container
//! `log_stream`s to those parts.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use futures::{AsyncBufReadExt, StreamExt};
use k8s_openapi::api::apps::v1::{DaemonSet, Deployment, ReplicaSet, StatefulSet};
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kube::api::{Api, LogParams};
use kube::runtime::WatchStreamExt;
use kube::runtime::watcher::{self, Event};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::api::{MuxLogEvent, MuxLogLine, WorkloadLogTarget, WorkloadSelector};
use crate::logs::LogBuffer;

/// Maximum number of lines in one [`MuxLogEvent::Lines`] batch.
pub const MAX_BATCH_LINES: usize = 500;

/// Minimum interval between two flushes of buffered lines to the sink.
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(100);

/// Capacity of the internal channel from per-container streams to the mux.
const STREAM_CHANNEL_CAPACITY: usize = 1024;

/// Lines queued in the mux between two flushes before log streams are throttled.
const MAX_PENDING_LINES: usize = 10 * MAX_BATCH_LINES;

/// Events (including line batches moved ahead of pod events) queued between two flushes.
const MAX_PENDING_EVENTS: usize = 64;

/// A `(pod, container)` log stream key.
pub type StreamKey = (String, String);

/// Result of one [`PodSetTracker::diff_pods`] call. All lists are sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PodSetChange {
    /// Streams to start.
    pub joined: Vec<StreamKey>,
    /// Streams to stop.
    pub left: Vec<StreamKey>,
    /// Pods that had no stream before and have at least one now.
    pub pods_joined: Vec<String>,
    /// Pods that had at least one stream before and have none now.
    pub pods_left: Vec<String>,
}

/// Tracks which `(pod, container)` streams should be open for a changing pod set.
#[derive(Debug, Clone, Default)]
pub struct PodSetTracker {
    container: Option<String>,
    active: BTreeSet<StreamKey>,
}

impl PodSetTracker {
    /// Creates a tracker; when `container` is set only that container is tracked.
    #[must_use]
    pub fn new(container: Option<String>) -> Self {
        Self {
            container,
            active: BTreeSet::new(),
        }
    }

    /// Compares `current` (pod name, streamable container names) with the
    /// previous call and returns `(joined, left)` streams.
    pub fn diff(&mut self, current: &[(String, Vec<String>)]) -> (Vec<StreamKey>, Vec<StreamKey>) {
        let change = self.diff_pods(current);
        (change.joined, change.left)
    }

    /// Like [`PodSetTracker::diff`], but also reports pod-level joins and leaves.
    pub fn diff_pods(&mut self, current: &[(String, Vec<String>)]) -> PodSetChange {
        let desired: BTreeSet<StreamKey> = current
            .iter()
            .flat_map(|(pod, containers)| {
                containers
                    .iter()
                    .filter(|c| self.container.as_ref().is_none_or(|want| want == *c))
                    .map(move |c| (pod.clone(), c.clone()))
            })
            .collect();

        let before_pods = pods_of(&self.active);
        let after_pods = pods_of(&desired);

        let change = PodSetChange {
            joined: desired.difference(&self.active).cloned().collect(),
            left: self.active.difference(&desired).cloned().collect(),
            pods_joined: after_pods
                .difference(&before_pods)
                .map(|p| (*p).to_owned())
                .collect(),
            pods_left: before_pods
                .difference(&after_pods)
                .map(|p| (*p).to_owned())
                .collect(),
        };
        self.active = desired;
        change
    }

    /// Streams currently considered open, sorted.
    pub fn active(&self) -> impl Iterator<Item = &StreamKey> {
        self.active.iter()
    }
}

fn pods_of(streams: &BTreeSet<StreamKey>) -> BTreeSet<&str> {
    streams.iter().map(|(pod, _)| pod.as_str()).collect()
}

/// `true` when the trimmed `text` parses as a JSON object.
#[must_use]
pub fn is_json_object(text: &str) -> bool {
    let trimmed = text.trim();
    // Cheap pre-check so plain text lines never reach the parser.
    trimmed.starts_with('{')
        && trimmed.ends_with('}')
        && serde_json::from_str::<serde_json::Value>(trimmed).is_ok_and(|v| v.is_object())
}

/// Multiplexes lines from many `(pod, container)` streams into one ordered feed.
#[derive(Debug, Default)]
pub struct LogMux {
    next_seq: u32,
    pending: Vec<MuxLogLine>,
    buffer: Option<LogBuffer>,
}

impl LogMux {
    /// Creates a mux whose first line gets `seq == 0`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a mux whose first line gets `seq == start` (used to test wrapping).
    #[must_use]
    pub fn starting_at(start: u32) -> Self {
        Self {
            next_seq: start,
            ..Self::default()
        }
    }

    /// Also records every line as `[pod/container] text` in `buffer`, so the
    /// existing windowed query, regex search and export work on the merged feed.
    #[must_use]
    pub fn with_buffer(mut self, buffer: LogBuffer) -> Self {
        self.buffer = Some(buffer);
        self
    }

    /// The attached [`LogBuffer`], if any.
    #[must_use]
    pub fn buffer(&self) -> Option<&LogBuffer> {
        self.buffer.as_ref()
    }

    /// Tags and queues one line, returning its `seq`. A trailing `\n`/`\r\n` is
    /// stripped. `seq` wraps from `u32::MAX` to `0`.
    pub fn push(&mut self, pod: &str, container: &str, text: &str) -> u32 {
        let text = text
            .strip_suffix('\n')
            .map_or(text, |t| t.strip_suffix('\r').unwrap_or(t));
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        if let Some(buffer) = self.buffer.as_mut() {
            buffer.push_line(format!("[{pod}/{container}] {text}"));
        }
        self.pending.push(MuxLogLine {
            seq,
            pod: pod.to_owned(),
            container: container.to_owned(),
            text: text.to_owned(),
            json: is_json_object(text),
        });
        seq
    }

    /// Number of queued lines not yet taken.
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Drains queued lines as [`MuxLogEvent::Lines`] batches of at most
    /// [`MAX_BATCH_LINES`] lines each, in `seq` order.
    pub fn take_batches(&mut self) -> Vec<MuxLogEvent> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        let mut batches = Vec::with_capacity(self.pending.len().div_ceil(MAX_BATCH_LINES));
        let mut rest = std::mem::take(&mut self.pending);
        while rest.len() > MAX_BATCH_LINES {
            let tail = rest.split_off(MAX_BATCH_LINES);
            batches.push(MuxLogEvent::Lines { lines: rest });
            rest = tail;
        }
        batches.push(MuxLogEvent::Lines { lines: rest });
        batches
    }
}

/// Containers of `pod` whose logs can be streamed now (running or terminated).
/// Waiting containers are skipped and join once they start.
#[must_use]
pub fn streamable_containers(pod: &Pod) -> Vec<String> {
    pod.status
        .as_ref()
        .and_then(|s| s.container_statuses.as_ref())
        .map(|statuses| {
            statuses
                .iter()
                .filter(|cs| {
                    cs.state
                        .as_ref()
                        .is_some_and(|st| st.running.is_some() || st.terminated.is_some())
                })
                .map(|cs| cs.name.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Converts a workload `spec.selector` into a label selector string.
///
/// An empty selector is rejected so a broken workload never streams a whole namespace.
pub fn label_selector_to_string(selector: LabelSelector) -> Result<String, String> {
    let parsed = kube::core::Selector::try_from(selector)
        .map_err(|err| format!("invalid workload selector: {}", err.0))?;
    if parsed.selects_all() {
        return Err("workload selector is empty and would match every pod".to_owned());
    }
    Ok(parsed.to_string())
}

pub(crate) async fn resolve_label_selector(
    client: &kube::Client,
    namespace: &str,
    selector: &WorkloadSelector,
) -> Result<String, String> {
    let (kind, name) = match selector {
        WorkloadSelector::Pod { .. } => {
            return Err("cannot resolve label selector for a single pod".to_owned());
        }
        WorkloadSelector::Labels { selector } => return Ok(selector.clone()),
        WorkloadSelector::Owner { kind, name } => (kind, name),
    };
    let not_found = || format!("{kind} {namespace}/{name} has no spec.selector");
    let read_err = |err: kube::Error| format!("cannot read {kind} {namespace}/{name}: {err}");
    let label_selector = match kind.to_ascii_lowercase().as_str() {
        "deployment" => Some(
            Api::<Deployment>::namespaced(client.clone(), namespace)
                .get(name)
                .await
                .map_err(read_err)?
                .spec
                .selector,
        ),
        "statefulset" => Some(
            Api::<StatefulSet>::namespaced(client.clone(), namespace)
                .get(name)
                .await
                .map_err(read_err)?
                .spec
                .selector,
        ),
        "daemonset" => Some(
            Api::<DaemonSet>::namespaced(client.clone(), namespace)
                .get(name)
                .await
                .map_err(read_err)?
                .spec
                .selector,
        ),
        "replicaset" => Some(
            Api::<ReplicaSet>::namespaced(client.clone(), namespace)
                .get(name)
                .await
                .map_err(read_err)?
                .spec
                .selector,
        ),
        "job" => Api::<Job>::namespaced(client.clone(), namespace)
            .get(name)
            .await
            .map_err(read_err)?
            .spec
            .and_then(|s| s.selector),
        _ => {
            return Err(format!(
                "unsupported workload kind '{kind}': expected Deployment, StatefulSet, DaemonSet, ReplicaSet or Job"
            ));
        }
    }
    .ok_or_else(not_found)?;
    label_selector_to_string(label_selector)
}

enum StreamMsg {
    Line {
        pod: String,
        container: String,
        text: String,
    },
    Error(String),
}

/// Open per-container log tasks; aborts them all when dropped (including when
/// the follower task itself is aborted).
#[derive(Default)]
struct StreamSet(HashMap<StreamKey, JoinHandle<()>>);

impl Drop for StreamSet {
    fn drop(&mut self) {
        for handle in self.0.values() {
            handle.abort();
        }
    }
}

/// Upper bound for [`WorkloadLogTarget::since_seconds`]: 7 days.
pub const MAX_SINCE_SECONDS: u32 = 7 * 24 * 60 * 60;

/// Normalizes a requested `since_seconds`: `0` means no limit, larger values
/// are capped at [`MAX_SINCE_SECONDS`].
fn clamp_since_seconds(since: Option<u32>) -> Option<u32> {
    since.filter(|&s| s > 0).map(|s| s.min(MAX_SINCE_SECONDS))
}

/// Initial history requested for a stream; `None`s mean "from the first line".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct History {
    tail_lines: Option<u32>,
    since_seconds: Option<u32>,
}

fn container_log_params(container: &str, history: History) -> LogParams {
    LogParams {
        container: Some(container.to_owned()),
        follow: true,
        tail_lines: history.tail_lines.map(i64::from),
        since_seconds: clamp_since_seconds(history.since_seconds).map(i64::from),
        ..LogParams::default()
    }
}

fn spawn_container_stream(
    api: Api<Pod>,
    (pod, container): StreamKey,
    history: History,
    tx: mpsc::Sender<StreamMsg>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let lp = container_log_params(&container, history);
        let reader = match api.log_stream(&pod, &lp).await {
            Ok(reader) => reader,
            Err(err) => {
                let msg = format!("cannot stream logs of {pod}/{container}: {err}");
                let _ = tx.send(StreamMsg::Error(msg)).await;
                return;
            }
        };
        let mut reader = std::pin::pin!(reader);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) => return,
                Ok(_) => {
                    let msg = StreamMsg::Line {
                        pod: pod.clone(),
                        container: container.clone(),
                        text: String::from_utf8_lossy(&buf).into_owned(),
                    };
                    if tx.send(msg).await.is_err() {
                        return;
                    }
                }
                Err(err) => {
                    let msg = format!("log stream of {pod}/{container} failed: {err}");
                    let _ = tx.send(StreamMsg::Error(msg)).await;
                    return;
                }
            }
        }
    })
}

/// Queues a pod/error event behind the lines received before it, so the UI
/// sees `PodJoined` before that pod's first line and `PodLeft` after its last.
fn queue_event(mux: &mut LogMux, events: &mut Vec<MuxLogEvent>, event: MuxLogEvent) {
    events.extend(mux.take_batches());
    events.push(event);
}

/// Sends `events`, then the lines queued since, to `sink`. Returns `false` once the sink is closed.
async fn flush(
    mux: &mut LogMux,
    events: Vec<MuxLogEvent>,
    sink: &mpsc::Sender<MuxLogEvent>,
) -> bool {
    for event in events.into_iter().chain(mux.take_batches()) {
        if sink.send(event).await.is_err() {
            return false;
        }
    }
    true
}

/// Follows the logs of every pod matching `target`, starting and stopping
/// per-container streams as pods come and go, and pushes [`MuxLogEvent`]s to
/// `sink` (lines at most every [`FLUSH_INTERVAL`]). The task ends when `sink`
/// is closed or the pod watch ends; aborting it stops all streams.
///
/// `target.tail_lines` and `target.since_seconds` apply to pods present at
/// start; later pods stream from their first line.
///
/// With `target.previous` (single pod only, else one [`MuxLogEvent::Error`])
/// the logs of the previous container instances are fetched once and the task
/// ends: no follow and no reconnect.
pub async fn spawn_workload_log_follower(
    client: kube::Client,
    target: WorkloadLogTarget,
    sink: mpsc::Sender<MuxLogEvent>,
) -> JoinHandle<()> {
    spawn_workload_log_follower_from(client, target, 0, sink)
}

/// [`spawn_workload_log_follower`] whose first line gets `seq == first_seq`, so a
/// stream restarted after a pause continues the sequence of the old one.
///
/// Must be called within a tokio runtime.
#[must_use]
pub fn spawn_workload_log_follower_from(
    client: kube::Client,
    target: WorkloadLogTarget,
    first_seq: u32,
    sink: mpsc::Sender<MuxLogEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(message) = run_follower(client, target, first_seq, &sink).await {
            let _ = sink.send(MuxLogEvent::Error { message }).await;
        }
    })
}

/// Extra seconds of history requested when a paused stream resumes, so lines
/// that were in flight when it stopped are not lost (they may arrive twice).
pub const RESUME_OVERLAP_SECS: u32 = 1;

/// Most lines per container replayed when a paused stream resumes.
pub const RESUME_MAX_TAIL_LINES: u32 = 1000;

/// Target that restarts a stream stopped `paused_for` ago (app in the background,
/// 03-04a) from where it stopped.
///
/// Log lines carry no timestamps here, so the restart asks for
/// `since_seconds = ceil(paused_for) + RESUME_OVERLAP_SECS`: the lines logged
/// while paused, plus up to ~2 s of lines that may already have been shown
/// (the API's resolution is one second). At most [`RESUME_MAX_TAIL_LINES`] (or the
/// target's own smaller `tail_lines`) per container are replayed, so a chatty
/// pod after a long pause cannot flood the view; older lines of such a pause are
/// skipped. Pods that started while paused are streamed from their first line.
#[must_use]
pub fn resume_target(target: &WorkloadLogTarget, paused_for: Duration) -> WorkloadLogTarget {
    let secs = paused_for
        .as_secs()
        .saturating_add(u64::from(paused_for.subsec_nanos() > 0));
    let since = u32::try_from(secs)
        .unwrap_or(u32::MAX)
        .saturating_add(RESUME_OVERLAP_SECS)
        .min(MAX_SINCE_SECONDS);
    WorkloadLogTarget {
        tail_lines: Some(
            target
                .tail_lines
                .map_or(RESUME_MAX_TAIL_LINES, |t| t.min(RESUME_MAX_TAIL_LINES)),
        ),
        since_seconds: Some(since),
        ..target.clone()
    }
}

/// Checks a target before any request: `previous` needs a single pod.
pub fn validate_target(target: &WorkloadLogTarget) -> Result<(), String> {
    if target.previous && !matches!(target.selector, WorkloadSelector::Pod { .. }) {
        return Err(
            "previous container logs are only available for a single pod, not a workload"
                .to_owned(),
        );
    }
    Ok(())
}

/// One-shot params for the logs of the previous instance of `container`.
fn previous_log_params(container: &str, target: &WorkloadLogTarget) -> LogParams {
    LogParams {
        container: Some(container.to_owned()),
        previous: true,
        follow: false,
        tail_lines: target.tail_lines.map(i64::from),
        since_seconds: clamp_since_seconds(target.since_seconds).map(i64::from),
        ..LogParams::default()
    }
}

/// `true` for the API answer to `previous=true` when the container never
/// terminated ("previous terminated container \"x\" in pod \"y\" not found").
fn is_no_previous_container(err: &kube::Error) -> bool {
    match err {
        // The API answers 400 BadRequest; matched on the message only so
        // a server returning another code is still recognised.
        kube::Error::Api(status) => {
            status.message.contains("previous terminated container")
                && status.message.contains("not found")
        }
        _ => false,
    }
}

/// User-readable message for a failed previous-logs fetch.
fn previous_error_message(pod: &str, container: &str, err: &kube::Error) -> String {
    if is_no_previous_container(err) {
        format!("No previous container logs for {container}")
    } else {
        format!("cannot read previous logs of {pod}/{container}: {err}")
    }
}

/// Sends the queued lines, then an error event (so the error follows the lines read before it).
async fn flush_then_error(
    mux: &mut LogMux,
    message: String,
    sink: &mpsc::Sender<MuxLogEvent>,
) -> bool {
    let mut events = Vec::new();
    queue_event(mux, &mut events, MuxLogEvent::Error { message });
    flush(mux, events, sink).await
}

/// Fetches the logs of the previous instance of every (or the chosen)
/// container of one pod and ends; no follow, no reconnect.
async fn run_previous(
    client: kube::Client,
    target: &WorkloadLogTarget,
    first_seq: u32,
    sink: &mpsc::Sender<MuxLogEvent>,
) -> Result<(), String> {
    let WorkloadSelector::Pod { name: pod } = &target.selector else {
        return validate_target(target);
    };
    let api: Api<Pod> = Api::namespaced(client, &target.namespace);
    let containers: Vec<String> = match &target.container {
        Some(container) => vec![container.clone()],
        None => api
            .get(pod)
            .await
            .map_err(|err| format!("cannot read pod {}/{pod}: {err}", target.namespace))?
            .spec
            .map(|s| s.containers.into_iter().map(|c| c.name).collect())
            .unwrap_or_default(),
    };
    let mut mux = LogMux::starting_at(first_seq);
    if sink
        .send(MuxLogEvent::PodJoined { pod: pod.clone() })
        .await
        .is_err()
    {
        return Ok(());
    }
    for container in containers {
        let lp = previous_log_params(&container, target);
        let reader = match api.log_stream(pod, &lp).await {
            Ok(reader) => reader,
            Err(err) => {
                let message = previous_error_message(pod, &container, &err);
                if !flush_then_error(&mut mux, message, sink).await {
                    return Ok(());
                }
                continue;
            }
        };
        let mut reader = std::pin::pin!(reader);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf);
                    if is_kubelet_missing_log_line(&line) {
                        let message = format!(
                            "No previous container logs for {container} (the container was removed)"
                        );
                        if !flush_then_error(&mut mux, message, sink).await {
                            return Ok(());
                        }
                        break;
                    }
                    mux.push(pod, &container, &line);
                    if mux.pending_len() >= MAX_BATCH_LINES
                        && !flush(&mut mux, Vec::new(), sink).await
                    {
                        return Ok(());
                    }
                }
                Err(err) => {
                    let message = format!("previous logs of {pod}/{container} failed: {err}");
                    if !flush_then_error(&mut mux, message, sink).await {
                        return Ok(());
                    }
                    break;
                }
            }
        }
        if !flush(&mut mux, Vec::new(), sink).await {
            return Ok(());
        }
    }
    Ok(())
}

async fn run_follower(
    client: kube::Client,
    target: WorkloadLogTarget,
    first_seq: u32,
    sink: &mpsc::Sender<MuxLogEvent>,
) -> Result<(), String> {
    validate_target(&target)?;
    if target.previous {
        return run_previous(client, &target, first_seq, sink).await;
    }
    let api: Api<Pod> = Api::namespaced(client.clone(), &target.namespace);
    let config = match &target.selector {
        WorkloadSelector::Pod { name } => {
            watcher::Config::default().fields(&format!("metadata.name={name}"))
        }
        WorkloadSelector::Labels { .. } | WorkloadSelector::Owner { .. } => {
            let selector =
                resolve_label_selector(&client, &target.namespace, &target.selector).await?;
            watcher::Config::default().labels(&selector)
        }
    };
    let watch = watcher::watcher(api.clone(), config).default_backoff();
    let mut watch = std::pin::pin!(watch);

    let (line_tx, mut line_rx) = mpsc::channel(STREAM_CHANNEL_CAPACITY);
    let mut tracker = PodSetTracker::new(target.container.clone());
    let mut mux = LogMux::starting_at(first_seq);
    let mut streams = StreamSet::default();
    let mut pods: HashMap<String, Vec<String>> = HashMap::new();
    let mut init_pods: HashMap<String, Vec<String>> = HashMap::new();
    let mut initial_sync_done = false;
    let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut events: Vec<MuxLogEvent> = Vec::new();

    loop {
        let mut pods_changed = false;
        tokio::select! {
            () = sink.closed() => return Ok(()),
            item = watch.next() => match item {
                None => return Ok(()),
                Some(Ok(Event::Apply(pod))) => {
                    if let Some(name) = pod.metadata.name.clone() {
                        pods.insert(name, streamable_containers(&pod));
                        pods_changed = true;
                    }
                }
                Some(Ok(Event::Delete(pod))) => {
                    if let Some(name) = pod.metadata.name.as_deref() {
                        pods_changed = pods.remove(name).is_some();
                    }
                }
                Some(Ok(Event::Init)) => init_pods.clear(),
                Some(Ok(Event::InitApply(pod))) => {
                    if let Some(name) = pod.metadata.name.clone() {
                        init_pods.insert(name, streamable_containers(&pod));
                    }
                }
                Some(Ok(Event::InitDone)) => {
                    pods = std::mem::take(&mut init_pods);
                    pods_changed = true;
                }
                Some(Err(err)) => {
                    tracing::warn!(namespace = %target.namespace, error = %err, "workload log pod watch error");
                    queue_event(&mut mux, &mut events, MuxLogEvent::Error { message: format!("pod watch error: {err}") });
                }
            },
            // Stop draining when enough is queued, so the bounded channel pushes
            // back on the log streams until the next flush.
            Some(msg) = line_rx.recv(), if mux.pending_len() < MAX_PENDING_LINES && events.len() < MAX_PENDING_EVENTS => match msg {
                StreamMsg::Line { pod, container, text } => {
                    mux.push(&pod, &container, &text);
                }
                StreamMsg::Error(message) => queue_event(&mut mux, &mut events, MuxLogEvent::Error { message }),
            },
            _ = ticker.tick() => {
                let pending = std::mem::take(&mut events);
                if !flush(&mut mux, pending, sink).await {
                    return Ok(());
                }
            }
        }

        if pods_changed {
            let current: Vec<(String, Vec<String>)> =
                pods.iter().map(|(p, c)| (p.clone(), c.clone())).collect();
            let change = tracker.diff_pods(&current);
            let history = if initial_sync_done {
                History::default()
            } else {
                History {
                    tail_lines: target.tail_lines,
                    since_seconds: target.since_seconds,
                }
            };
            initial_sync_done = true;
            for key in change.left {
                if let Some(handle) = streams.0.remove(&key) {
                    handle.abort();
                }
            }
            for key in change.joined {
                let handle =
                    spawn_container_stream(api.clone(), key.clone(), history, line_tx.clone());
                streams.0.insert(key, handle);
            }
            for pod in change.pods_left {
                queue_event(&mut mux, &mut events, MuxLogEvent::PodLeft { pod });
            }
            for pod in change.pods_joined {
                queue_event(&mut mux, &mut events, MuxLogEvent::PodJoined { pod });
            }
        }
    }
}

/// The kubelet reports a garbage-collected previous container log as a plain log line
/// rather than an API error; recognise both wordings it uses.
fn is_kubelet_missing_log_line(line: &str) -> bool {
    line.starts_with("unable to retrieve container logs")
        || (line.starts_with("failed to try resolving symlinks in path")
            && line.contains("no such file or directory"))
}

#[cfg(test)]
mod tests {
    use k8s_openapi::api::core::v1::{
        ContainerState, ContainerStateRunning, ContainerStateTerminated, ContainerStateWaiting,
        ContainerStatus, PodStatus,
    };
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelectorRequirement;

    use super::*;

    fn snapshot(pods: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
        pods.iter()
            .map(|(p, cs)| {
                (
                    (*p).to_owned(),
                    cs.iter().map(|c| (*c).to_owned()).collect(),
                )
            })
            .collect()
    }

    fn key(pod: &str, container: &str) -> StreamKey {
        (pod.to_owned(), container.to_owned())
    }

    fn lines(event: &MuxLogEvent) -> &[MuxLogLine] {
        match event {
            MuxLogEvent::Lines { lines } => lines,
            other => panic!("expected Lines, got {other:?}"),
        }
    }

    #[test]
    fn tracker_follows_a_rolling_update() {
        let mut t = PodSetTracker::new(None);

        let (joined, left) = t.diff(&snapshot(&[
            ("web-old-1", &["app"]),
            ("web-old-2", &["app"]),
        ]));
        assert_eq!(
            joined,
            vec![key("web-old-1", "app"), key("web-old-2", "app")]
        );
        assert!(left.is_empty());

        // Surge: a new pod starts, old ones remain.
        let change = t.diff_pods(&snapshot(&[
            ("web-old-1", &["app"]),
            ("web-old-2", &["app"]),
            ("web-new-1", &["app"]),
        ]));
        assert_eq!(change.joined, vec![key("web-new-1", "app")]);
        assert!(change.left.is_empty());
        assert_eq!(change.pods_joined, vec!["web-new-1".to_owned()]);
        assert!(change.pods_left.is_empty());

        // Old pod 1 goes away, new pod 2 comes up in the same step.
        let change = t.diff_pods(&snapshot(&[
            ("web-old-2", &["app"]),
            ("web-new-1", &["app"]),
            ("web-new-2", &["app"]),
        ]));
        assert_eq!(change.joined, vec![key("web-new-2", "app")]);
        assert_eq!(change.left, vec![key("web-old-1", "app")]);
        assert_eq!(change.pods_left, vec!["web-old-1".to_owned()]);

        // Rollout complete.
        let (joined, left) = t.diff(&snapshot(&[
            ("web-new-1", &["app"]),
            ("web-new-2", &["app"]),
        ]));
        assert!(joined.is_empty());
        assert_eq!(left, vec![key("web-old-2", "app")]);

        // Unchanged snapshot is a no-op.
        let (joined, left) = t.diff(&snapshot(&[
            ("web-new-2", &["app"]),
            ("web-new-1", &["app"]),
        ]));
        assert!(joined.is_empty() && left.is_empty());
        let active: Vec<&StreamKey> = t.active().collect();
        assert_eq!(
            active,
            vec![&key("web-new-1", "app"), &key("web-new-2", "app")]
        );
    }

    #[test]
    fn tracker_handles_containers_starting_later_and_pod_level_events() {
        let mut t = PodSetTracker::new(None);
        // Pod exists but nothing is running yet: no streams, no PodJoined.
        let change = t.diff_pods(&snapshot(&[("p", &[])]));
        assert_eq!(change, PodSetChange::default());

        let change = t.diff_pods(&snapshot(&[("p", &["app"])]));
        assert_eq!(change.joined, vec![key("p", "app")]);
        assert_eq!(change.pods_joined, vec!["p".to_owned()]);

        // A sidecar starting later joins without a second PodJoined.
        let change = t.diff_pods(&snapshot(&[("p", &["app", "sidecar"])]));
        assert_eq!(change.joined, vec![key("p", "sidecar")]);
        assert!(change.pods_joined.is_empty());

        let change = t.diff_pods(&[]);
        assert_eq!(change.left, vec![key("p", "app"), key("p", "sidecar")]);
        assert_eq!(change.pods_left, vec!["p".to_owned()]);
    }

    #[test]
    fn tracker_container_filter_only_tracks_that_container() {
        let mut t = PodSetTracker::new(Some("app".to_owned()));
        let change = t.diff_pods(&snapshot(&[
            ("a", &["app", "istio-proxy"]),
            ("b", &["istio-proxy"]),
        ]));
        assert_eq!(change.joined, vec![key("a", "app")]);
        // A pod without the container is never reported as joined.
        assert_eq!(change.pods_joined, vec!["a".to_owned()]);

        let (joined, left) = t.diff(&snapshot(&[("b", &["istio-proxy", "app"])]));
        assert_eq!(joined, vec![key("b", "app")]);
        assert_eq!(left, vec![key("a", "app")]);
    }

    #[test]
    fn mux_seq_is_global_across_pods_and_tags_lines() {
        let mut mux = LogMux::new();
        assert_eq!(mux.push("a", "app", "one\n"), 0);
        assert_eq!(mux.push("b", "app", "two\r\n"), 1);
        assert_eq!(mux.push("a", "side", "three"), 2);
        assert_eq!(mux.pending_len(), 3);

        let batches = mux.take_batches();
        assert_eq!(batches.len(), 1);
        let got: Vec<(u32, &str, &str, &str)> = lines(&batches[0])
            .iter()
            .map(|l| (l.seq, l.pod.as_str(), l.container.as_str(), l.text.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                (0, "a", "app", "one"),
                (1, "b", "app", "two"),
                (2, "a", "side", "three")
            ]
        );
        assert!(mux.take_batches().is_empty());
        // Numbering continues after a flush.
        assert_eq!(mux.push("c", "app", "four"), 3);
    }

    #[test]
    fn mux_seq_wraps_at_u32_max() {
        let mut mux = LogMux::starting_at(u32::MAX - 1);
        assert_eq!(mux.push("a", "app", "x"), u32::MAX - 1);
        assert_eq!(mux.push("b", "app", "y"), u32::MAX);
        assert_eq!(mux.push("a", "app", "z"), 0);
        assert_eq!(mux.push("b", "app", "w"), 1);
        let batches = mux.take_batches();
        let seqs: Vec<u32> = lines(&batches[0]).iter().map(|l| l.seq).collect();
        assert_eq!(seqs, vec![u32::MAX - 1, u32::MAX, 0, 1]);
    }

    #[test]
    fn json_detection_accepts_only_objects() {
        assert!(is_json_object(r#"{"level":"info","msg":"hi"}"#));
        assert!(is_json_object("  {\"a\": {\"b\": [1, 2]}}  \n"));
        assert!(is_json_object("{}"));
        assert!(!is_json_object("[1, 2, 3]"));
        assert!(!is_json_object("42"));
        assert!(!is_json_object("\"string\""));
        assert!(!is_json_object("null"));
        assert!(!is_json_object("plain text log line"));
        assert!(!is_json_object("{not json}"));
        assert!(!is_json_object(""));

        let mut mux = LogMux::new();
        mux.push("p", "c", r#"{"level":"warn"}"#);
        mux.push("p", "c", "[1]");
        let batches = mux.take_batches();
        let flags: Vec<bool> = lines(&batches[0]).iter().map(|l| l.json).collect();
        assert_eq!(flags, vec![true, false]);
    }

    #[test]
    fn batches_hold_at_most_500_lines() {
        let mut mux = LogMux::new();
        for i in 0..1_201u32 {
            mux.push(
                if i % 2 == 0 { "a" } else { "b" },
                "app",
                &format!("line {i}"),
            );
        }
        let batches = mux.take_batches();
        let sizes: Vec<usize> = batches.iter().map(|b| lines(b).len()).collect();
        assert_eq!(sizes, vec![500, 500, 201]);
        let seqs: Vec<u32> = batches
            .iter()
            .flat_map(|b| lines(b).iter().map(|l| l.seq))
            .collect();
        assert_eq!(seqs, (0..1_201).collect::<Vec<u32>>());
        assert_eq!(mux.pending_len(), 0);

        for i in 0..500u32 {
            mux.push("a", "app", &format!("{i}"));
        }
        assert_eq!(mux.take_batches().len(), 1);
    }

    #[test]
    fn mux_records_tagged_lines_in_log_buffer() {
        let mut mux = LogMux::new().with_buffer(LogBuffer::new(10, None));
        mux.push("web-1", "app", "hello");
        mux.push("web-2", "app", "error: boom");
        let buffer = mux.buffer().unwrap();
        assert_eq!(buffer.total_lines(), 2);
        let page = buffer.search_regex("error", 0, 10).unwrap();
        assert_eq!(page.lines.len(), 1);
        assert_eq!(page.lines[0].seq, 1);
        assert_eq!(page.lines[0].text, "[web-2/app] error: boom");
    }

    fn status(name: &str, state: ContainerState) -> ContainerStatus {
        ContainerStatus {
            name: name.to_owned(),
            state: Some(state),
            ..Default::default()
        }
    }

    #[test]
    fn streamable_containers_skip_waiting() {
        let pod = Pod {
            status: Some(PodStatus {
                container_statuses: Some(vec![
                    status(
                        "app",
                        ContainerState {
                            running: Some(ContainerStateRunning::default()),
                            ..Default::default()
                        },
                    ),
                    status(
                        "init-done",
                        ContainerState {
                            terminated: Some(ContainerStateTerminated::default()),
                            ..Default::default()
                        },
                    ),
                    status(
                        "pulling",
                        ContainerState {
                            waiting: Some(ContainerStateWaiting::default()),
                            ..Default::default()
                        },
                    ),
                ]),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(streamable_containers(&pod), vec!["app", "init-done"]);
        assert!(streamable_containers(&Pod::default()).is_empty());
    }

    #[test]
    fn workload_selector_to_label_string() {
        let sel = LabelSelector {
            match_labels: Some([("app".to_owned(), "web".to_owned())].into()),
            match_expressions: Some(vec![LabelSelectorRequirement {
                key: "tier".to_owned(),
                operator: "In".to_owned(),
                values: Some(vec!["api".to_owned()]),
            }]),
        };
        assert_eq!(
            label_selector_to_string(sel).unwrap(),
            "app=web,tier in (api)"
        );
        assert!(label_selector_to_string(LabelSelector::default()).is_err());
        let bad = LabelSelector {
            match_expressions: Some(vec![LabelSelectorRequirement {
                key: "x".to_owned(),
                operator: "Bogus".to_owned(),
                values: None,
            }]),
            ..Default::default()
        };
        assert!(label_selector_to_string(bad).is_err());
    }

    #[tokio::test]
    async fn resolve_label_selector_rejects_pod_selector() {
        // resolve_label_selector should reject Pod selector without making network calls.
        let config = kube::Config::new("https://127.0.0.1:1".parse().expect("valid test uri"));
        let client = kube::Client::try_from(config).expect("client");
        let sel = WorkloadSelector::Pod {
            name: "counter".to_owned(),
        };
        let err = resolve_label_selector(&client, "default", &sel)
            .await
            .unwrap_err();
        assert!(err.contains("single pod"));
    }

    #[test]
    fn since_seconds_zero_is_none_and_large_values_are_capped() {
        assert_eq!(clamp_since_seconds(None), None);
        assert_eq!(clamp_since_seconds(Some(0)), None);
        assert_eq!(clamp_since_seconds(Some(300)), Some(300));
        assert_eq!(clamp_since_seconds(Some(604_800)), Some(604_800));
        assert_eq!(clamp_since_seconds(Some(604_801)), Some(604_800));
        assert_eq!(clamp_since_seconds(Some(u32::MAX)), Some(604_800));
    }

    #[test]
    fn container_log_params_pass_tail_and_since() {
        let lp = container_log_params(
            "app",
            History {
                tail_lines: Some(100),
                since_seconds: Some(3600),
            },
        );
        assert_eq!(lp.container.as_deref(), Some("app"));
        assert!(lp.follow);
        assert_eq!(lp.tail_lines, Some(100));
        assert_eq!(lp.since_seconds, Some(3600));

        let lp = container_log_params(
            "app",
            History {
                tail_lines: None,
                since_seconds: Some(0),
            },
        );
        assert_eq!(lp.tail_lines, None);
        assert_eq!(lp.since_seconds, None);

        // Pods joining after the initial sync stream from their first line.
        let lp = container_log_params("app", History::default());
        assert_eq!(lp.tail_lines, None);
        assert_eq!(lp.since_seconds, None);
    }

    fn target(selector: WorkloadSelector, previous: bool) -> WorkloadLogTarget {
        WorkloadLogTarget {
            cluster: "c".to_owned(),
            namespace: "ns".to_owned(),
            selector,
            container: None,
            tail_lines: Some(50),
            since_seconds: Some(0),
            previous,
        }
    }

    #[test]
    fn previous_is_only_valid_for_a_single_pod() {
        let pod = WorkloadSelector::Pod {
            name: "p".to_owned(),
        };
        let labels = WorkloadSelector::Labels {
            selector: "app=web".to_owned(),
        };
        let owner = WorkloadSelector::Owner {
            kind: "Deployment".to_owned(),
            name: "web".to_owned(),
        };
        assert!(validate_target(&target(pod, true)).is_ok());
        assert!(validate_target(&target(labels.clone(), false)).is_ok());
        let err = validate_target(&target(labels, true)).unwrap_err();
        assert!(err.contains("single pod"), "{err}");
        assert!(validate_target(&target(owner, true)).is_err());
    }

    #[tokio::test]
    async fn previous_with_a_workload_selector_emits_one_error_and_ends() {
        let config = kube::Config::new("https://127.0.0.1:1".parse().expect("valid test uri"));
        let client = kube::Client::try_from(config).expect("client");
        let (tx, mut rx) = mpsc::channel(8);
        let t = target(
            WorkloadSelector::Labels {
                selector: "app=web".to_owned(),
            },
            true,
        );
        let handle = spawn_workload_log_follower(client, t, tx).await;
        handle.await.expect("follower task");
        match rx.recv().await {
            Some(MuxLogEvent::Error { message }) => assert!(message.contains("single pod")),
            other => panic!("expected an error, got {other:?}"),
        }
        assert!(rx.recv().await.is_none(), "stream must end");
    }

    #[test]
    fn previous_log_params_are_one_shot() {
        let mut t = target(
            WorkloadSelector::Pod {
                name: "p".to_owned(),
            },
            true,
        );
        let lp = previous_log_params("app", &t);
        assert!(lp.previous);
        assert!(!lp.follow);
        assert_eq!(lp.container.as_deref(), Some("app"));
        assert_eq!(lp.tail_lines, Some(50));
        assert_eq!(lp.since_seconds, None);
        t.since_seconds = Some(u32::MAX);
        t.tail_lines = None;
        let lp = previous_log_params("app", &t);
        assert_eq!(lp.since_seconds, Some(i64::from(MAX_SINCE_SECONDS)));
        assert_eq!(lp.tail_lines, None);
    }

    #[test]
    fn kubelet_missing_log_lines_are_recognised() {
        assert!(is_kubelet_missing_log_line(
            "unable to retrieve container logs for containerd://abc"
        ));
        assert!(is_kubelet_missing_log_line(
            "failed to try resolving symlinks in path \"/var/log/pods/ns_p_uid/main/0.log\": \
             lstat /var/log/pods/ns_p_uid/main/0.log: no such file or directory"
        ));
        assert!(!is_kubelet_missing_log_line(
            "failed to try resolving symlinks is my app log"
        ));
        assert!(!is_kubelet_missing_log_line("crashing on purpose"));
    }

    #[test]
    fn missing_previous_container_gets_a_readable_message() {
        let status = kube::core::Status::failure(
            "previous terminated container \"main\" in pod \"web-1\" not found",
            "BadRequest",
        )
        .with_code(400);
        let err = kube::Error::Api(status.boxed());
        assert!(is_no_previous_container(&err));
        assert_eq!(
            previous_error_message("web-1", "main", &err),
            "No previous container logs for main"
        );

        let other = kube::Error::Api(
            kube::core::Status::failure("pods \"web-1\" not found", "NotFound")
                .with_code(404)
                .boxed(),
        );
        assert!(!is_no_previous_container(&other));
        let msg = previous_error_message("web-1", "main", &other);
        assert!(
            msg.starts_with("cannot read previous logs of web-1/main"),
            "{msg}"
        );
    }

    #[test]
    fn resume_target_asks_for_the_paused_span_with_a_bounded_tail() {
        let t = target(
            WorkloadSelector::Owner {
                kind: "Deployment".to_owned(),
                name: "web".to_owned(),
            },
            false,
        );
        let r = resume_target(&t, Duration::from_millis(4_200));
        assert_eq!(r.since_seconds, Some(5 + RESUME_OVERLAP_SECS));
        assert_eq!(r.tail_lines, Some(50), "a smaller user tail is kept");
        assert_eq!(r.selector, t.selector);
        assert!(!r.previous);

        let mut untailed = t.clone();
        untailed.tail_lines = None;
        let r = resume_target(&untailed, Duration::ZERO);
        assert_eq!(r.since_seconds, Some(RESUME_OVERLAP_SECS));
        assert_eq!(r.tail_lines, Some(RESUME_MAX_TAIL_LINES));

        let r = resume_target(&t, Duration::from_secs(30 * 24 * 3600));
        assert_eq!(r.since_seconds, Some(MAX_SINCE_SECONDS));
    }
}
