import json
import threading
import unittest
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

from jrg.jev import MAX_REQUEST_BYTES, JevClient, JevError, make_batches
from jrg.search import Candidate


def candidate(name="policy.py", snippet="retry()\n"):
    return Candidate(name, 1, 1, (1,), snippet)


@contextmanager
def fake_api(responder):
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append((self.path, dict(self.headers), payload))
            status, body, headers = responder(payload, len(requests))
            self.send_response(status)
            for name, value in headers.items():
                self.send_header(name, value)
            self.end_headers()
            self.wfile.write(body if isinstance(body, bytes) else json.dumps(body).encode())

        def log_message(self, *args):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}/v1/systemone", requests
    finally:
        server.shutdown()
        server.server_close()
        worker.join()


def success(payload, count):
    scores = {"policy.py": 0.97, "metrics.py": 0.12}
    return (
        200,
        {
            "model": "jev-test",
            "answers": {
                key: {"type": "noul", "noul": scores.get(Path(item["path"]).name, 0.8)}
                for key, item in zip(
                    payload["questions"], payload["state"]["candidates"], strict=True
                )
            },
            "usage": {"input_tokens": 100, "output_tokens": 20},
        },
        {},
    )


class JevTests(unittest.TestCase):
    def test_http_contract_ranking_and_usage(self):
        with fake_api(success) as (url, requests):
            client = JevClient("test-key", api_url=url)
            result = client.rank([candidate("metrics.py"), candidate()], "HTTPの再試行判断")
        self.assertEqual([r.candidate.path for r in result.results], ["policy.py", "metrics.py"])
        self.assertEqual(result.results[0].relevance, 0.97)
        self.assertEqual(result.input_tokens, 100)
        self.assertEqual(result.models, ["jev-test"])
        self.assertEqual(result.requests, 1)
        path, headers, payload = requests[0]
        self.assertEqual(path, "/v1/systemone")
        self.assertEqual(headers["Authorization"], "Bearer test-key")
        self.assertEqual(payload["model"], "jev-latest")
        self.assertEqual(payload["state"]["intent"], "HTTPの再試行判断")
        self.assertIn(
            "`candidates[1].snippet`", payload["questions"]["candidate_1"]["instructions"]
        )

    def test_multiple_batches_sort_ties_deterministically(self):
        with fake_api(success) as (url, requests):
            result = JevClient("test", api_url=url, batch_size=1, workers=2).rank(
                [candidate("z.py"), candidate("a.py"), candidate("b.py")], "retry policy"
            )
        self.assertEqual(len(requests), 3)
        self.assertEqual(result.input_tokens, 300)
        self.assertEqual([r.candidate.path for r in result.results], ["a.py", "b.py", "z.py"])

    def test_request_byte_budget_includes_non_ascii_and_questions(self):
        items = [candidate(f"{n}.py", "再試行" * 500) for n in range(5)]
        batches = make_batches(items, "再試行を探す", "jev-latest", 8)
        self.assertGreater(len(batches), 1)
        self.assertEqual(sum(len(batch) for batch, _ in batches), 5)
        self.assertTrue(all(len(body) <= MAX_REQUEST_BYTES for _, body in batches))
        with self.assertRaisesRegex(JevError, "budget"):
            make_batches([candidate(snippet="a" * MAX_REQUEST_BYTES)], "retry", "jev-latest", 8)

    def test_rate_limit_and_overload_retried_with_backoff(self):
        for status in [429, 529]:
            with self.subTest(status=status):

                def overloaded(payload, count, status=status):
                    return (
                        (status, {}, {"Retry-After": "1"})
                        if count == 1
                        else success(payload, count)
                    )

                with fake_api(overloaded) as (url, requests), patch("jrg.jev.time.sleep") as sleep:
                    result = JevClient("test", api_url=url).rank([candidate()], "retry")
                self.assertEqual(result.requests, 2)
                sleep.assert_called_once_with(1.0)

    def test_no_retry_for_unauthorized_and_no_response_body_leak(self):
        with fake_api(lambda *_: (401, {"secret": "sensitive source"}, {})) as (url, requests):
            with self.assertRaisesRegex(JevError, "401") as error:
                JevClient("test", api_url=url).rank([candidate()], "retry")
        self.assertEqual(len(requests), 1)
        self.assertNotIn("sensitive", str(error.exception))

    def test_exhausted_retries_fail(self):
        with fake_api(lambda *_: (529, {}, {})) as (url, requests), patch("jrg.jev.time.sleep"):
            with self.assertRaisesRegex(JevError, "529"):
                JevClient("test", api_url=url, retries=1).rank([candidate()], "retry")
        self.assertEqual(len(requests), 2)

    def test_invalid_or_missing_judgments_fail(self):
        for answer in [
            None,
            {},
            {"type": "score", "noul": 0.5},
            {"type": "noul", "noul": True},
            {"type": "noul", "noul": "0.5"},
            {"type": "noul", "noul": 1.2},
            {"type": "noul", "noul": float("nan")},
        ]:
            with self.subTest(answer=answer):
                body = {"answers": {"candidate_0": answer}}
                with fake_api(lambda *_, body=body: (200, body, {})) as (url, _):
                    with self.assertRaisesRegex(JevError, "invalid Noul"):
                        JevClient("test", api_url=url).rank([candidate()], "retry")

    def test_invalid_json_unknown_usage_and_redirect(self):
        with fake_api(lambda *_: (200, b"not JSON", {})) as (url, _):
            with self.assertRaisesRegex(JevError, "invalid JSON"):
                JevClient("test", api_url=url).rank([candidate()], "retry")
        body = {"answers": {"candidate_0": {"type": "noul", "noul": 0.5}}}
        with fake_api(lambda *_: (200, body, {})) as (url, _):
            self.assertIsNone(
                JevClient("test", api_url=url).rank([candidate()], "retry").input_tokens
            )
        with fake_api(lambda *_: (307, {}, {"Location": "https://example.invalid"})) as (
            url,
            requests,
        ):
            with self.assertRaisesRegex(JevError, "307"):
                JevClient("test", api_url=url).rank([candidate()], "retry")
        self.assertEqual(len(requests), 1)

    def test_key_and_endpoint_validation(self):
        with self.assertRaisesRegex(JevError, "TYPESAFE_API_KEY"):
            JevClient("")
        with self.assertRaisesRegex(JevError, "HTTPS"):
            JevClient("test", api_url="http://example.com")

    def test_ambiguous_timeout_is_not_retried(self):
        with patch("jrg.jev.build_opener") as build:
            build.return_value.open.side_effect = TimeoutError
            with self.assertRaisesRegex(JevError, "timed out"):
                JevClient("test").rank([candidate()], "retry")
        self.assertEqual(build.return_value.open.call_count, 1)
