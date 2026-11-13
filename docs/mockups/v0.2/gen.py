#!/usr/bin/env python3
"""Generates the v0.2 UI mockups (static HTML) next to this file.

Render: firefox --headless --window-size=1440,900 --screenshot out.png file://$PWD/page.html
"""
from pathlib import Path

OUT = Path(__file__).parent

# Lucide-style line icons (ISC), simplified.
I = {
    "pod": '<path d="M12 2 3 7v10l9 5 9-5V7z"/><path d="M3 7l9 5 9-5M12 12v10"/>',
    "deploy": '<rect x="3" y="3" width="7" height="7" rx="1"/><rect x="14" y="3" width="7" height="7" rx="1"/><rect x="3" y="14" width="7" height="7" rx="1"/><rect x="14" y="14" width="7" height="7" rx="1"/>',
    "sts": '<ellipse cx="12" cy="5" rx="8" ry="3"/><path d="M4 5v14c0 1.7 3.6 3 8 3s8-1.3 8-3V5M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3"/>',
    "ds": '<circle cx="6" cy="6" r="2.5"/><circle cx="18" cy="6" r="2.5"/><circle cx="6" cy="18" r="2.5"/><circle cx="18" cy="18" r="2.5"/>',
    "job": '<path d="M5 12l4 4 10-10"/>',
    "cron": '<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>',
    "svc": '<circle cx="12" cy="12" r="3"/><path d="M12 3v6M12 15v6M3 12h6M15 12h6"/>',
    "ing": '<path d="M3 12h13M12 6l6 6-6 6M21 4v16"/>',
    "cm": '<path d="M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z"/><path d="M14 3v6h6M8 13h8M8 17h5"/>',
    "secret": '<rect x="4" y="11" width="16" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/>',
    "pvc": '<rect x="3" y="4" width="18" height="7" rx="1.5"/><rect x="3" y="13" width="18" height="7" rx="1.5"/><path d="M7 7.5h.01M7 16.5h.01"/>',
    "node": '<rect x="3" y="3" width="18" height="12" rx="2"/><path d="M8 21h8M12 15v6"/>',
    "ns": '<path d="M3 7l9-4 9 4-9 4z"/><path d="M3 12l9 4 9-4M3 17l9 4 9-4"/>',
    "event": '<path d="M13 2 4 14h7l-1 8 9-12h-7z"/>',
    "gw": '<path d="M4 21V9l8-6 8 6v12"/><path d="M9 21v-6h6v6"/>',
    "route": '<circle cx="6" cy="19" r="2.5"/><circle cx="18" cy="5" r="2.5"/><path d="M6 16.5V12a4 4 0 0 1 4-4h5.5"/>',
    "argo": '<circle cx="12" cy="12" r="9"/><path d="M8 14s1.5 2 4 2 4-2 4-2M9 9h.01M15 9h.01"/>',
    "flux": '<path d="M4 12c3-6 5 6 8 0s5 6 8 0"/>',
    "karp": '<path d="M12 3l8 4.5v9L12 21l-8-4.5v-9z"/><path d="M12 12l8-4.5M12 12v9M12 12 4 7.5"/>',
    "helm": '<circle cx="12" cy="12" r="8"/><circle cx="12" cy="12" r="2.5"/><path d="M12 2v5M12 17v5M2 12h5M17 12h5M5 5l3.5 3.5M15.5 15.5 19 19M5 19l3.5-3.5M15.5 8.5 19 5"/>',
    "home": '<path d="M3 11l9-8 9 8"/><path d="M5 10v10h14V10"/>',
    "all": '<rect x="2" y="4" width="8" height="7" rx="1"/><rect x="14" y="4" width="8" height="7" rx="1"/><rect x="8" y="14" width="8" height="7" rx="1"/>',
    "search": '<circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/>',
    "chev": '<path d="m6 9 6 6 6-6"/>',
    "plus": '<path d="M12 5v14M5 12h14"/>',
    "logs": '<path d="M4 6h16M4 10h16M4 14h10M4 18h7"/>',
    "term": '<path d="m4 17 6-5-6-5M12 19h8"/>',
    "fwd": '<path d="M5 12h14M13 6l6 6-6 6"/>',
    "yaml": '<path d="m8 6-6 6 6 6M16 6l6 6-6 6"/>',
    "restart": '<path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5"/>',
    "trash": '<path d="M3 6h18M8 6V4h8v2M6 6l1 14h10l1-14"/>',
    "scale": '<path d="M12 3v18M3 12h18"/>',
    "x": '<path d="M18 6 6 18M6 6l12 12"/>',
    "lock": '<rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/>',
    "box": '<path d="M21 8 12 3 3 8v8l9 5 9-5z"/><path d="M3 8l9 5 9-5M12 13v8"/>',
    "alert": '<path d="M12 3 2 20h20z"/><path d="M12 10v4M12 17h.01"/>',
    "check": '<path d="M5 12l4 4 10-10"/>',
    "filter": '<path d="M3 5h18l-7 8v6l-4-2v-4z"/>',
    "cols": '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9 4v16M15 4v16"/>',
    "cmd": '<path d="M9 6a3 3 0 1 0-3 3h12a3 3 0 1 0-3-3v12a3 3 0 1 0 3-3H6a3 3 0 1 0 3 3z"/>',
    "shield": '<path d="M12 3 4 6v6c0 5 3.5 8 8 9 4.5-1 8-4 8-9V6z"/>',
}


