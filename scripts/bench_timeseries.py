#!/usr/bin/env python3
"""
Chronos — Timeseries Report Generator
Reads the benchmark history JSON and produces a dedicated interactive HTML
timeseries dashboard with per-benchmark charts, trend analysis, min/max
annotations, and statistical overlays.
"""

import argparse
import json
import math
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


# ─── Data Processing ─────────────────────────────────────────────────────────

def load_history(path: str) -> list[dict]:
    p = Path(path)
    if not p.exists():
        print(f"[chronos] ERROR: History file not found: {path}", file=sys.stderr)
        sys.exit(1)
    with open(p) as f:
        return json.load(f)


def build_series(history: list[dict]) -> dict[str, list[dict]]:
    """Build per-benchmark timeseries from history."""
    series: dict[str, list[dict]] = {}
    for entry in history:
        ts = entry["timestamp"]
        commit = entry.get("commit", "?")
        branch = entry.get("branch", "?")
        for name, data in entry.get("benchmarks", {}).items():
            if name not in series:
                series[name] = []
            series[name].append({
                "timestamp": ts,
                "commit": commit,
                "branch": branch,
                "value_ns": data["point_estimate_ns"],
                "ci_lower": data.get("ci_lower_ns"),
                "ci_upper": data.get("ci_upper_ns"),
                "std_dev": data.get("std_dev_ns"),
            })
    return series


def compute_stats(points: list[dict]) -> dict[str, Any]:
    """Compute summary statistics for a benchmark series."""
    values = [p["value_ns"] for p in points]
    n = len(values)
    if n == 0:
        return {}

    mean = sum(values) / n
    variance = sum((v - mean) ** 2 for v in values) / n if n > 1 else 0
    std_dev = math.sqrt(variance)
    cv = (std_dev / mean * 100) if mean > 0 else 0

    min_val = min(values)
    max_val = max(values)
    min_idx = values.index(min_val)
    max_idx = values.index(max_val)

    # Trend: simple linear regression (least squares)
    if n >= 2:
        x_vals = list(range(n))
        x_mean = sum(x_vals) / n
        y_mean = mean
        num = sum((x - x_mean) * (y - y_mean) for x, y in zip(x_vals, values))
        den = sum((x - x_mean) ** 2 for x in x_vals)
        slope = num / den if den != 0 else 0
        intercept = y_mean - slope * x_mean
        # Trend as % change over the series
        trend_pct = (slope * (n - 1)) / values[0] * 100 if values[0] != 0 else 0
    else:
        slope = 0
        intercept = values[0] if values else 0
        trend_pct = 0

    # Latest vs first
    if n >= 2:
        total_change_pct = (values[-1] - values[0]) / values[0] * 100
    else:
        total_change_pct = 0

    return {
        "count": n,
        "mean": mean,
        "std_dev": std_dev,
        "cv_pct": cv,
        "min": min_val,
        "max": max_val,
        "min_idx": min_idx,
        "max_idx": max_idx,
        "min_commit": points[min_idx]["commit"],
        "max_commit": points[max_idx]["commit"],
        "latest": values[-1],
        "first": values[0],
        "total_change_pct": total_change_pct,
        "trend_slope": slope,
        "trend_intercept": intercept,
        "trend_pct": trend_pct,
    }


def format_ns(ns: float) -> str:
    if ns < 1_000:
        return f"{ns:.1f} ns"
    elif ns < 1_000_000:
        return f"{ns / 1_000:.2f} µs"
    elif ns < 1_000_000_000:
        return f"{ns / 1_000_000:.2f} ms"
    else:
        return f"{ns / 1_000_000_000:.3f} s"


