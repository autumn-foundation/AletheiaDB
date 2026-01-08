#!/usr/bin/env python3
"""
Generate HTML tables for GallifreyDB benchmark results.

This script processes Criterion benchmark results and generates:
1. Individual HTML tables for each benchmark suite
2. A comprehensive index page with all results
3. GitHub Pages-compatible output

Usage:
    python scripts/generate_benchmark_tables.py [--input target/criterion] [--output benchmark-results]
"""

import argparse
import json
import os
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Optional
from collections import defaultdict


@dataclass
class BenchmarkResult:
    """Represents a single benchmark result."""
    name: str
    mean: float
    mean_ns: float  # Raw mean in nanoseconds for comparison
    std_dev: float
    median: float
    unit: str
    throughput: Optional[str] = None


def parse_criterion_estimates(estimates_path: Path) -> Optional[BenchmarkResult]:
    """Parse a Criterion estimates.json file."""
    try:
        with open(estimates_path, 'r') as f:
            data = json.load(f)

        # Extract mean and std dev (in nanoseconds by default)
        mean_ns = data.get('mean', {}).get('point_estimate', 0)
        std_dev_ns = data.get('std_dev', {}).get('point_estimate', 0)
        median_ns = data.get('median', {}).get('point_estimate', 0)

        # Convert to appropriate unit
        mean, unit = format_time(mean_ns)
        std_dev, _ = format_time(std_dev_ns)
        median, _ = format_time(median_ns)

        # Get benchmark name from parent directory
        bench_name = estimates_path.parent.name

        return BenchmarkResult(
            name=bench_name,
            mean=mean,
            mean_ns=mean_ns,
            std_dev=std_dev,
            median=median,
            unit=unit
        )
    except (FileNotFoundError, json.JSONDecodeError, KeyError) as e:
        print(f"Warning: Could not parse {estimates_path}: {e}", file=sys.stderr)
        return None


def format_time(ns: float) -> tuple[float, str]:
    """Convert nanoseconds to appropriate unit."""
    if ns < 1000:
        return ns, "ns"
    elif ns < 1_000_000:
        return ns / 1000, "µs"
    elif ns < 1_000_000_000:
        return ns / 1_000_000, "ms"
    else:
        return ns / 1_000_000_000, "s"


def collect_benchmark_results(criterion_dir: Path) -> dict[str, list[BenchmarkResult]]:
    """Collect all benchmark results from Criterion output directory."""
    results = defaultdict(list)

    # Walk through criterion directory structure
    for root, dirs, files in os.walk(criterion_dir):
        if 'estimates.json' in files:
            estimates_path = Path(root) / 'estimates.json'
            result = parse_criterion_estimates(estimates_path)

            if result:
                # Determine which benchmark suite this belongs to
                # Criterion structure: target/criterion/<suite>/<benchmark>/estimates.json
                parts = Path(root).relative_to(criterion_dir).parts
                if len(parts) >= 1:
                    suite = parts[0]
                    results[suite].append(result)

    return dict(results)


def generate_html_table(suite_name: str, results: list[BenchmarkResult]) -> str:
    """Generate an HTML table for a benchmark suite."""
    # Sort results by name
    results.sort(key=lambda r: r.name)

    html = f"""
<div class="benchmark-suite">
    <h2>{suite_name}</h2>
    <table class="benchmark-table">
        <thead>
            <tr>
                <th>Benchmark</th>
                <th>Mean</th>
                <th>Std Dev</th>
                <th>Median</th>
            </tr>
        </thead>
        <tbody>
"""

    for result in results:
        html += f"""
            <tr>
                <td><code>{result.name}</code></td>
                <td>{result.mean:.2f} {result.unit}</td>
                <td>± {result.std_dev:.2f} {result.unit}</td>
                <td>{result.median:.2f} {result.unit}</td>
            </tr>
"""

    html += """
        </tbody>
    </table>
</div>
"""
    return html


def load_performance_targets() -> list[dict]:
    """Load performance targets from JSON file."""
    targets_path = Path(__file__).parent.parent / "benchmarks" / "performance-targets.json"
    try:
        with open(targets_path, 'r') as f:
            data = json.load(f)
            return data.get('targets', [])
    except (FileNotFoundError, json.JSONDecodeError) as e:
        print(f"Warning: Could not load performance targets: {e}", file=sys.stderr)
        return []


