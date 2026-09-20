"""Provide human-readable and machine-readable intent-aware search."""

import argparse
import json
import math
import os
import sys
import time

from . import __version__
from .jev import DEFAULT_API_URL, JevClient, JevError
from .search import SearchError, search


def _positive(value: str) -> int:
    """Reject unbounded or meaningless count settings before searching."""
    number = int(value)
    if number < 1:
        raise argparse.ArgumentTypeError("must be a positive integer")
    return number


def _nonnegative(value: str) -> int:
    """Accept zero where disabling context or retries is meaningful."""
    number = int(value)
    if number < 0:
        raise argparse.ArgumentTypeError("must be a nonnegative integer")
    return number


def _probability(value: str) -> float:
    """Validate thresholds including non-finite floating point input."""
    number = float(value)
    if not math.isfinite(number) or not 0 <= number <= 1:
        raise argparse.ArgumentTypeError("must be between 0 and 1")
    return number


def _timeout(value: str) -> float:
    """Require a finite deadline for each HTTP operation."""
    number = float(value)
    if not math.isfinite(number) or number <= 0:
        raise argparse.ArgumentTypeError("must be finite and greater than zero")
    return number


def parser() -> argparse.ArgumentParser:
    """Describe the prototype's explicit search and API boundaries."""
    result = argparse.ArgumentParser(
        prog="jrg",
        description="Search with ripgrep, then rerank by intent using Jev.",
        epilog="Normal searches send candidate paths and source snippets to TypeSafe. "
        "Use --dry-run for a local preview. Exit: 0 results, 1 no results, 2 error.",
    )
    result.add_argument("pattern", help="ripgrep regular expression (or literal with -F)")
    result.add_argument("paths", nargs="*", default=["."], help="files/directories (default: .)")
    result.add_argument("--about", "--intent", required=True, help="description of the code sought")
    result.add_argument("-C", "--context", type=_nonnegative, default=5)
    result.add_argument("-n", "--top", type=_positive, default=10)
    result.add_argument(
        "--min-relevance",
        type=_probability,
        default=0,
        help="inclusive Noul threshold (default: 0; retain uncertain hits)",
    )
    result.add_argument("--max-candidates", type=_positive, default=200)
    result.add_argument("--max-filesize", default="1M", help="rg file size limit (default: 1M)")
    result.add_argument("-g", "--glob", action="append", default=[])
    result.add_argument("-t", "--type", action="append", default=[], dest="file_types")
    result.add_argument("-i", "--ignore-case", action="store_true")
    result.add_argument("-F", "--fixed-strings", action="store_true")
    result.add_argument("--hidden", action="store_true")
    result.add_argument("--json", action="store_true", help="emit a JSON array on stdout")
    result.add_argument("--stats", action="store_true", help="emit JSON metrics on stderr")
    result.add_argument(
        "--dry-run", action="store_true", help="show all collected candidates without API calls"
    )
    result.add_argument("--model", default="jev-latest")
    result.add_argument("--api-url", default=DEFAULT_API_URL)
    result.add_argument("--timeout", type=_timeout, default=30)
    result.add_argument("--retries", type=_nonnegative, default=2)
    result.add_argument("--workers", type=_positive, default=4)
    result.add_argument("--batch-size", type=_positive, default=8)
    result.add_argument("--version", action="version", version=f"jrg {__version__}")
    return result


def _terminal_text(text: str) -> str:
    """Keep source control characters from acting as terminal commands."""
    return "".join(
        char if char.isprintable() or char == "\t" else ascii(char)[1:-1] for char in text
    )


def main(argv: list[str] | None = None) -> int:
    """Run one search and keep diagnostics separate from consumable results."""
    arg_parser = parser()
    args = arg_parser.parse_intermixed_args(argv)
    if not args.about.strip():
        arg_parser.error("--about must not be empty")
    try:
        start = time.monotonic()
        found = search(
            args.pattern,
            args.paths,
            context=args.context,
            max_candidates=args.max_candidates,
            globs=args.glob,
            file_types=args.file_types,
            ignore_case=args.ignore_case,
            fixed_strings=args.fixed_strings,
            hidden=args.hidden,
            max_filesize=args.max_filesize,
        )
        search_ms = (time.monotonic() - start) * 1000
        if found.truncated:
            print(
                f"jrg: candidate limit ({args.max_candidates}) reached; "
                "narrow the pattern/paths or raise --max-candidates.",
                file=sys.stderr,
            )

        start = time.monotonic()
        ranking = None
        if args.dry_run:
            rows = [{**candidate.to_dict(), "relevance": None} for candidate in found.candidates]
        elif found.candidates:
            client = JevClient(
                os.environ.get("TYPESAFE_API_KEY", ""),
                api_url=args.api_url,
                model=args.model,
                timeout=args.timeout,
                retries=args.retries,
                workers=args.workers,
                batch_size=args.batch_size,
            )
            ranking = client.rank(found.candidates, args.about)
            rows = [
                item.to_dict() for item in ranking.results if item.relevance >= args.min_relevance
            ][: args.top]
        else:
            rows = []
        rerank_ms = (time.monotonic() - start) * 1000

        if args.json:
            # ASCII escapes preserve non-UTF-8 filesystem paths through JSON as well.
            print(json.dumps(rows, ensure_ascii=True, separators=(",", ":")))
        else:
            for row in rows:
                score = "unscored" if row["relevance"] is None else f"{row['relevance']:.3f}"
                print(
                    f"{score} {_terminal_text(row['path'])}:{row['start_line']}-{row['end_line']}"
                )
                for number, line in enumerate(row["snippet"].split("\n"), row["start_line"]):
                    if number > row["end_line"]:
                        break
                    marker = ">" if number in row["match_lines"] else " "
                    print(f" {marker} {number:5} {_terminal_text(line.rstrip(chr(13)))}")
                print()

        if args.stats:
            print(
                json.dumps(
                    {
                        "mode": "dry-run" if args.dry_run else "jev",
                        "candidates": len(found.candidates),
                        "candidate_limit_reached": found.truncated,
                        "candidate_match_lines": sum(len(c.match_lines) for c in found.candidates),
                        "results": len(rows),
                        "candidate_chars": sum(len(c.snippet) for c in found.candidates),
                        "returned_chars": sum(len(row["snippet"]) for row in rows),
                        "search_ms": round(search_ms, 2),
                        "rerank_ms": round(rerank_ms, 2),
                        "api_requests": ranking.requests if ranking else 0,
                        "input_tokens": ranking.input_tokens if ranking else 0,
                        "models": ranking.models if ranking else [],
                    }
                ),
                file=sys.stderr,
            )
        return 0 if rows else 1
    except (SearchError, JevError, OSError, ValueError) as exc:
        print(f"jrg: {exc}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        return 130
