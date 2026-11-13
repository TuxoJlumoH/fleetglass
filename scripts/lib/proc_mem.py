#!/usr/bin/env python3
"""Process-tree memory and startup-log helper for scripts/measure-app.sh (V-06).

Linux only (reads /proc). Standard library only.

Usage:
  proc_mem.py snapshot PID              JSON of PID + all descendants: pid, ppid, name,
                                        rssMb, pssMb (from /proc/<pid>/smaps_rollup), totals
  proc_mem.py startup LOGFILE           prints "startupMs rows total" from the app's
                                        "fleetglass startup: first rows ..." line (exit 1 if absent)
  proc_mem.py startup-overview LOGFILE  prints "startupMs clusters connected attention" from the
                                        "fleetglass startup: first overview ..." line (exit 1 if absent)
  proc_mem.py page-loaded LOGFILE       prints the "main page loaded after N ms" value (exit 1 if absent)
  proc_mem.py report --runs RUNS.tsv --memory MEM.json --out OUT.json [--idle-secs S]
                     [--startup-metric overview|rows] [--bin PATH] [--kubeconfig PATH]
                     [--contexts a,b] [--current-context c]
                     [--selection current-context|all-clusters] [--commit SHA]
                                        assembles the result JSON and prints a summary table

RUNS.tsv: one line per run, either `key=value` tokens (startupMs=..., rows=/total= or
clusters=/connected=/attention=, pageLoadedMs=, wallMs=, ...) or the legacy positional form
`startupMs rows total [pageLoadedMs|-] [wallMs|-]` (rows metric).
  proc_mem.py --self-test               runs the parsers against fixture text

RSS sums double-count shared pages (libwebkit2gtk, mesa, libc ...) across processes;
PSS splits each shared page between the processes mapping it, so sum(PSS) is the fair
total for "how much memory does the app use".
"""

import json
import os
import platform
import re
import statistics
import sys

KB_PER_MB = 1024.0

STARTUP_RE = re.compile(
    r"fleetglass startup: first rows returned after (\d+) ms \(rows=(\d+), total=(\d+)"
)
OVERVIEW_RE = re.compile(
    r"fleetglass startup: first overview returned after (\d+) ms "
    r"\(clusters=(\d+), connected=(\d+), attention=(\d+)"
)
PAGE_LOADED_RE = re.compile(r"fleetglass startup: main page loaded after (\d+) ms")

# Per-metric: JSON field for the startup time, the extra per-run counters, summary heading.
METRICS = {
    "overview": {
        "msField": "firstOverviewMs",
        "counters": ("clusters", "connected", "attention"),
        "heading": "Startup (process start -> first Overview data with a connected cluster):",
        "note": "startupMs = firstOverviewMs: process start (first statement of main) -> first "
        "subscribe_overview push with >= 1 connected cluster, as logged by the app",
    },
    "rows": {
        "msField": "firstRowsMs",
        "counters": ("rows", "total"),
        "heading": "Startup (process start -> first Pods rows; INCLUDES waiting for the window "
        "and opening Pods with `g p`):",
        "note": "startupMs = firstRowsMs: process start (first statement of main) -> first "
        "query_page/subscribe_query response with >= 1 row. Since v0.2 the app opens on "
        "Overview, so this includes the script waiting for the page and sending `g p` "
        "(podsKeysAtMs = when the keys were sent, script clock since launch)",
    },
}


# --- parsers (pure; covered by --self-test) ----------------------------------------------------


def parse_smaps_rollup(text):
    """Returns {"rss_kb": int, "pss_kb": int} from /proc/<pid>/smaps_rollup text."""
    out = {}
    for line in text.splitlines():
        m = re.match(r"^(Rss|Pss):\s+(\d+)\s+kB", line)
        if m:
            out[m.group(1).lower() + "_kb"] = int(m.group(2))
    if "rss_kb" not in out or "pss_kb" not in out:
        raise ValueError("smaps_rollup without Rss/Pss lines")
    return out


def parse_status_rss_kb(text):
    """VmRSS in kB from /proc/<pid>/status (fallback when smaps_rollup is unreadable)."""
    m = re.search(r"^VmRSS:\s+(\d+)\s+kB", text, re.MULTILINE)
    return int(m.group(1)) if m else None


