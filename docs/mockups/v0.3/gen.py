#!/usr/bin/env python3
"""Generates the v0.3 phone mockups (static HTML) next to this file.

They reuse the desktop mockups' icons, data and components (../v0.2/gen.py and
../v0.2/mock.css) so the phone app looks like the desktop app, folded to one column.

Render: firefox --headless --window-size=412,915 --screenshot out.png file://$PWD/page.html
"""
from pathlib import Path

OUT = Path(__file__).parent
V2 = OUT.parent / "v0.2" / "gen.py"

# Load the desktop generator's helpers without running its file writes.
_src = V2.read_text().split('\n(OUT / "1-pod-list-dark.html")')[0]
d = {"__file__": str(V2)}
exec(compile(_src, str(V2), "exec"), d)
ic, use, PODS, cluster_card = d["ic"], d["use"], d["PODS"], d["cluster_card"]
d["I"]["menu"] = '<path d="M4 6h16M4 12h16M4 18h16"/>'
d["I"]["gear"] = '<circle cx="12" cy="12" r="3"/><path d="M12 2v3M12 19v3M2 12h3M19 12h3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M4.9 19.1 7 17M17 7l2.1-2.1"/>'
d["I"]["back"] = '<path d="m15 18-6-6 6-6"/>'
d["I"]["more"] = '<circle cx="12" cy="5" r="1"/><circle cx="12" cy="12" r="1"/><circle cx="12" cy="19" r="1"/>'


def page(body, theme=""):
    return (f'<!doctype html><html><head><meta charset="utf-8"><link rel="stylesheet" href="mobile.css"></head>'
            f'<body class="{theme}"><div class="phone">{body}</div></body></html>')


def appbar(ctx="kind-fleetglass-dev", action="search"):
    return f"""
<div class="appbar">
  <div class="ham">{ic('menu')}</div>
  <div class="ctxsel"><span class="sw"></span><span class="nm">{ctx}</span><span class="dot ok" style="margin-left:2px"></span><span class="chev">{ic('chev')}</span></div>
  <div class="act">{ic(action)}</div>
</div>"""


def bnav(on, fail_events=True):
    items = [("Overview", "home"), ("Workloads", "pod"), ("Events", "event"), ("Settings", "gear")]
    out = []
    for label, icon in items:
        cls = ' class="on"' if label == on else ""
        dot = '<span class="fail"></span>' if label == "Events" and fail_events else ""
        out.append(f"<div{cls}>{ic(icon)}{label}{dot}</div>")
    return f'<nav class="bnav">{"".join(out)}</nav>'


def status():
    return ('<footer class="status"><span><span class="dot ok"></span>Connected</span>'
            '<span>Updated 2s ago</span><div class="r"><span>' + ic('shield') + 'Read-write</span></div></footer>')


# ---------- 1 overview ----------
def overview(theme=""):
    att = [
        ("err", "CrashLoopBackOff", "crashloop-6f7b9c-x8k2", "default · kind-fleetglass-dev", "14 restarts · Error (1) 4m ago"),
        ("err", "ImagePullBackOff", "image-pull-7cd9-p2l8m", "default · kind-fleetglass-dev", "nginx:does-not-exist not found"),
        ("warn", "Unschedulable", "pending-q2m4", "default · kind-fleetglass-dev", "0/3 nodes have enough CPU"),
    ]
    rows = "".join(
        f'<div style="padding:10px 12px;border-bottom:1px solid var(--border);display:flex;flex-direction:column;gap:4px">'
        f'<div style="display:flex;align-items:center;gap:8px"><span class="pill {c}">{s}</span>'
        f'<span style="margin-left:auto;font-size:11.5px;color:var(--fg-subtle)">{w.split(" · ")[0]}</span></div>'
        f'<div style="font-weight:500">{n}</div><div style="font-size:12px;color:var(--fg-muted)">{x}</div></div>'
        for c, s, n, w, x in att)
    body = appbar() + f"""
<div class="titlebar"><span class="t">Overview</span><span class="s">3 clusters</span><span class="btn sm">Last 1h{ic('chev')}</span></div>
<div class="content"><div class="pad">
  {cluster_card('kind-fleetglass-dev', '#4c8dff', ('warn', '3 issues'), '3/3', 38, 3, 7, 38, 61, 54, 72)}
  <div style="display:flex;gap:8px;overflow:hidden">
    <div class="card" style="flex:1;border-left:3px solid #a371f7;padding:10px 12px"><div style="font-weight:600">staging-eu</div><span class="pill warn" style="margin-top:6px">1 issue</span></div>
    <div class="card" style="flex:1;border-left:3px solid #f85149;padding:10px 12px"><div style="font-weight:600;display:flex;gap:6px;align-items:center">prod-us{ic('lock')}</div><span class="pill ok" style="margin-top:6px">Healthy</span></div>
  </div>
  <div class="card"><h4>{ic('alert')}NEEDS ATTENTION<span class="r">5</span></h4>{rows}</div>
</div></div>""" + status() + bnav("Overview")
    return page(body, theme)