def generate_index_page(all_results: dict[str, list[BenchmarkResult]], output_dir: Path) -> None:
    """Generate the main index page with all benchmark results."""

    # Load performance targets
    targets = load_performance_targets()
    targets_html = ""
    if targets:
        targets_html = "<ul>\n"
        for target in targets:
            metric = target.get('metric', '')
            goal = target.get('target', '')
            targets_html += f"                <li>{metric}: {goal}</li>\n"
        targets_html += "            </ul>"
    else:
        targets_html = "<p>Performance targets not available</p>"

    html = """<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>GallifreyDB Benchmark Results</title>
    <style>
        * {
            margin: 0;
            padding: 0;
            box-sizing: border-box;
        }

        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif;
            line-height: 1.6;
            color: #333;
            background: #f5f5f5;
            padding: 20px;
        }

        .container {
            max-width: 1200px;
            margin: 0 auto;
            background: white;
            padding: 40px;
            border-radius: 8px;
            box-shadow: 0 2px 4px rgba(0,0,0,0.1);
        }

        h1 {
            color: #2c3e50;
            margin-bottom: 10px;
            font-size: 2.5em;
        }

        .subtitle {
            color: #7f8c8d;
            margin-bottom: 40px;
            font-size: 1.1em;
        }

        .benchmark-suite {
            margin-bottom: 50px;
        }

        h2 {
            color: #34495e;
            margin-bottom: 20px;
            padding-bottom: 10px;
            border-bottom: 2px solid #3498db;
            font-size: 1.8em;
        }

        .benchmark-table {
            width: 100%;
            border-collapse: collapse;
            margin-bottom: 20px;
            background: white;
        }

        .benchmark-table th {
            background: #3498db;
            color: white;
            padding: 12px;
            text-align: left;
            font-weight: 600;
        }

        .benchmark-table td {
            padding: 10px 12px;
            border-bottom: 1px solid #ecf0f1;
        }

        .benchmark-table tbody tr:hover {
            background: #f8f9fa;
        }

        .benchmark-table code {
            background: #ecf0f1;
            padding: 2px 6px;
            border-radius: 3px;
            font-family: 'Monaco', 'Menlo', 'Consolas', monospace;
            font-size: 0.9em;
        }

        .footer {
            margin-top: 60px;
            padding-top: 20px;
            border-top: 1px solid #ecf0f1;
            color: #7f8c8d;
            text-align: center;
            font-size: 0.9em;
        }

        .performance-target {
            background: #e8f5e9;
            border-left: 4px solid #4caf50;
            padding: 15px;
            margin-bottom: 30px;
            border-radius: 4px;
        }

        .performance-target h3 {
            color: #2e7d32;
            margin-bottom: 10px;
        }

        .performance-target ul {
            margin-left: 20px;
        }

        .performance-target li {
            margin: 5px 0;
        }
    </style>
</head>
<body>
    <div class="container">
        <h1>GallifreyDB Benchmark Results</h1>
        <p class="subtitle">Performance metrics for bi-temporal graph database operations</p>

        <div class="performance-target">
            <h3>Performance Targets</h3>
            """ + targets_html + """
        </div>
"""

    # Add each benchmark suite
    for suite_name in sorted(all_results.keys()):
        results = all_results[suite_name]
        html += generate_html_table(suite_name, results)

    html += """
        <div class="footer">
            <p>Generated by GallifreyDB benchmark suite using Criterion.rs</p>
            <p>View detailed reports in the <a href="report/index.html">Criterion report</a></p>
        </div>
    </div>
</body>
</html>
"""

    # Write index page
    index_path = output_dir / "index.html"
    with open(index_path, 'w') as f:
        f.write(html)

    print(f"Generated index page: {index_path}")


    print(f"Generated index page: {index_path}")


def parse_history_data(history_path: Path) -> dict[str, float]:
    """Parse historical benchmark data from data.js."""
    try:
        with open(history_path, 'r', encoding='utf-8') as f:
            content = f.read()

        # data.js typically starts with "window.BENCHMARK_DATA = "
        prefix = "window.BENCHMARK_DATA = "
        if content.startswith(prefix):
            json_str = content[len(prefix):]
            data = json.loads(json_str)
            
            latest_values = {}
            for bench_name, entries in data.get('entries', {}).items():
                if entries:
                    # Get the most recent entry
                    last_entry = entries[-1]
                    # Check if 'value' exists (github-action-benchmark format)
                    if 'value' in last_entry:
                         latest_values[bench_name] = float(last_entry['value'])
            
            return latest_values
    except Exception as e:
        print(f"Warning: Failed to parse historical data: {e}", file=sys.stderr)
    
    return {}


