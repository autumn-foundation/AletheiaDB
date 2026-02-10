#!/usr/bin/env bash
set -euo pipefail

FEATURES="config-toml,mcp-server,sharding-rpc"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT_ROOT="${ROOT_DIR}/verification/mutation"
MODULE_TSV="${OUT_ROOT}/module_scores.tsv"
SUMMARY_MD="${OUT_ROOT}/mutation_summary.md"
METRICS_JSON="${OUT_ROOT}/mutation_metrics.json"

mkdir -p "${OUT_ROOT}/mutants"
printf "module\tcaught\tmissed\ttimeout\tunviable\ttested\tkill_rate\n" > "${MODULE_TSV}"

count_lines() {
  local file="$1"
  if [[ -f "${file}" ]]; then
    wc -l < "${file}" | tr -d ' '
  else
    echo 0
  fi
}

run_module() {
  local module_name="$1"
  shift
  local patterns=("$@")
  local module_out="${OUT_ROOT}/mutants/${module_name}"
  mkdir -p "${module_out}"

  local cmd=(
    cargo mutants
    --in-place
    -vV
    --features "${FEATURES}"
    --output "${module_out}"
  )
  local pattern
  for pattern in "${patterns[@]}"; do
    cmd+=(--file "${pattern}")
  done

  "${cmd[@]}"

  local caught missed timeout unviable tested kill_rate
  caught="$(count_lines "${module_out}/caught.txt")"
  missed="$(count_lines "${module_out}/missed.txt")"
  timeout="$(count_lines "${module_out}/timeout.txt")"
  unviable="$(count_lines "${module_out}/unviable.txt")"
  tested=$((caught + missed))
  kill_rate="$(python3 - <<PY
caught=${caught}
tested=${tested}
print(f"{(100.0 * caught / tested) if tested else 0.0:.2f}")
PY
)"

  printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\n" \
    "${module_name}" "${caught}" "${missed}" "${timeout}" "${unviable}" "${tested}" "${kill_rate}" \
    >> "${MODULE_TSV}"
}

run_module "wal" \
  "src/storage/wal/**/*.rs"

run_module "temporal_vector" \
  "src/index/vector/temporal/**/*.rs" \
  "src/index/vector/**/*.rs"

run_module "query_planner" \
  "src/query/planner/**/*.rs" \
  "src/query/plan.rs"

python3 - <<PY
import csv
import json
import os
from datetime import datetime, timezone

module_tsv = r"${MODULE_TSV}"
summary_md = r"${SUMMARY_MD}"
metrics_json = r"${METRICS_JSON}"
commit_sha = os.environ.get("GITHUB_SHA", "").strip()
if not commit_sha:
    try:
        import subprocess
        commit_sha = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    except Exception:
        commit_sha = "unknown"

modules = {}
overall = {"caught": 0, "missed": 0, "timeout": 0, "unviable": 0, "tested": 0}

with open(module_tsv, newline="", encoding="utf-8") as f:
    reader = csv.DictReader(f, delimiter="\t")
    for row in reader:
        name = row["module"]
        caught = int(row["caught"])
        missed = int(row["missed"])
        timeout = int(row["timeout"])
        unviable = int(row["unviable"])
        tested = int(row["tested"])
        kill_rate = float(row["kill_rate"])
        modules[name] = {
            "caught": caught,
            "missed": missed,
            "timeout": timeout,
            "unviable": unviable,
            "tested": tested,
            "kill_rate_pct": kill_rate,
        }
        overall["caught"] += caught
        overall["missed"] += missed
        overall["timeout"] += timeout
        overall["unviable"] += unviable
        overall["tested"] += tested

overall["kill_rate_pct"] = round(
    (100.0 * overall["caught"] / overall["tested"]) if overall["tested"] else 0.0, 2
)

payload = {
    "generated_at_utc": datetime.now(timezone.utc).isoformat(),
    "commit_sha": commit_sha,
    "modules": modules,
    "overall": overall,
}

with open(metrics_json, "w", encoding="utf-8") as f:
    json.dump(payload, f, indent=2, sort_keys=True)
    f.write("\n")

with open(summary_md, "w", encoding="utf-8") as f:
    f.write("## Mutation Sweep (module scores)\\n\\n")
    f.write("| Module | Caught | Missed | Timeout | Unviable | Tested | Kill rate |\\n")
    f.write("| --- | ---: | ---: | ---: | ---: | ---: | ---: |\\n")
    for name in ("wal", "temporal_vector", "query_planner"):
        m = modules[name]
        f.write(
            f"| `{name}` | {m['caught']} | {m['missed']} | {m['timeout']} | "
            f"{m['unviable']} | {m['tested']} | {m['kill_rate_pct']:.2f}% |\\n"
        )
    f.write(
        f"| `overall` | {overall['caught']} | {overall['missed']} | {overall['timeout']} | "
        f"{overall['unviable']} | {overall['tested']} | {overall['kill_rate_pct']:.2f}% |\\n"
    )
PY

cat "${SUMMARY_MD}"
