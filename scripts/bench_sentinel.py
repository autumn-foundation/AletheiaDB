#!/usr/bin/env python3
"""
Chronos — Benchmark Sentinel
Parses Criterion benchmark results, maintains a JSON history ledger,
and generates an HTML report with tables, regression detection, and timeseries charts.
"""

import argparse
import json
import os
import sys
import glob
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

# ─── Criterion Parsing ───────────────────────────────────────────────────────

def parse_criterion_dir(criterion_dir: str) -> dict[str, dict[str, Any]]:
    """
    Walk target/criterion/ and extract point estimates from each benchmark.
    Returns { benchmark_name: { mean_ns, std_dev_ns, ... } }
    """
    results = {}
    base = Path(criterion_dir)

    if not base.exists():
        print(f"[chronos] WARNING: Criterion directory not found: {criterion_dir}", file=sys.stderr)
        return results

    # Criterion stores results in: target/criterion/<group>/<bench>/new/estimates.json
    # or sometimes: target/criterion/<bench>/new/estimates.json
    for estimates_path in base.rglob("new/estimates.json"):
        try:
            with open(estimates_path) as f:
                data = json.load(f)
        except (json.JSONDecodeError, OSError) as e:
            print(f"[chronos] WARNING: Could not read {estimates_path}: {e}", file=sys.stderr)
            continue

        # Derive benchmark name from path
        # e.g. target/criterion/insert_node/new/estimates.json -> "insert_node"
        # e.g. target/criterion/graph_ops/insert_node/new/estimates.json -> "graph_ops/insert_node"
        rel = estimates_path.relative_to(base)
        parts = list(rel.parts)
        # Remove "new" and "estimates.json"
        bench_parts = parts[:-2]
        bench_name = "/".join(bench_parts)

        # Extract the point estimate — prefer slope, fall back to mean
        if "slope" in data and data["slope"]:
            point = data["slope"]["point_estimate"]
            ci_lower = data["slope"]["confidence_interval"]["lower_bound"]
            ci_upper = data["slope"]["confidence_interval"]["upper_bound"]
        elif "mean" in data and data["mean"]:
            point = data["mean"]["point_estimate"]
            ci_lower = data["mean"]["confidence_interval"]["lower_bound"]
            ci_upper = data["mean"]["confidence_interval"]["upper_bound"]
        else:
            print(f"[chronos] WARNING: No slope/mean in {estimates_path}", file=sys.stderr)
            continue

        # Also grab std_dev if available
        std_dev = None
        if "std_dev" in data and data["std_dev"]:
            std_dev = data["std_dev"]["point_estimate"]

        results[bench_name] = {
            "point_estimate_ns": point,
            "ci_lower_ns": ci_lower,
            "ci_upper_ns": ci_upper,
            "std_dev_ns": std_dev,
        }

    return results


# ─── History Management ──────────────────────────────────────────────────────

def load_history(history_file: str) -> list[dict]:
    """Load the benchmark history ledger."""
    path = Path(history_file)
    if path.exists():
        with open(path) as f:
            return json.load(f)
    return []


def save_history(history: list[dict], history_file: str):
    """Save the benchmark history ledger."""
    path = Path(history_file)
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w") as f:
        json.dump(history, f, indent=2)
    print(f"[chronos] History saved: {history_file} ({len(history)} entries)")


def append_to_history(
    history: list[dict],
    benchmarks: dict[str, dict],
    commit: str,
    branch: str,
) -> dict:
    """Append a new entry to the history and return it."""
    entry = {
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "commit": commit,
        "branch": branch,
        "benchmarks": benchmarks,
    }
    history.append(entry)
    return entry


# ─── Comparison / Regression Detection ───────────────────────────────────────