def auto_unit(values_ns: list[float]) -> tuple[str, float]:
    """Pick the best display unit for a list of ns values."""
    if not values_ns:
        return ("ns", 1.0)
    median = sorted(values_ns)[len(values_ns) // 2]
    if median < 1_000:
        return ("ns", 1.0)
    elif median < 1_000_000:
        return ("µs", 1_000.0)
    elif median < 1_000_000_000:
        return ("ms", 1_000_000.0)
    else:
        return ("s", 1_000_000_000.0)


# ─── Group benchmarks by prefix ─────────────────────────────────────────────

def group_benchmarks(bench_names: list[str]) -> dict[str, list[str]]:
    """Group benchmark names by their first path component."""
    groups: dict[str, list[str]] = {}
    for name in sorted(bench_names):
        parts = name.split("/")
        group = parts[0] if len(parts) > 1 else "ungrouped"
        if group not in groups:
            groups[group] = []
        groups[group].append(name)
    return groups


# ─── HTML Generation ─────────────────────────────────────────────────────────

def generate_timeseries_html(
    series: dict[str, list[dict]],
    history: list[dict],
) -> str:
    now = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC")
    n_runs = len(history)
    n_benchmarks = len(series)
    groups = group_benchmarks(list(series.keys()))

    # Compute stats for all benchmarks
    all_stats = {}
    for name, points in series.items():
        all_stats[name] = compute_stats(points)

    # Build per-benchmark chart data
    # Each benchmark gets its own canvas
    chart_configs = {}
    for name, points in sorted(series.items()):
        values_ns = [p["value_ns"] for p in points]
        unit_label, divisor = auto_unit(values_ns)
        stats = all_stats[name]

        data_points = [{
            "x": p["timestamp"],
            "y": round(p["value_ns"] / divisor, 4),
            "commit": p["commit"],
            "branch": p["branch"],
            "ci_lower": round(p["ci_lower"] / divisor, 4) if p.get("ci_lower") else None,
            "ci_upper": round(p["ci_upper"] / divisor, 4) if p.get("ci_upper") else None,
        } for p in points]

        # Trend line points (first and last x, computed y)
        n = len(points)
        if n >= 2:
            trend_start = (stats["trend_intercept"]) / divisor
            trend_end = (stats["trend_slope"] * (n - 1) + stats["trend_intercept"]) / divisor
            trend_data = [
                {"x": points[0]["timestamp"], "y": round(trend_start, 4)},
                {"x": points[-1]["timestamp"], "y": round(trend_end, 4)},
            ]
        else:
            trend_data = []

        # Mean line
        mean_val = round(stats["mean"] / divisor, 4)

        chart_configs[name] = {
            "data": data_points,
            "trend": trend_data,
            "mean": mean_val,
            "unit": unit_label,
            "stats": {
                "mean": format_ns(stats["mean"]),
                "std_dev": format_ns(stats["std_dev"]),
                "cv": f"{stats['cv_pct']:.1f}%",
                "min": format_ns(stats["min"]),
                "max": format_ns(stats["max"]),
                "min_commit": stats["min_commit"],
                "max_commit": stats["max_commit"],
                "latest": format_ns(stats["latest"]),
                "total_change": f"{stats['total_change_pct']:+.1f}%",
                "trend": f"{stats['trend_pct']:+.1f}%",
                "count": stats["count"],
            },
        }

    chart_configs_json = json.dumps(chart_configs)

    # Build the navigation sidebar items and main content
    nav_items = ""
    chart_sections = ""

    for group_name, bench_names in groups.items():
        nav_items += f'<div class="nav-group-label">{group_name}</div>\n'
        for name in bench_names:
            short = name.split("/")[-1] if "/" in name else name
            safe_id = name.replace("/", "--")
            stats = all_stats[name]
            trend_class = "trend-down" if stats["total_change_pct"] < -2 else (
                "trend-up" if stats["total_change_pct"] > 2 else "trend-flat"
            )
            trend_arrow = "↓" if stats["total_change_pct"] < -2 else (
                "↑" if stats["total_change_pct"] > 2 else "→"
            )
            nav_items += f'''<a class="nav-item" href="#bench-{safe_id}" data-bench="{safe_id}">
  <span class="nav-bench-name">{short}</span>
  <span class="nav-trend {trend_class}">{trend_arrow} {stats['total_change_pct']:+.1f}%</span>
</a>\n'''

            chart_sections += f'''
<section class="bench-section" id="bench-{safe_id}" data-bench-name="{name}">
  <div class="bench-header">
    <h2 class="bench-title">{name}</h2>
    <div class="bench-latest">{format_ns(stats['latest'])}</div>
  </div>
  <div class="bench-stats-bar">
    <div class="bench-stat">
      <span class="bench-stat-label">Mean</span>
      <span class="bench-stat-value">{format_ns(stats['mean'])}</span>
    </div>
    <div class="bench-stat">
      <span class="bench-stat-label">Std Dev</span>
      <span class="bench-stat-value">{format_ns(stats['std_dev'])}</span>
    </div>
    <div class="bench-stat">
      <span class="bench-stat-label">CV</span>
      <span class="bench-stat-value">{stats['cv_pct']:.1f}%</span>
    </div>
    <div class="bench-stat">
      <span class="bench-stat-label">Best</span>
      <span class="bench-stat-value best">{format_ns(stats['min'])} <small>({stats['min_commit'][:7]})</small></span>
    </div>
    <div class="bench-stat">
      <span class="bench-stat-label">Worst</span>
      <span class="bench-stat-value worst">{format_ns(stats['max'])} <small>({stats['max_commit'][:7]})</small></span>
    </div>
    <div class="bench-stat">
      <span class="bench-stat-label">Overall</span>
      <span class="bench-stat-value {trend_class}">{stats['total_change_pct']:+.1f}%</span>
    </div>
  </div>
  <div class="chart-wrap">
    <canvas id="chart-{safe_id}" height="260"></canvas>
  </div>
</section>'''

    # Overview: find biggest winners and losers
    sorted_by_change = sorted(all_stats.items(), key=lambda x: x[1].get("total_change_pct", 0))
    top_improved = [(n, s) for n, s in sorted_by_change if s.get("total_change_pct", 0) < -2][:5]
    top_regressed = [(n, s) for n, s in reversed(sorted_by_change) if s.get("total_change_pct", 0) > 2][:5]

    movers_html = ""
    if top_improved:
        movers_html += '<div class="movers-col"><h3 class="movers-label improved-label">⬇ Biggest Improvements</h3>'
        for name, stats in top_improved:
            movers_html += f'<div class="mover-item improved">{name} <span>{stats["total_change_pct"]:+.1f}%</span></div>'
        movers_html += '</div>'
    if top_regressed:
        movers_html += '<div class="movers-col"><h3 class="movers-label regressed-label">⬆ Biggest Regressions</h3>'
        for name, stats in top_regressed:
            movers_html += f'<div class="mover-item regressed">{name} <span>{stats["total_change_pct"]:+.1f}%</span></div>'
        movers_html += '</div>'

    html = f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Chronos — Timeseries Dashboard</title>
<script src="https://cdnjs.cloudflare.com/ajax/libs/Chart.js/4.4.1/chart.umd.min.js"></script>
<script src="https://cdnjs.cloudflare.com/ajax/libs/chartjs-adapter-date-fns/3.0.0/chartjs-adapter-date-fns.bundle.min.js"></script>
<style>
  @import url('https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@300;400;500;600;700&family=DM+Sans:ital,wght@0,400;0,500;0,600;0,700;1,400&display=swap');

  :root {{
    --bg-void:      #06080d;
    --bg-primary:   #0b0f19;
    --bg-secondary: #111827;
    --bg-card:      #141c2b;
    --bg-elevated:  #1a2336;
    --bg-hover:     #1e293b;
    --border:       #1e293b;
    --border-dim:   #162032;
    --border-focus: #334155;
    --text-primary: #e2e8f0;
    --text-secondary: #94a3b8;
    --text-muted:   #64748b;
    --text-dim:     #475569;
    --cyan:         #22d3ee;
    --cyan-dim:     rgba(34, 211, 238, 0.15);
    --green:        #34d399;
    --green-dim:    rgba(52, 211, 153, 0.1);
    --red:          #f87171;
    --red-dim:      rgba(248, 113, 113, 0.1);
    --yellow:       #fbbf24;
    --purple:       #a78bfa;
    --orange:       #fb923c;
    --sidebar-w:    260px;
  }}

  * {{ margin: 0; padding: 0; box-sizing: border-box; }}

  html {{ scroll-behavior: smooth; }}

  body {{
    font-family: 'DM Sans', sans-serif;
    background: var(--bg-void);
    color: var(--text-primary);
    min-height: 100vh;
    display: flex;
  }}

  /* ─── Sidebar ─── */
  .sidebar {{
    position: fixed;
    top: 0;
    left: 0;
    width: var(--sidebar-w);
    height: 100vh;
    background: var(--bg-primary);
    border-right: 1px solid var(--border);
    overflow-y: auto;
    z-index: 10;
    display: flex;
    flex-direction: column;
  }}

  .sidebar-header {{
    padding: 1.25rem 1rem;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }}

  .sidebar-logo {{
    font-family: 'JetBrains Mono', monospace;
    font-size: 1.1rem;
    font-weight: 700;
    color: var(--cyan);
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }}

  .sidebar-sub {{
    font-size: 0.7rem;
    color: var(--text-muted);
    margin-top: 0.25rem;
    font-family: 'JetBrains Mono', monospace;
  }}

  .sidebar-nav {{
    flex: 1;
    overflow-y: auto;
    padding: 0.75rem 0;
  }}

  .nav-group-label {{
    font-family: 'JetBrains Mono', monospace;
    font-size: 0.65rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.1em;
    color: var(--text-dim);
    padding: 0.75rem 1rem 0.3rem;
  }}

  .nav-item {{
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.45rem 1rem;
    text-decoration: none;
    color: var(--text-secondary);
    font-size: 0.8rem;
    border-left: 2px solid transparent;
    transition: all 0.15s;
  }}

  .nav-item:hover {{
    background: var(--bg-hover);
    color: var(--text-primary);
    border-left-color: var(--cyan);
  }}

  .nav-item.active {{
    background: var(--cyan-dim);
    color: var(--cyan);
    border-left-color: var(--cyan);
  }}

  .nav-bench-name {{
    font-family: 'JetBrains Mono', monospace;
    font-size: 0.75rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }}

  .nav-trend {{
    font-family: 'JetBrains Mono', monospace;
    font-size: 0.65rem;
    font-weight: 600;
    flex-shrink: 0;
    margin-left: 0.5rem;
  }}

  .trend-down {{ color: var(--green); }}
  .trend-up {{ color: var(--red); }}
  .trend-flat {{ color: var(--text-muted); }}

  /* ─── Main Content ─── */
  .main {{
    margin-left: var(--sidebar-w);
    flex: 1;
    padding: 2rem 2.5rem;
    max-width: calc(100vw - var(--sidebar-w));
  }}

  /* ─── Top Header ─── */
  .page-header {{
    margin-bottom: 2rem;
    padding-bottom: 1.5rem;
    border-bottom: 1px solid var(--border);
  }}

  .page-header h1 {{
    font-family: 'JetBrains Mono', monospace;
    font-size: 1.5rem;
    font-weight: 700;
    color: var(--text-primary);
    margin-bottom: 0.5rem;
  }}

  .page-meta {{
    display: flex;
    gap: 1.5rem;
    flex-wrap: wrap;
    font-family: 'JetBrains Mono', monospace;
    font-size: 0.75rem;
    color: var(--text-muted);
  }}

  .page-meta span {{ display: flex; align-items: center; gap: 0.35rem; }}
  .page-meta .label {{ color: var(--text-dim); }}

  /* ─── Overview Cards ─── */
  .overview-row {{
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
    gap: 1rem;
    margin-bottom: 2rem;
  }}

  .overview-card {{
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 1rem 1.25rem;
  }}

  .overview-value {{
    font-family: 'JetBrains Mono', monospace;
    font-size: 2rem;
    font-weight: 700;
    color: var(--cyan);
  }}

  .overview-label {{
    font-size: 0.7rem;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.08em;
    margin-top: 0.2rem;
  }}

  /* ─── Movers ─── */
  .movers {{
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 1.5rem;
    margin-bottom: 2.5rem;
  }}

  .movers-col {{
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 1.25rem;
  }}

  .movers-label {{
    font-family: 'JetBrains Mono', monospace;
    font-size: 0.75rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    margin-bottom: 0.75rem;
  }}

  .improved-label {{ color: var(--green); }}
  .regressed-label {{ color: var(--red); }}

  .mover-item {{
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.4rem 0;
    font-family: 'JetBrains Mono', monospace;
    font-size: 0.8rem;
    color: var(--text-secondary);
    border-bottom: 1px solid var(--border-dim);
  }}

  .mover-item:last-child {{ border-bottom: none; }}

  .mover-item span {{
    font-weight: 600;
    font-size: 0.75rem;
  }}

  .mover-item.improved span {{ color: var(--green); }}
  .mover-item.regressed span {{ color: var(--red); }}

  /* ─── Toggle Controls ─── */
  .controls-bar {{
    display: flex;
    align-items: center;
    gap: 1rem;
    margin-bottom: 1.5rem;
    flex-wrap: wrap;
  }}

  .toggle-btn {{
    font-family: 'JetBrains Mono', monospace;
    font-size: 0.7rem;
    padding: 0.35rem 0.75rem;
    border-radius: 4px;
    border: 1px solid var(--border);
    background: var(--bg-card);
    color: var(--text-muted);
    cursor: pointer;
    transition: all 0.15s;
  }}

  .toggle-btn:hover {{ border-color: var(--border-focus); color: var(--text-secondary); }}
  .toggle-btn.active {{ background: var(--cyan-dim); border-color: var(--cyan); color: var(--cyan); }}

  .controls-label {{
    font-family: 'JetBrains Mono', monospace;
    font-size: 0.65rem;
    color: var(--text-dim);
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }}

  /* ─── Benchmark Sections ─── */
  .bench-section {{
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 1.5rem;
    margin-bottom: 1.5rem;
    transition: border-color 0.2s;
  }}

  .bench-section:target {{
    border-color: var(--cyan);
    box-shadow: 0 0 20px rgba(34, 211, 238, 0.05);
  }}

  .bench-header {{
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    margin-bottom: 1rem;
  }}

  .bench-title {{
    font-family: 'JetBrains Mono', monospace;
    font-size: 1rem;
    font-weight: 600;
    color: var(--text-primary);
  }}

  .bench-latest {{
    font-family: 'JetBrains Mono', monospace;
    font-size: 1.1rem;
    font-weight: 700;
    color: var(--cyan);
  }}

  .bench-stats-bar {{
    display: flex;
    gap: 1.5rem;
    flex-wrap: wrap;
    margin-bottom: 1rem;
    padding-bottom: 1rem;
    border-bottom: 1px solid var(--border-dim);
  }}

  .bench-stat {{
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
  }}

  .bench-stat-label {{
    font-family: 'JetBrains Mono', monospace;
    font-size: 0.6rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--text-dim);
  }}

  .bench-stat-value {{
    font-family: 'JetBrains Mono', monospace;
    font-size: 0.8rem;
    font-weight: 500;
    color: var(--text-secondary);
  }}

  .bench-stat-value small {{
    color: var(--text-muted);
    font-size: 0.7rem;
  }}

  .bench-stat-value.best {{ color: var(--green); }}
  .bench-stat-value.worst {{ color: var(--red); }}

  .chart-wrap {{
    position: relative;
    height: 260px;
  }}

  /* ─── Footer ─── */
  footer {{
    margin-top: 3rem;
    padding-top: 1.5rem;
    border-top: 1px solid var(--border);
    text-align: center;
    font-family: 'JetBrains Mono', monospace;
    font-size: 0.7rem;
    color: var(--text-dim);
  }}

  /* ─── Responsive ─── */
  @media (max-width: 900px) {{
    .sidebar {{ display: none; }}
    .main {{ margin-left: 0; padding: 1rem; }}
    .movers {{ grid-template-columns: 1fr; }}
  }}
