#!/usr/bin/env python3
"""Mutation-score gate for cargo-mutants output.

Subcommands:
  gate       Enforce the PR-diff mutation-score threshold against one
             mutants.out directory. Exit 0 = pass, 2 = score below threshold,
             1 = infrastructure error (bad config, unreadable output dir).
  summarize  Aggregate several weekly shard artifacts (each containing a
             mutants.out) into a combined, informational project score.
             Writes score.json and combined-missed.txt into the shards dir.
             Never exits 2; infrastructure error = 1.

Score formula (see TESTING.md "Mutation Testing"):
    score = (caught + timeout) / (caught + timeout + missed) * 100
Timeouts count as caught: the mutant made the test suite hang, i.e. it was
detected. Unviable mutants (failed to compile) are excluded entirely.

Configuration lives in .github/mutants-gate.toml (threshold, min_mutants).
Python 3.11+ standard library only (tomllib).
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import tomllib
from dataclasses import dataclass
from pathlib import Path

EXIT_PASS = 0
EXIT_INFRA = 1
EXIT_GATE_FAIL = 2

OUTCOME_FILES = ("caught.txt", "missed.txt", "timeout.txt", "unviable.txt")

SECONDS_PER_WEEK = 7 * 24 * 3600


class InfraError(Exception):
    """Configuration or environment problem — not a gate verdict."""


@dataclass
class Counts:
    caught: int = 0
    missed: int = 0
    timeout: int = 0
    unviable: int = 0
    missed_lines: tuple[str, ...] = ()

    @property
    def viable(self) -> int:
        return self.caught + self.timeout + self.missed

    @property
    def detected(self) -> int:
        return self.caught + self.timeout

    def score(self) -> float | None:
        """Mutation score in percent, or None when there are no viable mutants."""
        if self.viable == 0:
            return None
        return self.detected / self.viable * 100.0

    def add(self, other: "Counts") -> None:
        self.caught += other.caught
        self.missed += other.missed
        self.timeout += other.timeout
        self.unviable += other.unviable
        self.missed_lines = self.missed_lines + other.missed_lines


def load_config(path: Path) -> dict:
    try:
        with open(path, "rb") as f:
            raw = tomllib.load(f)
    except OSError as e:
        raise InfraError(f"cannot read gate config {path}: {e}") from e
    except tomllib.TOMLDecodeError as e:
        raise InfraError(f"malformed gate config {path}: {e}") from e

    threshold = raw.get("threshold")
    min_mutants = raw.get("min_mutants")
    if not isinstance(threshold, (int, float)) or isinstance(threshold, bool):
        raise InfraError(f"gate config {path}: 'threshold' must be a number, got {threshold!r}")
    if not isinstance(min_mutants, int) or isinstance(min_mutants, bool):
        raise InfraError(f"gate config {path}: 'min_mutants' must be an integer, got {min_mutants!r}")
    if not 0.0 <= float(threshold) <= 100.0:
        raise InfraError(f"gate config {path}: 'threshold' must be within [0, 100], got {threshold}")
    if min_mutants < 0:
        raise InfraError(f"gate config {path}: 'min_mutants' must be >= 0, got {min_mutants}")
    return {"threshold": float(threshold), "min_mutants": min_mutants}


def read_lines(path: Path) -> list[str]:
    try:
        return [ln for ln in path.read_text(encoding="utf-8", errors="replace").splitlines() if ln.strip()]
    except OSError as e:
        raise InfraError(f"cannot read {path}: {e}") from e


def read_counts(mutants_out: Path) -> Counts:
    """Read outcome counts from a mutants.out directory.

    A missing outcome file counts as 0, but only if the directory exists and
    contains at least one of the four outcome files — otherwise this is an
    infrastructure error (the run didn't produce usable output).
    """
    if not mutants_out.is_dir():
        raise InfraError(f"mutants output directory not found: {mutants_out}")
    present = [name for name in OUTCOME_FILES if (mutants_out / name).is_file()]
    if not present:
        raise InfraError(
            f"{mutants_out} exists but contains none of {', '.join(OUTCOME_FILES)}; "
            "the cargo-mutants run did not produce usable output"
        )

    def count(name: str) -> int:
        p = mutants_out / name
        return len(read_lines(p)) if p.is_file() else 0

    missed_path = mutants_out / "missed.txt"
    missed_lines = tuple(read_lines(missed_path)) if missed_path.is_file() else ()
    return Counts(
        caught=count("caught.txt"),
        missed=len(missed_lines),
        timeout=count("timeout.txt"),
        unviable=count("unviable.txt"),
        missed_lines=missed_lines,
    )


def emit(markdown: str) -> None:
    """Print markdown to stdout and append it to $GITHUB_STEP_SUMMARY if set."""
    print(markdown)
    summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary_path:
        try:
            with open(summary_path, "a", encoding="utf-8") as f:
                f.write(markdown + "\n")
        except OSError as e:
            print(f"warning: could not write GITHUB_STEP_SUMMARY: {e}", file=sys.stderr)


def fmt_score(score: float | None) -> str:
    return "n/a" if score is None else f"{score:.1f}%"


def missed_details(missed_lines: tuple[str, ...]) -> str:
    if not missed_lines:
        return ""
    body = "\n".join(missed_lines)
    return (
        "\n<details><summary>Missed mutants "
        f"({len(missed_lines)})</summary>\n\n```\n{body}\n```\n</details>\n"
    )


def cmd_gate(args: argparse.Namespace) -> int:
    config = load_config(Path(args.config))
    threshold: float = config["threshold"]
    min_mutants: int = config["min_mutants"]

    mutants_out = Path(args.mutants_out)
    if not mutants_out.is_dir() and args.allow_missing:
        emit(
            "## Mutation-score gate: PASS\n\n"
            "No mutants output directory was produced — no mutants in diff.\n"
        )
        return EXIT_PASS

    counts = read_counts(mutants_out)
    score = counts.score()

    if counts.viable == 0:
        verdict, reason, code = "PASS", "no mutants in diff", EXIT_PASS
    elif counts.viable < min_mutants:
        verdict, code = "PASS", EXIT_PASS
        reason = (
            f"only {counts.viable} viable mutants (< min_mutants = {min_mutants}); "
            "sample too small to enforce — reported for information only"
        )
    elif score >= threshold:
        verdict, code = "PASS", EXIT_PASS
        reason = f"score {fmt_score(score)} >= threshold {threshold:.1f}%"
    else:
        verdict, code = "FAIL", EXIT_GATE_FAIL
        reason = f"score {fmt_score(score)} < threshold {threshold:.1f}%"

    lines = [
        f"## Mutation-score gate: {verdict}",
        "",
        f"{reason}",
        "",
        "| Metric | Value |",
        "|---|---|",
        f"| Caught | {counts.caught} |",
        f"| Timeout (counted as caught) | {counts.timeout} |",
        f"| Missed | {counts.missed} |",
        f"| Unviable (excluded) | {counts.unviable} |",
        f"| Viable total | {counts.viable} |",
        f"| Score | {fmt_score(score)} |",
        f"| Threshold | {threshold:.1f}% |",
        f"| min_mutants floor | {min_mutants} |",
        missed_details(counts.missed_lines),
    ]
    emit("\n".join(lines))
    return code


def find_shard_outputs(shards_dir: Path) -> dict[str, Path]:
    """Map shard-artifact name -> mutants.out-style directory.

    Each immediate subdirectory of shards_dir is one downloaded artifact.
    Outcome files may sit at the artifact root or under a nested mutants.out/.
    """
    if not shards_dir.is_dir():
        raise InfraError(f"shards directory not found: {shards_dir}")
    found: dict[str, Path] = {}
    for sub in sorted(p for p in shards_dir.iterdir() if p.is_dir()):
        for candidate in (sub, sub / "mutants.out"):
            if candidate.is_dir() and any((candidate / n).is_file() for n in OUTCOME_FILES):
                found[sub.name] = candidate
                break
    if not found:
        raise InfraError(
            f"no shard artifacts with mutants output found under {shards_dir}"
        )
    return found


def cmd_summarize(args: argparse.Namespace) -> int:
    config = load_config(Path(args.config))
    threshold: float = config["threshold"]
    shards_dir = Path(args.shards_dir)

    shard_outputs = find_shard_outputs(shards_dir)
    total = Counts()
    rows = []
    for name, out_dir in shard_outputs.items():
        c = read_counts(out_dir)
        total.add(c)
        rows.append(
            f"| {name} | {c.caught} | {c.timeout} | {c.missed} | {c.unviable} | {fmt_score(c.score())} |"
        )

    score = total.score()
    week = int(time.time()) // SECONDS_PER_WEEK

    score_json = {
        "week": week,
        "shards": len(shard_outputs),
        "caught": total.caught,
        "timeout": total.timeout,
        "missed": total.missed,
        "unviable": total.unviable,
        "viable": total.viable,
        "score": None if score is None else round(score, 2),
        "threshold": threshold,
    }
    try:
        (shards_dir / "score.json").write_text(json.dumps(score_json, indent=2) + "\n")
        (shards_dir / "combined-missed.txt").write_text(
            "".join(line + "\n" for line in total.missed_lines)
        )
    except OSError as e:
        raise InfraError(f"cannot write aggregate outputs into {shards_dir}: {e}") from e

    comparison = (
        "no viable mutants in this sample"
        if score is None
        else f"{fmt_score(score)} vs PR-gate threshold {threshold:.1f}% "
        + ("(at or above)" if score >= threshold else "(below — informational, does not fail this run)")
    )
    lines = [
        f"## Weekly mutation score (rotating sample, week {week})",
        "",
        f"Combined score across {len(shard_outputs)} shard(s): **{fmt_score(score)}** — {comparison}",
        "",
        "| Shard | Caught | Timeout | Missed | Unviable | Score |",
        "|---|---|---|---|---|---|",
        *rows,
        f"| **Total** | {total.caught} | {total.timeout} | {total.missed} | {total.unviable} | {fmt_score(score)} |",
        missed_details(total.missed_lines),
    ]
    emit("\n".join(lines))
    return EXIT_PASS


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    p_gate = sub.add_parser("gate", help="enforce PR-diff mutation-score threshold")
    p_gate.add_argument("--mutants-out", required=True, help="path to mutants.out directory")
    p_gate.add_argument("--config", required=True, help="path to mutants-gate.toml")
    p_gate.add_argument(
        "--allow-missing",
        action="store_true",
        help="treat a missing mutants.out directory as zero mutants (empty diff) "
        "instead of an infrastructure error",
    )
    p_gate.set_defaults(func=cmd_gate)

    p_sum = sub.add_parser("summarize", help="aggregate weekly shard artifacts (informational)")
    p_sum.add_argument("--shards-dir", required=True, help="directory of downloaded shard artifacts")
    p_sum.add_argument("--config", required=True, help="path to mutants-gate.toml")
    p_sum.set_defaults(func=cmd_summarize)

    args = parser.parse_args(argv)
    try:
        return args.func(args)
    except InfraError as e:
        print(f"error: {e}", file=sys.stderr)
        return EXIT_INFRA


if __name__ == "__main__":
    sys.exit(main())
