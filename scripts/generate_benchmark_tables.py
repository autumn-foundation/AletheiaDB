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


def generate_pr_comment(all_results: dict[str, list[BenchmarkResult]], output_path: Path) -> None:
    """Generate a markdown summary for PR comments."""
    # Get top benchmarks from each suite
    top_benchmarks = []
    for suite_name, results in all_results.items():
        # Sort by mean time and take top 3
        sorted_results = sorted(results, key=lambda r: r.mean)[:3]
        top_benchmarks.extend(sorted_results)

    # Sort all top benchmarks and take top 10 overall
    top_benchmarks = sorted(top_benchmarks, key=lambda r: r.mean)[:10]

    md = """## 🚀 Benchmark Results

Benchmarks have been run for this PR. Top performers:

### Performance Summary

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

    with open(output_path, 'w') as f:
        f.write(md)

    print(f"Generated PR comment: {output_path}")


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
        choices=['html', 'pr-comment'],
        default='html',
        help='Output format: html (default) or pr-comment (markdown for PR comments)'
    )

    args = parser.parse_args()

    # Validate input directory
    if not args.input.exists():
        print(f"Error: Input directory not found: {args.input}", file=sys.stderr)
        return 1

    # Create output directory
    args.output.mkdir(parents=True, exist_ok=True)

    # Collect benchmark results
    print(f"Collecting benchmark results from {args.input}...")
    all_results = collect_benchmark_results(args.input)

    if not all_results:
        print("Warning: No benchmark results found", file=sys.stderr)
        return 1

    print(f"Found {len(all_results)} benchmark suites")
    for suite, results in all_results.items():
        print(f"  - {suite}: {len(results)} benchmarks")

    # Generate output based on format
    if args.format == 'html':
        print(f"\nGenerating HTML tables in {args.output}...")
        generate_index_page(all_results, args.output)
        print("\nDone! Open benchmark-results/index.html to view results")
    elif args.format == 'pr-comment':
        print(f"\nGenerating PR comment...")
        output_file = args.output / 'pr_comment.md' if args.output.is_dir() else args.output
        generate_pr_comment(all_results, output_file)
        print(f"\nDone! PR comment written to {output_file}")

    return 0


if __name__ == '__main__':
    sys.exit(main())