</style>
</head>
<body>

<!-- Sidebar Navigation -->
<nav class="sidebar">
  <div class="sidebar-header">
    <div class="sidebar-logo">⏱ CHRONOS</div>
    <div class="sidebar-sub">timeseries dashboard</div>
  </div>
  <div class="sidebar-nav">
    {nav_items}
  </div>
</nav>

<!-- Main Content -->
<main class="main">
  <div class="page-header">
    <h1>Performance Timeseries</h1>
    <div class="page-meta">
      <span><span class="label">runs</span> {n_runs}</span>
      <span><span class="label">benchmarks</span> {n_benchmarks}</span>
      <span><span class="label">generated</span> {now}</span>
    </div>
  </div>

  <div class="overview-row">
    <div class="overview-card">
      <div class="overview-value">{n_runs}</div>
      <div class="overview-label">Total Runs</div>
    </div>
    <div class="overview-card">
      <div class="overview-value">{n_benchmarks}</div>
      <div class="overview-label">Benchmarks</div>
    </div>
    <div class="overview-card">
      <div class="overview-value">{len(top_improved)}</div>
      <div class="overview-label">Trending Faster</div>
    </div>
    <div class="overview-card">
      <div class="overview-value">{len(top_regressed)}</div>
      <div class="overview-label">Trending Slower</div>
    </div>
  </div>

  <div class="movers">
    {movers_html}
  </div>

  <div class="controls-bar">
    <span class="controls-label">Overlays:</span>
    <button class="toggle-btn active" id="toggle-trend" onclick="toggleOverlay('trend')">Trend Line</button>
    <button class="toggle-btn active" id="toggle-mean" onclick="toggleOverlay('mean')">Mean</button>
    <button class="toggle-btn" id="toggle-ci" onclick="toggleOverlay('ci')">Confidence Interval</button>
  </div>

  {chart_sections}

  <footer>
    Chronos — The Temporal Benchkeeper &nbsp;·&nbsp; Timeseries Dashboard
  </footer>
