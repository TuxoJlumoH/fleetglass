# ADR 0002: Backend-Owned Compact Row Store and Windowed Query Architecture

- **Status:** Accepted (pending user confirmation at `S-GATE`)
- **Date:** 2026-10-31
- **Tasks:** `S-01`, `S-02`, `S-03`, `S-04`, `S-GATE`

## Context
Existing Kubernetes desktop clients (e.g. Lens/FreeLens, Headlamp) degrade or freeze on large clusters (10k–50k pods across multiple clusters) for two reasons:
1. They cache full Kubernetes JSON objects in memory for every watched resource.
2. They stream full resource lists across IPC to JavaScript and filter/sort in the renderer thread.

`docs/PLAN.md` §3 sets hard performance budgets:
- Open pod list (50k pods across 3 clusters): first rows **< 1.5 s**
- Scrolling / sorting / filtering 50k rows: **< 100 ms** per interaction (**< 50 ms** in the Rust query engine)
- Idle memory (1 cluster): **< 200 MB**
- Memory (3 clusters with 50k pods total): **< 500 MB**

## Decision
We accept and lock in the headless-core data architecture prototyped in `S-01`..`S-04`:

1. **Immediate projection into compact `Row`s (`crates/core/src/projection.rs`):**
   - Watch events from `kube::runtime::watcher` are projected immediately into `api::Row { uid, cluster, kind, namespace, name, status, status_reason, cells, labels }`.
   - Full Kubernetes objects are **not** retained in the list store; they will be fetched on demand only when a user opens a single resource's Detail/YAML view (`P1-05`).
   - Status (`RowStatus` + `status_reason`) is computed in Rust using `kubectl`'s precedence rules (`deletionTimestamp` → `initContainerStatuses` → `containerStatuses` → `status.reason`/`phase`).

2. **Per-`(cluster, kind)` `ResourceStore` with reference-counted idle eviction (`crates/core/src/cluster.rs`):**
   - `ClusterManager` connects to a kubeconfig context lazily (`connect_client`), injecting the cached login-shell environment (`ShellEnv`) for exec plugins.
   - Each `(cluster, kind)` has a reference-counted `ResourceStore` with a monotonic `AtomicU32` version and a `tokio::sync::watch` version notifier.
   - When a store's `ref_count` drops to `0`, `last_zero_refs_at` is recorded and `evict_idle_stores` drops stores idle for longer than `DEFAULT_IDLE_TIMEOUT` (5 minutes).

3. **Zero-copy filtering + partial sorting in the query engine (`crates/core/src/query.rs`):**
   - `execute_query(rows: &[Row], spec: &QuerySpec, version: u32) -> Page` filters `&Row` references without cloning strings, uses `select_nth_unstable_by` to partition and sort only the `0..offset+limit` prefix when paging, and clones only the final `offset..offset+limit` window (clamped to `MAX_PAGE_LIMIT = 1000`).
   - Multi-cluster queries merge snapshots across matching `(cluster, kind)` stores in `ClusterManager::query`.

4. **Coalesced `PageDelta` subscriptions (`<= 10/s`) over Tauri Channels (`crates/app/src/lib.rs`):**
   - `ClusterManager::subscribe` coalesces store version ticks over `SUBSCRIPTION_BATCH_INTERVAL` (`100 ms`), emitting at most 10 `PageDelta` updates per second over `tauri::ipc::Channel<PageDelta>`.

5. **Sparse virtualised table in React (`ui/src/stores/pods.ts`, `ui/src/components/PodTable.tsx`):**
   - `usePodsStore` stores a sparse index map (`rowsByIndex: Record<number, Row>`) rather than an array of 50,000 items, fetching `PAGE_SIZE = 100` windows on demand as `@tanstack/react-virtual` scrolls and forwarding all filter/sort changes to the Rust backend.

## Measured Spike Results (`docs/research/bench-spike.md`)
- **50,000 pods across 3 clusters (release):**
  - Projection + first page: **15.31 ms** (budget `< 1,500 ms`)
  - Query latency `p50` / `p95` / `max`: **1.09 ms / 1.19 ms / 1.20 ms** (budget `< 50 ms` core / `< 100 ms` E2E)
  - Process RSS: **59.51 MB** (budget `< 500 MB`)

## Consequences
- **Positive:** 10x–80x headroom on latency and 8x headroom on memory at 50k pods; zero business-logic duplication between Rust and TypeScript; ready for Phase 1 multi-resource expansion (`P1-03`).
- **Trade-off:** Adding a new sortable/filterable column requires adding its projected `Cell` in `crates/core/src/projection.rs` rather than ad-hoc JS access to raw K8s JSON. For custom JSONPath columns in Phase 2, the projection config will evaluate configured paths at ingest time.