def ic(name):
    return f'<svg class="i" viewBox="0 0 24 24">{I[name]}</svg>'


def nav(icon, label, n="", sel=False, fail=False, badge=""):
    cls = "nav sel" if sel else "nav"
    extra = f'<span class="fail"></span>' if fail else ""
    right = f'<span class="badge">{badge}</span>' if badge else f'<span class="n">{n}</span>'
    return f'<div class="{cls}">{ic(icon)}<span>{label}</span>{extra}{right}</div>'


def sidebar(selected):
    s = lambda k: selected == k
    return f"""
<aside class="side">
  <div class="ctxsel"><span class="sw"></span>kind-fleetglass-dev<span class="dot ok" style="margin-left:6px"></span><span class="chev">{ic('chev')}</span></div>
  {nav('home', 'Overview', sel=s('overview'))}
  {nav('all', 'All Clusters', '3', sel=s('all'))}
  <div class="grp"><h6>WORKLOADS {ic('chev')}</h6>
    {nav('pod', 'Pods', '42', sel=s('pods'), fail=True)}
    {nav('deploy', 'Deployments', '8')}
    {nav('sts', 'StatefulSets', '2')}
    {nav('ds', 'DaemonSets', '3')}
    {nav('job', 'Jobs', '4')}
    {nav('cron', 'CronJobs', '1')}
  </div>
  <div class="grp"><h6>NETWORK {ic('chev')}</h6>
    {nav('svc', 'Services', '9')}
    {nav('ing', 'Ingresses', '2')}
  </div>
  <div class="grp"><h6>CONFIG {ic('chev')}</h6>
    {nav('cm', 'ConfigMaps', '14')}
    {nav('secret', 'Secrets', '11')}
  </div>
  <div class="grp"><h6>STORAGE {ic('chev')}</h6>{nav('pvc', 'PVCs', '3')}</div>
  <div class="grp"><h6>CLUSTER {ic('chev')}</h6>
    {nav('node', 'Nodes', '3')}
    {nav('ns', 'Namespaces', '6')}
    {nav('event', 'Events', '', fail=True)}
  </div>
  <div class="grp"><h6>ECOSYSTEM {ic('chev')}</h6>
    {nav('gw', 'Gateways', badge='Gateway API')}
    {nav('argo', 'Argo Apps', badge='Argo CD')}
    {nav('helm', 'Helm releases', '5')}
  </div>
</aside>"""