</main>

<script>
const CONFIGS = {chart_configs_json};

const overlays = {{ trend: true, mean: true, ci: false }};
const charts = {{}};

const COLORS = {{
  main: '#22d3ee',
  mainFill: 'rgba(34, 211, 238, 0.08)',
  trend: '#a78bfa',
  mean: 'rgba(251, 191, 36, 0.5)',
  ci: 'rgba(52, 211, 153, 0.12)',
  ciBorder: 'rgba(52, 211, 153, 0.3)',
}};

function buildChart(benchName, canvasId) {{
  const cfg = CONFIGS[benchName];
  if (!cfg) return;

  const ctx = document.getElementById(canvasId);
  if (!ctx) return;

  const datasets = [
    {{
      label: benchName,
      data: cfg.data,
      borderColor: COLORS.main,
      backgroundColor: COLORS.mainFill,
      fill: true,
      tension: 0.3,
      pointRadius: 5,
      pointHoverRadius: 8,
      pointBackgroundColor: COLORS.main,
      pointBorderColor: '#0b0f19',
      pointBorderWidth: 2,
      borderWidth: 2,
      order: 1,
    }},
  ];

  // Trend line
  if (cfg.trend.length >= 2) {{
    datasets.push({{
      label: 'Trend',
      data: cfg.trend,
      borderColor: COLORS.trend,
      borderDash: [6, 4],
      borderWidth: 1.5,
      pointRadius: 0,
      fill: false,
      hidden: !overlays.trend,
      order: 2,
    }});
  }}

  // Mean line (annotation via dataset)
  if (cfg.data.length >= 2) {{
    datasets.push({{
      label: 'Mean',
      data: [
        {{ x: cfg.data[0].x, y: cfg.mean }},
        {{ x: cfg.data[cfg.data.length - 1].x, y: cfg.mean }},
      ],
      borderColor: COLORS.mean,
      borderDash: [3, 3],
      borderWidth: 1,
      pointRadius: 0,
      fill: false,
      hidden: !overlays.mean,
      order: 3,
    }});
  }}

  // Confidence interval bands
  if (cfg.data.some(d => d.ci_upper !== null)) {{
    datasets.push({{
      label: 'CI Upper',
      data: cfg.data.map(d => ({{ x: d.x, y: d.ci_upper || d.y }})),
      borderColor: COLORS.ciBorder,
      backgroundColor: COLORS.ci,
      borderWidth: 0.5,
      pointRadius: 0,
      fill: '+1',
      hidden: !overlays.ci,
      order: 4,
    }});
    datasets.push({{
      label: 'CI Lower',
      data: cfg.data.map(d => ({{ x: d.x, y: d.ci_lower || d.y }})),
      borderColor: COLORS.ciBorder,
      borderWidth: 0.5,
      pointRadius: 0,
      fill: false,
      hidden: !overlays.ci,
      order: 5,
    }});
  }}

  const chart = new Chart(ctx, {{
    type: 'line',
    data: {{ datasets }},
    options: {{
      responsive: true,
      maintainAspectRatio: false,
      interaction: {{ mode: 'index', intersect: false }},
      plugins: {{
        legend: {{
          display: true,
          position: 'bottom',
          labels: {{
            color: '#64748b',
            font: {{ family: "'JetBrains Mono', monospace", size: 10 }},
            boxWidth: 10,
            padding: 12,
            filter: (item) => !item.text.startsWith('CI'),
          }},
        }},
        tooltip: {{
          backgroundColor: '#1a2336',
          titleColor: '#e2e8f0',
          bodyColor: '#94a3b8',
          borderColor: '#334155',
          borderWidth: 1,
          titleFont: {{ family: "'JetBrains Mono', monospace", size: 12 }},
          bodyFont: {{ family: "'JetBrains Mono', monospace", size: 11 }},
          filter: (item) => item.datasetIndex === 0,
          callbacks: {{
            afterTitle: function(items) {{
              const pt = cfg.data[items[0].dataIndex];
              return pt ? 'commit: ' + pt.commit + ' (' + pt.branch + ')' : '';
            }},
            label: function(ctx) {{
              const pt = cfg.data[ctx.dataIndex];
              let s = ctx.parsed.y.toFixed(2) + ' ' + cfg.unit;
              if (pt && pt.ci_lower !== null) {{
                s += '  [' + pt.ci_lower.toFixed(2) + ' – ' + pt.ci_upper.toFixed(2) + ']';
              }}
              return s;
            }},
          }},
        }},
      }},
      scales: {{
        x: {{
          type: 'time',
          time: {{
            tooltipFormat: 'yyyy-MM-dd HH:mm',
            displayFormats: {{ hour: 'MMM d HH:mm', day: 'MMM d', week: 'MMM d' }},
          }},
          ticks: {{ color: '#475569', font: {{ family: "'JetBrains Mono', monospace", size: 10 }}, maxRotation: 45 }},
          grid: {{ color: 'rgba(30, 41, 59, 0.3)' }},
        }},
        y: {{
          beginAtZero: false,
          ticks: {{
            color: '#475569',
            font: {{ family: "'JetBrains Mono', monospace", size: 10 }},
            callback: (v) => v.toFixed(1) + ' ' + cfg.unit,
          }},
          grid: {{ color: 'rgba(30, 41, 59, 0.3)' }},
        }},
      }},
    }},
  }});

  charts[benchName] = chart;
}}

