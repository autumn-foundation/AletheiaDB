#!/usr/bin/env python3
"""Turn per-query latency-sample files into a machine-readable results JSON.

Reads, from the directory named by ``$SAMPLE_DIR``:

* ``<key>.samples``          — whitespace-separated integer microsecond samples
                               (one measured query invocation each).
* ``<key>.notexpressible``   — marker file: the engine cannot express this
                               workload natively. Its (optional) contents become
                               the ``reason``. NO numbers are emitted for it.
* ``<key>.desc``  (optional) — a human description for the query key.

Emits ``$OUT`` (a JSON file) shaped to line up with the AletheiaDB report
(``crates/aletheia-bench-ldbc/src/report.rs``): an ``engine``/``version`` header
plus a ``per_query`` array of ``{key, p50_us, p95_us, p99_us, iterations}`` (or
``{key, not_expressible: true, reason}``).

Percentiles use the **nearest-rank** method to match
``crates/aletheia-bench-ldbc/src/stats.rs`` exactly, so incumbent numbers are
computed the same way as AletheiaDB's. This script NEVER invents a sample: a
query with neither a ``.samples`` nor a ``.notexpressible`` file is simply
absent from the output.
"""

import glob
import json
import math
import os
import sys


def percentile_nearest_rank(sorted_samples, p):
    """Nearest-rank percentile over an ascending-sorted list (see stats.rs)."""
    n = len(sorted_samples)
    if n == 0:
        raise ValueError("percentile of empty sample set")
    if n == 1:
        return float(sorted_samples[0])
    p = max(0.0, min(100.0, p))
    rank = math.ceil(p / 100.0 * n)
    idx = min(max(rank - 1, 0), n - 1)
    return float(sorted_samples[idx])


def main():
    engine = os.environ["ENGINE"]
    version = os.environ.get("VERSION", "unknown")
    sample_dir = os.environ["SAMPLE_DIR"]
    out_path = os.environ["OUT"]

    per_query = []

    # Not-expressible markers first (kept in the honest capability table).
    for marker in sorted(glob.glob(os.path.join(sample_dir, "*.notexpressible"))):
        key = os.path.basename(marker)[: -len(".notexpressible")]
        with open(marker, encoding="utf-8") as fh:
            reason = fh.read().strip()
        per_query.append(
            {
                "key": key,
                "not_expressible": True,
                "reason": reason or "not expressible natively on this engine",
            }
        )

    # Timed queries.
    for samples_file in sorted(glob.glob(os.path.join(sample_dir, "*.samples"))):
        key = os.path.basename(samples_file)[: -len(".samples")]
        with open(samples_file, encoding="utf-8") as fh:
            samples = [int(tok) for tok in fh.read().split() if tok.strip()]
        if not samples:
            # An empty samples file is a measurement bug, not a zero-latency
            # result. Skip it rather than fabricate a value.
            print(
                f"[emit_results] WARNING: {key} has an empty samples file; skipping",
                file=sys.stderr,
            )
            continue
        samples.sort()
        desc_path = os.path.join(sample_dir, f"{key}.desc")
        desc = ""
        if os.path.exists(desc_path):
            with open(desc_path, encoding="utf-8") as fh:
                desc = fh.read().strip()
        entry = {
            "key": key,
            "p50_us": round(percentile_nearest_rank(samples, 50.0), 3),
            "p95_us": round(percentile_nearest_rank(samples, 95.0), 3),
            "p99_us": round(percentile_nearest_rank(samples, 99.0), 3),
            "iterations": len(samples),
        }
        if desc:
            entry["description"] = desc
        per_query.append(entry)

    report = {
        "engine": engine,
        "version": version,
        "disclaimer": (
            "Incumbent comparison run — LDBC-style, not an audited result. "
            "Same logical queries as the AletheiaDB suite, run on the same box. "
            "Queries marked not_expressible are a capability fact, not a "
            "performance claim (see incumbents/README.md)."
        ),
        "per_query": per_query,
    }

    with open(out_path, "w", encoding="utf-8") as fh:
        json.dump(report, fh, indent=2)
        fh.write("\n")


if __name__ == "__main__":
    main()
