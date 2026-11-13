# Fleetglass Performance Benchmark Report (`S-03` & `R-14`)

Last updated: 2026-11-02 (R-14a/R-14b section)  
Commands:
- Local kwok scale clusters: `scripts/kwok-local.sh up --clusters 3 --pods-per-cluster 16667` (then `down`)
- Multi-cluster: `KUBECONFIG=$PWD/.dev/kwok/kubeconfig cargo run -p bench --release -- --kubeconfig .dev/kwok/kubeconfig --cluster kwok-fleetglass-scale-1,kwok-fleetglass-scale-2,kwok-fleetglass-scale-3 --runs 3 --iterations 50 --json-out docs/research/bench-data/<name>.json [--assert-budgets]`
- VM kind cluster: `cargo run -p bench --release -- --cluster kind-fleetglass-dev --iterations 50 [--assert-budgets]` (default kubeconfig `.dev/kubeconfig`)
- Synthetic: `cargo run -p bench --release -- --pods <N> --clusters 3 --namespaces 20 --iterations 50 --assert-budgets`

---

## Progressive publish (R-14a) + mimalloc (R-14b), 2026-11-02

**Headline (worst run of every scenario, both allocators): first full labelled page ≤ 185 ms (budget 1500 ms) on every scenario, 50k pods included. With mimalloc, every budget passes in all four scenarios. With glibc, idle RSS fails at 1 × 10k, 1 × 16.7k and 5 × 10k.**

