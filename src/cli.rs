//! Keep the Python prototype's CLI and JSON contract while adding project credentials.

use crate::{
    check_cancelled,
    config::{self, Exclusions},
    jev::{self, RankedCandidate},
    search::{self, SearchOptions},
};
use anyhow::Result;
use clap::Parser;
use serde_json::json;
use std::{
    io::{self, Write},
    path::PathBuf,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

/// Find lexical candidates and rerank them by the described behavior.
#[derive(Parser)]
#[command(
    version,
    about = "Search with ripgrep, then rerank by intent using Jev.",
    after_help = "Normal searches send candidate paths and source snippets to TypeSafe. Use --dry-run for a local preview. Exit: 0 results, 1 no results, 2 error, 130 interrupted."
)]
pub struct Args {
    /// ripgrep regular expression (or literal with -F)
    pub pattern: String,
    /// Files/directories (default: .)
    pub paths: Vec<PathBuf>,
    /// Description of the code sought
    #[arg(long, visible_alias = "intent", value_parser = nonempty)]
    pub about: String,
    #[arg(short = 'C', long, default_value_t = 5)]
    pub context: usize,
    #[arg(short = 'n', long, default_value = "10", value_parser = positive)]
    pub top: usize,
    /// Inclusive Noul threshold; zero retains uncertain hits
    #[arg(long, default_value = "0", value_parser = probability)]
    pub min_relevance: f64,
    #[arg(long, default_value = "200", value_parser = positive)]
    pub max_candidates: usize,
    #[arg(long, default_value = "1M")]
    pub max_filesize: String,
    #[arg(short = 'g', long = "glob")]
    pub globs: Vec<String>,
    #[arg(short = 't', long = "type")]
    pub file_types: Vec<String>,
    #[arg(short = 'i', long)]
    pub ignore_case: bool,
    #[arg(short = 'F', long)]
    pub fixed_strings: bool,
    #[arg(long)]
    pub hidden: bool,
    /// Emit a JSON array on stdout
    #[arg(long)]
    pub json: bool,
    /// Emit JSON metrics on stderr
    #[arg(long)]
    pub stats: bool,
    /// Show all collected candidates without API calls or reading the key
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long, default_value = "jev-latest")]
    pub model: String,
    #[arg(long, default_value = jev::DEFAULT_API_URL)]
    pub api_url: String,
    #[arg(long, default_value = "30", value_parser = timeout)]
    pub timeout: f64,
    #[arg(long, default_value_t = 2)]
    pub retries: u32,
    #[arg(long, default_value = "4", value_parser = positive)]
    pub workers: usize,
    #[arg(long, default_value = "8", value_parser = positive)]
    pub batch_size: usize,
    /// Read this dotenv file instead of discovering .jrgenv; environment takes precedence
    #[arg(long, conflicts_with = "no_env_file")]
    pub env_file: Option<PathBuf>,
    /// Use only the process environment for credentials
    #[arg(long)]
    pub no_env_file: bool,
}

fn nonempty(value: &str) -> std::result::Result<String, String> {
    if value.trim().is_empty() {
        Err("must not be empty".into())
    } else {
        Ok(value.into())
    }
}

fn positive(value: &str) -> std::result::Result<usize, String> {
    value
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or("must be a positive integer".into())
}

fn probability(value: &str) -> std::result::Result<f64, String> {
    value
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
        .ok_or("must be between 0 and 1".into())
}

fn timeout(value: &str) -> std::result::Result<f64, String> {
    value
        .parse::<f64>()
        .ok()
        .filter(|value| {
            value.is_finite() && *value > 0.0 && Duration::try_from_secs_f64(*value).is_ok()
        })
        .ok_or("must be a finite positive timeout".into())
}

fn terminal_text(text: &str) -> String {
    text.chars()
        .flat_map(|ch| {
            if ch.is_control() && ch != '\t' {
                ch.escape_default().collect::<Vec<_>>()
            } else {
                vec![ch]
            }
        })
        .collect()
}

