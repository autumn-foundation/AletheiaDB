#!/usr/bin/env python3
"""Compare current and previous mutation metrics and emit a markdown trend report."""

from __future__ import annotations

import json
import sys


def load(path: str) -> dict:
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def fmt_delta(curr: float, prev: float) -> str:
    delta = curr - prev
    return f"{delta:+.2f} pts"


def main() -> int:
    if len(sys.argv) != 4:
        print("usage: compare_mutation_metrics.py <current_json> <previous_json> <output_md>")
        return 2

    current = load(sys.argv[1])
    previous = load(sys.argv[2])
    out_path = sys.argv[3]

    modules = ["wal", "temporal_vector", "query_planner"]
    lines = []
    lines.append("## Mutation Score Trend")
    lines.append("")
    lines.append("| Module | Previous | Current | Delta |")
    lines.append("| --- | ---: | ---: | ---: |")

    for module in modules:
        curr = current.get("modules", {}).get(module, {})
        prev = previous.get("modules", {}).get(module, {})
        curr_rate = float(curr.get("kill_rate_pct", 0.0))
        prev_rate = float(prev.get("kill_rate_pct", 0.0))
        lines.append(
            f"| `{module}` | {prev_rate:.2f}% | {curr_rate:.2f}% | {fmt_delta(curr_rate, prev_rate)} |"
        )

    curr_overall = float(current.get("overall", {}).get("kill_rate_pct", 0.0))
    prev_overall = float(previous.get("overall", {}).get("kill_rate_pct", 0.0))
    lines.append(
        f"| `overall` | {prev_overall:.2f}% | {curr_overall:.2f}% | {fmt_delta(curr_overall, prev_overall)} |"
    )
    lines.append("")
    lines.append(f"Previous commit: `{previous.get('commit_sha', 'unknown')}`")
    lines.append(f"Current commit: `{current.get('commit_sha', 'unknown')}`")

    with open(out_path, "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
