"""Build bounded source candidates from ripgrep's search snapshot."""

import base64
import json
import os
import subprocess
import tempfile
from collections.abc import Iterable, Iterator
from dataclasses import asdict, dataclass


class SearchError(Exception):
    """Report a search failure without returning misleading partial results."""


@dataclass(frozen=True)
class Candidate:
    """Keep source coordinates with the exact snippet that was judged."""

    path: str
    start_line: int
    end_line: int
    match_lines: tuple[int, ...]
    snippet: str

    def to_dict(self) -> dict:
        """Expose a JSON-compatible candidate for API and CLI consumers."""
        return asdict(self)


@dataclass(frozen=True)
class SearchResult:
    """Distinguish a complete candidate set from a bounded search prefix."""

    candidates: list[Candidate]
    truncated: bool


def _rg_text(value: dict, *, path: bool = False) -> str:
    """Decode ripgrep's alternate byte representation without corrupting paths."""
    if "text" in value:
        return value["text"]
    raw = base64.b64decode(value["bytes"], validate=True)
    if path:
        return os.fsdecode(raw)
    try:
        return raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise SearchError("Non-UTF-8 source encountered; narrow the search with --glob.") from exc


def candidates_from_events(
    events: Iterable[str], *, max_lines: int = 120, max_bytes: int = 12_000
) -> Iterator[Candidate]:
    """Merge contiguous context while bounding dense matches before API submission."""
    path = ""
    numbers: list[int] = []
    lines: list[str] = []
    matches: list[int] = []
    size = 0

    for event in events:
        obj = json.loads(event)
        kind = obj["type"]
        if kind not in {"match", "context", "end"}:
            continue
        data = obj["data"]
        next_path = _rg_text(data["path"], path=True)
        number = data.get("line_number")
        line = _rg_text(data["lines"]) if kind != "end" else ""
        line_size = len(line.encode("utf-8"))
        if line_size > max_bytes:
            raise SearchError(
                f"Source line exceeds {max_bytes} bytes at {next_path}:{number}; "
                "exclude this file with --glob."
            )

        # Use rg's emitted source instead of reopening files: a concurrent edit must
        # not pair old match locations with different code sent to Jev.
        boundary = numbers and (
            kind == "end"
            or next_path != path
            or number != numbers[-1] + 1
            or len(lines) >= max_lines
            or size + line_size > max_bytes
        )
        if boundary:
            if matches:
                yield Candidate(path, numbers[0], numbers[-1], tuple(matches), "".join(lines))
            numbers, lines, matches, size = [], [], [], 0

        if kind == "end":
            continue
        path = next_path
        numbers.append(number)
        lines.append(line)
        size += line_size
        if kind == "match":
            matches.append(number)

    if matches:
        yield Candidate(path, numbers[0], numbers[-1], tuple(matches), "".join(lines))


def search(
    pattern: str,
    paths: list[str],
    *,
    context: int = 5,
    max_candidates: int = 200,
    globs: list[str] | None = None,
    file_types: list[str] | None = None,
    ignore_case: bool = False,
    fixed_strings: bool = False,
    hidden: bool = False,
    max_filesize: str = "1M",
) -> SearchResult:
    """Collect a deterministic, bounded prefix of ripgrep candidates."""
    if context < 0 or max_candidates < 1:
        raise ValueError("context must be nonnegative and max_candidates must be positive")
    if "-" in paths:
        raise SearchError("stdin search is unsupported; pass file or directory paths.")

    # Disable personal rg config so preprocessors and output-changing flags cannot
    # silently alter the source being sent. Sorting makes candidate limits repeatable.
    command = [
        "rg",
        "--no-config",
        "--json",
        "--sort",
        "path",
        "--color",
        "never",
        "--context",
        str(context),
        "--max-filesize",
        max_filesize,
    ]
    for glob in globs or []:
        command.extend(["--glob", glob])
    for file_type in file_types or []:
        command.extend(["--type", file_type])
    if ignore_case:
        command.append("--ignore-case")
    if fixed_strings:
        command.append("--fixed-strings")
    if hidden:
        command.append("--hidden")
    command.extend(["--regexp", pattern, "--", *(paths or ["."])])

    candidates: list[Candidate] = []
    truncated = False
    # A tempfile prevents stderr backpressure while stdout is consumed incrementally.
    with tempfile.TemporaryFile() as errors:
        try:
            process = subprocess.Popen(
                command,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=errors,
                text=True,
                encoding="utf-8",
            )
        except FileNotFoundError as exc:
            raise SearchError("ripgrep (rg) is required; install it and add it to PATH.") from exc
        try:
            assert process.stdout is not None
            for candidate in candidates_from_events(process.stdout):
                if len(candidates) == max_candidates:
                    truncated = True
                    break
                candidates.append(candidate)
            if not truncated:
                process.wait()
        finally:
            if process.poll() is None:
                process.terminate()
            process.wait()
            if process.stdout:
                process.stdout.close()
        errors.seek(0)
        diagnostic = errors.read().decode("utf-8", errors="replace").strip()
        if diagnostic or (not truncated and process.returncode not in (0, 1)):
            raise SearchError(diagnostic or f"ripgrep exited with status {process.returncode}")
    return SearchResult(candidates, truncated)