def statusbar():
    return f"""
<footer class="status">
  <span><span class="dot ok"></span>Connected</span>
  <span>kind-fleetglass-dev</span>
  <span>metrics-server available</span>
  <span>Updated 2s ago</span>
  <div class="r"><span>{ic('shield')}Read-write</span><span><kbd>Ctrl K</kbd> Commands</span><span><kbd>?</kbd> Shortcuts</span></div>
</footer>"""


def tabs(active):
    out = []
    for name, icon in (("Overview", "home"), ("Pods", "pod"), ("Deployments", "deploy")):
        cls = "tab on" if name == active else "tab"
        out.append(f'<div class="{cls}">{ic(icon)}{name}<span class="x">✕</span></div>')
    out.append(f'<div class="tab">{ic("plus")}</div>')
    return f'<div class="tabs">{"".join(out)}</div>'


def use(val, unit_note, pct, req=None, cls=""):
    reqbar = f'<i class="req" style="width:{req}%"></i>' if req else ""
    return (f'<div class="use"><div class="v">{val}<span>{unit_note}</span></div>'
            f'<div class="bar">{reqbar}<i class="{cls}" style="width:{pct}%"></i></div></div>')


PODS = [
    # name, ns, status(cls,label), ready, restarts, last restart, cpu, mem, node, age, rowcls
    ("web-7d9f8c6b5-k2x4p", "default", ("ok", "Running"), "1/1", "0", "—",
     use("3m", "3% req", 3, 100), use("12Mi", "9% req", 9, 100), "worker", "2d", "sel"),
    ("api-5c6b2d9f7-wq8zn", "default", ("ok", "Running"), "2/2", "1", '<span class="lr">3h · OOMKilled</span>',
     use("41m", "41% req", 41, 100), use("118Mi", "92% lim", 92, 100, "warn"), "worker", "2d", ""),
    ("crashloop-6f7b9c-x8k2", "default", ("err", "CrashLoopBackOff"), "0/1", '<span class="restarts hot">14</span>',
     '<span class="lr">4m · <span class="r">Error (1)</span></span>', use("0m", "", 0), use("2Mi", "", 2), "worker2", "2d", "err"),
    ("pending-q2m4", "default", ("warn", "Unschedulable"), "0/1", "0", "—",
     '<span class="mut">—</span>', '<span class="mut">—</span>', '<span class="mut">none</span>', "2d", ""),
    ("redis-0", "data", ("ok", "Running"), "1/1", "0", "—",
     use("7m", "14% req", 14, 100), use("31Mi", "24% req", 24, 100), "worker2", "2d", ""),
    ("migrate-28713400-h7jc2", "jobs", ("off", "Completed"), "0/1", "0", "—",
     '<span class="mut">—</span>', '<span class="mut">—</span>', "worker", "5h", ""),
    ("image-pull-7cd9-p2l8m", "default", ("err", "ImagePullBackOff"), "0/1", "0", "—",
     '<span class="mut">—</span>', '<span class="mut">—</span>', "worker", "1h", "err"),
    ("coredns-668d6bf9bc-9xk4t", "kube-system", ("ok", "Running"), "1/1", "0", "—",
     use("2m", "2% req", 2, 100), use("16Mi", "23% req", 23, 100), "control-plane", "2d", ""),
    ("coredns-668d6bf9bc-tq7lm", "kube-system", ("ok", "Running"), "1/1", "0", "—",
     use("2m", "2% req", 2, 100), use("15Mi", "21% req", 21, 100), "control-plane", "2d", ""),
    ("etcd-fleetglass-dev", "kube-system", ("ok", "Running"), "1/1", "0", "—",
     use("19m", "19% req", 19, 100), use("44Mi", "44% req", 44, 100), "control-plane", "2d", ""),
    ("kube-apiserver-fleetglass", "kube-system", ("ok", "Running"), "1/1", "0", "—",
     use("48m", "19% req", 19, 100), use("261Mi", "—", 30), "control-plane", "2d", ""),
    ("metrics-server-6d94bc8694", "kube-system", ("ok", "Running"), "1/1", "0", "—",
     use("4m", "4% req", 4, 100), use("19Mi", "9% req", 9, 100), "worker", "2d", ""),
    ("kindnet-5wq2r", "kube-system", ("ok", "Running"), "1/1", "0", "—",
     use("1m", "1% req", 1, 100), use("9Mi", "18% req", 18, 100), "worker2", "2d", ""),
    ("local-path-provisioner-7b8", "local-path", ("ok", "Running"), "1/1", "0", "—",
     use("1m", "", 1), use("8Mi", "", 6), "control-plane", "2d", ""),
    ("sample-cron-28713420-m3n", "jobs", ("off", "Completed"), "0/1", "0", "—",
     '<span class="mut">—</span>', '<span class="mut">—</span>', "worker2", "12m", ""),
    ("worker-58d7f6c9-r4v2b", "default", ("ok", "Running"), "1/1", "2", '<span class="lr">1d · Error (137)</span>',
     use("12m", "24% req", 24, 100), use("64Mi", "50% req", 50, 100), "worker2", "2d", ""),
]


