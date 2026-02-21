#!/usr/bin/env python3
"""Run and validate the fused GroupCommit threshold benchmark."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path


def run_command(command: list[str]) -> None:
    print(f"[bench-check] running: {' '.join(command)}")
    result = subprocess.run(command, check=False)
    if result.returncode != 0:
        raise RuntimeError(f"benchmark command failed with exit code {result.returncode}")


def load_mean_point_estimate_ns(path: Path) -> float:
    if not path.exists():
        raise FileNotFoundError(f"missing criterion estimates file: {path}")
    payload = json.loads(path.read_text(encoding="utf-8"))
    return float(payload["mean"]["point_estimate"])


def main() -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Run group_commit_fused_concurrent benchmark, save baseline, and verify "
            "half-batch threshold remains significantly faster than timer-only."
        )
    )
    parser.add_argument(
        "--criterion-root",
        default="target/criterion",
        help="Criterion output root directory (default: target/criterion)",
    )
    parser.add_argument(
        "--baseline-name",
        default="fused_commit_wait_threshold",
        help="Criterion baseline name for --save-baseline",
    )
    parser.add_argument(
        "--max-ratio",
        type=float,
        default=0.70,
        help=(
            "Maximum allowed ratio of half_batch/timer_only mean runtime. "
            "Lower is stricter (default: 0.70)."
        ),
    )
    parser.add_argument(
        "--skip-run",
        action="store_true",
        help="Skip cargo bench execution and only validate existing Criterion output.",
    )
    args = parser.parse_args()

    if args.max_ratio <= 0:
        print("[bench-check] --max-ratio must be > 0", file=sys.stderr)
        return 2

    if not args.skip_run:
        run_command(
            [
                "cargo",
                "bench",
                "--all-features",
                "--bench",
                "durability_modes",
                "--",
                "group_commit_fused_concurrent",
                "--save-baseline",
                args.baseline_name,
            ]
        )

    criterion_root = Path(args.criterion_root)
    half_path = (
        criterion_root
        / "group_commit_fused_concurrent"
        / "half_batch_threshold"
        / "new"
        / "estimates.json"
    )
    timer_path = (
        criterion_root
        / "group_commit_fused_concurrent"
        / "timer_only_threshold"
        / "new"
        / "estimates.json"
    )

    half_ns = load_mean_point_estimate_ns(half_path)
    timer_ns = load_mean_point_estimate_ns(timer_path)
    if timer_ns <= 0:
        print("[bench-check] timer-only estimate must be > 0", file=sys.stderr)
        return 2

    ratio = half_ns / timer_ns
    speedup = timer_ns / half_ns if half_ns > 0 else float("inf")
    print(f"[bench-check] half_batch mean: {half_ns:.0f} ns")
    print(f"[bench-check] timer_only mean: {timer_ns:.0f} ns")
    print(f"[bench-check] half_batch/timer_only ratio: {ratio:.4f}")
    print(f"[bench-check] half_batch speedup vs timer_only: {speedup:.2f}x")

    if ratio > args.max_ratio:
        print(
            (
                f"[bench-check] FAILURE: ratio {ratio:.4f} exceeded allowed "
                f"max {args.max_ratio:.4f}"
            ),
            file=sys.stderr,
        )
        return 1

    print("[bench-check] PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