function toggleOverlay(name) {{
  overlays[name] = !overlays[name];
  const btn = document.getElementById('toggle-' + name);
  btn.classList.toggle('active', overlays[name]);

  // Map overlay name to dataset label patterns
  const labelMap = {{
    trend: ['Trend'],
    mean: ['Mean'],
    ci: ['CI Upper', 'CI Lower'],
  }};

  const labels = labelMap[name] || [];

  Object.values(charts).forEach(chart => {{
    chart.data.datasets.forEach((ds, i) => {{
      if (labels.includes(ds.label)) {{
        chart.setDatasetVisibility(i, overlays[name]);
      }}
    }});
    chart.update('none');
  }});
}}

// Initialize all charts
document.addEventListener('DOMContentLoaded', () => {{
  document.querySelectorAll('.bench-section').forEach(section => {{
    const name = section.dataset.benchName;
    const canvasId = section.querySelector('canvas').id;
    buildChart(name, canvasId);
  }});

  // Sidebar scroll-spy
  const observer = new IntersectionObserver((entries) => {{
    entries.forEach(entry => {{
      const id = entry.target.id;
      const link = document.querySelector(`.nav-item[href="#${{id}}"]`);
      if (link) {{
        link.classList.toggle('active', entry.isIntersecting);
      }}
    }});
  }}, {{ rootMargin: '-20% 0px -60% 0px' }});

  document.querySelectorAll('.bench-section').forEach(s => observer.observe(s));
}});
</script>

</body>
</html>"""

    return html


# ─── Main ────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        description="Chronos — Timeseries Report. Generate a dedicated timeseries dashboard from benchmark history.",
    )
    parser.add_argument("--history-file", default="benchmarks/history.json", help="Path to history JSON")
    parser.add_argument("--output", default="benchmarks/timeseries.html", help="Output HTML path")
    args = parser.parse_args()

    history = load_history(args.history_file)
    if not history:
        print("[chronos] No history entries found.", file=sys.stderr)
        sys.exit(1)

    series = build_series(history)
    html = generate_timeseries_html(series, history)

    out = Path(args.output)
    out.parent.mkdir(parents=True, exist_ok=True)
    with open(out, "w") as f:
        f.write(html)

    print(f"[chronos] Timeseries dashboard written to {args.output}")
    print(f"[chronos] {len(series)} benchmarks across {len(history)} runs")


if __name__ == "__main__":
    main()