def pod_table(dense_rows=16):
    rows = []
    for p in PODS[:dense_rows]:
        name, ns, (sc, sl), ready, rs, lr, cpu, mem, node, age, rc = p
        rows.append(f'<tr class="{rc}"><td class="name">{name}</td><td class="mut">{ns}</td>'
                    f'<td><span class="pill {sc}">{sl}</span></td><td>{ready}</td><td>{rs}</td><td>{lr}</td>'
                    f'<td>{cpu}</td><td>{mem}</td><td class="mut">{node}</td><td class="mut">{age}</td></tr>')
    return f"""
<table>
<thead><tr><th style="width:250px">NAME</th><th>NAMESPACE</th><th>STATUS</th><th>READY</th><th>RESTARTS</th>
<th>LAST RESTART</th><th class="sorted" style="width:120px">CPU ▾</th><th style="width:120px">MEMORY</th><th>NODE</th><th>AGE</th></tr></thead>
<tbody>{''.join(rows)}</tbody></table>"""


def toolbar():
    return f"""
<div class="toolbar">
  <div class="search">{ic('search')}Filter pods…<kbd>/</kbd></div>
  <div class="btn">{ic('ns')}All namespaces{ic('chev')}</div>
  <div class="btn">{ic('filter')}Status: any{ic('chev')}</div>
  <div class="btn">{ic('cols')}Columns</div>
  <div class="summary"><span><b>42</b> pods</span><span><b class="e">3</b> failing</span><span><b>1</b> pending</span></div>
</div>"""


def page(body, theme="", annos=""):
    return f"""<!doctype html><html><head><meta charset="utf-8"><link rel="stylesheet" href="mock.css"></head>
<body class="{theme}"><div class="app">{body}</div>{annos}</body></html>"""


def anno(n, text, x, y):
    return ""
    return f'<div class="anno" style="left:{x}px;top:{y}px"><b>{n}</b>{text}</div>'


POD_ANNOS = "".join([
    anno(1, "Status pills; failing rows tinted", 700, 205),
    anno(2, "CPU/memory with % of request or limit", 1060, 104),
    anno(3, "Last restart age + reason", 830, 300),
    anno(4, "Grouped sidebar, icons, counts, failure dot", 238, 185),
    anno(5, "Live counts", 1290, 45),
])


def pod_list(theme="", annos=True):
    body = (sidebar("pods") + '<main class="main">' + tabs("Pods") + toolbar()
            + '<div style="flex:1;overflow:hidden">' + pod_table() + "</div></main>" + statusbar())
    return page(body, theme, POD_ANNOS if annos else "")