def compare_to_previous(
    history: list[dict],
    current: dict[str, dict],
    threshold_improved: float = -0.02,  # 2% faster
    threshold_regressed: float = 0.02,  # 2% slower
    threshold_critical: float = 0.10,   # 10% slower
) -> list[dict]:
    """
    Compare current benchmarks to the most recent historical entry.
    Returns a list of comparison records.
    """
    comparisons = []

    # Find the previous entry (second to last, since current was just appended)
    prev_entry = None
    for entry in reversed(history[:-1]):
        if entry.get("benchmarks"):
            prev_entry = entry
            break

    for bench_name, current_data in sorted(current.items()):
        comp = {
            "name": bench_name,
            "current_ns": current_data["point_estimate_ns"],
            "current_ci_lower": current_data["ci_lower_ns"],
            "current_ci_upper": current_data["ci_upper_ns"],
            "previous_ns": None,
            "change_pct": None,
            "status": "new",
        }

        if prev_entry and bench_name in prev_entry.get("benchmarks", {}):
            prev_ns = prev_entry["benchmarks"][bench_name]["point_estimate_ns"]
            comp["previous_ns"] = prev_ns

            if prev_ns > 0:
                change = (comp["current_ns"] - prev_ns) / prev_ns
                comp["change_pct"] = change

                if change <= threshold_improved:
                    comp["status"] = "improved"
                elif change >= threshold_critical:
                    comp["status"] = "critical_regression"
                elif change >= threshold_regressed:
                    comp["status"] = "regressed"
                else:
                    comp["status"] = "stable"

        comparisons.append(comp)

    return comparisons


# ─── Formatting Helpers ──────────────────────────────────────────────────────

def format_duration(ns: float | None) -> str:
    """Format nanoseconds into a human-readable string."""
    if ns is None:
        return "—"
    if ns < 1_000:
        return f"{ns:.1f} ns"
    elif ns < 1_000_000:
        return f"{ns / 1_000:.2f} µs"
    elif ns < 1_000_000_000:
        return f"{ns / 1_000_000:.2f} ms"
    else:
        return f"{ns / 1_000_000_000:.3f} s"


def format_change(pct: float | None) -> str:
    """Format a percentage change."""
    if pct is None:
        return "NEW"
    sign = "+" if pct >= 0 else ""
    return f"{sign}{pct * 100:.1f}%"


# ─── HTML Report Generation ─────────────────────────────────────────────────

def build_timeseries_data(history: list[dict]) -> dict[str, list[dict]]:
    """
    Build timeseries data per benchmark.
    Returns { bench_name: [ { timestamp, commit, value_ns }, ... ] }
    """
    series = {}
    for entry in history:
        ts = entry["timestamp"]
        commit = entry.get("commit", "?")
        for bench_name, bench_data in entry.get("benchmarks", {}).items():
            if bench_name not in series:
                series[bench_name] = []
            series[bench_name].append({
                "timestamp": ts,
                "commit": commit,
                "value_ns": bench_data["point_estimate_ns"],
            })
    return series


def generate_html_report(
    comparisons: list[dict],
    history: list[dict],
    commit: str,
    branch: str,
) -> str:
    """Generate a full HTML benchmark report."""

    timeseries = build_timeseries_data(history)

    # Count statuses
    n_improved = sum(1 for c in comparisons if c["status"] == "improved")
    n_regressed = sum(1 for c in comparisons if c["status"] in ("regressed", "critical_regression"))
    n_stable = sum(1 for c in comparisons if c["status"] == "stable")
    n_new = sum(1 for c in comparisons if c["status"] == "new")
    total = len(comparisons)

    # Overall verdict
    has_critical = any(c["status"] == "critical_regression" for c in comparisons)
    if has_critical:
        verdict_class = "verdict-critical"
        verdict_text = "🚨 CRITICAL REGRESSIONS DETECTED"
        verdict_sub = "Merge should be blocked until these are addressed."
    elif n_regressed > 0:
        verdict_class = "verdict-warn"
        verdict_text = "⚠️ Regressions Detected"
        verdict_sub = f"{n_regressed} benchmark{'s' if n_regressed != 1 else ''} regressed."
    elif n_improved > 0:
        verdict_class = "verdict-good"
        verdict_text = "✅ Performance Improved"
        verdict_sub = f"{n_improved} benchmark{'s' if n_improved != 1 else ''} got faster."
    else:
        verdict_class = "verdict-neutral"
        verdict_text = "➖ No Significant Changes"
        verdict_sub = "All benchmarks within noise threshold."

    # Build table rows
    status_icon = {
        "improved": "🟢",
        "stable": "🟡",
        "regressed": "🔴",
        "critical_regression": "🚨",
        "new": "🆕",
    }
    status_class = {
        "improved": "row-improved",
        "stable": "row-stable",
        "regressed": "row-regressed",
        "critical_regression": "row-critical",
        "new": "row-new",
    }

    table_rows = ""
    for comp in comparisons:
        table_rows += f"""
        <tr class="{status_class.get(comp['status'], '')}">
            <td class="bench-name">{comp['name']}</td>
            <td class="numeric">{format_duration(comp['previous_ns'])}</td>
            <td class="numeric">{format_duration(comp['current_ns'])}</td>
            <td class="change">{format_change(comp['change_pct'])}</td>
            <td class="status-cell">{status_icon.get(comp['status'], '?')}</td>
        </tr>"""

    # Build timeseries JSON for Chart.js
    # We'll pick distinct colors per series
    palette = [
        "#22d3ee", "#a78bfa", "#f472b6", "#34d399", "#fbbf24",
        "#f87171", "#60a5fa", "#c084fc", "#fb923c", "#4ade80",
        "#e879f9", "#38bdf8", "#a3e635", "#f43f5e", "#14b8a6",
    ]

    chart_datasets = []
    bench_names_sorted = sorted(timeseries.keys())
    for i, bench_name in enumerate(bench_names_sorted):
        points = timeseries[bench_name]
        color = palette[i % len(palette)]
        # Normalize to µs for readability
        data_points = []
        for pt in points:
            data_points.append({
                "x": pt["timestamp"],
                "y": round(pt["value_ns"] / 1_000, 3),  # to µs
                "commit": pt["commit"],
            })
        chart_datasets.append({
            "label": bench_name,
            "data": data_points,
            "borderColor": color,
            "backgroundColor": color + "33",
            "fill": False,
            "tension": 0.3,
            "pointRadius": 4,
            "pointHoverRadius": 7,
        })

    chart_data_json = json.dumps(chart_datasets)
    now = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC")

    html = f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Chronos — Benchmark Report</title>