### Environment
- **Machine**: AMD Ryzen 7 9800X3D (8 cores / 16 threads), 60 GB RAM, CachyOS, Linux 7.3.0-5-cachyos x86_64. Bench client and kwok clusters ran on the same host.
- **Code**: `788b34ec1f8641b062867c02db221913650427c1` **plus uncommitted R-14a/R-14b** (progressive publish with `StoreAccessState::Loading { rows_so_far }` in `crates/core/src/{api/mod.rs,cluster.rs}`; mimalloc behind the bench's default `mimalloc` feature) and the new bench definition below, as of 19:28 local time. The core working tree changed again during the runs (19:48–19:55: `StoreAccessState::Error { partial_rows }` and more `cluster.rs` edits); **those later edits are not in these binaries**. Release builds: `cargo build -p bench --release --no-default-features` (`allocator: "system"`, glibc) and `cargo build -p bench --release` (`allocator: "mimalloc"`).
- **Clusters**: kwokctl **v0.8.0**, binary runtime, through `scripts/kwok-local.sh up --clusters C --pods-per-cluster N --hold`. The pods are realistic (~15 KB JSON each; see §0). Each scenario used a fresh set of clusters, waited about 15–20 s after every pod was Running, and was torn down with `down` afterwards.
- **Command** (per scenario, both binaries back to back, order alternated: system first for 1 × 10k and 3 × 16.7k, mimalloc first for 1 × 16.7k and 5 × 10k): `KUBECONFIG=$PWD/.dev/kwok/kubeconfig .dev/bench-<alloc> --kubeconfig .dev/kwok/kubeconfig --cluster kwok-fleetglass-scale-1,… --runs 3 --iterations 50 --json-out docs/research/bench-data/<scenario>-<alloc>.json`.
- **Load before each binary** (`uptime`, 1/5/15 min): 1 × 10k system 0.94/0.98/0.44, mimalloc 2.16/1.23/0.52 · 1 × 16.7k mimalloc 0.71/1.76/0.96, system 0.68/1.72/0.95 · 3 × 16.7k system 0.36/2.35/2.71, mimalloc 0.77/2.35/2.70 · 5 × 10k mimalloc 1.42/3.86/5.11, system 1.98/3.90/5.11. The 5- and 15-minute averages were still high from creating the clusters, and the machine was otherwise idle.

### What the bench now measures (changed definition)
- The bench polls the UI's initial query every ~5 ms from `ensure_watching`: Pods over all contexts, sort by name asc, offset 0, limit **100**. This matches `defaultQuerySpec`/`PAGE_SIZE` in `ui/src/stores/pods.ts`; earlier sections used limit 60.
- **`first_page_ms` (budgeted) = the first *full labelled page*.** This is the first poll that returned `rows ≥ min(limit, total)` with `total > 0`, and every store in `page.access` was `Ready` or `Loading { .. }`. A store in `Error`, `Forbidden`, `NotFound` or `Unsupported` aborts the run. **Be clear about what this means:** with progressive publish, this page usually comes from stores that are still running their first LIST. It holds the first 100 names *among the rows received so far*, so the rows and their order change as more list pages arrive. The UI labels the page as partial ("partial lists are always labelled", user decision). The budget does not measure a complete, final first page. That is `full_sync_ms`.
- **`first_rows_ms`**: the first poll with ≥ 1 row. It is equal to `first_page_ms` in every run, because the first list chunk (500 pods) already fills a 100-row window.
- **`full_sync_ms` (no budget)**: time until every store is `Ready`. This is what earlier sections called "first page".
- **`loading_query_p95_ms` / `max`**: latency of every polled query while any store was not `Ready`, pooled over the 3 runs. The p95 is checked against the 50 ms query budget.
- Unchanged: query p95 = 50 mixed queries after run 1; peak RSS sampled during run 1's sync; idle RSS 5 s after it. KB/pod = (idle − RSS before watching) / pods. **RSS covers the bench process only (no Tauri/WebKit baseline).**

### Results

| Scenario | Allocator | First page per run (ms) | **First page worst** (< 1500) | Full sync per run (worst), no budget | Query p95 (< 50) | Loading-query p95 / max (p95 < 50) | Peak RSS | **Idle RSS** | Idle KB/pod | Verdict |
|---|---|---|---:|---|---:|---:|---:|---:|---:|---|
| 1 × 10k | system | 86 / 60 / 49 | **86 ms** ✅ | 1006 / 963 / 1009 (1009) | 1.24 ms ✅ | 0.57 / 4.04 ms ✅ | 213.5 MB | **208.5 MB** ❌ (< 200) | 20.5 | **FAIL** (idle RSS) |
| 1 × 10k | mimalloc | 60 / 60 / 60 | **60 ms** ✅ | 780 / 816 / 792 (816) | 0.87 ms ✅ | 0.33 / 0.67 ms ✅ | 123.4 MB | **123.4 MB** ✅ (< 200) | 11.1 | PASS |
| 1 × 16.7k | system | 72 / 66 / 54 | **72 ms** ✅ | 1621 / 1756 / 1592 (1756) | 2.33 ms ✅ | 0.84 / 13.96 ms ✅ | 227.0 MB | **218.6 MB** ❌ (< 200) | 12.9 | **FAIL** (idle RSS) |
| 1 × 16.7k | mimalloc | 70 / 60 / 55 | **70 ms** ✅ | 1356 / 1343 / 1346 (1356) | 1.56 ms ✅ | 0.55 / 1.01 ms ✅ | 143.3 MB | **143.3 MB** ✅ (< 200) | 7.8 | PASS |
| **3 × 16.7k (PLAN)** | system | 105 / 74 / 67 | **105 ms** ✅ | 2820 / 2940 / 3032 (3032) | 8.49 ms ✅ | 3.59 / 46.12 ms ✅ | 424.2 MB | **424.2 MB** ✅ (< 500) | 8.5 | PASS |
| **3 × 16.7k (PLAN)** | mimalloc | 79 / 67 / 61 | **79 ms** ✅ | 2528 / 2852 / 2695 (2852) | 5.65 ms ✅ | 3.13 / 44.45 ms ✅ | 308.3 MB | **303.5 MB** ✅ (< 500) | 5.9 | PASS |
| 5 × 10k | system | 105 / 185 / 106 | **185 ms** ✅ | 2792 / 3090 / 2340 (3090) | 45.84 ms ✅ (barely) | 13.14 / 54.83 ms ✅ | 546.5 MB | **537.1 MB** ❌ (< 500) | 10.8 | **FAIL** (idle RSS) |
| 5 × 10k | mimalloc | 100 / 93 / 74 | **100 ms** ✅ | 1855 / 2578 / 1824 (2578) | 4.65 ms ✅ | 8.50 / 39.15 ms ✅ | 411.9 MB | **403.0 MB** ✅ (< 500) | 7.9 | PASS |

Raw JSON: `bench-data/{1x10k,1x16.7k,3x16.7k,5x10k}-{system,mimalloc}.json`.

**Re-run on the final R-14a tree** (after fix round 1: `Error { partial_rows }`, `list_failed`, pinned timer), mimalloc, same machine, fresh clusters, run by the orchestrator, 2026-11-02 20:09–20:21:

| Scenario | First page per run (worst) | Full sync per run | Query p95 | Loading-query p95 / max | Idle RSS | Verdict |
|---|---|---|---:|---:|---:|---|
| 5 × 10k, run A (load avg 16 from a concurrent QA build) | 98 / 86 / 67 (**98 ms**) | 1668 / 1448 / 1347 | 5.43 ms | 4.98 / 36.2 ms | **463.9 MB** | PASS |
| 5 × 10k, run B (load avg 1.9) | 130 / 86 / 73 (**130 ms**) | 1414 / 1451 / 1342 | 6.32 ms | 4.52 / 11.5 ms | **436.0 MB** | PASS |
| 3 × 16.7k (PLAN, load avg 1.2) | 90 / 72 / 67 (**90 ms**) | 1916 / 1812 / 1786 | 5.70 ms | 1.78 / 29.4 ms | **320.0 MB** | PASS |

Raw JSON: `bench-data/5x10k-mimalloc-final{,-2}.json`, `bench-data/3x16.7k-mimalloc-final.json`. Every budget still passes on the final tree. 5 × 10k idle RSS is 436–464 MB, against 403 MB in the earlier run, so the headroom under 500 MB is only 36–64 MB, still without the Tauri/WebKit baseline. Whole-app memory is V-06.

### Verdicts
- **First page (R-14a): PASS everywhere.** The worst run is 185 ms (5 × 10k, glibc), down from 2.5–3.4 s for a complete sync before. The budget now passes because the definition is "first full *labelled* page". The full sync still takes 0.8–3.1 s, and that time is reported without a budget. Loading-time queries stay cheap: the worst p95 is 13 ms, and the worst single polled query is 55 ms (5 × 10k glibc, above the 50 ms budget but not at p95).
- **Allocator (R-14b): keep mimalloc.** Idle RSS is lower in every scenario: −85 MB (−41 %) at 1 × 10k, −75 MB (−34 %) at 1 × 16.7k, −121 MB (−28 %) at 3 × 16.7k, and −134 MB (−25 %) at 5 × 10k. The worst first page is not worse: 60 vs 86, 70 vs 72, 79 vs 105, and 100 vs 185 ms. Full sync and query p95 are also faster. mimalloc turns the three glibc idle-RSS FAILs into PASSes. 1 × 16.7k is the closest case, at 143 MB against the 200 MB budget, and **this is still without the Tauri/WebKit baseline**.
- **Caveats**: 3 runs per cell, and each allocator ran on a single cluster set. The first-page numbers (50–185 ms) are close to one list round-trip and vary from run to run. The glibc 5 × 10k query p95 (45.8 ms) is an outlier against 3 × 16.7k glibc (8.5 ms). It was measured while the 5-minute load average was still about 3.9 from cluster creation, so treat it as noisy. The uncommitted R-14b diff also sets mimalloc as the global allocator in `crates/app/src/main.rs`, so the allocator choice carries over to the app. The app was not measured here.

---

## 0. Local kwok scale runs (`R-14` reopened, 2026-11-02)

### Environment
- **Machine (client and clusters on the same host; user-approved)**: AMD Ryzen 7 9800X3D (8 cores / 16 threads), 60 GB RAM, CachyOS, Linux 7.3.0-5-cachyos x86_64. Allocator: glibc malloc (default; no `MALLOC_*` env vars unless stated).
- **Code**: commit `a433f85` (the crates are identical to `9d3fc3c`; `a433f85` touches only `ui/`) **plus the working tree with the uncommitted R-14 core change** (`InitStaging` in `crates/core/src/cluster.rs`) and the new bench in `crates/bench/src/main.rs`. Release build.
- **"HEAD core" column**: the same new bench harness built against the committed `cluster.rs` (initial list buffered as `Vec<Pod>`), on the same clusters minutes apart. This is an A/B of the staging change only.
- **Clusters**: `scripts/kwok-local.sh` → kwokctl **v0.8.0**, `--runtime binary` (etcd v3.6.10, kube-apiserver v1.36.1), one cluster per context `kwok-fleetglass-scale-N` on `127.0.0.1:17443+N`, `--disable-qps-limits`. Each cluster has `ceil(N/1000)` fake nodes and Deployments of 50 replicas in `N/250` namespaces.
- **Pods are realistic**: 1 init container + 3 containers, env (literal + `fieldRef`), ports, probes, resources, 2 volumes + the SA token volume, 8 labels, 3 annotations, kwok-filled status (conditions, IPs, 3 container statuses), managedFields. A pod is about **15 KB of JSON** from the API server (5.5 MB per 500-item list page).
- **Method**: one `ClusterManager` watches Pods on every listed context at once (like the app). `--runs 3`: each run uses a fresh `ClusterManager`. First page = time from `ensure_watching` until every store is `Ready` and the first 60-row page (sort by name) over all contexts is returned. RSS is sampled every 5 ms during run 1 (peak) and read again 5 s after the sync (idle). KB/pod = (RSS − RSS before watching) / pods. Query p95 = 50 queries (text filter, namespace + restarts sort, deep offset, label selector) over all contexts after run 1.
- **Scope of RSS**: bench process only (ClusterManager + watch stores); **excludes the Tauri/WebKit baseline** of the desktop app, so the app's real RSS is higher.
- Raw JSON for every run: [`bench-data/`](bench-data/).

### Results (new core = working tree with InitStaging)

| Scenario | Pods | First page per run (ms) | First page worst | Query p95 | Peak RSS during sync | Idle RSS (5 s after sync) | Idle KB/pod | Budgets (PLAN §3) |
|---|---:|---|---:|---:|---:|---:|---:|---|
| 1 cluster, 1k | 1,000 | 129 / 127 / 122 | **129 ms** ✅ | 1.20 ms ✅ | 129.8 MB | **129.9 MB** ✅ (< 200) | 125.0 | PASS |
| 1 cluster, 5k | 5,000 | 562 / 517 / 489 | **562 ms** ✅ | 5.81 ms ✅ | 155.3 MB | **155.4 MB** ✅ (< 200) | 30.2 | PASS |
| 1 cluster, 10k | 10,000 | 1453 / 1261 / 1211 | **1453 ms** ✅ (just under) | 11.68 ms ✅ | 212.8 MB | **202.6 MB** ❌ (< 200) | 19.9 | **FAIL** (idle RSS) |
| 1 cluster, 16.7k | 16,667 | 1832 / 1725 / 1712 | **1832 ms** ❌ | 21.49 ms ✅ | 224.4 MB | **215.0 MB** ❌ (< 200) | 12.7 | **FAIL** (first page, idle RSS) |
| **3 clusters × 16.7k (PLAN scenario)** | 50,001 | 2415 / 2462 / 2479 | **2479 ms** ❌ | **143.0 ms** ❌ | 453.5 MB | **453.5 MB** ✅ (< 500) | 9.1 | **FAIL** (first page, query p95) |
| 5 clusters × 10k | 50,000 | 2393 / 2568 / 2488 | **2568 ms** ❌ | **98.0 ms** ❌ | 607.5 MB | **607.5 MB** ❌ (< 500) | 12.3 | **FAIL** (all three) |
| Synthetic 50k / 3 clusters (reference, in-memory) | 50,000 | 20 | 20 ms | 1.23 ms | - | 63.7 MB (end) | - | PASS |

Budgets: first rows < 1500 ms; per interaction < 100 ms (bench uses the 50 ms backend share); idle memory, 1 cluster < 200 MB; 3 clusters / 50k pods < 500 MB. The 1-cluster budget is labelled "idle", so for 1 cluster we compare idle RSS; for 3+ clusters the budget has no "idle" qualifier, and peak ≈ idle anyway.

### Did the staging change help? (A/B, same harness and clusters)

| Scenario | Core | First page worst | Query p95 | Peak RSS (sync) | Idle RSS | Idle KB/pod |
|---|---|---:|---:|---:|---:|---:|
| 1 × 5k | HEAD (buffers `Vec<Pod>`) | 615 ms | 4.56 ms | 571.0 MB | 569.1 MB ❌ | 114.9 |
| 1 × 5k | **InitStaging** | 562 ms | 5.81 ms | 155.3 MB | **155.4 MB** ✅ | 30.2 |
| 1 × 16.7k | HEAD | 2179 ms | 25.35 ms | 1839.1 MB | 1801.2 MB ❌ | 110.2 |
| 1 × 16.7k | **InitStaging** | 1832 ms | 21.49 ms | 224.4 MB | **215.0 MB** ❌ | 12.7 |
| 3 × 16.7k | HEAD | 3085 ms | 95.27 ms | 5386.8 MB | 5303.2 MB ❌ | 108.4 |
| 3 × 16.7k | **InitStaging** | 2479 ms | 143.0 ms | 453.5 MB | **453.5 MB** ✅ | 9.1 |
| 1 × 5k, **pre-fix VM numbers** (kind on a remote VM, 2026-11-01, old harness, minimal kwok pods) | 0de811e | 645 / 1016 ms | 4.07 ms | 182.8 MB (`max(peak, end)`) | not measured | ~33 |

- **Yes, a lot.** With realistic ~15 KB pods, HEAD core keeps ~110 KB/pod (5.3 GB for 50k, so the app would be unusable), and the memory is not released after the sync (idle ≈ peak). With InitStaging, peak RSS during sync falls 3.7× at 5k, 8× at 16.7k and **12× at 50k**, and 3 × 16.7k fits the 500 MB budget (453.5 MB, but only 46 MB headroom, and that is *without* the Tauri/WebKit baseline).
- The old VM figure (182.8 MB at 5k, ~33 KB/pod) used minimal kwok pods from `scale-cluster.sh`, so its per-pod cost was far lower than for realistic pods. That is why the VM did not show the 110 KB/pod problem.

### Findings: what still fails, and likely causes (not tuned here; for the core owner)

1. **Idle RSS has a large fixed component, not per-row cost.** 1k pods → 130 MB idle (122 MB over baseline), yet 50k pods → 453 MB. In-memory rows cost about 1–1.2 KB each (synthetic 50k = 64 MB total). Diagnostic run with the same binary and `MALLOC_ARENA_MAX=2` (env only): **5 × 10k idle 607.5 → 433.1 MB, 1 × 10k 202.6 → 144.6 MB**, but 5-cluster first page 2568 → 4161 ms. So most of the excess is **glibc allocator retention**. Each tokio worker thread gets its own malloc arena. The ~5.5 MB deserialized list pages (500 × ~15 KB pods → `ObjectList<Pod>`) and the JSON buffers fragment those arenas, and the freed memory is never returned. More concurrent clusters means more arenas touched, so 5 × 10k is worse than 3 × 16.7k. Candidate fixes for core/app (owner decision): a different global allocator (e.g. mimalloc/jemalloc, MIT/BSD; needs a dependency justification), `malloc_trim(0)` after `InitDone`, or a smaller list `page_size` (kube-runtime default 500; `watcher::Config::default()` is used at `cluster.rs:542`). Also, deserializing into a partial type (`metadata` + the fields `project` needs) instead of a full `Pod` would avoid allocating managedFields, env and probes at all.
2. **First page > 1.5 s from ~10k pods per cluster.** The server side alone (one `kubectl get --raw /api/v1/pods?limit=20000` of 16.7k pods = 184 MB of JSON) takes **1346 ms**, and three in parallel take 1413 ms, on this machine. So a full LIST of 16.7k realistic pods is already ~1.3 s before any client work. The list is paged in 500s sequentially, and InitStaging publishes nothing until `InitDone`. Meeting the budget at this size needs a design change such as progressive publish of staged pages (with care: non-negotiable "no partial lists" vs "first rows < 1.5 s" is a product decision), the watch-list streaming feature (`sendInitialEvents`), or a `metadata`-only/partial-object list. The PLAN budget "first rows < 1.5 s" might also be read as "first rows of the first ready cluster": the bench logs that too, and it is ~1.7 s at 16.7k (1 cluster) and 2.0–2.4 s at 3/5 clusters, which still fails.
3. **Query p95 at 50k real rows = 98–143 ms** (synthetic 50k = 1.2 ms). `ClusterManager::query` calls `ResourceStore::snapshot_rows()` (`cluster.rs:438`), which **clones every row of every matched store** into a `Vec<Row>` on each query, before `execute_query` filters and sorts. With realistic rows (labels, owner refs and other cells as `String`s), cloning 50k rows dominates. Fix direction: query under the read lock over `&Row` (or `Arc<Row>` in the map), as the synthetic path already does.
4. **Peak ≈ idle** in all InitStaging runs: memory is not released after the sync. That matches finding 1 (retention, not live data).

### Query p95 fix (`f4a881a`, borrowed rows instead of `snapshot_rows()` clones), 2026-11-02

Same machine and harness, 3 × 16.7k local kwok, 3 runs × 50 iterations. A/B in back-to-back runs, both binaries built from clean trees (`4ec9506` vs `5e7ae81`), load average < 1:

| Core | First page per run (ms) | Query p95 | Idle RSS |
|---|---|---:|---:|
| `4ec9506` (clones rows per query) | 3973 / 3257 / 3224 | **115.5 ms** ❌ | 439.9 MB ✅ |
| `5e7ae81` (borrowed rows) | 3182 / 3165 / 3146 | **19.0 ms** ✅ | 427.5 MB ✅ |
| `5e7ae81`, second run (raw JSON `bench-data/local-3x16667-postquery.json`) | 2986 / 3419 / 3009 | **17.5 ms** ✅ | 423.8 MB ✅ |

- Query p95 at 50k real rows now passes (6× faster). Remaining query cost is filter + sort over 50k borrowed rows.
- First page is **~3.0–3.4 s today vs 2.4–2.5 s in the earlier table**, on both the old and new core, so this is run-to-run/cluster variance (fresh kwok clusters, apiserver warm-up), not caused by the query change. Either way it still fails the 1.5 s budget; see finding 2 (needs a product decision).

---

## 1. Real Cluster Benchmarks (`R-14`, pre-fix, VM)

> **Pre-fix numbers.** Measured on 2026-11-01 with the previous bench (`rss_mb = max(peak during sync, end)` compared with the idle budget, single first-page run, cluster first-page budget loosened to 5 s) and with the core that buffered the full `Vec<Pod>` during the initial list. Minimal kwok pods from `scale-cluster.sh`. Kept for history; superseded by section 0.

### Test Environment
- **Commit**: `0de811efec467e8187fdf7cb62a2f5fa2acdbede` (`0de811e`)
- **Host Machine (Client)**: AMD Ryzen 7 9800X3D (8 cores / 16 threads), 64 GB RAM, Linux 7.3.0-5-cachyos x86_64
- **Cluster VM (API & Nodes)**: remote VM (4 vCPU, 7 GB RAM, remote Docker over SSH) running kind v0.33.0 / Kubernetes v1.37.0 with kwok v0.8.0 fake nodes and pods
- **Harness**: `crates/bench` with `ClusterManager` connected via `.dev/kubeconfig` (`kind-fleetglass-dev` context)

### 1,000 Pods (Real Cluster)
Scaled with `scripts/scale-cluster.sh up --pods 1000` (1,023 total pods across 8 namespaces, 2 fake nodes + 1 control plane node, 50 query iterations).

| Metric | Measured | Budget (`PLAN.md` §3) | Status |
|---|---:|---:|:---:|
| Benchmark Mode | **Real cluster (`kind-fleetglass-dev`)** | - | - |
| Pods / Clusters / Namespaces | **1,023 / 1 / 8** | - | - |
| Time to first page (`first_page_ms`) | **394.42 ms** | `< 1500.0 ms` | **PASS** |
| Query latency p50 (`query_p50_ms`) | **0.81 ms** | - | - |
| Query latency p95 (`query_p95_ms`) | **0.85 ms** | `< 50.0 ms` (`< 100 ms` E2E) | **PASS** |
| Query latency max (`query_max_ms`) | **1.24 ms** | - | - |
| Process RSS memory (`rss_mb`) | **49.63 MB** | `< 200.0 MB` (1 cluster budget) | **PASS** |
| Overall (`budgets_passed`) | **true** | all pass | **PASS** |

### 5,000 Pods (Real Cluster, Host Cap)
Scaled with `scripts/scale-cluster.sh up --pods 5000` (5,039 total pods across 24 namespaces, 10 fake nodes + 1 control plane node, 50 query iterations).

| Metric | Measured | Budget (`PLAN.md` §3) | Status |
|---|---:|---:|:---:|
| Benchmark Mode | **Real cluster (`kind-fleetglass-dev`)** | - | - |
| Pods / Clusters / Namespaces | **5,039 / 1 / 24** | - | - |
| Time to first page (`first_page_ms`) | **645.13 ms** (cold) / **1015.72 ms** | `< 1500.0 ms` | **PASS** |
| Query latency p50 (`query_p50_ms`) | **3.22 ms** | - | - |
| Query latency p95 (`query_p95_ms`) | **4.07 ms** | `< 50.0 ms` (`< 100 ms` E2E) | **PASS** |
| Query latency max (`query_max_ms`) | **4.09 ms** | - | - |
| Process RSS memory (`rss_mb`) | **182.80 MB** (peak sync) | `< 200.0 MB` (1 cluster budget) | **PASS** |
| Overall (`budgets_passed`) | **true** | all pass | **PASS** |

### Real-Cluster Findings
1. **Time-to-first-page well under budget**: Even over remote network TLS to the kind cluster on the LAN VM, starting the watch on `Pod` and querying the first windowed 60 rows takes **394.42 ms** for 1,000 pods and **645.13 ms** for 5,000 pods (well under the 1,500 ms budget).
2. **Query latency remains sub-5ms**: Executing filtered, sorted, paginated queries via `ClusterManager::query` over 5,039 real cluster rows achieves a **4.07 ms p95**, ~12x faster than the 50 ms backend budget and ~24x faster than the 100 ms UI budget.
3. **Memory within 1-cluster budget**: Peak RSS memory during the initial 5,000-pod list sync reached **182.80 MB**, staying within the 200 MB budget for a single cluster.

---

## 2. Synthetic Benchmarks (In-Memory Simulation, `S-03`)

### Scope & Environment Note
Per `docs/TASKS.md`, live kwok scale runs on the shared remote VM (4 vCPU, 7 GB RAM) were verified end to end up to the host cap of **5,000 fake kwok pods** (`5,039` total pods in `fleetglass-dev`, `P0-07`, converging in 68 s). The 10,000-pod and 50,000-pod synthetic measurements below run through `crates/bench` against `fleetglass-core`'s real `project_pod` and `execute_query` engine across 3 simulated clusters and 20 namespaces in release mode.

### 10,000 Pods (Synthetic, 3 clusters, 20 namespaces, 50 mixed query iterations)

| Metric | Measured | Budget | Status |
|---|---:|---:|:---:|
| Benchmark Mode | **Synthetic (in-memory)** | - | - |
| Pods / Clusters / Namespaces | 10,000 / 3 / 20 | - | - |
| Project all rows (`project_all_ms`) | **3.11 ms** | - | - |
| Time to first page (`first_page_ms`) | **3.33 ms** | `< 1500.0 ms` | **PASS** |
| Query latency p50 (`query_p50_ms`) | **0.22 ms** | - | - |
| Query latency p95 (`query_p95_ms`) | **0.26 ms** | `< 50.0 ms` (`< 100 ms` E2E) | **PASS** |
| Query latency max (`query_max_ms`) | **0.26 ms** | - | - |
| Process RSS memory (`rss_mb`) | **19.55 MB** | `< 200.0 MB` | **PASS** |

### 50,000 Pods (Synthetic, 3 clusters, 20 namespaces, 50 mixed query iterations)

| Metric | Measured | Budget | Status |
|---|---:|---:|:---:|
| Benchmark Mode | **Synthetic (in-memory)** | - | - |
| Pods / Clusters / Namespaces | 50,000 / 3 / 20 | - | - |
| Project all rows (`project_all_ms`) | **14.18 ms** | - | - |
| Time to first page (`first_page_ms`) | **15.31 ms** | `< 1500.0 ms` | **PASS** |
| Query latency p50 (`query_p50_ms`) | **1.09 ms** | - | - |
| Query latency p95 (`query_p95_ms`) | **1.19 ms** | `< 50.0 ms` (`< 100 ms` E2E) | **PASS** |
| Query latency max (`query_max_ms`) | **1.20 ms** | - | - |
| Process RSS memory (`rss_mb`) | **59.51 MB** | `< 500.0 MB` | **PASS** |

## Workload Mix Tested
Each iteration executes `fleetglass_core::query::execute_query` over the full projected store, cycling across:
1. Text search + numeric cell sort (`Restarts` descending, `offset: 0, limit: 60`)
2. Multi-cluster + namespace + label selector (`app=web,tier!=canary`) + `Name` ascending (`offset: 200, limit: 60`)
3. Deep virtual-scroll window (`offset: N / 2, limit: 60`)

## Key Findings (Synthetic)
- **Compact row projection (`Row` + `Vec<Cell>`) keeps RSS under 60 MB for 50,000 pods** (budget: 500 MB) because full `k8s_openapi::api::core::v1::Pod` structs are dropped immediately after projection.
- **Borrowing `&Row` during filtering and using `select_nth_unstable_by` partial sorting** avoids cloning rows outside the requested `offset..offset+limit` window, keeping 50k-row `p95` query latency at **1.19 ms** (~42x faster than the 50 ms core budget and ~84x faster than the 100 ms UI interaction budget).
