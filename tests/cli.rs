use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::Path,
    process::{Command, Output},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use tempfile::TempDir;

struct Project(TempDir);

impl Project {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }
    fn path(&self) -> &Path {
        self.0.path()
    }
    fn write(&self, name: &str, contents: &str) {
        let path = self.path().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    fn corpus(&self) {
        self.write("policy.py", "def retry():\n    return status >= 500\n");
        self.write("metrics.py", "retry_metric = 'attempts'\n");
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_jrg"));
        command
            .current_dir(self.path())
            .env_remove("TYPESAFE_API_KEY")
            .env_remove("RIPGREP_CONFIG_PATH");
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
fn rows(output: &Output) -> Vec<Value> {
    assert!(
        matches!(output.status.code(), Some(0 | 1)),
        "{}",
        text(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn stats(output: &Output) -> Value {
    serde_json::from_str(text(&output.stderr).lines().last().unwrap()).unwrap()
}

#[derive(Clone)]
struct Request {
    authorization: String,
    path: String,
    body: Value,
}
struct Response {
    status: u16,
    body: String,
    headers: Vec<(String, String)>,
}
impl Response {
    fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            body: body.to_string(),
            headers: vec![],
        }
    }
}
struct Server {
    url: String,
    requests: Arc<Mutex<Vec<Request>>>,
    stopped: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(respond: impl Fn(&Request, usize) -> Response + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/systemone", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&requests);
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let worker = thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                // Accepted sockets can inherit nonblocking mode on macOS; only
                // the listener should poll so request bodies can arrive in pieces.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let path = first.split_whitespace().nth(1).unwrap().to_string();
                let mut length = 0;
                let mut authorization = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    let (key, value) = line.split_once(':').unwrap();
                    if key.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap();
                    }
                    if key.eq_ignore_ascii_case("authorization") {
                        authorization = value.trim().to_string();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let request = Request {
                    authorization,
                    path,
                    body: serde_json::from_slice(&body).unwrap(),
                };
                let count = {
                    let mut log = log.lock().unwrap();
                    log.push(request.clone());
                    log.len()
                };
                let response = respond(&request, count);
                let mut headers = format!(
                    "HTTP/1.1 {} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                    response.status,
                    response.body.len()
                );
                for (key, value) in response.headers {
                    headers.push_str(&format!("{key}: {value}\r\n"));
                }
                let _ = stream.write_all(format!("{headers}\r\n{}", response.body).as_bytes());
            }
        });
        Self {
            url,
            requests,
            stopped,
            worker: Some(worker),
        }
    }
    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        let joined = self.worker.take().unwrap().join();
        if !thread::panicking() {
            joined.unwrap();
        }
    }
}
fn success(request: &Request, _: usize) -> Response {
    let candidates = request.body["state"]["candidates"].as_array().unwrap();
    let answers: serde_json::Map<String, Value> = candidates
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let name = Path::new(item["path"].as_str().unwrap())
                .file_name()
                .unwrap()
                .to_str()
                .unwrap();
            let score = match name {
                "policy.py" => 0.97,
                "metrics.py" => 0.12,
                _ => 0.8,
            };
            (
                format!("candidate_{index}"),
                json!({"type": "noul", "noul": score}),
            )
        })
        .collect();
    Response::json(
        200,
        json!({"model": "jev-test", "answers": answers, "usage": {"input_tokens": 100, "output_tokens": 20}}),
    )
}
fn live_args(url: &str) -> Vec<&str> {
    vec![
        "retry",
        "--about",
        "HTTPの再試行判断",
        "--json",
        "--stats",
        "--api-url",
        url,
    ]
}

#[test]
fn dry_run_is_local_and_ignores_top_and_threshold() {
    let project = Project::new();
    project.corpus();
    let output = project.run(&[
        "retry",
        "--about",
        "retry policy",
        "--dry-run",
        "--json",
        "--stats",
        "--top",
        "1",
        "--min-relevance",
        "1",
        "--api-url",
        "invalid",
        "--env-file",
        "missing",
    ]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(rows(&output).len(), 2);
    assert!(rows(&output).iter().all(|row| row["relevance"].is_null()));
    assert_eq!(stats(&output)["api_requests"], 0);
    assert_eq!(stats(&output)["mode"], "dry-run");
}

#[test]
fn merged_context_matches_python_coordinates_and_eof() {
    let project = Project::new();
    project.write("source file.py", "前置き\nretry()\nafter\nretry()\nlast");
    let output = project.run(&[
        "retry",
        "--about",
        "retry",
        "source file.py",
        "-C",
        "1",
        "--dry-run",
        "--json",
    ]);
    let result = rows(&output);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0]["match_lines"], json!([2, 4]));
    assert_eq!(result[0]["start_line"], 1);
    assert_eq!(result[0]["end_line"], 5);
    assert_eq!(
        result[0]["snippet"],
        "前置き\nretry()\nafter\nretry()\nlast"
    );
}