<script src="https://cdnjs.cloudflare.com/ajax/libs/Chart.js/4.4.1/chart.umd.min.js"></script>
<script src="https://cdnjs.cloudflare.com/ajax/libs/chartjs-adapter-date-fns/3.0.0/chartjs-adapter-date-fns.bundle.min.js"></script>
<style>
  @import url('https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@300;400;500;600;700&family=DM+Sans:wght@400;500;600;700&display=swap');

  :root {{
    --bg-primary: #0a0e17;
    --bg-secondary: #111827;
    --bg-tertiary: #1a2235;
    --bg-card: #151d2e;
    --border: #1e293b;
    --border-bright: #334155;
    --text-primary: #e2e8f0;
    --text-secondary: #94a3b8;
    --text-muted: #64748b;
    --accent-cyan: #22d3ee;
    --accent-green: #34d399;
    --accent-red: #f87171;
    --accent-yellow: #fbbf24;
    --accent-purple: #a78bfa;
    --accent-orange: #fb923c;
  }}

  * {{ margin: 0; padding: 0; box-sizing: border-box; }}

  body {{
    font-family: 'DM Sans', sans-serif;
    background: var(--bg-primary);
    color: var(--text-primary);
    min-height: 100vh;
    line-height: 1.6;
  }}

  .container {{
    max-width: 1200px;
    margin: 0 auto;
    padding: 2rem;
  }}

  /* ─── Header ─── */
  header {{
    border-bottom: 1px solid var(--border);
    padding-bottom: 2rem;
    margin-bottom: 2rem;
  }}

  .header-top {{
    display: flex;
    align-items: baseline;
    gap: 1rem;
    margin-bottom: 0.5rem;
  }}

  h1 {{
    font-family: 'JetBrains Mono', monospace;
    font-size: 1.75rem;
    font-weight: 700;
    color: var(--accent-cyan);
    letter-spacing: -0.02em;
  }}

  .header-icon {{
    font-size: 1.5rem;
  }}

  .meta {{
    font-family: 'JetBrains Mono', monospace;
    font-size: 0.8rem;
    color: var(--text-muted);
    display: flex;
    gap: 2rem;
    flex-wrap: wrap;
  }}

  .meta span {{
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }}

  .meta-label {{
    color: var(--text-secondary);
  }}

  /* ─── Verdict Banner ─── */
  .verdict {{
    border-radius: 8px;
    padding: 1.25rem 1.5rem;
    margin-bottom: 2rem;
    border: 1px solid;
  }}

  .verdict-critical {{
    background: rgba(248, 113, 113, 0.08);
    border-color: rgba(248, 113, 113, 0.3);
  }}
  .verdict-warn {{
    background: rgba(251, 191, 36, 0.08);
    border-color: rgba(251, 191, 36, 0.3);
  }}
  .verdict-good {{
    background: rgba(52, 211, 153, 0.08);
    border-color: rgba(52, 211, 153, 0.3);
  }}
  .verdict-neutral {{
    background: rgba(148, 163, 184, 0.06);
    border-color: rgba(148, 163, 184, 0.2);
  }}

  .verdict h2 {{
    font-size: 1.2rem;
    font-weight: 600;
    margin-bottom: 0.25rem;
  }}

  .verdict p {{
    color: var(--text-secondary);
    font-size: 0.9rem;
  }}

  /* ─── Stats Row ─── */
  .stats-row {{
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
    gap: 1rem;
    margin-bottom: 2rem;
  }}

  .stat-card {{
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 1rem;
    text-align: center;
  }}

  .stat-value {{
    font-family: 'JetBrains Mono', monospace;
    font-size: 1.75rem;
    font-weight: 700;
  }}

  .stat-label {{
    font-size: 0.75rem;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.08em;
    margin-top: 0.25rem;
  }}

  .stat-improved .stat-value {{ color: var(--accent-green); }}
  .stat-regressed .stat-value {{ color: var(--accent-red); }}
  .stat-stable .stat-value {{ color: var(--accent-yellow); }}
  .stat-new .stat-value {{ color: var(--accent-purple); }}
  .stat-total .stat-value {{ color: var(--accent-cyan); }}

  /* ─── Section Headers ─── */
  .section-header {{
    font-family: 'JetBrains Mono', monospace;
    font-size: 1rem;
    font-weight: 600;
    color: var(--text-secondary);
    margin-bottom: 1rem;
    padding-bottom: 0.5rem;
    border-bottom: 1px solid var(--border);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }}

  /* ─── Table ─── */
  .table-wrapper {{
    overflow-x: auto;
    margin-bottom: 3rem;
    border-radius: 8px;
    border: 1px solid var(--border);
  }}

  table {{
    width: 100%;
    border-collapse: collapse;
    font-size: 0.9rem;
  }}

  thead th {{
    font-family: 'JetBrains Mono', monospace;
    font-size: 0.7rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--text-muted);
    padding: 0.75rem 1rem;
    text-align: left;
    background: var(--bg-tertiary);
    border-bottom: 1px solid var(--border-bright);
    position: sticky;
    top: 0;
  }}

  thead th.numeric,
  thead th.change,
  thead th.status-cell {{
    text-align: right;
  }}

  td {{
    padding: 0.65rem 1rem;
    border-bottom: 1px solid var(--border);
    transition: background 0.15s;
  }}

  tr:hover td {{
    background: rgba(255, 255, 255, 0.02);
  }}

  .bench-name {{
    font-family: 'JetBrains Mono', monospace;
    font-weight: 500;
    color: var(--text-primary);
    font-size: 0.85rem;
  }}

  .numeric {{
    font-family: 'JetBrains Mono', monospace;
    text-align: right;
    color: var(--text-secondary);
    font-size: 0.85rem;
  }}

  .change {{
    font-family: 'JetBrains Mono', monospace;
    text-align: right;
    font-weight: 600;
    font-size: 0.85rem;
  }}

  .status-cell {{
    text-align: right;
    font-size: 1rem;
  }}

  .row-improved .change {{ color: var(--accent-green); }}
  .row-regressed .change {{ color: var(--accent-red); }}
  .row-critical .change {{ color: var(--accent-red); font-weight: 700; }}
  .row-stable .change {{ color: var(--text-muted); }}
  .row-new .change {{ color: var(--accent-purple); }}

  .row-critical {{
    background: rgba(248, 113, 113, 0.05);
  }}
  .row-improved {{
    background: rgba(52, 211, 153, 0.03);
  }}

  /* ─── Chart ─── */
  .chart-container {{
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 1.5rem;
    margin-bottom: 2rem;
  }}

  .chart-canvas-wrap {{
    position: relative;
    height: 400px;
  }}

  /* ─── Footer ─── */
  footer {{
    margin-top: 3rem;
    padding-top: 1.5rem;
    border-top: 1px solid var(--border);
    text-align: center;
    color: var(--text-muted);
    font-size: 0.75rem;
    font-family: 'JetBrains Mono', monospace;
  }}

  footer a {{
    color: var(--accent-cyan);
    text-decoration: none;
  }}

  /* ─── Responsive ─── */
  @media (max-width: 640px) {{
    .container {{ padding: 1rem; }}
    h1 {{ font-size: 1.25rem; }}
    .meta {{ flex-direction: column; gap: 0.5rem; }}
    .chart-canvas-wrap {{ height: 280px; }}
  }}