def parse_stat(text):
    """(pid, comm, ppid) from /proc/<pid>/stat. comm may contain spaces and ')'."""
    lpar = text.index("(")
    rpar = text.rindex(")")
    pid = int(text[:lpar].strip())
    comm = text[lpar + 1 : rpar]
    rest = text[rpar + 2 :].split()
    return pid, comm, int(rest[1])


def descendants(root, ppid_of):
    """root + every transitive child, given {pid: ppid}; sorted, root first."""
    children = {}
    for pid, ppid in ppid_of.items():
        children.setdefault(ppid, []).append(pid)
    seen, stack = [], [root]
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.append(pid)
        stack.extend(sorted(children.get(pid, []), reverse=True))
    return [root] + sorted(p for p in seen if p != root)


def parse_startup(text):
    """(ms, rows, total) of the first startup line, or None."""
    m = STARTUP_RE.search(text)
    return (int(m.group(1)), int(m.group(2)), int(m.group(3))) if m else None


def parse_overview(text):
    """(ms, clusters, connected, attention) of the first-overview startup line, or None."""
    m = OVERVIEW_RE.search(text)
    return tuple(int(m.group(i)) for i in range(1, 5)) if m else None


def parse_page_loaded(text):
    m = PAGE_LOADED_RE.search(text)
    return int(m.group(1)) if m else None


def mb(kb):
    return round(kb / KB_PER_MB, 1)


def summarise_processes(procs):
    """procs: [{pid, ppid, name, rss_kb, pss_kb|None}] -> memory JSON object."""
    rows = []
    for p in procs:
        rows.append(
            {
                "pid": p["pid"],
                "ppid": p["ppid"],
                "name": p["name"],
                "rssMb": mb(p["rss_kb"]),
                "pssMb": None if p["pss_kb"] is None else mb(p["pss_kb"]),
                **({"note": p["note"]} if p.get("note") else {}),
            }
        )
    total_rss = sum(p["rss_kb"] for p in procs)
    pss_known = [p["pss_kb"] for p in procs if p["pss_kb"] is not None]
    return {
        "processes": rows,
        "totalRssMb": mb(total_rss),
        "totalPssMb": mb(sum(pss_known)),
        "pssComplete": len(pss_known) == len(procs),
    }


def build_report(runs, memory, meta, metric="overview"):
    """Result JSON. `startupMs` / `startup{Median,Max,Min}Ms` stay the generic names (whatever
    the metric is); the metric-named copies (`firstOverviewMs`, `firstRowsMs`, ...) are added."""
    if metric not in METRICS:
        raise ValueError(f"unknown startup metric {metric!r}")
    field = METRICS[metric]["msField"]
    for r in runs:
        r.setdefault(field, r["startupMs"])
    ms = [r["startupMs"] for r in runs]
    median = statistics.median(ms) if ms else None
    return {
        "startup_metric": metric,
        "runs": runs,
        "startupMedianMs": median,
        "startupMaxMs": max(ms) if ms else None,
        "startupMinMs": min(ms) if ms else None,
        f"{field[:-2]}MedianMs": median,
        "memory": memory,
        **meta,
    }


def format_table(report):
    # Reports written before v0.2 have no startup_metric and measured first rows.
    metric = report.get("startup_metric", "rows")
    spec = METRICS[metric]
    lines = [spec["heading"]]
    for i, r in enumerate(report["runs"], 1):
        pl = r.get("pageLoadedMs")
        wall = r.get("wallMs")
        keys = r.get("podsKeysAtMs")
        counters = " ".join(f"{c}={r.get(c, '?')}" for c in spec["counters"])
        lines.append(
            f"  run {i}: {r['startupMs']:>6} ms  {counters}"
            + (f"  page loaded {pl} ms" if pl is not None else "")
            + (f"  `g p` sent at ~{keys} ms" if keys is not None else "")
            + (f"  first overview {r['firstOverviewMs']} ms"
               if metric != "overview" and r.get("firstOverviewMs") is not None else "")
            + (f"  wall (exec->line seen) {wall} ms" if wall is not None else "")
        )
    lines.append(
        f"  median {report['startupMedianMs']} ms, max {report['startupMaxMs']} ms"
        + ("  [budget: cold start < 1000 ms]" if metric == "overview"
           else "  [not the cold-start budget metric since v0.2: includes opening Pods]")
    )
    mem = report.get("memory")
    if mem:
        lines.append(f"Memory after {report.get('idleSecs')} s idle (selection: {report.get('selection')}):")
        lines.append(f"  {'pid':>8}  {'RSS MB':>8}  {'PSS MB':>8}  name")
        for p in mem["processes"]:
            pss = "?" if p["pssMb"] is None else f"{p['pssMb']:.1f}"
            lines.append(f"  {p['pid']:>8}  {p['rssMb']:>8.1f}  {pss:>8}  {p['name']}")
        lines.append(
            f"  {'total':>8}  {mem['totalRssMb']:>8.1f}  {mem['totalPssMb']:>8.1f}"
            f"  (sum RSS double-counts shared libs; sum PSS is the fair total"
            f"{'' if mem['pssComplete'] else ', INCOMPLETE: some PSS unreadable'})"
        )
    return "\n".join(lines)