def generate_pr_comment(all_results: dict[str, list[BenchmarkResult]], output_path: Path, history: dict[str, float]) -> None:
    """Generate a markdown summary for PR comments."""
    
    regressions = []
    improvements = []
    
    # Flatten results
    current_results = []
    for suite_name, results in all_results.items():
        current_results.extend(results)
    
    # Compare with history
    threshold = 0.10 # 10%
    
    for bench in current_results:
        if bench.name in history:
            old_val_ns = history[bench.name]
            new_val_ns = bench.mean_ns
            
            if old_val_ns > 0:
                diff_percent = (new_val_ns - old_val_ns) / old_val_ns
                
                # Slower = Regression (positive diff)
                if diff_percent > threshold:
                    regressions.append((bench, diff_percent))
                # Faster = Improvement (negative diff)
                elif diff_percent < -threshold:
                    improvements.append((bench, diff_percent))

    md = """## 🚀 Benchmark Results
    
Benchmarks have been run for this PR.
"""

    if regressions:
        md += "\n### ⚠️ Regressions (>10% Slower)\n\n"
        md += "| Benchmark | Current | Previous | Change |\n"
        md += "|-----------|---------|----------|--------|\n"
        for bench, diff in regressions:
            old_val_fmt, _ = format_time(history[bench.name])
            md += f"| {bench.name} | {bench.mean:.2f} {bench.unit} | {old_val_fmt:.2f} {bench.unit} | 🔴 +{diff:.1%} |\n"
            
    if improvements:
        md += "\n### ✅ Improvements (>10% Faster)\n\n"
        md += "| Benchmark | Current | Previous | Change |\n"
        md += "|-----------|---------|----------|--------|\n"
        for bench, diff in improvements:
            old_val_fmt, _ = format_time(history[bench.name])
            md += f"| {bench.name} | {bench.mean:.2f} {bench.unit} | {old_val_fmt:.2f} {bench.unit} | 🟢 {diff:.1%} |\n"

    # Get top benchmarks from each suite
    top_benchmarks = []
    for suite_name, results in all_results.items():
        # Sort by mean time and take top 3
        sorted_results = sorted(results, key=lambda r: r.mean)[:3]
        top_benchmarks.extend(sorted_results)

    # Sort all top benchmarks and take top 10 overall
    top_benchmarks = sorted(top_benchmarks, key=lambda r: r.mean)[:10]

    md += """
### 📊 Performance Summary (Top 10)

| Benchmark | Mean | Std Dev |
|-----------|------|---------|
"""

    for bench in top_benchmarks:
        md += f"| {bench.name} | {bench.mean:.2f} {bench.unit} | ± {bench.std_dev:.2f} {bench.unit} |\n"

    md += """
---
*Full benchmark results available in workflow artifacts*

📊 [View detailed results](https://madmax983.github.io/GallifreyDB/benchmarks/)
📈 [Historical trends](https://madmax983.github.io/GallifreyDB/dev/bench/)
"""

    with open(output_path, 'w', encoding='utf-8') as f:
        f.write(md)

    with open(output_path, 'w', encoding='utf-8') as f:
        f.write(md)

    print(f"Generated PR comment: {output_path}")


def generate_json_output(all_results: dict[str, list[BenchmarkResult]], output_path: Path) -> None:
    """Generate JSON output compatible with github-action-benchmark customSmallerIsBetter."""
    
    json_data = []
    for suite_name, results in all_results.items():
        for bench in results:
            json_data.append({
                "name": bench.name,
                "unit": "ns",
                "value": bench.mean_ns
            })

    with open(output_path, 'w', encoding='utf-8') as f:
        json.dump(json_data, f, indent=2)



def main():
    parser = argparse.ArgumentParser(description='Generate HTML tables for benchmark results')
    parser.add_argument(
        '--input',
        type=Path,
        default=Path('target/criterion'),
        help='Input directory containing Criterion results (default: target/criterion)'
    )
    parser.add_argument(
        '--output',
        type=Path,
        default=Path('benchmark-results'),
        help='Output directory for HTML tables (default: benchmark-results)'
    )
    parser.add_argument(
        '--format',
        type=str,
        choices=['html', 'pr-comment', 'json'],
        default='html',
        help='Output format: html (default), pr-comment (markdown), or json (for github-action-benchmark)'
    )

    parser.add_argument(
        '--history',
        type=Path,
        help='Path to historical data.js file for comparison'
    )

    args = parser.parse_args()

    # Validate input directory
    if not args.input.exists():
        print(f"Error: Input directory not found: {args.input}", file=sys.stderr)
        return 1

    # Create output directory only for HTML format
    if args.format == 'html':
        args.output.mkdir(parents=True, exist_ok=True)
    elif args.format in ['pr-comment', 'json']:
        # Ensure parent directory exists for file outputs
        args.output.parent.mkdir(parents=True, exist_ok=True)

    # Collect benchmark results
    print(f"Collecting benchmark results from {args.input}...")
    all_results = collect_benchmark_results(args.input)

    if not all_results:
        print("Warning: No benchmark results found", file=sys.stderr)
        return 1

    print(f"Found {len(all_results)} benchmark suites")
    for suite, results in all_results.items():
        print(f"  - {suite}: {len(results)} benchmarks")

    # Parse historical data if provided
    history = {}
    if args.history and args.history.exists():
        print(f"Parsing historical data from {args.history}...")
        history = parse_history_data(args.history)
        print(f"Found {len(history)} historical benchmarks")

    # Generate output based on format
    if args.format == 'html':
        print(f"\nGenerating HTML tables in {args.output}...")
        generate_index_page(all_results, args.output)
        print("\nDone! Open benchmark-results/index.html to view results")
    elif args.format == 'pr-comment':
        print(f"\nGenerating PR comment...")
        generate_pr_comment(all_results, args.output, history)
        print(f"\nDone! PR comment written to {args.output}")
    elif args.format == 'json':
        print(f"\nGenerating JSON for github-action-benchmark...")
        generate_json_output(all_results, args.output)
        print(f"\nDone! JSON written to {args.output}")

    return 0


if __name__ == '__main__':
    sys.exit(main())
