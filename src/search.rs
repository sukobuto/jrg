//! Collect bounded candidates from the same source snapshot emitted by ripgrep.

use crate::{check_cancelled, config::Exclusions};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::AtomicBool,
};

/// Preserve source coordinates with the exact snippet that will be judged.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Candidate {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub match_lines: Vec<usize>,
    pub snippet: String,
}

/// Keep bounded searches distinguishable from complete searches.
pub struct SearchResult {
    pub candidates: Vec<Candidate>,
    pub truncated: bool,
}

/// Describe only rg options with supported candidate semantics.
pub struct SearchOptions {
    pub pattern: String,
    pub paths: Vec<PathBuf>,
    pub context: usize,
    pub max_candidates: usize,
    pub globs: Vec<String>,
    pub file_types: Vec<String>,
    pub ignore_case: bool,
    pub fixed_strings: bool,
    pub hidden: bool,
    pub max_filesize: String,
}

#[derive(Deserialize)]
struct RgText {
    text: Option<String>,
    bytes: Option<String>,
}

impl RgText {
    fn decode(self) -> Result<String> {
        if let Some(text) = self.text {
            return Ok(text);
        }
        let bytes = STANDARD
            .decode(self.bytes.context("Invalid ripgrep text object")?)
            .context("Invalid ripgrep base64 data")?;
        String::from_utf8(bytes).map_err(|_| {
            anyhow::anyhow!("Non-UTF-8 source or path encountered; narrow the search with --glob.")
        })
    }
}

#[derive(Deserialize)]
struct Event {
    #[serde(rename = "type")]
    kind: String,
    data: serde_json::Value,
}

#[derive(Deserialize)]
struct LineData {
    path: RgText,
    line_number: Option<usize>,
    lines: Option<RgText>,
}

#[derive(Default)]
struct Region {
    path: String,
    start: usize,
    end: usize,
    matches: Vec<usize>,
    snippet: String,
}

impl Region {
    fn finish(&mut self) -> Option<Candidate> {
        let region = std::mem::take(self);
        (!region.matches.is_empty()).then_some(Candidate {
            path: region.path,
            start_line: region.start,
            end_line: region.end,
            match_lines: region.matches,
            snippet: region.snippet,
        })
    }
}

fn collect(
    reader: impl BufRead,
    options: &SearchOptions,
    excluded: &Exclusions,
    cancelled: &AtomicBool,
) -> Result<SearchResult> {
    let mut region = Region::default();
    let mut candidates = Vec::new();
    for line in reader.lines() {
        check_cancelled(cancelled)?;
        let event: Event = serde_json::from_str(&line?).context("Invalid ripgrep JSON")?;
        if !matches!(event.kind.as_str(), "match" | "context" | "end") {
            continue;
        }
        let data: LineData = serde_json::from_value(event.data).context("Invalid ripgrep event")?;
        let path = data.path.decode()?;
        if excluded.contains(Path::new(&path)) {
            continue;
        }
        let number = data.line_number.unwrap_or_default();
        let text = match data.lines {
            Some(text) => text.decode()?,
            None => String::new(),
        };
        if text.len() > 12_000 {
            bail!(
                "Source line exceeds 12000 bytes at {path}:{number}; exclude this file with --glob."
            );
        }
        // Reopening source files could pair old match positions with edited code.
        // rg's context stream also merges nearby hits without duplicate source reads.
        let boundary = region.start != 0
            && (event.kind == "end"
                || path != region.path
                || number != region.end + 1
                || region.end - region.start + 1 >= 120
                || region.snippet.len() + text.len() > 12_000);
        if boundary && let Some(candidate) = region.finish() {
            if candidates.len() == options.max_candidates {
                return Ok(SearchResult {
                    candidates,
                    truncated: true,
                });
            }
            candidates.push(candidate);
        }
        if event.kind == "end" {
            continue;
        }
        if number == 0 {
            bail!("ripgrep omitted the source line number");
        }
        if region.start == 0 {
            region.start = number;
            region.path = path;
        }
        region.end = number;
        region.snippet.push_str(&text);
        if event.kind == "match" {
            region.matches.push(number);
        }
    }
    if let Some(candidate) = region.finish() {
        if candidates.len() == options.max_candidates {
            return Ok(SearchResult {
                candidates,
                truncated: true,
            });
        }
        candidates.push(candidate);
    }
    Ok(SearchResult {
        candidates,
        truncated: false,
    })
}

/// Reap rg on every exit path, including malformed source and cancellation.
struct SearchChild(Child);

impl Drop for SearchChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Find a deterministic candidate prefix without retaining rg's full output.
pub fn search(
    options: &SearchOptions,
    excluded: &Exclusions,
    cancelled: &AtomicBool,
) -> Result<SearchResult> {
    if options.paths.iter().any(|path| path == Path::new("-")) {
        bail!("stdin search is unsupported; pass file or directory paths.");
    }
    let mut command = Command::new("rg");
    // User rg configuration can enable preprocessors or change output semantics.
    // Path sorting makes the candidate cap independent of worker scheduling.
    command
        .args([
            "--no-config",
            "--json",
            "--sort",
            "path",
            "--color",
            "never",
            "--context",
        ])
        .arg(options.context.to_string())
        .args(["--max-filesize", &options.max_filesize]);
    for glob in &options.globs {
        command.args(["--glob", glob]);
    }
    for file_type in &options.file_types {
        command.args(["--type", file_type]);
    }
    if options.ignore_case {
        command.arg("--ignore-case");
    }
    if options.fixed_strings {
        command.arg("--fixed-strings");
    }
    if options.hidden {
        command.arg("--hidden");
    }
    // Enforce credential exclusions on events: explicit input files can bypass
    // rg's glob exclusions, so command-line globs alone cannot protect credentials.
    command.args(["--regexp", &options.pattern, "--"]);
    if options.paths.is_empty() {
        command.arg(".");
    } else {
        command.args(&options.paths);
    }
    // stderr must drain even while stdout is being parsed, otherwise an unreadable
    // directory can fill its pipe and deadlock both processes.
    let mut errors: File = tempfile::tempfile().context("Could not create rg diagnostic buffer")?;
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(errors.try_clone()?)
        .spawn()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!("ripgrep (rg) is required; install it and add it to PATH.")
            } else {
                anyhow::anyhow!("Could not start ripgrep: {error}")
            }
        })?;
    let mut child = SearchChild(child);
    let stdout = child
        .0
        .stdout
        .take()
        .context("Could not read ripgrep output")?;
    let found = collect(BufReader::new(stdout), options, excluded, cancelled)?;
    if found.truncated {
        let _ = child.0.kill();
    }
    let status = child.0.wait()?;
    check_cancelled(cancelled)?;
    errors.rewind()?;
    let mut diagnostic = String::new();
    errors.read_to_string(&mut diagnostic)?;
    if !diagnostic.trim().is_empty() {
        bail!("{}", diagnostic.trim());
    }
    if !found.truncated && !matches!(status.code(), Some(0 | 1)) {
        bail!("ripgrep exited with status {status}");
    }
    Ok(found)
}