# --- /proc access ------------------------------------------------------------------------------


def read(path):
    with open(path, encoding="utf-8", errors="replace") as f:
        return f.read()


def ppid_map():
    out = {}
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            pid, _, ppid = parse_stat(read(f"/proc/{d}/stat"))
        except (OSError, ValueError, IndexError):
            continue
        out[pid] = ppid
    return out


def snapshot(root):
    procs = []
    for pid in descendants(root, ppid_map()):
        try:
            _, comm, ppid = parse_stat(read(f"/proc/{pid}/stat"))
        except (OSError, ValueError, IndexError):
            continue  # exited meanwhile
        try:
            name = os.path.basename(read(f"/proc/{pid}/cmdline").split("\0")[0]) or comm
        except OSError:
            name = comm
        entry = {"pid": pid, "ppid": ppid, "name": name}
        try:
            entry.update(parse_smaps_rollup(read(f"/proc/{pid}/smaps_rollup")))
        except (OSError, ValueError) as e:
            rss = None
            try:
                rss = parse_status_rss_kb(read(f"/proc/{pid}/status"))
            except OSError:
                pass
            if rss is None:
                continue
            entry.update({"rss_kb": rss, "pss_kb": None, "note": f"smaps_rollup unreadable: {e}"})
        procs.append(entry)
    if not procs or procs[0]["pid"] != root:
        raise SystemExit(f"process {root} not found or unreadable")
    return summarise_processes(procs)


def host_info():
    cpu = platform.processor() or ""
    try:
        m = re.search(r"^model name\s*:\s*(.+)$", read("/proc/cpuinfo"), re.MULTILINE)
        if m:
            cpu = m.group(1).strip()
    except OSError:
        pass
    mem_total = None
    try:
        m = re.search(r"^MemTotal:\s+(\d+)\s+kB", read("/proc/meminfo"), re.MULTILINE)
        mem_total = round(int(m.group(1)) / KB_PER_MB / 1024, 1) if m else None
    except OSError:
        pass
    return {"cpu": cpu, "cpus": os.cpu_count(), "kernel": platform.release(), "memTotalGb": mem_total}


def read_runs(path):
    runs = []
    for line in read(path).splitlines():
        parts = line.split()
        if not parts:
            continue
        if "=" in parts[0]:
            # key=value tokens; "-" means not available.
            run = {}
            for tok in parts:
                k, _, v = tok.partition("=")
                if v != "-":
                    run[k] = int(v)
            if "startupMs" not in run:
                raise ValueError(f"run line without startupMs: {line!r}")
            runs.append(run)
            continue
        # Legacy positional form: startupMs rows total [pageLoadedMs|-] [wallMs|-]
        run = {"startupMs": int(parts[0]), "rows": int(parts[1]), "total": int(parts[2])}
        if len(parts) > 3 and parts[3] != "-":
            run["pageLoadedMs"] = int(parts[3])
        if len(parts) > 4 and parts[4] != "-":
            run["wallMs"] = int(parts[4])
        runs.append(run)
    return runs


# --- self-test ---------------------------------------------------------------------------------

FIXTURE_SMAPS = """\
55d0c0a00000-7ffd3b5fe000 ---p 00000000 00:00 0                          [rollup]
Rss:              143360 kB
Pss:               98765 kB
Pss_Dirty:         80000 kB
Pss_Anon:          70000 kB
Pss_File:          28765 kB
Shared_Clean:      40000 kB
Private_Dirty:     80000 kB
Swap:                  0 kB
SwapPss:               0 kB
"""

