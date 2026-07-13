#!/usr/bin/env python3
"""Unit tests for mutants_gate.py (the mutation-score CI gate).

Run directly:  python3 .github/scripts/test_mutants_gate.py

These tests shell out to the script as a subprocess so that exit codes,
stdout, and $GITHUB_STEP_SUMMARY handling are tested exactly as CI sees them.
Python 3.11+ standard library only.
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "mutants_gate.py"

# Exit-code contract of mutants_gate.py
EXIT_PASS = 0
EXIT_INFRA = 1
EXIT_GATE_FAIL = 2

GOOD_CONFIG = 'threshold = 60.0\nmin_mutants = 10\n'


def write_mutants_out(dir_path: Path, caught=0, missed=0, timeout=0, unviable=0,
                      omit_empty=False):
    """Create a mutants.out-style directory with N synthetic lines per file.

    With omit_empty=True, files whose count is zero are not created at all
    (cargo-mutants sometimes writes all four, but the gate must tolerate
    missing files as long as the directory itself exists with at least one).
    """
    dir_path.mkdir(parents=True, exist_ok=True)
    for name, count in (("caught.txt", caught), ("missed.txt", missed),
                        ("timeout.txt", timeout), ("unviable.txt", unviable)):
        if omit_empty and count == 0:
            continue
        lines = [f"src/fake.rs:{i}:1: replace thing_{name[:-4]}_{i} with ()"
                 for i in range(count)]
        (dir_path / name).write_text("".join(line + "\n" for line in lines))


class GateTestBase(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)
        self.config = self.tmp / "gate.toml"
        self.config.write_text(GOOD_CONFIG)
        self.summary_file = self.tmp / "step_summary.md"

    def run_cmd(self, *args):
        env = dict(os.environ)
        env["GITHUB_STEP_SUMMARY"] = str(self.summary_file)
        return subprocess.run(
            [sys.executable, str(SCRIPT), *args],
            capture_output=True, text=True, env=env,
        )

    def run_gate(self, mutants_out, *extra):
        return self.run_cmd("gate", "--mutants-out", str(mutants_out),
                            "--config", str(self.config), *extra)


class TestGate(GateTestBase):
    def test_zero_mutants_passes(self):
        out = self.tmp / "mutants.out"
        write_mutants_out(out)  # all four files present, all empty
        r = self.run_gate(out)
        self.assertEqual(r.returncode, EXIT_PASS, r.stdout + r.stderr)
        self.assertIn("no mutants", r.stdout.lower())
        self.assertIn("PASS", r.stdout)

    def test_below_min_mutants_does_not_fail(self):
        # Score 40% < threshold 60%, but only 5 viable mutants < min_mutants 10.
        out = self.tmp / "mutants.out"
        write_mutants_out(out, caught=2, missed=3)
        r = self.run_gate(out)
        self.assertEqual(r.returncode, EXIT_PASS, r.stdout + r.stderr)
        self.assertIn("PASS", r.stdout)
        # Must warn that the sample was too small to enforce.
        self.assertRegex(r.stdout.lower(), r"min_mutants|too few|small sample")

    def test_score_exactly_equal_threshold_passes(self):
        # 6 caught / 10 viable = exactly 60.0% == threshold.
        out = self.tmp / "mutants.out"
        write_mutants_out(out, caught=6, missed=4)
        r = self.run_gate(out)
        self.assertEqual(r.returncode, EXIT_PASS, r.stdout + r.stderr)
        self.assertIn("60.0", r.stdout)
        self.assertIn("PASS", r.stdout)

    def test_score_below_threshold_fails_exit_2(self):
        # 5 caught / 10 viable = 50.0% < 60%.
        out = self.tmp / "mutants.out"
        write_mutants_out(out, caught=5, missed=5)
        r = self.run_gate(out)
        self.assertEqual(r.returncode, EXIT_GATE_FAIL, r.stdout + r.stderr)
        self.assertIn("FAIL", r.stdout)
        # Missed mutants must be listed for diagnosis.
        self.assertIn("thing_missed_0", r.stdout)

    def test_exact_threshold_pathological_float_ratio_passes(self):
        # 29 / 50 is exactly 58% but 29/50*100 = 57.99999999999999 in floats.
        # The gate must compare without division (detected*100 >= threshold*viable).
        self.config.write_text("threshold = 58.0\nmin_mutants = 10\n")
        out = self.tmp / "mutants.out"
        write_mutants_out(out, caught=29, missed=21)
        r = self.run_gate(out)
        self.assertEqual(r.returncode, EXIT_PASS, r.stdout + r.stderr)
        self.assertIn("PASS", r.stdout)
        # Near-threshold scores are displayed with 2 decimals to avoid the
        # nonsensical-looking "58.0% < 58.0%".
        self.assertIn("58.00", r.stdout)

    def test_pathological_ratio_against_other_thresholds(self):
        for threshold, expected in ((57.0, EXIT_PASS), (29.0, EXIT_PASS),
                                    (58.5, EXIT_GATE_FAIL)):
            with self.subTest(threshold=threshold):
                self.config.write_text(f"threshold = {threshold}\nmin_mutants = 10\n")
                out = self.tmp / f"mutants-{threshold}.out"
                write_mutants_out(out, caught=29, missed=21)
                r = self.run_gate(out)
                self.assertEqual(r.returncode, expected, r.stdout + r.stderr)

    def test_timeout_counts_toward_caught(self):
        # (5 caught + 2 timeout) / (5+2+3) = 70.0% >= 60%.
        out = self.tmp / "mutants.out"
        write_mutants_out(out, caught=5, timeout=2, missed=3)
        r = self.run_gate(out)
        self.assertEqual(r.returncode, EXIT_PASS, r.stdout + r.stderr)
        self.assertIn("70.0", r.stdout)

    def test_unviable_excluded_from_denominator(self):
        # 6/(6+4) = 60.0% passes; if unviable leaked into the denominator the
        # score would be 6/110 = 5.5% and the gate would fail.
        out = self.tmp / "mutants.out"
        write_mutants_out(out, caught=6, missed=4, unviable=100)
        r = self.run_gate(out)
        self.assertEqual(r.returncode, EXIT_PASS, r.stdout + r.stderr)
        self.assertIn("60.0", r.stdout)

    def test_missing_outcome_files_tolerated_if_dir_has_one(self):
        # Only caught.txt exists -> other counts default to 0.
        out = self.tmp / "mutants.out"
        write_mutants_out(out, caught=12, omit_empty=True)
        r = self.run_gate(out)
        self.assertEqual(r.returncode, EXIT_PASS, r.stdout + r.stderr)
        self.assertIn("100.0", r.stdout)

    def test_missing_mutants_out_dir_is_infra_error(self):
        r = self.run_gate(self.tmp / "does-not-exist")
        self.assertEqual(r.returncode, EXIT_INFRA, r.stdout + r.stderr)

    def test_missing_mutants_out_dir_passes_with_allow_missing(self):
        r = self.run_gate(self.tmp / "does-not-exist", "--allow-missing")
        self.assertEqual(r.returncode, EXIT_PASS, r.stdout + r.stderr)
        self.assertIn("no mutants", r.stdout.lower())

    def test_allow_missing_rejects_existing_non_directory(self):
        # A FILE at the mutants.out path is not "no mutants in diff" — it is
        # an infrastructure problem even with --allow-missing.
        bogus = self.tmp / "mutants.out"
        bogus.write_text("i am not a directory\n")
        r = self.run_gate(bogus, "--allow-missing")
        self.assertEqual(r.returncode, EXIT_INFRA, r.stdout + r.stderr)

    def test_missed_list_truncated_at_200_lines(self):
        # GitHub silently drops step summaries over 1 MiB; the missed list is
        # capped at 200 entries with a pointer to the artifact.
        self.config.write_text("threshold = 99.0\nmin_mutants = 10\n")
        out = self.tmp / "mutants.out"
        write_mutants_out(out, caught=1, missed=250)
        r = self.run_gate(out)
        self.assertEqual(r.returncode, EXIT_GATE_FAIL, r.stdout + r.stderr)
        self.assertIn("thing_missed_199 ", r.stdout)      # 200th entry shown
        self.assertNotIn("thing_missed_200 ", r.stdout)   # 201st entry cut
        self.assertIn("and 50 more", r.stdout)
        self.assertIn("artifact", r.stdout)

    def test_unknown_config_key_is_infra_error(self):
        self.config.write_text(GOOD_CONFIG + "treshold = 90.0\n")
        out = self.tmp / "mutants.out"
        write_mutants_out(out, caught=10)
        r = self.run_gate(out)
        self.assertEqual(r.returncode, EXIT_INFRA, r.stdout + r.stderr)
        self.assertIn("treshold", r.stderr)

    def test_dir_without_any_outcome_files_is_infra_error(self):
        out = self.tmp / "mutants.out"
        out.mkdir()
        (out / "unrelated.log").write_text("hello\n")
        r = self.run_gate(out)
        self.assertEqual(r.returncode, EXIT_INFRA, r.stdout + r.stderr)

    def test_malformed_toml_is_infra_error(self):
        self.config.write_text("threshold = [not valid toml\n")
        out = self.tmp / "mutants.out"
        write_mutants_out(out, caught=10)
        r = self.run_gate(out)
        self.assertEqual(r.returncode, EXIT_INFRA, r.stdout + r.stderr)

    def test_config_missing_threshold_is_infra_error(self):
        self.config.write_text("min_mutants = 10\n")
        out = self.tmp / "mutants.out"
        write_mutants_out(out, caught=10)
        r = self.run_gate(out)
        self.assertEqual(r.returncode, EXIT_INFRA, r.stdout + r.stderr)

    def test_step_summary_written(self):
        out = self.tmp / "mutants.out"
        write_mutants_out(out, caught=8, missed=2)
        r = self.run_gate(out)
        self.assertEqual(r.returncode, EXIT_PASS)
        summary = self.summary_file.read_text()
        self.assertIn("80.0", summary)
        self.assertIn("PASS", summary)


class TestSummarize(GateTestBase):
    def make_shards(self):
        shards = self.tmp / "shards"
        # Artifact layout: one subdirectory per downloaded artifact. Cover both
        # layouts: outcome files at the artifact root, and nested mutants.out/.
        write_mutants_out(shards / "mutants-weekly-shard-0", caught=10, missed=5)
        write_mutants_out(shards / "mutants-weekly-shard-1" / "mutants.out",
                          caught=20, missed=5, timeout=5, unviable=3)
        write_mutants_out(shards / "mutants-weekly-shard-2", caught=30,
                          missed=10, omit_empty=True)
        return shards

    def run_summarize(self, shards, *extra):
        return self.run_cmd("summarize", "--shards-dir", str(shards),
                            "--config", str(self.config), *extra)

    def test_summarize_aggregates_three_shards(self):
        shards = self.make_shards()
        r = self.run_summarize(shards)
        self.assertEqual(r.returncode, EXIT_PASS, r.stdout + r.stderr)
        # Totals: caught 60, timeout 5, missed 20 -> 65/85 = 76.5%
        self.assertIn("76.5", r.stdout)
        self.assertIn("mutants-weekly-shard-2", r.stdout)

    def test_summarize_writes_score_json(self):
        shards = self.make_shards()
        r = self.run_summarize(shards)
        self.assertEqual(r.returncode, EXIT_PASS, r.stdout + r.stderr)
        data = json.loads((shards / "score.json").read_text())
        self.assertEqual(data["caught"], 60)
        self.assertEqual(data["timeout"], 5)
        self.assertEqual(data["missed"], 20)
        self.assertEqual(data["unviable"], 3)
        self.assertEqual(data["viable"], 85)
        self.assertAlmostEqual(data["score"], 65 / 85 * 100, places=1)
        self.assertEqual(data["shards"], 3)
        self.assertIn("week", data)

    def test_summarize_never_fails_the_gate(self):
        # Aggregate score far below threshold -> still exit 0 (informational).
        shards = self.tmp / "shards"
        write_mutants_out(shards / "s0", caught=1, missed=99)
        r = self.run_summarize(shards)
        self.assertEqual(r.returncode, EXIT_PASS, r.stdout + r.stderr)

    def test_summarize_empty_dir_is_infra_error(self):
        shards = self.tmp / "shards"
        shards.mkdir()
        r = self.run_summarize(shards)
        self.assertEqual(r.returncode, EXIT_INFRA, r.stdout + r.stderr)

    def test_summarize_writes_combined_missed_list(self):
        shards = self.make_shards()
        r = self.run_summarize(shards)
        self.assertEqual(r.returncode, EXIT_PASS)
        combined = (shards / "combined-missed.txt").read_text()
        self.assertEqual(len(combined.splitlines()), 20)

    def test_summarize_week_flag_recorded_in_score_json(self):
        # The workflow pins WEEK once in a setup job and passes it down;
        # summarize must record exactly that value, not time.time().
        shards = self.make_shards()
        r = self.run_summarize(shards, "--week", "2948")
        self.assertEqual(r.returncode, EXIT_PASS, r.stdout + r.stderr)
        data = json.loads((shards / "score.json").read_text())
        self.assertEqual(data["week"], 2948)

    def test_summarize_partial_when_fewer_shards_than_expected(self):
        shards = self.make_shards()  # 3 shard artifacts
        r = self.run_summarize(shards, "--expected-shards", "12")
        self.assertEqual(r.returncode, EXIT_PASS, r.stdout + r.stderr)
        self.assertIn("PARTIAL", r.stdout)
        data = json.loads((shards / "score.json").read_text())
        self.assertIs(data["partial"], True)
        self.assertEqual(data["expected_shards"], 12)

    def test_summarize_not_partial_when_expected_met(self):
        shards = self.make_shards()
        r = self.run_summarize(shards, "--expected-shards", "3")
        self.assertEqual(r.returncode, EXIT_PASS, r.stdout + r.stderr)
        self.assertNotIn("PARTIAL", r.stdout)
        data = json.loads((shards / "score.json").read_text())
        self.assertIs(data["partial"], False)

    def test_summarize_reports_skipped_shard_dirs(self):
        # A shard directory with no outcome files (e.g. empty artifact from a
        # crashed job) must be surfaced, not silently dropped.
        shards = self.make_shards()
        (shards / "mutants-weekly-shard-3").mkdir()
        r = self.run_summarize(shards, "--expected-shards", "4")
        self.assertEqual(r.returncode, EXIT_PASS, r.stdout + r.stderr)
        self.assertIn("PARTIAL", r.stdout)
        self.assertIn("mutants-weekly-shard-3", r.stdout)
        data = json.loads((shards / "score.json").read_text())
        self.assertIs(data["partial"], True)
        self.assertEqual(data["skipped_shards"], ["mutants-weekly-shard-3"])
        self.assertEqual(data["shards"], 3)


if __name__ == "__main__":
    unittest.main(verbosity=2)
