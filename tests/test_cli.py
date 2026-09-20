import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from tests.test_jev import fake_api, success

ROOT = Path(__file__).resolve().parents[1]


@unittest.skipUnless(shutil.which("rg"), "ripgrep is required for CLI integration tests")
class CliTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "policy.py").write_text("def retry():\n    return status >= 500\n")
        (self.root / "metrics.py").write_text('retry_metric = "attempts"\n')

    def run_cli(self, *args, key=""):
        env = {**os.environ, "PYTHONPATH": str(ROOT), "TYPESAFE_API_KEY": key}
        return subprocess.run(
            [sys.executable, "-m", "jrg", *args],
            cwd=self.root,
            env=env,
            text=True,
            capture_output=True,
            timeout=15,
        )

    def test_dry_run_is_local_and_ignores_top_and_threshold(self):
        result = self.run_cli(
            "retry",
            "--about",
            "再試行判断",
            "--dry-run",
            "--json",
            "--stats",
            "--top",
            "1",
            "--min-relevance",
            "1",
            "--api-url",
            "invalid",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        rows = json.loads(result.stdout)
        self.assertEqual(len(rows), 2)
        self.assertTrue(all(row["relevance"] is None for row in rows))
        stats = json.loads(result.stderr)
        self.assertEqual(stats["api_requests"], 0)
        self.assertEqual(stats["mode"], "dry-run")

    def test_complete_cli_ranking_and_threshold(self):
        with fake_api(success) as (url, requests):
            result = self.run_cli(
                "retry",
                "--about",
                "HTTP retry decision",
                ".",
                "--json",
                "--stats",
                "--api-url",
                url,
                "--min-relevance",
                "0.5",
                key="test",
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        rows = json.loads(result.stdout)
        self.assertEqual([row["path"] for row in rows], ["./policy.py"])
        self.assertEqual(rows[0]["relevance"], 0.97)
        self.assertEqual(json.loads(result.stderr)["input_tokens"], 100)
        self.assertEqual(len(requests), 1)

    def test_no_matches_and_threshold_empty_result_have_exit_one(self):
        result = self.run_cli("absent", "--about", "retry", "--json")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(json.loads(result.stdout), [])
        with fake_api(success) as (url, _):
            result = self.run_cli(
                "retry",
                "--about",
                "retry",
                "--api-url",
                url,
                "--json",
                "--min-relevance",
                "1",
                key="test",
            )
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(json.loads(result.stdout), [])

    def test_missing_key_and_api_error_never_emit_unranked_results(self):
        result = self.run_cli("retry", "--about", "retry", "--json")
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, "")
        self.assertIn("TYPESAFE_API_KEY", result.stderr)
        with fake_api(lambda *_: (401, {}, {})) as (url, _):
            result = self.run_cli(
                "retry", "--about", "retry", "--json", "--api-url", url, key="test"
            )
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, "")
        self.assertNotIn("Traceback", result.stderr)

    def test_truncation_warns_and_is_present_in_stats(self):
        result = self.run_cli(
            "retry", "--about", "retry", "--dry-run", "--json", "--stats", "--max-candidates", "1"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(json.loads(result.stdout)), 1)
        self.assertIn("candidate limit", result.stderr)
        self.assertTrue(json.loads(result.stderr.splitlines()[-1])["candidate_limit_reached"])

    def test_later_batch_failure_does_not_emit_partial_results(self):
        def responder(payload, count):
            return success(payload, count) if count == 1 else (401, {}, {})

        with fake_api(responder) as (url, requests):
            result = self.run_cli(
                "retry",
                "--about",
                "retry",
                "--json",
                "--api-url",
                url,
                "--batch-size",
                "1",
                "--workers",
                "1",
                key="test",
            )
        self.assertEqual(len(requests), 2)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, "")

    def test_text_mode_shows_locations_and_unscored_label(self):
        result = self.run_cli("retry", "--about", "retry", "policy.py", "--dry-run")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("unscored policy.py:1-2", result.stdout)
        self.assertIn("def retry()", result.stdout)

    def test_invalid_arguments_fail_before_search(self):
        for args in [
            ("--about", " "),
            ("--about", "retry", "--top", "0"),
            ("--about", "retry", "--min-relevance", "nan"),
            ("--about", "retry", "--timeout", "inf"),
        ]:
            with self.subTest(args=args):
                result = self.run_cli("retry", *args)
                self.assertEqual(result.returncode, 2)
                self.assertEqual(result.stdout, "")