# ---------- 2 pod list ----------
def pod_row(p):
    name, ns, (sc, sl), ready, rs, lr, cpu, mem, node, age, rc = p
    hot = ' hot' if rs.startswith('<span class="restarts hot">') else ""
    rs_txt = rs.replace('<span class="restarts hot">', '').replace('</span>', '')
    cpu_cell = cpu if cpu.startswith("<div") else '<span class="mut" style="width:86px">CPU —</span>'
    mem_cell = mem if mem.startswith("<div") else '<span class="mut" style="width:86px">Mem —</span>'
    return f"""
<div class="row {rc}">
  <div class="l1"><span class="name">{name}</span><span class="pill {sc}">{sl}</span></div>
  <div class="l2">{cpu_cell}{mem_cell}<span class="meta">{ns} · {ready} · <span class="rs{hot}">↻{rs_txt}</span> · {age}</span></div>
</div>"""


def pods(theme=""):
    order = [PODS[i] for i in (2, 5, 3, 1, 0, 4, 12, 7, 8, 9)]
    body = appbar() + f"""
<div class="titlebar"><span class="t">Pods</span><span class="s">kind-fleetglass-dev</span><span class="btn sm">{ic('pod')}Workloads{ic('chev')}</span></div>
<div class="toolbar">
  <div class="search">{ic('search')}Filter pods…</div>
  <div class="btn">{ic('ns')}All ns{ic('chev')}</div>
  <div class="summary"><span><b>42</b> pods</span><span><b class="e">3</b> failing</span><span><b>1</b> pending</span>
    <span style="margin-left:auto">Sort: CPU ▾</span></div>
</div>
<div class="content"><div class="rows">{''.join(pod_row(p) for p in order)}</div></div>""" + status() + bnav("Workloads")
    return page(body, theme)


# ---------- 3 pod detail ----------
def detail(theme=""):
    body = f"""
<div class="appbar">
  <div class="ham">{ic('back')}</div>
  <div style="flex:1;min-width:0"><div style="font-weight:600;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">crashloop-6f7b9c-x8k2</div>
    <div style="font-size:11.5px;color:var(--fg-muted)">Pod · default · kind-fleetglass-dev · 2d</div></div>
  <div class="act">{ic('more')}</div>
</div>
<div class="sheet-h">
  <span class="pill err">CrashLoopBackOff</span>
  <div style="display:flex;gap:8px;margin-top:10px">
    <span class="btn sm">{ic('logs')}Logs</span><span class="btn sm">{ic('restart')}Restart</span>
    <span class="btn sm danger" style="margin-left:auto">{ic('trash')}Delete</span>
  </div>
</div>
<div class="sheet-tabs"><span class="on">Summary</span><span>YAML</span><span>Logs</span><span>Events <span class="badge" style="font-size:10px;padding:0 5px;border-radius:8px;background:var(--off-bg)">3</span></span></div>
<div class="content"><div class="pad">
  <div class="card" style="border-color:color-mix(in srgb, var(--err) 45%, var(--border))"><div class="body" style="display:flex;gap:10px;align-items:flex-start">
    <span style="color:var(--err)">{ic('alert')}</span><div><div style="font-weight:600">Back-off restarting failed container</div>
    <div style="font-size:12px;color:var(--fg-muted)">14 restarts · last: <span style="color:var(--err)">Error (1)</span> 4m ago · see previous logs</div></div></div></div>
  <div class="card"><h4>OVERVIEW</h4><div class="body"><dl class="kv" style="margin:0">
    <dt>Node</dt><dd><a>fleetglass-dev-worker2</a></dd><dt>Pod IP</dt><dd>10.244.2.7</dd>
    <dt>QoS class</dt><dd>BestEffort</dd><dt>Controlled by</dt><dd><a>ReplicaSet crashloop-6f7b9c</a></dd>
  </dl></div></div>
  <div class="card"><h4>CONTAINERS<span class="r">1</span></h4><div class="body">
    <div style="display:flex;align-items:center;gap:8px"><span style="color:var(--fg-muted)">{ic('box')}</span><b>app</b><span class="pill err">Waiting</span>
      <span style="margin-left:auto;color:var(--fg-subtle);font:12px var(--mono)">busybox:1.36</span></div>
    <div style="display:grid;grid-template-columns:1fr 1fr;gap:12px;margin-top:12px;font-size:12px">
      <div><div style="color:var(--fg-subtle)">Ready</div>0/1 · <span style="color:var(--err)">14 restarts</span></div>
      <div><div style="color:var(--fg-subtle)">Last termination</div>Error (1) · 4m ago</div>
    </div>
  </div></div>
  <div class="card"><h4>CONDITIONS</h4><div class="body" style="padding-bottom:6px">
    <span class="chip ok">{ic('check')}PodScheduled</span><span class="chip ok">{ic('check')}Initialized</span>
    <span class="chip" style="color:var(--err)">{ic('x')}Ready</span>
  </div></div>
</div></div>""" + bnav("Workloads")
    return page(body, theme)


# ---------- 4 navigation (the desktop sidebar as a slide-over) ----------
def navigation(theme=""):
    behind = appbar() + '<div class="titlebar"><span class="t">Pods</span></div><div class="content"></div>' + status() + bnav("Workloads")
    side = d["sidebar"]("pods").replace('<aside class="side">', '<aside class="slide">')
    return page(behind + '<div class="scrim"></div>' + side, theme)


(OUT / "1-overview-dark.html").write_text(overview())
(OUT / "2-pods-dark.html").write_text(pods())
(OUT / "3-pod-detail-dark.html").write_text(detail())
(OUT / "4-navigation-dark.html").write_text(navigation())
(OUT / "5-pods-light.html").write_text(pods("light"))
print("ok")