</style>
</head>
<body>

<div class="container">
  <header>
    <div class="header-top">
      <span class="header-icon">⏱</span>
      <h1>CHRONOS</h1>
    </div>
    <div class="meta">
      <span><span class="meta-label">commit</span> {commit}</span>
      <span><span class="meta-label">branch</span> {branch}</span>
      <span><span class="meta-label">generated</span> {now}</span>
      <span><span class="meta-label">history</span> {len(history)} runs</span>
    </div>
  </header>

  <div class="verdict {verdict_class}">
    <h2>{verdict_text}</h2>
    <p>{verdict_sub}</p>
  </div>

  <div class="stats-row">
    <div class="stat-card stat-total">
      <div class="stat-value">{total}</div>
      <div class="stat-label">Total</div>
    </div>
    <div class="stat-card stat-improved">
      <div class="stat-value">{n_improved}</div>
      <div class="stat-label">Improved</div>
    </div>
    <div class="stat-card stat-stable">
      <div class="stat-value">{n_stable}</div>
      <div class="stat-label">Stable</div>
    </div>
    <div class="stat-card stat-regressed">
      <div class="stat-value">{n_regressed}</div>
      <div class="stat-label">Regressed</div>
    </div>
    <div class="stat-card stat-new">
      <div class="stat-value">{n_new}</div>
      <div class="stat-label">New</div>
    </div>
  </div>

  <div class="section-header">Benchmark Results</div>
  <div class="table-wrapper">
    <table>
      <thead>
        <tr>
          <th>Benchmark</th>
          <th class="numeric">Previous</th>
          <th class="numeric">Current</th>
          <th class="change">Change</th>
          <th class="status-cell">Status</th>
        </tr>
      </thead>
      <tbody>
        {table_rows}
      </tbody>
    </table>
  </div>

  <div class="section-header">Performance Over Time (µs)</div>
  <div class="chart-container">
    <div class="chart-canvas-wrap">
      <canvas id="timeseriesChart"></canvas>
    </div>
  </div>

  <footer>
    Generated by <strong>Chronos</strong> — The Temporal Benchkeeper &nbsp;|&nbsp; Powered by Criterion
  </footer>