fn print_results(rows: &[RankedCandidate], as_json: bool) -> Result<()> {
    let mut output = io::stdout().lock();
    if as_json {
        serde_json::to_writer(&mut output, rows)?;
        writeln!(output)?;
    } else {
        for row in rows {
            let candidate = &row.candidate;
            let score = row
                .relevance
                .map_or("unscored".into(), |score| format!("{score:.3}"));
            writeln!(
                output,
                "{score} {}:{}-{}",
                terminal_text(&candidate.path),
                candidate.start_line,
                candidate.end_line
            )?;
            for (offset, line) in candidate.snippet.split('\n').enumerate() {
                let number = candidate.start_line + offset;
                if number > candidate.end_line {
                    break;
                }
                let marker = if candidate.match_lines.contains(&number) {
                    '>'
                } else {
                    ' '
                };
                writeln!(
                    output,
                    " {marker} {number:5} {}",
                    terminal_text(line.trim_end_matches('\r'))
                )?;
            }
            writeln!(output)?;
        }
    }
    Ok(())
}

/// Execute one atomic search, keeping metadata and diagnostics off stdout.
pub fn run(args: Args, cancelled: &AtomicBool) -> Result<i32> {
    let cwd = std::env::current_dir()?;
    let env_file = if args.no_env_file {
        None
    } else {
        args.env_file
            .clone()
            .or_else(|| config::discover_env_file(&cwd))
    };
    let start = Instant::now();
    let options = SearchOptions {
        pattern: args.pattern,
        paths: args.paths,
        context: args.context,
        max_candidates: args.max_candidates,
        globs: args.globs,
        file_types: args.file_types,
        ignore_case: args.ignore_case,
        fixed_strings: args.fixed_strings,
        hidden: args.hidden,
        max_filesize: args.max_filesize,
    };
    let found = search::search(&options, &Exclusions::new(env_file.as_deref()), cancelled)?;
    let search_ms = start.elapsed().as_secs_f64() * 1000.0;
    if found.truncated {
        eprintln!(
            "jrg: candidate limit ({}) reached; narrow the pattern/paths or raise --max-candidates.",
            args.max_candidates
        );
    }
    let count = found.candidates.len();
    let match_lines: usize = found
        .candidates
        .iter()
        .map(|item| item.match_lines.len())
        .sum();
    let candidate_chars: usize = found
        .candidates
        .iter()
        .map(|item| item.snippet.chars().count())
        .sum();
    let start = Instant::now();
    let mut requests = 0;
    let mut input_tokens = Some(0);
    let mut models = Vec::new();
    let rows = if args.dry_run || found.candidates.is_empty() {
        found
            .candidates
            .into_iter()
            .map(|candidate| RankedCandidate {
                candidate,
                relevance: None,
            })
            .collect::<Vec<_>>()
    } else {
        let key = config::api_key(env_file.as_deref())?;
        let ranking = jev::rank(
            found.candidates,
            &args.about,
            &key,
            &jev::Options {
                api_url: args.api_url,
                model: args.model,
                timeout: Duration::from_secs_f64(args.timeout),
                retries: args.retries,
                workers: args.workers,
                batch_size: args.batch_size,
            },
            cancelled,
        )?;
        requests = ranking.requests;
        input_tokens = ranking.input_tokens;
        models = ranking.models;
        ranking
            .results
            .into_iter()
            .filter(|item| {
                item.relevance
                    .is_some_and(|score| score >= args.min_relevance)
            })
            .take(args.top)
            .collect()
    };
    let rerank_ms = start.elapsed().as_secs_f64() * 1000.0;
    check_cancelled(cancelled)?;
    print_results(&rows, args.json)?;
    if args.stats {
        eprintln!(
            "{}",
            json!({
                "mode": if args.dry_run { "dry-run" } else { "jev" }, "candidates": count,
                "candidate_limit_reached": found.truncated, "candidate_match_lines": match_lines,
                "results": rows.len(), "candidate_chars": candidate_chars,
                "returned_chars": rows.iter().map(|item| item.candidate.snippet.chars().count()).sum::<usize>(),
                "search_ms": (search_ms * 100.0).round() / 100.0,
                "rerank_ms": (rerank_ms * 100.0).round() / 100.0,
                "api_requests": requests, "input_tokens": input_tokens, "models": models,
            })
        );
    }
    Ok(if rows.is_empty() { 1 } else { 0 })
}
