import base64
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from jrg.search import SearchError, candidates_from_events, search


def event(kind, number=None, text=None, path="source.py"):
    data = {"path": {"text": path}}
    if number is not None:
        data.update(line_number=number, lines={"text": text})
    return json.dumps({"type": kind, "data": data})


class CandidateTests(unittest.TestCase):
    def test_merge_nearby_hits_and_preserve_separate_regions_and_files(self):
        events = [
            event("context", 1, "前置き\n"),
            event("match", 2, "retry()\n"),
            event("match", 3, "retry_again()\n"),
            event("context", 4, "done\n"),
            event("match", 20, "other_retry()\n"),
            event("end"),
            event("match", 1, "retry()", path="other.py"),
        ]
        candidates = list(candidates_from_events(events))
        self.assertEqual(len(candidates), 3)
        self.assertEqual(candidates[0].match_lines, (2, 3))
        self.assertEqual((candidates[0].start_line, candidates[0].end_line), (1, 4))
        self.assertEqual(candidates[0].snippet, "前置き\nretry()\nretry_again()\ndone\n")
        self.assertEqual(candidates[-1].snippet, "retry()")

    def test_dense_matches_split_without_losing_matching_lines(self):
        events = [event("match", n, f"retry {n}\n") for n in range(1, 8)]
        candidates = list(candidates_from_events(events, max_lines=3))
        self.assertEqual([c.match_lines for c in candidates], [(1, 2, 3), (4, 5, 6), (7,)])

    def test_context_only_chunks_do_not_become_candidates(self):
        events = [event("context", n, "context\n") for n in range(1, 5)]
        events.append(event("match", 5, "retry\n"))
        candidates = list(candidates_from_events(events, max_lines=2))
        self.assertEqual(len(candidates), 1)
        self.assertEqual(candidates[0].match_lines, (5,))

    def test_byte_budget_splits_and_oversize_line_is_explicit_error(self):
        events = [event("match", n, "再試行\n") for n in range(1, 4)]
        self.assertEqual(len(list(candidates_from_events(events, max_bytes=20))), 2)
        with self.assertRaisesRegex(SearchError, "exceeds"):
            list(candidates_from_events(events, max_bytes=3))

    def test_base64_source_and_non_utf8_filename(self):
        obj = json.loads(event("match", 1, ""))
        obj["data"]["lines"] = {"bytes": base64.b64encode(b"retry\n").decode()}
        raw_path = b"source-\xff.py"
        obj["data"]["path"] = {"bytes": base64.b64encode(raw_path).decode()}
        candidate = list(candidates_from_events([json.dumps(obj)]))[0]
        self.assertEqual(os.fsencode(candidate.path), raw_path)
        obj["data"]["lines"] = {"bytes": base64.b64encode(b"\xffretry\n").decode()}
        with self.assertRaisesRegex(SearchError, "Non-UTF-8"):
            list(candidates_from_events([json.dumps(obj)]))


@unittest.skipUnless(shutil.which("rg"), "ripgrep is required for integration tests")
class RipgrepTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def write(self, name, content):
        path = self.root / name
        path.write_text(content)
        return str(path)

    def test_real_rg_context_merge_and_eof(self):
        path = self.write("source file.py", "before\nretry()\nafter\nretry()\nlast")
        found = search("retry", [path], context=1)
        self.assertFalse(found.truncated)
        self.assertEqual(len(found.candidates), 1)
        self.assertEqual(found.candidates[0].match_lines, (2, 4))
        self.assertEqual(found.candidates[0].end_line, 5)
        self.assertTrue(found.candidates[0].snippet.endswith("last"))

    def test_exact_limit_and_truncated_limit_are_distinct(self):
        path = self.write("dense.py", "retry\nnone\nretry\nnone\nretry\n")
        found = search("retry", [path], context=0, max_candidates=2)
        self.assertEqual(len(found.candidates), 2)
        self.assertTrue(found.truncated)
        complete = search("retry", [path], context=0, max_candidates=3)
        self.assertFalse(complete.truncated)

    def test_ignore_hidden_binary_and_glob_behavior(self):
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        self.write(".gitignore", "ignored.py\n")
        for name in ["visible.py", "other.rs", "ignored.py", ".secret.py"]:
            self.write(name, "retry\n")
        (self.root / "binary.bin").write_bytes(b"\0retry\n")
        found = search("retry", [str(self.root)])
        self.assertEqual([Path(c.path).name for c in found.candidates], ["other.rs", "visible.py"])
        found = search("retry", [str(self.root)], globs=["*.py"])
        # Positive rg globs explicitly opt matching files into the search, even
        # when those files would ordinarily be hidden or ignored.
        self.assertEqual(
            [Path(c.path).name for c in found.candidates],
            [".secret.py", "ignored.py", "visible.py"],
        )
        found = search("retry", [str(self.root)], hidden=True, file_types=["py"])
        self.assertEqual(
            [Path(c.path).name for c in found.candidates], [".secret.py", "visible.py"]
        )

    def test_personal_rg_config_is_ignored(self):
        source = self.write("source.py", "retry\n")
        config = self.write("rg.conf", "--files-with-matches\n")
        with patch.dict(os.environ, {"RIPGREP_CONFIG_PATH": config}):
            self.assertEqual(len(search("retry", [source]).candidates), 1)

    def test_option_like_pattern_is_not_executed_as_a_flag(self):
        source = self.write("source.py", "--help\nRetry[\n")
        self.assertEqual(len(search("--help", [source]).candidates), 1)
        found = search("retry[", [source], fixed_strings=True, ignore_case=True)
        self.assertEqual(found.candidates[0].match_lines, (2,))

    def test_no_matches_errors_and_file_size_limit(self):
        source = self.write("source.py", "retry\n")
        self.assertEqual(search("absent", [source]).candidates, [])
        self.write("tiny.py", "x")
        self.assertEqual(search("retry", [str(self.root)], max_filesize="1").candidates, [])
        with self.assertRaises(SearchError):
            search("[", [source])
        with self.assertRaises(SearchError):
            search("retry", [str(self.root / "missing.py")])
        with self.assertRaisesRegex(SearchError, "stdin"):
            search("retry", ["-"])

    def test_missing_rg_has_actionable_error(self):
        with patch("jrg.search.subprocess.Popen", side_effect=FileNotFoundError):
            with self.assertRaisesRegex(SearchError, "ripgrep.*required"):
                search("retry", ["."])
