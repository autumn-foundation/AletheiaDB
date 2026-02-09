#!/usr/bin/env python3
"""Fetch mutation_metrics.json from the latest prior run artifact named mutation-metrics."""

from __future__ import annotations

import io
import json
import os
import sys
import urllib.request
import zipfile


def gh_get(url: str, token: str) -> dict:
    req = urllib.request.Request(
        url,
        headers={
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.github+json",
            "User-Agent": "verification-mutation-trend",
        },
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.load(resp)


def gh_get_bytes(url: str, token: str) -> bytes:
    req = urllib.request.Request(
        url,
        headers={
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.github+json",
            "User-Agent": "verification-mutation-trend",
        },
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        return resp.read()


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: fetch_previous_mutation_metrics.py <output_json_path>")
        return 2

    out_path = sys.argv[1]
    token = os.environ.get("GITHUB_TOKEN", "").strip()
    repo = os.environ.get("GITHUB_REPOSITORY", "").strip()
    current_run_id = os.environ.get("GITHUB_RUN_ID", "").strip()

    if not token or not repo or not current_run_id:
        print("Missing GitHub CI environment; skipping previous-metrics fetch.")
        return 0

    owner, name = repo.split("/", 1)
    runs_url = (
        f"https://api.github.com/repos/{owner}/{name}/actions/workflows/"
        "verification-tiers.yml/runs?status=success&per_page=30"
    )
    runs = gh_get(runs_url, token).get("workflow_runs", [])

    for run in runs:
        run_id = str(run.get("id", ""))
        if run_id == current_run_id:
            continue

        artifacts_url = f"https://api.github.com/repos/{owner}/{name}/actions/runs/{run_id}/artifacts?per_page=100"
        artifacts = gh_get(artifacts_url, token).get("artifacts", [])
        for artifact in artifacts:
            if artifact.get("name") != "mutation-metrics":
                continue
            if artifact.get("expired", False):
                continue

            zip_bytes = gh_get_bytes(artifact["archive_download_url"], token)
            with zipfile.ZipFile(io.BytesIO(zip_bytes)) as zf:
                for member in zf.namelist():
                    if member.endswith("mutation_metrics.json"):
                        data = zf.read(member)
                        os.makedirs(os.path.dirname(out_path), exist_ok=True)
                        with open(out_path, "wb") as f:
                            f.write(data)
                        print(f"Fetched previous metrics from run {run_id} artifact {artifact['id']}")
                        return 0

    print("No previous mutation-metrics artifact found.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
