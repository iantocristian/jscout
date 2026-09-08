//! Progress must be visible during work without contaminating JSON/JSONL stdout.
//! Providers are loopback HTTP fixtures; no test uses an external inference service.
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const DEADLINE: Duration = Duration::from_secs(10);

struct Running {
    child: Child,
    stderr_lines: Receiver<String>,
    stdout: Option<JoinHandle<Vec<u8>>>,
    stderr: Option<JoinHandle<Vec<u8>>>,
}

impl Running {
    fn start(root: &Path, arguments: &[&str]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_jscout"));
        // Remove inherited provider credentials and JSCOUT_* settings. The only
        // configuration is the fixture's repository-local .jscout.toml.
        command.env_clear().current_dir(root);
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        let mut child = command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start jscout");
        let mut stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (sender, stderr_lines) = mpsc::channel();
        Self {
            child,
            stderr_lines,
            stdout: Some(thread::spawn(move || {
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes).unwrap();
                bytes
            })),
            stderr: Some(thread::spawn(move || {
                let mut bytes = Vec::new();
                for line in BufReader::new(stderr).lines() {
                    let line = line.unwrap();
                    bytes.extend_from_slice(line.as_bytes());
                    bytes.push(b'\n');
                    let _ = sender.send(line);
                }
                bytes
            })),
        }
    }

    fn wait_for_progress(&self, expected: &str) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let line = self
                .stderr_lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| panic!("missing live progress {expected:?}: {error}"));
            if line.contains(expected) {
                assert!(line.starts_with("progress ["), "{line}");
                return;
            }
        }
    }

    fn finish(mut self) -> Output {
        let deadline = Instant::now() + DEADLINE;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "jscout did not finish");
            thread::sleep(Duration::from_millis(10));
        };
        Output {
            status,
            stdout: self.stdout.take().unwrap().join().unwrap(),
            stderr: self.stderr.take().unwrap().join().unwrap(),
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        // A failed assertion must not leave a child awaiting a provider reply.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn run(root: &Path, arguments: &[&str]) -> Output {
    Running::start(root, arguments).finish()
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture(endpoint: &str, documents: usize) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fs::write(
        root.join(".jscout.toml"),
        format!(
            "version = 1\n[embedding]\nprovider = 'openai'\nmodel = 'fake-docs-2d'\nurl = '{endpoint}'\nquery_prefix = ''\n[llm]\nauth_file = 'unused-auth.json'\n"
        ),
    )
    .unwrap();
    fs::write(
        root.join("greeting.ts"),
        "export function greeting(name: string) { return `Hello ${name}`; }\n",
    )
    .unwrap();
    for index in 0..documents {
        fs::write(
            root.join(format!("guide-{index}.md")),
            format!("# Guide {index}\n\nUnique deployment instruction {index}.\n"),
        )
        .unwrap();
    }
    success(&run(root, &["index", ".", "--no-progress"]));
    directory
}

struct Provider {
    listener: TcpListener,
}