#[test]
fn dense_matches_split_and_truncation_is_explicit() {
    let project = Project::new();
    project.write("dense.py", &"retry\n".repeat(241));
    let output = project.run(&[
        "retry",
        "--about",
        "retry",
        "--dry-run",
        "--json",
        "--stats",
        "--max-candidates",
        "2",
    ]);
    assert_eq!(rows(&output).len(), 2);
    assert_eq!(stats(&output)["candidate_limit_reached"], true);
    assert_eq!(stats(&output)["candidate_match_lines"], 240);
    let output = project.run(&[
        "retry",
        "--about",
        "retry",
        "--dry-run",
        "--json",
        "--stats",
        "--max-candidates",
        "3",
    ]);
    assert_eq!(rows(&output).len(), 3);
    assert_eq!(stats(&output)["candidate_limit_reached"], false);
    assert_eq!(stats(&output)["candidate_match_lines"], 241);
}

#[test]
fn separate_regions_and_exact_limit_are_preserved() {
    let project = Project::new();
    project.write("source.py", "retry\nnone\nretry\n");
    let output = project.run(&[
        "retry",
        "--about",
        "retry",
        "--dry-run",
        "--json",
        "--stats",
        "-C",
        "0",
        "--max-candidates",
        "2",
    ]);
    assert_eq!(rows(&output).len(), 2);
    assert_eq!(stats(&output)["candidate_limit_reached"], false);
}

#[test]
fn rg_flags_ignore_and_configuration_behavior() {
    let project = Project::new();
    fs::create_dir(project.path().join(".git")).unwrap();
    project.write(".gitignore", "ignored.py\n");
    for name in ["visible.py", "other.rs", "ignored.py", ".hidden.py"] {
        project.write(name, "Retry[\n");
    }
    fs::write(project.path().join("binary.bin"), b"\0Retry[\n").unwrap();
    project.write("rg.conf", "--files-with-matches\n");
    let output = project
        .command()
        .env("RIPGREP_CONFIG_PATH", project.path().join("rg.conf"))
        .args([
            "retry[",
            "--intent",
            "retry",
            "-i",
            "-F",
            "--dry-run",
            "--json",
            "-t",
            "py",
        ])
        .output()
        .unwrap();
    let paths: Vec<_> = rows(&output)
        .iter()
        .map(|row| row["path"].as_str().unwrap().to_owned())
        .collect();
    assert!(paths.iter().any(|path| path.ends_with("visible.py")));
    assert!(!paths.iter().any(|path| path.ends_with("ignored.py")));
    // Compare positive type filtering to the installed rg, including its hidden
    // file behavior, rather than hard-coding an upstream version's semantics.
    let expected = Command::new("rg")
        .current_dir(project.path())
        .args(["--no-config", "--files", "--sort", "path", "-t", "py", "."])
        .output()
        .unwrap();
    assert_eq!(paths, text(&expected.stdout).lines().collect::<Vec<_>>());
    let output = project.run(&[
        "Retry",
        "--about",
        "retry",
        "--dry-run",
        "--json",
        "-g",
        "*.py",
    ]);
    assert_eq!(rows(&output).len(), 3);
}

