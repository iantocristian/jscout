#![cfg(unix)]

use std::{
    fs,
    io::{BufRead, BufReader},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

struct Watch {
    child: Child,
    lines: mpsc::Receiver<String>,
    log: Vec<String>,
}
impl Drop for Watch {
    fn drop(&mut self) {
        // SAFETY: this PID belongs to the watcher spawned by this test.
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGINT);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Watch {
    fn until(&mut self, matches: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let line = self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| panic!("watch: {error}: {}", self.log.join("\n")));
            let done = matches(&line);
            self.log.push(line);
            if done {
                return;
            }
        }
    }
}

fn facts(root: &Path) -> i64 {
    rusqlite::Connection::open(root.join(".jscout.db"))
        .unwrap()
        .query_row(
            "SELECT count(*) FROM checker_enrichments WHERE batch_id IN
         (SELECT id FROM checker_enrichment_batches WHERE active=1)",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn check_failure(code: &str, attempts: usize) {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    for project in ["good", "bad"] {
        let directory = root.join(project);
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join("tsconfig.json"),
            r#"{"compilerOptions":{"noLib":true},"files":["main.ts"]}"#,
        )
        .unwrap();
        fs::write(
            directory.join("main.ts"),
            "export class S { ping() {} } export function run(s: S) { s.ping(); }",
        )
        .unwrap();
    }
    // Keep the wrapper and failure counter outside the watched repository.
    let diagnostics = tempfile::tempdir().unwrap();
    let counter = diagnostics.path().join("attempts.txt");
    let wrapper = diagnostics.path().join("checker.mjs");
    let sidecar = Path::new(env!("CARGO_MANIFEST_DIR")).join("checker/src/main.mjs");
    fs::write(&wrapper, r"
import {spawn} from 'node:child_process';
import {createInterface} from 'node:readline';
import {appendFileSync, readFileSync} from 'node:fs';
const root = process.argv[2];
const child = spawn(process.execPath, [__SIDECAR__, root], {stdio:['pipe','pipe','inherit']});
child.stdout.pipe(process.stdout);
child.on('exit', code => process.exit(code ?? 1));
process.on('exit', () => child.kill());
createInterface({input:process.stdin}).on('line', line => {
    const request = JSON.parse(line);
    if(request.kind === 'resolve_members' && request.project_id === 'bad/tsconfig.json'
       && !readFileSync(root+'/bad/main.ts','utf8').includes('recovered')) {
        appendFileSync(__COUNTER__, 'attempt\n');
        process.stdout.write(JSON.stringify({protocol:4,id:request.id,kind:'error',error:{code:__CODE__,message:'missing skipped generic function inference'}})+'\n');
    } else child.stdin.write(line+'\n');
});
".replace("__SIDECAR__", &serde_json::to_string(&sidecar).unwrap())
 .replace("__COUNTER__", &serde_json::to_string(&counter).unwrap())
 .replace("__CODE__", &serde_json::to_string(code).unwrap())).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_jscout"))
        .args([
            "--no-progress",
            "watch",
            "--enrich",
            "--reconcile-seconds",
            "1",
            "--debounce-ms",
            "20",
            "--sidecar-path",
        ])
        .arg(&wrapper)
        .arg(root)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (sender, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if sender.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let mut watch = Watch {
        child,
        lines,
        log: Vec::new(),
    };
    watch.until(|line| line.contains("phase=enrich status=paused"));
    let reason = if code == "checker_failure" {
        "terminal-failure"
    } else {
        "retry-limit"
    };
    assert!(watch.log.last().unwrap().contains(reason));
    watch.until(|line| line.contains("status=partial") && !line.contains("phase="));
    assert_eq!(
        fs::read_to_string(&counter).unwrap().lines().count(),
        attempts
    );
    assert_eq!(facts(root), 1, "healthy project must remain published");
    for _ in 0..2 {
        watch.until(|line| {
            line.contains("phase=refresh") && line.contains("reasons=periodic-reconciliation")
        });
        watch.until(|line| line.contains("status=partial") && !line.contains("phase="));
    }
    assert_eq!(
        fs::read_to_string(&counter).unwrap().lines().count(),
        attempts,
        "unchanged reconciliation must not retry failed checking"
    );
    assert_eq!(
        facts(root),
        1,
        "reconciliation must retain the partial publication"
    );
    assert_eq!(
        watch
            .log
            .iter()
            .filter(|line| line.contains("phase=enrich") && line.contains("status=started"))
            .count(),
        attempts
    );
    fs::write(
        root.join("bad/main.ts"),
        "export class S { ping() { return 'recovered'; } } export function run(s: S) { s.ping(); }",
    )
    .unwrap();
    watch.until(|line| line.contains("phase=enrich status=succeeded"));
    watch.until(|line| line.contains("status=clean"));
    assert_eq!(
        facts(root),
        2,
        "source edit must resume and publish healthy checking"
    );
    assert_eq!(
        fs::read_to_string(&counter).unwrap().lines().count(),
        attempts
    );
}

#[test]
fn terminal_failure_stays_parked_across_reconciliation_and_recovers_on_edit() {
    check_failure("checker_failure", 1);
}

#[test]
fn transient_failure_stops_after_three_attempts_and_recovers_on_edit() {
    check_failure("EBUSY", 3);
}