def cluster_card(name, color, health, nodes, pods_ok, pods_bad, warns, cpu, cpu_req, mem, mem_req, protected=False):
    hc, hl = health
    lock = f'<span class="chip" style="margin:0 0 0 auto">{ic("lock")}protected</span>' if protected else ""
    bad = f' · <span style="color:var(--err)">{pods_bad} failing</span>' if pods_bad else ""
    def cap(label, used, req):
        return (f'<div style="margin-top:12px"><div style="display:flex;justify-content:space-between;font-size:12px">'
                f'<span style="color:var(--fg-muted)">{label}</span><span>{used}% used · <span style="color:var(--fg-subtle)">{req}% requested</span></span></div>'
                f'<div class="bar" style="height:7px;margin-top:5px"><i class="req" style="width:{req}%"></i>'
                f'<i class="{"warn" if used > 80 else ""}" style="width:{used}%"></i></div></div>')
    return f"""
<div class="card" style="flex:1;border-left:3px solid {color}">
  <h4 style="color:var(--fg);font-size:14px;letter-spacing:0">{name}{lock}</h4>
  <div class="body">
    <div style="display:flex;align-items:center;gap:8px;margin-bottom:12px"><span class="pill {hc}">{hl}</span></div>
    <div style="display:grid;grid-template-columns:repeat(3,1fr);gap:8px">
      <div><div style="color:var(--fg-subtle);font-size:11px">NODES READY</div><div style="font-size:20px;font-weight:600">{nodes}</div></div>
      <div><div style="color:var(--fg-subtle);font-size:11px">PODS</div><div style="font-size:20px;font-weight:600">{pods_ok}</div><div style="font-size:11.5px;color:var(--fg-muted)">running{bad}</div></div>
      <div><div style="color:var(--fg-subtle);font-size:11px">WARNINGS · 1H</div><div style="font-size:20px;font-weight:600;color:{'var(--warn)' if warns else 'var(--fg)'}">{warns}</div></div>
    </div>
    {cap('CPU', cpu, cpu_req)}{cap('Memory', mem, mem_req)}
  </div>
</div>"""


def overview():
    attention = [
        ("err", "CrashLoopBackOff", "crashloop-6f7b9c-x8k2", "default · kind-fleetglass-dev", "14 restarts · last: Error (1) 4m ago"),
        ("err", "ImagePullBackOff", "image-pull-7cd9-p2l8m", "default · kind-fleetglass-dev", "nginx:does-not-exist not found"),
        ("warn", "Unschedulable", "pending-q2m4", "default · kind-fleetglass-dev", "0/3 nodes have enough CPU"),
        ("warn", "Near limit", "api-5c6b2d9f7-wq8zn", "default · kind-fleetglass-dev", "memory 92% of limit"),
        ("warn", "Not ready", "Deployment checkout", "shop · staging-eu", "2/3 replicas available"),
    ]
    att = "".join(
        f'<div style="display:flex;align-items:center;gap:12px;padding:9px 14px;border-bottom:1px solid var(--border)">'
        f'<span class="pill {c}" style="width:150px">{s}</span><div style="flex:1"><div style="font-weight:500">{n}</div>'
        f'<div style="font-size:11.5px;color:var(--fg-subtle)">{w}</div></div><div style="color:var(--fg-muted);font-size:12px">{d}</div></div>'
        for c, s, n, w, d in attention)
    events = [
        ("2m", "BackOff", "crashloop-6f7b9c-x8k2", "Back-off restarting failed container"),
        ("5m", "FailedScheduling", "pending-q2m4", "0/3 nodes are available: 3 Insufficient cpu"),
        ("9m", "Failed", "image-pull-7cd9-p2l8m", "Failed to pull image \"nginx:does-not-exist\""),
        ("12m", "Unhealthy", "checkout-6b5d-2kq9x", "Readiness probe failed: HTTP 503"),
        ("31m", "OOMKilling", "api-5c6b2d9f7-wq8zn", "Memory cgroup out of memory"),
    ]
    ev = "".join(
        f'<div style="display:flex;gap:12px;padding:9px 14px;border-bottom:1px solid var(--border)">'
        f'<span style="width:34px;color:var(--fg-subtle);font-size:12px">{t}</span><div style="flex:1;min-width:0">'
        f'<div><span style="color:var(--warn);font-weight:600">{r}</span> · <span>{o}</span></div>'
        f'<div style="font-size:11.5px;color:var(--fg-muted);white-space:nowrap;overflow:hidden;text-overflow:ellipsis">{m}</div></div></div>'
        for t, r, o, m in events)
    body = sidebar("overview") + f"""
<main class="main">{tabs('Overview')}
<div style="padding:18px 22px;display:flex;flex-direction:column;gap:16px;overflow:hidden">
  <div style="display:flex;align-items:center;gap:12px">
    <div style="font-size:18px;font-weight:600">Overview</div><span style="color:var(--fg-muted)">3 clusters</span>
    <div class="btn sm" style="margin-left:auto">Last 1h{ic('chev')}</div>
  </div>
  <div style="display:flex;gap:14px">
    {cluster_card('kind-fleetglass-dev', '#4c8dff', ('warn', '3 issues'), '3/3', 38, 3, 7, 38, 61, 54, 72)}
    {cluster_card('staging-eu', '#a371f7', ('warn', '1 issue'), '5/5', 120, 0, 2, 22, 48, 41, 66)}
    {cluster_card('prod-us', '#f85149', ('ok', 'Healthy'), '12/12', 486, 0, 0, 61, 70, 83, 88, True)}
  </div>
  <div style="display:flex;gap:14px;min-height:0">
    <div class="card" style="flex:1.25"><h4>{ic('alert')}NEEDS ATTENTION<span class="r">5</span></h4>{att}</div>
    <div class="card" style="flex:1"><h4>{ic('event')}RECENT WARNING EVENTS<span class="r">All clusters</span></h4>{ev}</div>
  </div>
</div></main>""" + statusbar()
    annos = "".join([
        anno(1, "One card per cluster, coloured like its context", 470, 150),
        anno(2, "Used vs requested capacity", 900, 395),
        anno(3, "Clickable: opens the object", 560, 470),
        anno(4, "Merged across clusters", 1180, 470),
    ])
    return page(body, "", annos)