FIXTURE_STATUS = "Name:\tWebKitWebProces\nVmPeak:\t  999 kB\nVmRSS:\t  204800 kB\nThreads:\t12\n"

FIXTURE_LOG = """\
2026-11-02T10:00:00Z  WARN fleetglass_core::cluster: something unrelated
fleetglass startup: main page loaded after 412 ms
fleetglass startup: first overview returned after 655 ms (clusters=3, connected=2, attention=7, via=subscribe_overview)
fleetglass startup: first rows returned after 873 ms (rows=100, total=16667, via=subscribe_query)
"""


def self_test():
    s = parse_smaps_rollup(FIXTURE_SMAPS)
    assert s == {"rss_kb": 143360, "pss_kb": 98765}, s
    try:
        parse_smaps_rollup("Swap: 0 kB\n")
        raise AssertionError("missing Rss/Pss must raise")
    except ValueError:
        pass
    assert parse_status_rss_kb(FIXTURE_STATUS) == 204800
    assert parse_status_rss_kb("Name: x\n") is None

    assert parse_stat("4242 (fleetglass) S 1 4242 4242 0 -1") == (4242, "fleetglass", 1)
    assert parse_stat("77 (Web Content (x)) R 4242 1 1 0") == (77, "Web Content (x)", 4242)

    tree = {10: 1, 11: 10, 12: 10, 13: 11, 20: 1, 21: 20, 14: 13}
    assert descendants(10, tree) == [10, 11, 12, 13, 14], descendants(10, tree)
    assert descendants(20, tree) == [20, 21]
    assert descendants(99, tree) == [99]

    assert parse_startup(FIXTURE_LOG) == (873, 100, 16667)
    assert parse_startup("nothing here") is None
    assert parse_overview(FIXTURE_LOG) == (655, 3, 2, 7), parse_overview(FIXTURE_LOG)
    assert parse_overview("fleetglass startup: first rows returned after 1 ms (rows=1, total=1)") is None
    assert parse_page_loaded(FIXTURE_LOG) == 412

    mem = summarise_processes(
        [
            {"pid": 10, "ppid": 1, "name": "fleetglass", "rss_kb": 143360, "pss_kb": 98765},
            {"pid": 11, "ppid": 10, "name": "WebKitWebProcess", "rss_kb": 204800, "pss_kb": 150000},
            {"pid": 12, "ppid": 10, "name": "WebKitNetworkProcess", "rss_kb": 51200, "pss_kb": None,
             "note": "smaps_rollup unreadable"},
        ]
    )
    assert mem["totalRssMb"] == 390.0, mem
    assert mem["totalPssMb"] == round((98765 + 150000) / 1024, 1), mem
    assert mem["pssComplete"] is False
    assert mem["processes"][1] == {"pid": 11, "ppid": 10, "name": "WebKitWebProcess",
                                   "rssMb": 200.0, "pssMb": 146.5}
    assert mem["processes"][2]["note"] == "smaps_rollup unreadable"

    rep = build_report(
        [{"startupMs": m, "rows": 100, "total": 18} for m in (900, 700, 1200, 800, 750)],
        mem,
        {"idleSecs": 60, "selection": "current-context"},
        "rows",
    )
    assert rep["startupMedianMs"] == 800 and rep["startupMaxMs"] == 1200 and rep["startupMinMs"] == 700
    assert rep["startup_metric"] == "rows" and rep["firstRowsMedianMs"] == 800
    assert rep["runs"][0]["firstRowsMs"] == 900 and rep["runs"][0]["startupMs"] == 900
    even = build_report([{"startupMs": m, "rows": 1, "total": 1} for m in (100, 200)], None, {}, "rows")
    assert even["startupMedianMs"] == 150
    assert build_report([], None, {})["startupMedianMs"] is None
    try:
        build_report([], None, {}, "bogus")
        raise AssertionError("unknown metric must raise")
    except ValueError:
        pass
    import tempfile

    with tempfile.NamedTemporaryFile("w", suffix=".tsv", delete=False) as f:
        f.write(
            "873 100 16667 412 951\n\n640 18 18 - -\n"
            "startupMs=655 clusters=1 connected=1 attention=3 pageLoadedMs=- wallMs=702\n"
        )
    try:
        assert read_runs(f.name) == [
            {"startupMs": 873, "rows": 100, "total": 16667, "pageLoadedMs": 412, "wallMs": 951},
            {"startupMs": 640, "rows": 18, "total": 18},
            {"startupMs": 655, "clusters": 1, "connected": 1, "attention": 3, "wallMs": 702},
        ], read_runs(f.name)
    finally:
        os.unlink(f.name)
    table = format_table(rep)
    assert "median 800 ms" in table and "WebKitNetworkProcess" in table and "INCOMPLETE" in table, table
    assert "opening Pods" in table and "rows=100 total=18" in table, table
    json.dumps(rep)  # serialisable

    ov = build_report(
        [{"startupMs": m, "clusters": 1, "connected": 1, "attention": 2, "wallMs": m + 60}
         for m in (640, 610, 700)],
        None,
        {},
    )
    assert ov["startup_metric"] == "overview" and ov["firstOverviewMedianMs"] == 640, ov
    assert ov["runs"][1]["firstOverviewMs"] == 610
    t = format_table(ov)
    assert "first Overview data with a connected cluster" in t, t
    assert "clusters=1 connected=1 attention=2" in t and "budget: cold start < 1000 ms" in t, t
    # Pre-v0.2 reports (no startup_metric) still format as rows.
    legacy = {k: v for k, v in rep.items() if k != "startup_metric"}
    assert "first Pods rows" in format_table(legacy)
    json.dumps(ov)

    # Live smoke on this very process (needs a readable /proc).
    if os.path.exists("/proc/self/smaps_rollup"):
        live = snapshot(os.getpid())
        assert live["processes"][0]["pid"] == os.getpid() and live["totalRssMb"] > 0, live

    host = host_info()
    assert host["kernel"], host
    print("proc_mem.py self-test: OK")