impl Provider {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        Self { listener }
    }

    fn endpoint(&self) -> String {
        format!(
            "http://{}/v1/embeddings",
            self.listener.local_addr().unwrap()
        )
    }

    fn request(&self) -> (TcpStream, Value) {
        let deadline = Instant::now() + DEADLINE;
        let stream = loop {
            match self.listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "no embedding request arrived");
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept embedding request: {error}"),
            }
        };
        stream.set_read_timeout(Some(DEADLINE)).unwrap();
        stream.set_write_timeout(Some(DEADLINE)).unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(line.starts_with("POST /v1/embeddings "), "{line}");
        let mut content_length = None;
        loop {
            line.clear();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                content_length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
        let mut body = vec![0; content_length.expect("request content length")];
        reader.read_exact(&mut body).unwrap();
        (reader.into_inner(), serde_json::from_slice(&body).unwrap())
    }

    fn no_request(&self) {
        assert!(matches!(
            self.listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    }
}

fn reply(mut stream: TcpStream, body: Value) {
    let body = body.to_string();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .unwrap();
    stream.flush().unwrap();
}

fn vectors(count: usize) -> Value {
    json!({
        "object": "list",
        "model": "fake-docs-2d",
        "data": (0..count).map(|index| json!({
            "object": "embedding", "index": index, "embedding": [1.0, 0.0]
        })).collect::<Vec<_>>()
    })
}

#[test]
fn docs_json_has_live_progress_and_cached_reruns_make_no_provider_calls() {
    let provider = Provider::new();
    let directory = fixture(&provider.endpoint(), 1);
    let root = directory.path();
    let mut running = Running::start(root, &["docs", "embed", ".", "--json"]);
    let (stream, request) = provider.request();
    assert_eq!(request["input"].as_array().unwrap().len(), 1);
    // The server has the request but deliberately has not replied. Seeing the
    // work-unit line now proves progress is not printed only after completion.
    running.wait_for_progress("docs embeddings: missing representations (0 cached) 0/1");
    // A second line for this unchanged phase can only be the background
    // heartbeat: the caller remains blocked inside the provider request.
    running.wait_for_progress("docs embeddings: missing representations (0 cached) 0/1");
    assert!(running.child.try_wait().unwrap().is_none());
    reply(stream, vectors(1));
    let output = running.finish();
    success(&output);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["embedded"], 1);
    assert_eq!(report["generation_published"], true);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("missing representations (0 cached) 1/1"),
        "{stderr}"
    );
    assert!(
        stderr.contains("progress [docs embed] completed:"),
        "{stderr}"
    );

    let cached = run(root, &["docs", "embed", ".", "--json"]);
    success(&cached);
    let report: Value = serde_json::from_slice(&cached.stdout).unwrap();
    assert_eq!(report["embedded"], 0);
    assert_eq!(report["cached_reused"], 1);
    assert!(
        String::from_utf8_lossy(&cached.stderr).contains("missing representations (1 cached) 0/0")
    );
    provider.no_request();

    let quiet = run(root, &["docs", "embed", ".", "--json", "--no-progress"]);
    success(&quiet);
    assert_eq!(
        serde_json::from_slice::<Value>(&quiet.stdout).unwrap(),
        report
    );
    assert!(quiet.stderr.is_empty(), "{:?}", quiet.stderr);
    provider.no_request();

    // Quick commands defer progress, but a search waiting on inference must
    // become visible while it is still running, not only after its response.
    let running = Running::start(
        root,
        &[
            "docs",
            "search",
            ".",
            "deployment",
            "--vector",
            "--no-rerank",
            "--json",
        ],
    );
    let (stream, request) = provider.request();
    assert_eq!(request["input"].as_array().unwrap().len(), 1);
    running.wait_for_progress("progress [docs search] running: searching documentation");
    reply(stream, vectors(1));
    let output = running.finish();
    success(&output);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["hits"].as_array().unwrap().len(), 1);
}

#[test]
fn later_docs_batch_failure_reports_only_committed_work() {
    let provider = Provider::new();
    let directory = fixture(&provider.endpoint(), 3);
    let running = Running::start(
        directory.path(),
        &["docs", "embed", ".", "--batch", "2", "--json"],
    );
    let (stream, request) = provider.request();
    assert_eq!(request["input"].as_array().unwrap().len(), 2);
    reply(stream, vectors(2));
    let (stream, request) = provider.request();
    assert_eq!(request["input"].as_array().unwrap().len(), 1);
    // Contract-invalid 200 avoids retry backoff while exercising the real
    // provider response validator and a failure after one committed batch.
    reply(stream, json!({"data": "invalid"}));
    let output = running.finish();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unexpected embedding response"), "{stderr}");
    assert!(
        stderr.contains("failed: docs embeddings: missing representations (0 cached) 2/3"),
        "{stderr}"
    );
    assert!(!stderr.contains("3/3"), "{stderr}");
    assert!(!stderr.contains("completed:"), "{stderr}");
    let connection = rusqlite::Connection::open(directory.path().join(".jscout.db")).unwrap();
    let counts: (i64, i64) = connection.query_row(
        "SELECT (SELECT COUNT(*) FROM embeddings), (SELECT COUNT(*) FROM doc_vector_generations)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(counts, (2, 0));
}

#[test]
fn no_progress_preserves_embedding_error_diagnostics() {
    let provider = Provider::new();
    let directory = fixture(&provider.endpoint(), 1);
    let running = Running::start(
        directory.path(),
        &["docs", "embed", ".", "--json", "--no-progress"],
    );
    let (stream, _) = provider.request();
    reply(stream, json!({"data": "invalid"}));
    let output = running.finish();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unexpected embedding response"), "{stderr}");
    assert!(!stderr.contains("progress ["), "{stderr}");
}

#[test]
fn chunks_jsonl_and_scout_dry_run_json_stay_machine_readable() {
    let directory = fixture("http://127.0.0.1:1/v1/embeddings", 0);
    let root = directory.path();
    let chunks = run(root, &["chunks", "."]);
    success(&chunks);
    let records = serde_json::Deserializer::from_slice(&chunks.stdout)
        .into_iter::<Value>()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(!records.is_empty());
    assert!(String::from_utf8_lossy(&chunks.stderr).contains("progress [chunks]"));
    let quiet = run(root, &["--no-progress", "chunks", "."]);
    success(&quiet);
    assert_eq!(quiet.stdout, chunks.stdout);
    assert!(quiet.stderr.is_empty());

    let scout = run(
        root,
        &["scout", "workflows", ".", "--seed", "greeting", "--dry-run"],
    );
    success(&scout);
    let report: Value = serde_json::from_slice(&scout.stdout).unwrap();
    assert_eq!(report["dry_run"], true);
    assert!(String::from_utf8_lossy(&scout.stderr).contains("scout workflows: planning"));
}