#[test]
fn no_matches_invalid_pattern_stdin_and_oversized_lines() {
    let project = Project::new();
    project.corpus();
    let output = project.run(&["absent", "--about", "retry", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(rows(&output).is_empty());
    let output = project.run(&["[", "--about", "retry", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let output = project.run(&["retry", "--about", "retry", "-"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("stdin"));
    project.write("big.py", &format!("retry{}", "x".repeat(12_000)));
    let output = project.run(&["retry", "big.py", "--about", "retry", "--dry-run"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("exceeds"));
}

#[test]
fn invalid_arguments_fail_before_any_search() {
    let project = Project::new();
    for extra in [
        vec!["--top", "0"],
        vec!["--min-relevance", "NaN"],
        vec!["--timeout", "inf"],
        vec!["--workers", "0"],
        vec!["--batch-size", "0"],
        vec!["--env-file", "x", "--no-env-file"],
    ] {
        let output = project
            .command()
            .args(["retry", "--about", "retry"])
            .args(extra)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
    let output = project.run(&["retry", "--about", " "]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn text_mode_has_unscored_label_and_control_characters_are_escaped() {
    let project = Project::new();
    project.write("source.py", "retry\x1b[2J\n");
    let output = project.run(&["retry", "source.py", "--about", "retry", "--dry-run"]);
    assert!(text(&output.stdout).contains("unscored source.py:1-1"));
    assert!(!output.stdout.contains(&27));
}

#[test]
fn api_contract_ranking_and_usage() {
    let project = Project::new();
    project.corpus();
    let server = Server::new(success);
    let output = project
        .command()
        .env("TYPESAFE_API_KEY", "test-key")
        .args(live_args(&server.url))
        .output()
        .unwrap();
    let results = rows(&output);
    assert_eq!(results[0]["relevance"], 0.97);
    assert_eq!(results[1]["relevance"], 0.12);
    assert_eq!(stats(&output)["input_tokens"], 100);
    assert_eq!(stats(&output)["models"], json!(["jev-test"]));
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/v1/systemone");
    assert_eq!(requests[0].authorization, "Bearer test-key");
    assert_eq!(requests[0].body["state"]["intent"], "HTTPの再試行判断");
    assert!(
        requests[0].body["questions"]["candidate_1"]["instructions"]
            .as_str()
            .unwrap()
            .contains("candidates[1].snippet")
    );
}

#[test]
fn threshold_top_and_multi_batch_sorting() {
    let project = Project::new();
    project.corpus();
    project.write("a.py", "retry\n");
    project.write("z.py", "retry\n");
    let server = Server::new(success);
    let output = project
        .command()
        .env("TYPESAFE_API_KEY", "test")
        .args(live_args(&server.url))
        .args([
            "--batch-size",
            "1",
            "--workers",
            "2",
            "--min-relevance",
            "0.8",
            "--top",
            "3",
        ])
        .output()
        .unwrap();
    assert_eq!(
        rows(&output)
            .iter()
            .map(|row| Path::new(row["path"].as_str().unwrap())
                .file_name()
                .unwrap()
                .to_str()
                .unwrap())
            .collect::<Vec<_>>(),
        ["policy.py", "a.py", "z.py"]
    );
    assert_eq!(server.count(), 4);
    assert_eq!(stats(&output)["input_tokens"], 400);
    let output = project
        .command()
        .env("TYPESAFE_API_KEY", "test")
        .args(live_args(&server.url))
        .args(["--min-relevance", "1"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(rows(&output).is_empty());
}

#[test]
fn overloaded_requests_retry_but_authorization_failures_do_not() {
    let project = Project::new();
    project.corpus();
    for status in [429, 503, 529] {
        let server = Server::new(move |request, count| {
            if count == 1 {
                Response {
                    status,
                    body: "{}".into(),
                    headers: vec![("Retry-After".into(), "0".into())],
                }
            } else {
                success(request, count)
            }
        });
        let output = project
            .command()
            .env("TYPESAFE_API_KEY", "test")
            .args(live_args(&server.url))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
        assert_eq!(stats(&output)["api_requests"], 2);
    }
    let server = Server::new(|_, _| Response::json(401, json!({"secret": "private source"})));
    let output = project
        .command()
        .env("TYPESAFE_API_KEY", "test")
        .args(live_args(&server.url))
        .output()
        .unwrap();
    assert_eq!(server.count(), 1);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!text(&output.stderr).contains("private source"));
}

#[test]
fn later_batch_failure_emits_no_partial_results() {
    let project = Project::new();
    project.corpus();
    let server = Server::new(|request, count| {
        if count == 1 {
            success(request, count)
        } else {
            Response::json(401, json!({}))
        }
    });
    let output = project
        .command()
        .env("TYPESAFE_API_KEY", "test")
        .args(live_args(&server.url))
        .args(["--workers", "1", "--batch-size", "1"])
        .output()
        .unwrap();
    assert_eq!(server.count(), 2);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}

#[test]
fn invalid_json_missing_answers_and_redirects_fail() {
    let project = Project::new();
    project.corpus();
    for kind in [0, 1, 2] {
        let server = Server::new(move |_, _| match kind {
            0 => Response {
                status: 200,
                body: "not JSON".into(),
                headers: vec![],
            },
            1 => Response::json(200, json!({"answers": {}})),
            _ => Response {
                status: 307,
                body: "{}".into(),
                headers: vec![("Location".into(), "https://example.invalid".into())],
            },
        });
        let output = project
            .command()
            .env("TYPESAFE_API_KEY", "test")
            .args(live_args(&server.url))
            .output()
            .unwrap();
        assert_eq!(server.count(), 1);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn timeout_is_not_retried() {
    let project = Project::new();
    project.corpus();
    let server = Server::new(|request, count| {
        thread::sleep(Duration::from_millis(200));
        success(request, count)
    });
    let output = project
        .command()
        .env("TYPESAFE_API_KEY", "test")
        .args(live_args(&server.url))
        .args(["--timeout", "0.05"])
        .output()
        .unwrap();
    assert_eq!(server.count(), 1);
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("timed out"));
}

#[test]
fn credentials_load_from_nearest_project_file_without_shell_execution() {
    let project = Project::new();
    project.corpus();
    fs::create_dir(project.path().join(".git")).unwrap();
    fs::create_dir(project.path().join("subdir")).unwrap();
    project.write(
        ".jrgenv",
        "export TYPESAFE_API_KEY='$(touch marker)'\nOTHER=unused\n",
    );
    let server = Server::new(success);
    let output = project
        .command()
        .current_dir(project.path().join("subdir"))
        .args(live_args(&server.url))
        .arg("../policy.py")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(
        server.requests.lock().unwrap()[0].authorization,
        "Bearer $(touch marker)"
    );
    assert!(!project.path().join("subdir/marker").exists());
}

#[test]
fn environment_overrides_explicit_file_and_disable_flag_works() {
    let project = Project::new();
    project.corpus();
    project.write(".jrgenv", "TYPESAFE_API_KEY=auto\n");
    project.write("custom.env", "TYPESAFE_API_KEY=explicit\n");
    let server = Server::new(success);
    let output = project
        .command()
        .env("TYPESAFE_API_KEY", "environment")
        .args(live_args(&server.url))
        .args(["--env-file", "custom.env"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        server.requests.lock().unwrap()[0].authorization,
        "Bearer environment"
    );
    let output = project
        .command()
        .args(live_args(&server.url))
        .args(["--env-file", "custom.env"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        server.requests.lock().unwrap()[1].authorization,
        "Bearer explicit"
    );
    let output = project
        .command()
        .args(live_args(&server.url))
        .arg("--no-env-file")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("Set TYPESAFE_API_KEY"));
    assert_eq!(server.count(), 2);
}

#[test]
fn credentials_are_excluded_even_with_hidden_globs_and_explicit_paths() {
    let project = Project::new();
    project.corpus();
    for name in [".env", ".env.local", ".jrgenv", ".jrgenv.local", "keys.txt"] {
        project.write(name, "TYPESAFE_API_KEY=retry_super_secret\n");
    }
    let output = project.run(&[
        "retry",
        "--about",
        "retry",
        "--dry-run",
        "--json",
        "--hidden",
        "-g",
        "*",
        "--env-file",
        "keys.txt",
    ]);
    assert_eq!(rows(&output).len(), 2);
    assert!(!text(&output.stdout).contains("retry_super_secret"));
    for name in [".env", ".jrgenv", "keys.txt"] {
        let output = project.run(
            &[
                "retry",
                "name-placeholder",
                "--about",
                "retry",
                "--dry-run",
                "--json",
                "--env-file",
                "keys.txt",
            ]
            .map(|arg| if arg == "name-placeholder" { name } else { arg }),
        );
        assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr));
        assert!(rows(&output).is_empty());
    }
}

#[cfg(unix)]
#[test]
fn credential_symlink_alias_is_excluded() {
    let project = Project::new();
    project.write(".jrgenv", "TYPESAFE_API_KEY=retry_secret\n");
    std::os::unix::fs::symlink(
        project.path().join(".jrgenv"),
        project.path().join("alias.txt"),
    )
    .unwrap();
    let output = project.run(&[
        "retry",
        "alias.txt",
        "--about",
        "retry",
        "--dry-run",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(rows(&output).is_empty());
}

#[test]
fn malformed_dotenv_diagnostic_never_contains_the_secret() {
    let project = Project::new();
    project.corpus();
    project.write(".jrgenv", "TYPESAFE_API_KEY='PRIVATE_SENTINEL\n");
    let output = project.run(&["retry", "--about", "retry", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!text(&output.stderr).contains("PRIVATE_SENTINEL"));
    assert!(text(&output.stderr).contains("Invalid dotenv syntax"));
}

#[test]
fn insecure_endpoint_is_rejected_before_transmission() {
    let project = Project::new();
    project.corpus();
    let output = project
        .command()
        .env("TYPESAFE_API_KEY", "test")
        .args(live_args("http://example.com"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("HTTPS"));
}