</div>

<script>
  const datasets = {chart_data_json};

  const ctx = document.getElementById('timeseriesChart').getContext('2d');
  new Chart(ctx, {{
    type: 'line',
    data: {{ datasets }},
    options: {{
      responsive: true,
      maintainAspectRatio: false,
      interaction: {{
        mode: 'index',
        intersect: false,
      }},
      plugins: {{
        legend: {{
          position: 'bottom',
          labels: {{
            color: '#94a3b8',
            font: {{ family: "'JetBrains Mono', monospace", size: 11 }},
            boxWidth: 12,
            padding: 16,
          }},
        }},
        tooltip: {{
          backgroundColor: '#1a2235',
          titleColor: '#e2e8f0',
          bodyColor: '#94a3b8',
          borderColor: '#334155',
          borderWidth: 1,
          titleFont: {{ family: "'JetBrains Mono', monospace" }},
          bodyFont: {{ family: "'JetBrains Mono', monospace", size: 12 }},
          callbacks: {{
            afterTitle: function(items) {{
              const pt = items[0];
              const ds = datasets[pt.datasetIndex];
              if (ds && ds.data[pt.dataIndex]) {{
                return 'commit: ' + ds.data[pt.dataIndex].commit;
              }}
              return '';
            }},
            label: function(ctx) {{
              return ctx.dataset.label + ': ' + ctx.parsed.y.toFixed(2) + ' µs';
            }},
          }},
        }},
      }},
      scales: {{
        x: {{
          type: 'time',
          time: {{
            tooltipFormat: 'yyyy-MM-dd HH:mm',
            displayFormats: {{
              hour: 'MMM d HH:mm',
              day: 'MMM d',
              week: 'MMM d',
              month: 'MMM yyyy',
            }},
          }},
          ticks: {{
            color: '#64748b',
            font: {{ family: "'JetBrains Mono', monospace", size: 10 }},
            maxRotation: 45,
          }},
          grid: {{
            color: 'rgba(30, 41, 59, 0.5)',
          }},
        }},
        y: {{
          beginAtZero: false,
          ticks: {{
            color: '#64748b',
            font: {{ family: "'JetBrains Mono', monospace", size: 10 }},
            callback: function(val) {{ return val.toFixed(1) + ' µs'; }},
          }},
          grid: {{
            color: 'rgba(30, 41, 59, 0.5)',
          }},
        }},
      }},
    }},
  }});
