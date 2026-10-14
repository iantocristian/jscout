use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn invoke(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jscout"))
        .args(args)
        .arg(root)
        .output()
        .unwrap()
}
fn success(root: &Path, args: &[&str]) -> (Value, String) {
    let output = invoke(root, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        serde_json::from_slice(&output.stdout).unwrap_or(Value::Null),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}
fn facts(database: &Path) -> Vec<(String, i64, String, String, String, String)> {
    let conn = rusqlite::Connection::open(database).unwrap();
    conn.prepare("SELECT source_file, call_start, project_id, target_anchor, confidence, receiver_type
        FROM checker_enrichments WHERE batch_id IN (SELECT id FROM checker_enrichment_batches WHERE active=1)
        ORDER BY source_file, call_start, project_id, target_anchor").unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)))
        .unwrap().collect::<Result<Vec<_>, _>>().unwrap()
}
fn compare(compiler: &str) {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    #[cfg(unix)]
    if compiler == "typescript7" {
        let native = Path::new(env!("CARGO_MANIFEST_DIR")).join("checker/node_modules/typescript7");
        assert!(
            native.join("package.json").is_file(),
            "run npm ci --prefix checker first"
        );
        fs::create_dir(root.join("node_modules")).unwrap();
        std::os::unix::fs::symlink(native, root.join("node_modules/typescript")).unwrap();
    }
    for index in 0..4 {
        let project = root.join(format!("packages/p{index}"));
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join("tsconfig.json"),
            r#"{"compilerOptions":{"noLib":true},"files":["main.ts"]}"#,
        )
        .unwrap();
        fs::write(project.join("main.ts"), "export class Service { ping() {} } export function run(service: Service) { service.ping(); }\n").unwrap();
    }
    fs::write(
        root.join(".jscout.toml"),
        "version = 1\n[checker]\njobs = 2\n",
    )
    .unwrap();
    success(root, &["--no-progress", "index"]);
    let serial = root.join("serial.db");
    let parallel = root.join("parallel.db");
    fs::copy(root.join(".jscout.db"), &serial).unwrap();
    fs::copy(&serial, &parallel).unwrap();
    let backend = if compiler == "tsr" {
        "--experimental-tsr=true"
    } else {
        "--experimental-tsr=false"
    };
    let sidecar = Path::new(env!("CARGO_MANIFEST_DIR")).join("checker/src/main.mjs");
    let (one, log) = success(
        root,
        &[
            backend,
            "enrich",
            "--all",
            "--checker-jobs",
            "1",
            "--database",
            serial.to_str().unwrap(),
            "--sidecar-path",
            sidecar.to_str().unwrap(),
        ],
    );
    assert!(
        log.contains("checker project 3/4"),
        "serial project position is missing: {log}"
    );
    assert!(
        log.contains("(1 pending, 0 resumed)"),
        "serial counts are missing: {log}"
    );
    assert!(
        !log.contains("checking projects (2 workers)"),
        "CLI must override TOML"
    );
    let (many, log) = success(
        root,
        &[
            backend,
            "enrich",
            "--all",
            "--database",
            parallel.to_str().unwrap(),
            "--sidecar-path",
            sidecar.to_str().unwrap(),
        ],
    );
    assert!(log.contains("checking projects (2 workers)"), "{log}");
    assert_eq!(
        log.lines()
            .filter(|line| line.contains("checking projects (2 workers) 4/4"))
            .count(),
        1,
        "duplicate completion: {log}"
    );
    for report in [&one, &many] {
        assert!(report["peak_project_rss_bytes"].is_u64());
        assert!(report["peak_project_heap_bytes"].is_u64());
        assert!(
            report.get("peak_rss_bytes").is_none(),
            "ambiguous memory label remains"
        );
        assert!(report.get("peak_heap_bytes").is_none());
    }
    assert!(
        !log.contains("checker preparation:"),
        "parallel workers must not overwrite aggregate progress"
    );
    if compiler == "typescript7" {
        assert_eq!(one["checker_version"], "7.0.2");
        assert_eq!(many["checker_version"], "7.0.2");
        assert_eq!(many["checker_source"], "repository");
    }
    assert_eq!(one["facts_published"], 4);
    assert_eq!(many["facts_published"], 4);
    assert_eq!(one["occurrences_queried"], many["occurrences_queried"]);
    assert_eq!(facts(&serial), facts(&parallel));
    let (reused, _) = success(
        root,
        &[
            backend,
            "enrich",
            "--all",
            "--checker-jobs",
            "3",
            "--database",
            parallel.to_str().unwrap(),
            "--sidecar-path",
            sidecar.to_str().unwrap(),
        ],
    );
    assert_eq!(
        reused["exact_batch_reused"], true,
        "worker count must not invalidate checker facts"
    );
}
#[test]
fn embedded_parallel_results_match_serial_and_reuse_across_job_counts() {
    compare("tsr");
}
#[test]
fn sidecar_parallel_results_match_serial_and_reuse_across_job_counts() {
    compare("typescript");
}
#[cfg(unix)]
#[test]
fn native_typescript7_parallel_results_match_serial_and_reuse_across_job_counts() {
    compare("typescript7");
}
#[test]
fn checker_jobs_rejects_zero_and_config_reports_the_value() {
    let repo = tempfile::tempdir().unwrap();
    assert!(
        !invoke(repo.path(), &["enrich", "--checker-jobs", "0"])
            .status
            .success()
    );
    fs::write(
        repo.path().join(".jscout.toml"),
        "version=1\n[checker]\njobs=0\n",
    )
    .unwrap();
    let output = invoke(repo.path(), &["config", "show", "--json"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid checker.jobs"));
    fs::write(
        repo.path().join(".jscout.toml"),
        "version=1\n[checker]\njobs=4\n",
    )
    .unwrap();
    for value in ["0", "17", "64"] {
        let output = invoke(repo.path(), &["config", "show", "--checker-jobs", value]);
        assert!(
            !output.status.success(),
            "invalid CLI job count {value} was accepted"
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("between 1 and 16"));
    }
    success(
        repo.path(),
        &["config", "show", "--json", "--checker-jobs", "16"],
    );
    let (config, _) = success(repo.path(), &["config", "show", "--json"]);
    assert_eq!(config["effective"]["checker"]["jobs"], 4);
    assert_eq!(config["sources"]["checker.jobs"], "config");
    for value in [17, 64] {
        fs::write(
            repo.path().join(".jscout.toml"),
            format!("version=1\n[checker]\njobs={value}\n"),
        )
        .unwrap();
        let output = invoke(repo.path(), &["config", "show", "--json"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("between 1 and 16"));
    }
}

#[test]
fn parallel_project_failure_preserves_healthy_work_and_resumes() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    for index in 0..4 {
        let project = root.join(format!("p{index}"));
        fs::create_dir(&project).unwrap();
        fs::write(
            project.join("tsconfig.json"),
            r#"{"compilerOptions":{"noLib":true},"files":["main.ts"]}"#,
        )
        .unwrap();
        fs::write(
            project.join("main.ts"),
            "export class Service { ping() {} } export function run(s: Service) { s.ping(); }",
        )
        .unwrap();
    }
    success(root, &["--no-progress", "index"]);
    let wrapper_dir = tempfile::tempdir().unwrap();
    let wrapper = wrapper_dir.path().join("checker.mjs");
    let sidecar = Path::new(env!("CARGO_MANIFEST_DIR")).join("checker/src/main.mjs");
    fs::write(&wrapper, r"
import {spawn} from 'node:child_process';
import {createInterface} from 'node:readline';
import {existsSync} from 'node:fs';
const root = process.argv[2];
const child = spawn(process.execPath, [__SIDECAR__, root], {stdio:['pipe','pipe','inherit']});
child.stdout.pipe(process.stdout);
child.on('exit', code => process.exit(code ?? 1));
process.on('exit', () => child.kill());
createInterface({input:process.stdin}).on('line', line => {
    const request = JSON.parse(line);
    if(request.kind === 'resolve_members' && request.project_id === 'p1/tsconfig.json' && existsSync(root+'/fail-one')) {
        process.stdout.write(JSON.stringify({protocol:4,id:request.id,kind:'error',error:{code:'checker_failure',message:'injected project failure'}})+'\n');
    } else child.stdin.write(line+'\n');
});
".replace("__SIDECAR__", &serde_json::to_string(&sidecar).unwrap())).unwrap();
    fs::write(root.join("fail-one"), "fail").unwrap();
    let args = [
        "--no-progress",
        "--checker-jobs",
        "2",
        "enrich",
        "--all",
        "--sidecar-path",
        wrapper.to_str().unwrap(),
    ];
    let failed = invoke(root, &args);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("injected project failure"));
    assert_eq!(facts(&root.join(".jscout.db")).len(), 3);
    fs::remove_file(root.join("fail-one")).unwrap();
    let (resumed, _) = success(root, &args);
    assert_eq!(resumed["projects_resumed"], 3);
    assert_eq!(resumed["facts_published"], 4);
    assert_eq!(resumed["occurrences_queried"], 1);
}

#[cfg(unix)]
#[test]
fn watch_uses_cli_worker_count_and_publishes_parallel_enrichment() {
    use std::{
        io::{BufRead, BufReader},
        process::Stdio,
        sync::mpsc,
        time::{Duration, Instant},
    };
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    for index in 0..3 {
        let project = root.join(format!("p{index}"));
        fs::create_dir(&project).unwrap();
        fs::write(
            project.join("tsconfig.json"),
            r#"{"compilerOptions":{"noLib":true},"files":["main.ts"]}"#,
        )
        .unwrap();
        fs::write(
            project.join("main.ts"),
            "export class S { ping() {} } export function run(s: S) { s.ping(); }",
        )
        .unwrap();
    }
    fs::write(root.join(".jscout.toml"), "version=1\n[checker]\njobs=1\n").unwrap();
    struct Watch(std::process::Child);
    impl Drop for Watch {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Watch(
        Command::new(env!("CARGO_BIN_EXE_jscout"))
            .args([
                "--experimental-tsr",
                "--checker-jobs",
                "3",
                "watch",
                "--enrich",
                "--reconcile-seconds",
                "0",
            ])
            .arg(root)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stderr = child.0.stderr.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if sender.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let mut lines = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let line = receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|_| panic!("watch did not finish startup: {}", lines.join("\n")));
        let clean = line.contains("status=clean");
        lines.push(line);
        if clean {
            break;
        }
    }
    assert!(lines.iter().any(|line| line.contains("checker_jobs=3")));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("checking projects (3 workers)"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("phase=enrich status=succeeded") && line.contains("facts=3"))
    );
    assert_eq!(facts(&root.join(".jscout.db")).len(), 3);
    // SAFETY: this PID is the live watch process spawned by this test.
    unsafe {
        libc::kill(child.0.id() as i32, libc::SIGINT);
    }
    child.0.wait().unwrap();
    reader.join().unwrap();
}