def drawer():
    body = (sidebar("pods") + '<main class="main">' + tabs("Pods") + toolbar()
            + '<div style="flex:1;overflow:hidden;opacity:.45">' + pod_table() + "</div>" + f"""
<section style="position:absolute;top:36px;right:0;bottom:0;width:620px;background:var(--bg);border-left:1px solid var(--border);
  box-shadow:-12px 0 30px rgba(0,0,0,.35);display:flex;flex-direction:column">
  <div style="padding:14px 18px 10px;border-bottom:1px solid var(--border)">
    <div style="display:flex;align-items:center;gap:10px">
      <span style="color:var(--accent)">{ic('pod')}</span>
      <div style="font-size:15px;font-weight:600">web-7d9f8c6b5-k2x4p</div>
      <span class="pill ok">Running</span>
      <span style="margin-left:auto;color:var(--fg-subtle)">{ic('x')}</span>
    </div>
    <div style="color:var(--fg-muted);font-size:12px;margin:3px 0 10px 25px">Pod · default · kind-fleetglass-dev · 2d</div>
    <div style="display:flex;gap:6px">
      <span class="btn sm">{ic('logs')}Logs <kbd>l</kbd></span><span class="btn sm">{ic('term')}Shell <kbd>x</kbd></span>
      <span class="btn sm">{ic('fwd')}Forward <kbd>f</kbd></span><span class="btn sm">{ic('restart')}Restart</span>
      <span class="btn sm danger" style="margin-left:auto">{ic('trash')}Delete</span>
    </div>
  </div>
  <div style="display:flex;gap:18px;padding:0 18px;border-bottom:1px solid var(--border);height:36px;align-items:flex-end">
    <span style="padding-bottom:8px;border-bottom:2px solid var(--accent);font-weight:500">Summary</span>
    <span style="padding-bottom:8px;color:var(--fg-muted)">YAML</span><span style="padding-bottom:8px;color:var(--fg-muted)">Logs</span>
    <span style="padding-bottom:8px;color:var(--fg-muted)">Terminal</span><span style="padding-bottom:8px;color:var(--fg-muted)">Events <span class="badge" style="font-size:10px;padding:0 5px;border-radius:8px;background:var(--off-bg)">3</span></span>
  </div>
  <div style="padding:14px 18px;display:flex;flex-direction:column;gap:12px;overflow:hidden">
    <div class="card"><h4>OVERVIEW</h4><div class="body"><dl class="kv" style="margin:0">
      <dt>Node</dt><dd><a>fleetglass-dev-worker</a></dd><dt>Pod IP</dt><dd>10.244.1.12</dd>
      <dt>QoS class</dt><dd>Burstable</dd><dt>Service account</dt><dd><a>web</a></dd>
      <dt>Controlled by</dt><dd><a>ReplicaSet web-7d9f8c6b5</a></dd><dt>Priority</dt><dd>0</dd>
    </dl></div></div>
    <div class="card"><h4>CONDITIONS</h4><div class="body" style="padding-bottom:6px">
      <span class="chip ok">{ic('check')}PodScheduled</span><span class="chip ok">{ic('check')}Initialized</span>
      <span class="chip ok">{ic('check')}ContainersReady</span><span class="chip ok">{ic('check')}Ready</span>
    </div></div>
    <div class="card"><h4>CONTAINERS<span class="r">1</span></h4><div class="body">
      <div style="display:flex;align-items:center;gap:10px">
        <span style="color:var(--fg-muted)">{ic('box')}</span><b>web</b><span class="pill ok">Running</span>
        <span style="color:var(--fg-subtle);font:12px var(--mono)">nginx:1.27-alpine</span>
        <span style="margin-left:auto;display:flex;gap:6px"><span class="btn sm">{ic('logs')}Logs</span><span class="btn sm">{ic('term')}Shell</span></span>
      </div>
      <div style="display:grid;grid-template-columns:repeat(4,1fr);gap:14px;margin-top:12px;font-size:12px">
        <div><div style="color:var(--fg-subtle)">Ready</div>1/1 · 0 restarts</div>
        <div><div style="color:var(--fg-subtle)">Ports</div>80/TCP <a>Forward</a></div>
        <div>{use('3m / 100m', '', 3, 100)}<div style="color:var(--fg-subtle);margin-top:2px">CPU</div></div>
        <div>{use('12Mi / 128Mi', '', 9, 100)}<div style="color:var(--fg-subtle);margin-top:2px">Memory</div></div>
      </div>
    </div></div>
    <div class="card"><h4>USES</h4><div class="body" style="padding-bottom:6px">
      <span class="chip link">{ic('cm')}<span class="k">ConfigMap</span>web-config</span>
      <span class="chip link">{ic('secret')}<span class="k">Secret</span>web-secret</span>
      <span class="chip link">{ic('pvc')}<span class="k">PVC</span>web-data</span>
      <span class="chip link">{ic('svc')}<span class="k">Service</span>web</span>
    </div></div>
    <div class="card"><h4>LABELS</h4><div class="body" style="padding-bottom:6px">
      <span class="chip"><span class="k">app=</span>web</span><span class="chip"><span class="k">tier=</span>frontend</span>
      <span class="chip"><span class="k">pod-template-hash=</span>7d9f8c6b5</span>
    </div></div>
  </div>
</section></main>""" + statusbar())
    annos = "".join([
        anno(1, "Sticky header: status + actions with keys", 560, 70),
        anno(2, "Links open that object", 560, 290),
        anno(3, "Per-container card with usage vs request/limit", 560, 470),
        anno(4, "Objects this pod mounts or references", 560, 615),
    ])
    return page(body, "", annos)


(OUT / "1-pod-list-dark.html").write_text(pod_list())
(OUT / "2-overview-dark.html").write_text(overview())
(OUT / "3-pod-drawer-dark.html").write_text(drawer())
(OUT / "4-pod-list-light.html").write_text(pod_list("light", annos=False))
print("ok")