</script>

</body>
</html>"""

    return html


# ─── Main ────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        description="Chronos — Benchmark Sentinel. Parse Criterion results and generate HTML reports.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--criterion-dir",
        default="target/criterion",
        help="Path to Criterion output directory (default: target/criterion)",
    )
    parser.add_argument(
        "--history-file",
        default="benchmarks/history.json",
        help="Path to the JSON history ledger (default: benchmarks/history.json)",
    )
    parser.add_argument(
        "--output",
        default="benchmarks/report.html",
        help="Path for the generated HTML report (default: benchmarks/report.html)",
    )
    parser.add_argument(
        "--commit",
        default="unknown",
        help="Git commit hash for this run",
    )
    parser.add_argument(
        "--branch",
        default="unknown",
        help="Git branch name",
    )
    parser.add_argument(
        "--threshold-improved",
        type=float,
        default=0.02,
        help="Improvement threshold as fraction (default: 0.02 = 2%%)",
    )
    parser.add_argument(
        "--threshold-regressed",
        type=float,
        default=0.02,
        help="Regression threshold as fraction (default: 0.02 = 2%%)",
    )
    parser.add_argument(
        "--threshold-critical",
        type=float,
        default=0.10,
        help="Critical regression threshold as fraction (default: 0.10 = 10%%)",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Parse and report but don't save history",
    )
    parser.add_argument(
        "--no-block",
        action="store_true",
        help="Do not exit with error code on regressions (useful for nightly runs)",
    )

    args = parser.parse_args()

    # 1. Parse Criterion results
    print(f"[chronos] Parsing benchmarks from {args.criterion_dir}...")
    benchmarks = parse_criterion_dir(args.criterion_dir)

    if not benchmarks:
        print("[chronos] ERROR: No benchmark results found.", file=sys.stderr)
        sys.exit(1)

    print(f"[chronos] Found {len(benchmarks)} benchmarks:")
    for name in sorted(benchmarks.keys()):
        ns = benchmarks[name]["point_estimate_ns"]
        print(f"  • {name}: {format_duration(ns)}")

    # 2. Load and update history
    history = load_history(args.history_file)
    entry = append_to_history(history, benchmarks, args.commit, args.branch)

    if not args.dry_run:
        save_history(history, args.history_file)

    # 3. Compare to previous
    comparisons = compare_to_previous(
        history,
        benchmarks,
        threshold_improved=-args.threshold_improved,
        threshold_regressed=args.threshold_regressed,
        threshold_critical=args.threshold_critical,
    )

    # 4. Print summary to stdout
    print(f"\n[chronos] ═══ Results for {args.commit} on {args.branch} ═══")
    for comp in comparisons:
        icon = {"improved": "🟢", "stable": "🟡", "regressed": "🔴",
                "critical_regression": "🚨", "new": "🆕"}.get(comp["status"], "?")
        print(f"  {icon} {comp['name']}: {format_duration(comp['current_ns'])} "
              f"({format_change(comp['change_pct'])})")

    # 5. Generate HTML report
    html = generate_html_report(comparisons, history, args.commit, args.branch)
    output_path = Path(args.output)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    with open(output_path, "w") as f:
        f.write(html)
    print(f"\n[chronos] HTML report written to {args.output}")

    # 6. Exit with appropriate code
    has_critical = any(c["status"] == "critical_regression" for c in comparisons)
    has_regression = any(c["status"] in ("regressed", "critical_regression") for c in comparisons)

    if has_critical:
        print("\n[chronos] 🚨 CRITICAL REGRESSIONS — recommend blocking merge.")
        sys.exit(0 if args.no_block else 2)
    elif has_regression:
        print("\n[chronos] ⚠️  Regressions detected — review recommended.")
        sys.exit(0 if args.no_block else 1)
    else:
        print("\n[chronos] ✅ All clear.")
        sys.exit(0)


if __name__ == "__main__":
    main()