#![cfg(unix)]

use std::{
    fs,
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

struct Watch(Child);
impl Drop for Watch {
    fn drop(&mut self) {
        // SAFETY: this is the live watcher spawned by this test.
        unsafe {
            libc::kill(self.0.id() as i32, libc::SIGINT);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if matches!(self.0.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn watcher_enriches_settles_and_ignores_reads_then_handles_a_write() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    fs::write(root.join(".jscout.toml"), "version=1\n").unwrap();
    for index in 0..3 {
        let dir = root.join(format!("p{index}"));
        fs::create_dir(&dir).unwrap();
        fs::write(
            dir.join("tsconfig.json"),
            r#"{"compilerOptions":{"noLib":true},"files":["main.ts"]}"#,
        )
        .unwrap();
        fs::write(
            dir.join("main.ts"),
            "export class S { ping() {} } export function run(s: S) { s.ping(); }",
        )
        .unwrap();
    }
    let mut child = Watch(
        Command::new(env!("CARGO_BIN_EXE_jscout"))
            .args([
                "--no-progress",
                "--experimental-tsr",
                "watch",
                "--enrich",
                "--reconcile-seconds",
                "0",
                "--debounce-ms",
                "50",
            ])
            .arg(root)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stderr = child.0.stderr.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if sender.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let mut log = Vec::new();
    {
        let mut until = |predicate: &dyn Fn(&str) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                let line = receiver
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .unwrap_or_else(|error| {
                        panic!("watch did not settle: {error}: {}", log.join("\n"))
                    });
                let done = predicate(&line);
                log.push(line);
                if done {
                    break;
                }
            }
        };
        until(&|line| line.contains("phase=enrich status=succeeded") && line.contains("facts=3"));
        until(&|line| line.contains("status=clean"));
    }
    for _ in 0..3 {
        fs::read(root.join(".jscout.toml")).unwrap();
        fs::read_dir(root)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for index in 0..3 {
            fs::read(root.join(format!("p{index}/main.ts"))).unwrap();
            fs::read_dir(root.join(format!("p{index}")))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
        }
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(line) => assert!(
                !line.contains("status=started"),
                "reads triggered a new run: {line}"
            ),
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(error) => panic!("watch exited: {error}"),
        }
    }
    fs::write(
        root.join("p0/main.ts"),
        "export class S { ping() { return 1; } } export function run(s: S) { s.ping(); }",
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut enriched = false;
    loop {
        let line = receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|error| panic!("write did not finish: {error}: {}", log.join("\n")));
        enriched |= line.contains("phase=enrich status=succeeded");
        let clean = line.contains("status=clean");
        log.push(line);
        if clean && enriched {
            break;
        }
    }
}