# --- CLI ---------------------------------------------------------------------------------------


def main(argv):
    if not argv or argv[0] in ("-h", "--help"):
        print(__doc__)
        return 0
    cmd = argv[0]
    if cmd == "--self-test":
        self_test()
        return 0
    if cmd == "snapshot" and len(argv) == 2:
        print(json.dumps(snapshot(int(argv[1]))))
        return 0
    if cmd == "startup" and len(argv) == 2:
        r = parse_startup(read(argv[1]))
        if r is None:
            return 1
        print(*r)
        return 0
    if cmd == "startup-overview" and len(argv) == 2:
        r = parse_overview(read(argv[1]))
        if r is None:
            return 1
        print(*r)
        return 0
    if cmd == "page-loaded" and len(argv) == 2:
        r = parse_page_loaded(read(argv[1]))
        if r is None:
            return 1
        print(r)
        return 0
    if cmd == "report":
        import argparse

        ap = argparse.ArgumentParser(prog="proc_mem.py report")
        ap.add_argument("--runs", required=True)
        ap.add_argument("--memory")
        ap.add_argument("--out", required=True)
        ap.add_argument("--idle-secs", type=int)
        ap.add_argument("--startup-metric", choices=sorted(METRICS), default="overview")
        ap.add_argument("--bin")
        ap.add_argument("--kubeconfig")
        ap.add_argument("--contexts", default="")
        ap.add_argument("--current-context")
        ap.add_argument("--selection")
        ap.add_argument("--commit")
        a = ap.parse_args(argv[1:])
        memory = json.loads(read(a.memory)) if a.memory and os.path.exists(a.memory) else None
        meta = {
            "idleSecs": a.idle_secs,
            "bin": a.bin,
            "kubeconfig": a.kubeconfig,
            "contexts": [c for c in a.contexts.split(",") if c],
            "currentContext": a.current_context,
            "selection": a.selection,
            "commit": a.commit,
            "host": host_info(),
            "notes": [
                METRICS[a.startup_metric]["note"],
                "wallMs: script clock, launch -> startup line seen in the log (~50 ms polling); "
                "an upper bound that includes exec and dynamic linking",
                "fresh XDG_{CONFIG,DATA,CACHE}_HOME per run (no saved app state); the OS page "
                "cache is warm after run 1 (binary, WebKitGTK libs), so this is not a true cold boot",
                "memory: PID + all descendants via /proc/<pid>/smaps_rollup; sum(PSS) is the fair total",
            ],
        }
        rep = build_report(read_runs(a.runs), memory, meta, a.startup_metric)
        with open(a.out, "w", encoding="utf-8") as f:
            json.dump(rep, f, indent=2)
            f.write("\n")
        print(format_table(rep))
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
